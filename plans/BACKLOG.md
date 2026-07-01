# Backlog — Deferred Scope & Technical Debt

Work consciously deferred during Phase 2 / Phase 3. Each feature below shipped a
deliberate, scoped subset; the remainder is recorded here so it is not lost.
None of these are regressions — they are known, bounded follow-ups.

## Shipped in Phase 5 backlog sprints

Items completed after the `v0.5.0-beta.1` cut, in the continuous
backlog-sprint phase — sprints 1–11 released as `v0.5.0-beta.2`, sprint 12 as
`v0.5.0-beta.3`. Each stays in its numbered section below — annotated
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
  viewport culling for the cold open followed post-`beta.3` (see item 13).
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
- **Sprint 10 (2026-05-22)** — Vello/WebGPU runtime activation (item 4): the
  worker detects the backend at INIT and builds the engine via
  `Engine::with_vello` or `Engine::new`; `render_document` dispatches to either
  surface. #4 stays open — Canvas2D is still the default, pending a GPU-runner
  golden suite.
- **Sprint 11 (2026-05-22)** — Inline IME composition preview (item 8): the
  active composition is spliced into its paragraph's layout as a transient
  underlined preview and repainted on every `UpdateComposition`.
- **Sprint 12 (2026-05-23)** — Core navigation & selection (item 14, complete):
  arrow-key caret motion with ideal-x preservation, Shift + Arrow extend,
  Ctrl / Cmd + A select-all, and triple-click paragraph selection. A render
  follow-up also taught the Vello path the same faux bold / italic synthesis
  the Canvas2D backend already had from Sprint 6 (commit `55059c2`).

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

**✅ Shipped (post-`beta.3`):** the `/W` glyph-width array. Each CID font
dictionary now carries the font program's own `hmtx` advances in 1000-em units
(`cid.widths().consecutive(0, face.widths_em1000())`) — PDF/A-1b §6.3.5
requires `/W` to match the embedded font. Glyph *positioning* still rides
explicit per-glyph text matrices by design: our `x_advance` carries
justification + Kashida adjustments the font's intrinsic widths do not.

**Deferred:**

- **Font subsetting.** The whole font still embeds rather than just the used
  glyphs. Subsetting would shrink the output further.
- **PDF/A-2 / PDF/X.** `PdfConformance::A2u` and `X3` currently fall back to a
  plain PDF; only `A1b` is implemented.

## 4. Vello (WebGPU) render path

**Shipped (P3-4):** the full `wgpu` + `vello` pipeline — `render::vello_backend`
encodes a `DisplayList` into a `vello::Scene` and presents it through a `wgpu`
surface; `render::backend::detect_backend` is the worker-safe WebGPU probe.

**✅ Shipped (Phase 5 sprint 10) — runtime activation.** The worker picks the
renderer at INIT: `detect_backend()` runs **before** the canvas takes a
context, then the engine is built via `Engine::with_vello` (WebGPU) or
`Engine::new` (Canvas2D), and `render_document` dispatches to whichever surface
the engine holds. The **canvas context conflict** is resolved by construction —
the `OffscreenCanvas` is handed to exactly one of `getContext("2d")` (inside
`Engine::new`) or `wgpu` (inside `VelloRenderer::new`), decided once and never
revisited. Canvas2D is the fallback and is byte-for-byte untouched: Vello is
chosen on a fresh interactive INIT when a WebGPU adapter is acquirable; the
visual-diff `?test=` harness stays Canvas2D-locked *by default* (opt-in below).
The old `init_vello` DCE-retention root is gone — superseded by the real path.

**✅ Shipped (post-`beta.3`) — golden suite + runtime verification:**

- **Vello golden suite.** The `?test=` harness grew a Vello mode:
  `tools/visual-diff/run.mjs --renderer vello` switches the golden dir to
  `golden/vello/` and appends `?renderer=vello` to the harness URL (the worker
  INIT then boots `Engine::with_vello`, failing closed to a visible mismatch
  when no adapter exists). A committed Vello corpus (a4-justified-mixed,
  docx-round-trip, editing-arabic, rich-text) lives under
  `tools/visual-diff/golden/vello/` at the ~0.5 % tolerance
  (PHASE_3_RENDER_RTL.md §2, D3.4).
- **Runtime verification — GitHub issue #1, closed.** Verified end-to-end on a
  real discrete-GPU machine (Chrome, WebGPU enabled): `detect_backend()` now
  picks Vello whenever a WebGPU adapter + device is acquirable, and the Vello
  goldens pass. The dev/CI environment still has no WebGPU, so
  `detect_backend()` resolves to `canvas2d` there.

**Still deferred — #4 stays open until Vello is the *default*:**

- **GPU CI runner.** The Vello golden farm can only run on GPU hardware — CI
  cannot exercise it continuously until a GPU runner exists.
- **Promoting Vello to default.** Canvas2D stays the fallback and the CI /
  golden-farm default until the Vello suite is green on a GPU runner.

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

**✅ Shipped (post-`beta.3` follow-up).** `Command::SetParagraphDirection`
sets an explicit per-paragraph override (`<w:bidi>`) over a range — a real
edit (undo + reflow) — dispatched from the `AlignmentButtons` LTR / RTL
toggle in `@nge/ui`. `Event::SelectionChanged` now also carries
`paragraph_direction`: the caret paragraph's *effective* direction (the
explicit override if set, else the auto-detected one; `None` over a
mixed-direction selection, so the toggle renders indeterminate).

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
composed string on a committing end; the in-progress text also shows in the OS
IME candidate popup, anchored at the caret-tracked hidden textarea.

**✅ Shipped (Phase 5 sprint 11).** `build_page` splices the active composition
into its paragraph's layout: `composition_layout_spans` shifts the committed
style spans past the inserted bytes and gives the composition an `underline`
span, so the Sprint-6 decoration renderer draws the IME marker and the line
reflows around it. The splice is transient — gated by a `with_composition`
flag, it reaches only the interactive paint, never the document model, the
layout cache, PDF export, or hit-test geometry. `UpdateComposition` (and a
cancelled `EndComposition`) repaint, so the preview tracks every keystroke and
clears on commit / cancel. Riding the normal `DisplayList`, the preview renders
on both the Canvas2D and Vello backends with no backend-specific code.

**Still deferred:** `target_range`-specific styling — the whole composition is
underlined uniformly rather than distinguishing the actively-converting
sub-segment. Real-OS-IME verification (CJK conversion, candidate windows)
needs macOS / Windows hardware — GitHub issue #2.

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
  carries a `font_family: Option<FontFamily>` (then a closed enum; since
  widened to a string-backed `Custom` variant — GitHub issue #23, closed);
  `Command::ApplyFormatting` sets it, and `FontStack::resolve` takes
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

**✅ Viewport-culled lazy pagination — shipped (post-`beta.3`, audit gap
C.H1).** `build_page` no longer lays out the whole document eagerly: layout
stops past a cull budget beyond the viewport band (`LazyLayoutState` in
`engine-wasm`). `Command::SetViewport` records the visible band (rAF-coalesced
from the shell's scroll / resize handler), `Command::ExpandLayout { target_y }`
flows further pages on scroll / `Ctrl+End`, and a running virtual-height
estimate keeps the scrollbar stable, converging to the real height as
`ExpandLayout` calls fill in. One residue: `tools/perf/run.mjs` still reports
the open-doc metric outside its `--strict` gate — re-gating it is a
harness-side follow-up now that the cold open is cull-bounded.

## 14. Core navigation & selection — the "missing basics"

**✅ Shipped (Phase 5 sprint 12).** Pre-`beta.2` the editor had no arrow-key
caret motion, no `Ctrl/Cmd+A`, and no triple-click paragraph selection —
everything had to be driven through the pointer or the Phase-4 RPC. Sprint 12
closed all three:

- **`Command::MoveCaret { direction, extend }`** routes the four arrow keys.
  Left / Right step one Unicode char in logical order (paragraph-local, hopping
  the boundary at the ends); Up / Down walk to the adjacent `LineGeom` and snap
  to the slot nearest the stored ideal-x. With `extend: true` the anchor stays
  put so Shift + Arrow extends the selection.
- **Ideal-x preservation** lives on `SelectionState` — a `Option<f32>` device-
  pixel column that is `None` everywhere except mid-vertical-walk. Every
  horizontal move, click, edit, paste, or selection-set builds a fresh
  `SelectionState` with `ideal_x: None`, so the reset is automatic; only
  `do_move_caret(Up | Down)` carries the value forward. Vertical motion
  through short lines therefore keeps its column rather than drifting.
- **`Command::SelectAll`** — previously a `phase3_stub` — anchors at byte 0
  of paragraph 0 and runs the caret to the last paragraph's byte length.
  `Ctrl/Cmd+A` in the hidden textarea dispatches it (`preventDefault` stops
  the textarea acting on its own empty content).
- **`Command::SelectParagraphAt { at }`** hit-tests then selects `[0..len)`
  of the paragraph under the click. `pointer.ts` fires it from a `click`
  listener when `e.detail === 3`, bumping the same `gesture` counter as
  `dblclick` so the third pointerdown's in-flight `placeCaret` is dropped.
