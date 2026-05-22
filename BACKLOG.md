# Backlog — Deferred Scope & Technical Debt

Work consciously deferred during Phase 2 / Phase 3. Each feature below shipped a
deliberate, scoped subset; the remainder is recorded here so it is not lost.
None of these are regressions — they are known, bounded follow-ups.

## Shipped in Phase 5 backlog sprints

Items completed after the `v0.5.0-beta.1` cut, in the continuous
backlog-sprint phase. Each stays in its numbered section below — annotated
**✅ Shipped** — so the cross-references between items keep resolving.

- **Sprint 1 (2026-05-22)** — Pending (sticky) formatting (item 11, complete);
  paragraph alignment (item 9 — the `AlignmentPicker` half; per-span font
  family is still deferred).
- **Sprint 2 (2026-05-22)** — Discontinuous BiDi selection rectangles
  (item 7, complete); multi-line plain paste (item 12, complete).
- **Sprint 3 (2026-05-22)** — Kashida Tatweel ink injection (item 2,
  complete); bold / italic face resolution (item 1 — the bold/italic
  sub-item, via faux synthesis; underline, strikethrough, background colour
  and docx `<w:rPr>` are still deferred).
- **Sprint 4 (2026-05-22)** — Incremental relayout (item 13): paragraph
  layout caching, an O(N) line breaker, and a cached parsed font face. The
  50-page open dropped ~22 s → ~4.7 s and insert p95 ~27 ms → ~8 ms;
  sub-budget cold open still needs viewport culling.
- **Sprint 5 (2026-05-22)** — Dynamic line height (item 5, complete);
  first-strong paragraph auto-direction (item 6 — a UI override toggle is
  still deferred).
- **Sprint 6 (2026-05-22)** — Text decorations and per-span font family:
  underline + strikethrough strokes and background-colour highlight (item 1 —
  the render sub-items; the docx `<w:rPr>` round-trip followed in sprint 7);
  the toolbar `FontFamilyPicker` (item 9, now complete).
- **Sprint 7 (2026-05-22)** — Interoperability: the docx `<w:rPr>`
  run-property round-trip closes item 1; rich clipboard — HTML + `.docx`-
  fragment copy and an HTML-parsing `PasteHtml` — closes item 12.
- **Sprint 8 (2026-05-22)** — Professional PDF export (item 3): `FlateDecode`
  stream compression and a `/ToUnicode` CMap for text extraction. `/W` glyph
  widths, font subsetting and PDF/A-2 / PDF/X are still deferred.
- **Sprint 9 (2026-05-22)** — Fine-grained accessibility deltas (item 10): the
  per-mutation broadcast is now an `AccessibilityTreeDelta` of `A11yPatch`es; a
  keystroke patches one mirror `<p>` instead of rebuilding the whole tree.

## 1. Rich text formatting

**Shipped (rich-text spans):** per-character style spans (`engine::StyleRun`)
in the document model, `Command::ApplyFormatting` (split / merge / coalesce),
and per-span **font size + colour** carried through layout and render.

**✅ Fully shipped.** Per-span styling renders (size, colour, bold/italic
faces, underline, strikethrough, background colour) and now survives the
`.docx` round-trip — every numbered sub-item below is complete.

- **✅ Bold / italic face resolution — shipped (Phase 5 sprint 3).** `FontStack`
  now resolves a face by script **and** weight/slant; `LoadedFont` reports its
  own bold/italic metadata so a real variant face is used when one is loaded.
  No bold/italic `.ttf` ships for the bundled families (Amiri, an Arabic Naskh
  face, has no italic at all), so the renderer **synthesizes** the missing
  styles — faux bold dilates the rasterized alpha mask, faux italic shears it
  (`render::synth`). Real designed faces remain a drop-in upgrade.
- **✅ Underline & strikethrough — shipped (Phase 5 sprint 6).**
  `build_page_scene` emits a thin `FillRect` per decoration — the underline
  just below the baseline, the strikethrough centred near the x-height
  midpoint — each positioned and sized from the run's pixel size.
- **✅ Background colour — shipped (Phase 5 sprint 6).** `build_page_scene`
  emits a `FillRect` spanning the run's advance and the line height *before*
  its glyphs; `paint_alpha_glyph` then composites each glyph over the
  highlight, so `put_image_data` does not punch transparent holes through it.
- **✅ docx `<w:rPr>` round-trip — shipped (Phase 5 sprint 7).** The
  `format-docx` reader maps each `<w:r>`'s `<w:rPr>` (`<w:b>`, `<w:i>`,
  `<w:u>`, `<w:strike>`, `<w:color>`, `<w:highlight>` / `<w:shd>`,
  `<w:rFonts>`) onto a `SpanStyle`; the writer emits one `<w:r>` per style
  segment with the matching `<w:rPr>`. A span-free paragraph still serializes
  as a single bare run, so the round-trip harness's plain fixtures stay
  byte-stable. Font *size* is excluded — `px` ↔ half-point reconciliation is a
  separate follow-up.

## 2. Kashida justification — Tatweel glyph insertion

**✅ Shipped (D3.5 + Phase 5 sprint 3).** D3.5 landed joining-type-aware
candidate detection via `icu_properties`, the Microsoft P1–P5 priority bands,
and one Kashida per word at its highest-priority stroke.

Phase 5 sprint 3 replaced the white-gap elongation with real ink: `layout`
injects synthetic `U+0640` Tatweel glyphs into the `VisualRun`, tiled to fill
the elongation width with the sub-Tatweel remainder parked on the elongated
glyph's advance. Every synthetic glyph copies the elongated glyph's `cluster`
and carries a `synthetic` flag, so the renderer draws them while caret /
hit-test slot emission skips them — the `source_range` / `cluster` byte-to-glyph
map is preserved.

## 3. PDF export — compression, subsetting, ToUnicode

**Shipped (D3.7):** `crates/format-pdf` — box tree to a single-page PDF, Y-axis
inversion, full `Type0` / `CIDFontType2` / `Identity-H` font embedding.

**Shipped (D5.4):** strict PDF/A-1b conformance for `PdfProfile::A1b` — a PDF
1.4 header, an `OutputIntent` with an embedded sRGB ICC profile, an XMP
metadata packet, and a document `/ID`. The ICC profile is **synthesized at
build time** by `crates/format-pdf/build.rs`, so no binary blob lands in the
source tree — CLAUDE.md's no-blobs rule holds without an exception.
`tools/pdf-validate` is the veraPDF harness, surfaced by the non-blocking CI.

**✅ Shipped (Phase 5 sprint 8):** `FlateDecode` stream compression and a
`/ToUnicode` CMap.

- **Stream compression.** The content stream and every embedded `FontFile2`
  program are zlib-compressed and tagged `/Filter /FlateDecode`; a
  `FontFile2`'s `/Length1` keeps the *uncompressed* program length, per the
  spec. A mixed Arabic/English page embedding the 431 KB Amiri face exports at
  ~215 KB — half the raw font alone, where an uncompressed export would exceed
  it.
- **`/ToUnicode` CMap.** Each `Type0` font carries a `/ToUnicode` CMap (built
  with `pdf_writer`'s `UnicodeCmap`) harvested from the box tree: a glyph's
  `cluster` byte-range over its run's `source_range` resolves to the source
  characters it consumed. A ligature glyph maps to *all* its characters, and
  the four Arabic positional forms all map back to one base letter — so
  `pdftotext` / `mutool` extract real Unicode, not glyph-id gibberish.

**Deferred:**

- **`/W` glyph widths + font subsetting.** Glyph advances still ride the
  content stream's per-glyph text matrices rather than a `/W` array, and the
  whole font embeds rather than just the used glyphs. Subsetting would shrink
  the output further.
- **PDF/A-2 / PDF/X.** `PdfConformance::A2u` and `X3` currently fall back to a
  plain PDF; only `A1b` is implemented.

## 4. Vello (WebGPU) render path

**Shipped (P3-4):** the full `wgpu` + `vello` pipeline is implemented and kept
reachable via `Engine::init_vello` (a dead-code-elimination retention root).
Canvas2D remains the hardcoded active renderer; the worker never calls
`init_vello`.

**Deferred:** making Vello the default active path. Blockers identified:

- **Canvas context conflict.** A canvas element is one-context-for-life — a
  `2d` and a `webgpu` context cannot coexist on the same element, so renderer
  activation must choose at INIT.
- **Separate golden suite.** Vello (skrifa, GPU compute) rasterizes glyphs
  differently from Canvas2D (Skia). The current 0.000 %-diff goldens will not
  match a Vello render; a Vello-specific suite at ~0.5 % tolerance
  (PHASE_3_RENDER_RTL.md §2, D3.4) is required.
- **Runtime availability.** WebGPU in a Web Worker with `OffscreenCanvas` under
  headless Chrome is unverified.

## 5. Line height — dynamic from run metrics

**✅ Shipped (Phase 5 sprint 5).** `layout_paragraph` computes each line's
height from its runs: `line_extents` max-reduces ascent and descent over the
line's `VisualRun`s (each from `LoadedFont::metrics` at the run's pixel size),
so `LineBox.height = max_ascent + max_descent` and `LineBox.baseline =
max_ascent`. Lines stack by accumulated height, so a line carrying a larger
font size or a taller script grows to fit instead of clipping its neighbours.
A line with no resolvable run metrics falls back to the configured
`line_height`.

## 6. Paragraph base direction — auto-detection

**✅ Shipped (Phase 5 sprint 5).** `text_pipeline::first_strong_direction`
scans a paragraph's text for its first strong character (UAX #9 P2/P3) via
`icu_properties`' `BidiClass` — class `L` → LTR, class `R`/`AL` → RTL.
`build_page` resolves each paragraph's base direction from it, falling back to
the document direction when the text has no strong character. BiDi resolution
and direction-relative alignment then follow per paragraph, so an English
paragraph below an Arabic one aligns left while the Arabic aligns right,
automatically.

**Still deferred:** a UI / keyboard toggle to override the auto-detected
direction; `Event::SelectionChanged.direction` still reports the document
direction rather than the caret paragraph's.

## 7. Selection rendering — discontinuous BiDi rectangles

**Shipped (D4.6 pragmatic subset):** per-line bounding selection rectangles —
the engine emits one `Rect` per line spanning the selected range's leftmost to
rightmost caret slot (`selection_rects_geom` in `engine-wasm`). Exact for
LTR-only, RTL-only, and whole-line selections.

**✅ Shipped (Phase 5 sprint 2).** `selection_rects_geom` clips the selected
byte range against each line's `VisualRun`s and emits one tight rect per
run-clipped sub-span. A contiguous logical selection crossing an LTR↔RTL seam
now renders as separate, accurate segments instead of one bounding rect that
over-covers the unselected gap. A single-run (non-BiDi) line still yields one
rect, as before.

## 8. IME composition — inline on-canvas preview

**Shipped (D4.4 commit-on-end):** the engine tracks an IME composition
(`BeginComposition` / `UpdateComposition` / `EndComposition`) and commits the
composed string on a committing end. The in-progress text shows in the OS IME
candidate popup, which anchors at the caret-tracked hidden textarea. Arabic
(direct `insertText`, no composition) and CJK both commit correctly.

**Deferred:** rendering the in-progress composition inline on the canvas — the
provisional underlined text appearing in the document flow as it is typed,
re-rendered on every `UpdateComposition`. Needs a transient composition layer
threaded through layout + render so the engine can paint text that is not in
the document model, plus `target_range` underline styling. Until then a
composition is visible only in the OS popup, not in the page itself.

## 9. Toolbar — paragraph alignment + font family

**Shipped (D4.7):** the formatting toolbar — Undo, Redo, Bold, Italic,
Underline, font size, text colour. Bold/italic/underline are stored on
`SpanStyle` and round-trip (the buttons reflect them), though layout/render
still ignore them (item 1 above). Size + colour render immediately.

**✅ Shipped.** §11's `AlignmentPicker` shipped in Phase 5 sprint 1 and the
`FontFamilyPicker` in Phase 5 sprint 6 — this item is complete.

- **✅ Paragraph alignment — shipped (Phase 5 sprint 1).** `engine::Paragraph`
  carries an `alignment: Option<Alignment>`, threaded through every model
  mutation; `Command::SetParagraphAlign` sets it over a range; `build_page`
  passes each paragraph's own value to `layout_paragraph`; `alignment_origin_x`
  resolves `End` direction-relatively. The toolbar `AlignmentPicker` maps the
  absolute Left/Center/Right/Justify buttons onto the engine's
  direction-relative model via the document base direction.
- **✅ Per-span font family — shipped (Phase 5 sprint 6).** `engine::SpanStyle`
  carries a `font_family: Option<FontFamily>` (an enum, so `SpanStyle` stays
  `Copy`); `Command::ApplyFormatting` sets it, and `FontStack::resolve` takes
  a family override that wins over the script default when that face is
  loaded. `App.tsx` loads three families (Amiri, Liberation Sans, Noto Naskh
  Arabic) and the toolbar `FontFamilyPicker` dispatches the change.

## 10. Accessibility tree — fine-grained deltas

**Shipped (D4.8):** the engine emits a full `A11yTree` (every paragraph, each
split into style runs) after every document mutation.

**✅ Shipped (Phase 5 sprint 9).** The per-mutation broadcast is now an
`AccessibilityTreeDelta` carrying `A11yPatch`es — `Replace` (the first sync of
any engine instance, including post-recovery), `Update`, `Insert` and
`Remove`. `engine-wasm` caches the last broadcast tree and `diff_a11y` runs a
prefix/suffix trim against the freshly built one, so a keystroke confined to
one paragraph emits a single `Update`. `ts/src/a11y/tree.ts` reconciles the
patches into the `.a11y-mirror` DOM, replacing only the changed `<p>` — the
browser no longer rebuilds the whole accessibility subtree per keystroke.

**Still deferred:** stable per-paragraph ids. `diff_a11y` matches paragraphs
by content, which is optimal for typing, Enter and delete-merge but re-emits a
genuinely *moved* paragraph as remove + insert. True positional identity needs
an id on `engine::Paragraph` — a wider document-model change left for later.
The `A11yParagraph.id` field (previously the array index, which is not stable)
was dropped rather than kept as a misleading value.

## 11. Pending (sticky) formatting

**✅ Shipped (Phase 5 sprint 1).** Clicking Bold/Italic/Underline with a
collapsed caret arms a pending `SpanStyle` overlay on the engine
(`Engine::pending_format`). The next interactive `InsertText` applies it to
the typed run; it persists across keystrokes and is cleared on a caret move
(`SetSelection` / `ExtendSelection` / `SelectWordAt`). `attrs_at_caret`
reflects the armed style, so the toolbar button reads as pressed before any
text exists.

One refinement over the original sketch: the overlay is cleared on the caret
move, **not** on the consuming `InsertText` — clearing on insert would style
only the first character of a multi-character typed run.

## 12. Clipboard — rich payloads + multi-line paste

**Shipped (D4.9):** plain-text clipboard — `GetSelectionAsClipboard` returns
the selection text, `copy`/`cut` write it via `navigator.clipboard.writeText`,
`paste` reads `readText` into `PastePlain`. Bound to the hidden textarea's
native `copy` / `cut` / `paste` events.

**✅ Fully shipped** — rich copy/paste landed in Phase 5 sprint 7.

- **✅ Rich copy — shipped (Phase 5 sprint 7).** `GetSelectionAsClipboard`
  fills `ClipboardPayload.html` (semantic HTML from `engine::html::to_html`)
  and `docx_fragment` (a minimal standalone `.docx` of the selection). `copy`
  / `cut` write a multi-MIME `ClipboardItem` carrying `text/plain` +
  `text/html` via `navigator.clipboard.write`.
- **✅ `PasteHtml` — shipped (Phase 5 sprint 7).** `Command::PasteHtml` parses
  HTML into styled paragraphs (`engine::html::from_html`, a hand-rolled tag
  tokenizer over a style stack) and splices them at the caret via
  `DocumentTree::insert_rich`. `paste` reads `text/html` when present, else
  `text/plain`. A dedicated `PasteDocxFragment` is intentionally **not** built:
  no browser exposes a raw `.docx` blob from a system paste — Word and Google
  Docs both place `text/html` on the clipboard, which `PasteHtml` consumes.
  The custom-MIME `.docx` clipboard *write* is likewise skipped (negligible
  browser support); `docx_fragment` is still generated for completeness.
- **✅ Multi-line paste — shipped (Phase 5 sprint 2).** `DocumentTree::
  insert_multiline` splits pasted text on newlines (`\r\n` / `\r` normalized)
  into separate paragraphs, carrying the caret to the end of the last line;
  `do_paste_plain` routes multi-line text through it. A newline-free paste
  keeps the original single-line path.

## 13. Incremental relayout

**✅ Shipped (Phase 5 sprint 4).** Three changes took the synthetic 50-page
open from ~22 s to ~4.7 s and insert-char p95 from ~27 ms to ~8 ms:

- **Paragraph layout cache.** `build_page` memoizes `layout_paragraph` in an
  LRU keyed by a content + render-config hash (`engine-wasm`). An edit changes
  only the edited paragraph's hash — every other paragraph is a cheap clone
  shifted by a Y delta, with no re-shaping. Editing a multi-page document is
  now incremental.
- **O(N) line breaker.** `compose_lines` accumulates per-segment widths
  instead of re-measuring every growing prefix — O(breaks²) → O(breaks).
- **Cached parsed face.** `LoadedFont` builds its `rustybuzz::Face` once at
  load instead of re-parsing the whole font file on every `shape_text` call —
  this was the dominant cold-open cost.

**Still deferred:** a *cold* open is still ~4.7 s — over the §6 2.5 s budget —
because all 1000 paragraphs are genuinely laid out once (the cache is empty on
the first open). Driving the cold open under budget needs **viewport
culling**: laying out only the visible page and deferring the rest. That is a
larger architectural change than caching — `build_page`, hit-testing and the
`PageBox` contract all assume a whole-document layout — so `tools/perf/run.mjs`
keeps the open-doc metric out of its `--strict` gate until then.
