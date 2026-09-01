# `tools/differential` — differential oracle harness (issue #89)

Compares our engine's PDF export against LibreOffice headless (and,
manifest-only, Word 365) as black-box baselines for pagination, line
breaking, and rendering. This is the epic's *black-box* clean-room channel.

## Clean-room statement

**This harness compares PDF output only.** It shells out to `soffice`
(headless), `pdftotext`, `pdfinfo`, and `pdftoppm`, and diffs page counts,
extracted text, line-box geometry, and low-DPI rasters. It never reads,
greps, or otherwise opens anything under `/data/code/reference/`, and it
never inspects LibreOffice's or Word's source or internal state — only the
`.pdf` bytes they produce from a `.docx` input. See
`plans/cleanroom/PROTOCOL.md`: this is the "black-box observation" rung
(source-preference order step 4), the legal channel for comparing our
behavior against a mature competitor without touching its source.

## Layout

```
tools/differential-native/       Rust bin crate: read_docx -> native layout
                                  (crates/layout + crates/text-pipeline) ->
                                  crates/format-pdf::export_pdf. No wasm, no
                                  browser. Also generates the Arabic/RTL
                                  fixture corpus (--gen-fixtures).
tools/differential/
  run.mjs                        The Node runner (this directory).
  package.json                   pixelmatch + pngjs (same deps as
                                  tools/visual-diff).
  fixtures/arabic/*.docx         The committed Arabic/RTL fixture corpus
                                  (hand-written OOXML — see
                                  differential-native/src/fixtures.rs).
  fixtures/arabic/_manifest.json Per-fixture description + Word-oracle
                                  status (every entry is PENDING here — no
                                  Word 365 available in this environment).
```

## Running it

```sh
# 1. Build the native PDF exporter.
cargo build -p differential-native --release

# 2. (Re)generate the Arabic/RTL fixture corpus (idempotent; only needed
#    after editing tools/differential-native/src/fixtures.rs).
cargo run -p differential-native --release -- --gen-fixtures

# 3. Run the comparison. Default corpus = the Arabic/RTL set +
#    crates/format-docx/tests/fixtures/*.docx.
cd tools/differential
node run.mjs
```

Useful flags: `node run.mjs <dir-or-file.docx>...` for an explicit corpus,
`--out <dir>` (default `tmp/differential/`, gitignored), `--raster-dpi <n>`
(default 100), `--json` (also dump the ranked report as JSON to stdout).
Every generated `.pdf` / `.png` / `report.json` lands under `tmp/` — never
commit them (see `.gitignore`).

`soffice` is invoked with a fresh `-env:UserInstallation=file://<tmpdir>`
profile per fixture (via `fs.mkdtempSync`), so parallel/back-to-back runs
never collide on a shared LibreOffice user-profile lock.

## What it measures, per fixture

1. **Page count** (`pdfinfo`).
2. **Page size** (`pdfinfo`'s `Page size: W x H pts` line) — a resolution-
   independent geometry check, reported separately from raster diffing (see
   "Known findings" below).
3. **Per-page text ordering** — `pdftotext -layout` (NOT `-bbox`: bbox mode
   does not apply poppler's visual-to-logical bidi remap, so RTL words come
   out character-reversed; confirmed against this harness's own output
   during development). Compared via normalized Levenshtein similarity
   (0-1) rather than exact match, because known-acceptable extraction noise
   (e.g. a duplicated combining mark around Arabic tanween, also observed
   during development) must not drown a real ordering disagreement.
4. **Line-break oracle** — `pdftotext -bbox-layout` line geometry
   (`xMin`/`yMin`/`xMax`/`yMax`), grouped into paragraph-like chunks by
   vertical gap (`groupIntoParagraphs`, threshold 2pt) and diffed by
   per-paragraph line count. This is the highest-signal typographic metric
   per the issue.
5. **Low-DPI raster similarity** — `pdftoppm` renders each page to PNG,
   compared with `pixelmatch` (same library `tools/visual-diff` uses).
   Skipped (with a stated reason, not silently) if `pdftoppm` is not on
   PATH, or per-page if the two PDFs' rendered dimensions disagree (see
   "Known findings").

Ranked by a severity score combining all of the above (page-size and
page-count mismatches weigh heaviest); worst-first in the console output
and in `report.json`.

## Word 365 ground truth — PENDING

The manifest schema (`fixtures/arabic/_manifest.json`, one entry per
fixture: `description`, `generator`, `word_pdf`, `word_pdf_status`) and the
comparison path are both wired for it: `word_pdf` is currently `null` /
`"PENDING"` on every fixture because no Word 365 instance is available in
this environment (per the issue: "Word 365 (VM, per OOXML_ROADMAP decision
1)"). Once a `word_pdf` is checked in for a fixture, `run.mjs` treats any
oracle PDF generically (`convertLibreOffice`'s output and a future
`word_pdf` both just become "the oracle path" fed to the same extraction +
comparison functions) — wiring in a real Word oracle is a manifest change,
not a `run.mjs` change.

## Known limitations (scoped, not silently approximated)

- **`differential-native`'s pipeline is deliberately smaller than
  `engine-wasm`'s interactive one.** No headers/footers, no PAGE/NUMPAGES/
  DATE field evaluation, no footnotes, no inline images, no hyperlinks, no
  tracked-change overlays, no multi-column sections, no custom `<w:tabs>`
  stops, table cells always top-aligned with no cross-row vertical-merge
  height sync. See the module docs in `tools/differential-native/src/
  pipeline.rs`. None of these are exercised by the Arabic/RTL corpus.
- **The paragraph-grouping heuristic is a best-effort proxy, not ground
  truth.** It groups `<line>` elements by vertical gap; it has no way to
  know a real paragraph boundary independent of a rendered gap. The Arabic
  fixtures stamp `<w:spacing w:after="120"/>` (6pt) on every top-level
  paragraph specifically to give it a reliable signal — confirmed during
  development that intra-paragraph line-to-line gaps measure ~0pt on both
  this harness's own PDFs and LibreOffice's, vs. a clean ~6pt at paragraph
  boundaries. Documents with zero paragraph spacing (most of the
  pre-existing `crates/format-docx/tests/fixtures/*.docx`) fall back to
  reporting one paragraph per contiguous block of flowing text. **Table
  content breaks this heuristic** (`rtl_table.docx`'s cells sit side by
  side at the same y, so a flat top-to-bottom line list does not reflect
  reading order) — its paragraph-level numbers are noise, not a real
  disagreement; only its page count / text-similarity / raster numbers are
  trustworthy.
- **Poppler's `<block>` grouping is asymmetric across the two PDFs** (this
  harness's own PDF: one block per line; LibreOffice's PDF: one block per
  paragraph) — confirmed during development, which is why paragraph
  grouping is re-derived from `<line>` geometry instead of trusted from
  `<block>`.
- **No pixel-exact parity, by design** (the issue's own Out-of-scope): font
  substitution and hinting differences between our embedded Amiri/Liberation/
  Noto-Naskh faces and whatever LibreOffice substitutes are expected. The
  raster bands (`close` ≤5%, `moderate` ≤15%, `divergent` >15%) are for
  triage, not a pass/fail gate — unlike the same-renderer
  `tools/visual-diff` tiers in `.claude/rules/visual-diff.md`.

## Known findings (real, from a run of this harness)

- **Page-size default mismatch.** A `.docx` whose `<w:sectPr>` omits
  `<w:pgSz>` lays out as A4 in our engine (`engine::PageGeometry::default()`)
  but as US Letter in this environment's LibreOffice (its locale default).
  Every one of the 15 pre-existing `crates/format-docx/tests/fixtures/*.docx`
  fixtures — none of which specify `<w:pgSz>` — shows this. It is one root
  cause manifesting 15 times, not 15 independent bugs; `run.mjs` reports it
  as a dedicated `pageSizeMismatch` field precisely so it doesn't masquerade
  as 15 separate raster/line-break disagreements. The Arabic/RTL fixtures
  pin an explicit A4 `<w:pgSz>`/`<w:pgMar>` specifically to isolate their
  real purpose (justify/kashida/bidi/list/table agreement) from this
  already-known, separately-reported gap.
- **Systematic line-wrap width disagreement on justified Arabic body text.**
  `long_document.docx` (24 identical-filler justified RTL paragraphs) wraps
  every single paragraph into 3 lines in our engine vs. 4 lines in
  LibreOffice (72 vs. 96 total lines) — a consistent, reproducible
  narrower-measured-width (or LibreOffice-wider) disagreement worth
  triaging (Amiri glyph advance widths vs. LibreOffice's Arabic-capable
  substitute font).
- See the "first 10 disagreements" list relayed in the PR / issue-filing
  pass for the full ranked breakdown from an actual run.
