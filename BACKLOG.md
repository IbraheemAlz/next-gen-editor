# Backlog — Deferred Scope & Technical Debt

Work consciously deferred during Phase 2 / Phase 3. Each feature below shipped a
deliberate, scoped subset; the remainder is recorded here so it is not lost.
None of these are regressions — they are known, bounded follow-ups.

## 1. Rich text formatting

**Shipped (rich-text spans):** per-character style spans (`engine::StyleRun`)
in the document model, `Command::ApplyFormatting` (split / merge / coalesce),
and per-span **font size + colour** carried through layout and render.

**Deferred:**

- **Bold / italic face resolution.** The bridge `TextAttrs` / `TextAttrsPatch`
  already carry `bold` and `italic`, and engine spans can store them — but only
  Regular faces are loaded and `FontStack` resolves by script alone. Needs
  bold/italic font files plus `FontStack` variant selection keyed on the flags.
- **Underline & strikethrough.** No `DisplayList` primitive draws decoration
  lines yet; needs a stroke/line command and baseline-relative metrics.
- **Background colour.** `TextAttrs.bg_color` is unused; needs a `FillRect`
  emitted behind each run before its glyphs.
- **docx `<w:rPr>` round-trip.** `format-docx` flattens runs to plain text;
  `Paragraph.spans` is empty on import and ignored on export. A faithful
  reader/writer would map `<w:rPr>` to and from `StyleRun`s.

## 2. Kashida justification — Tatweel glyph insertion

**Shipped (D3.5):** joining-type-aware candidate detection via
`icu_properties`, the Microsoft P1–P5 priority bands, and one Kashida per word
at its highest-priority stroke.

**Deferred:** the elongation is applied by widening a glyph's `x_advance` —
i.e. inserting a **white gap**, not ink. PHASE_3_RENDER_RTL.md §6's intended
approach inserts `U+0640` Tatweel glyphs so the join renders as a real
elongated stroke. Needs: font glyph lookup for U+0640, tiling/scaling the
Tatweel to fill the target width, and inserting synthetic glyphs into a
`VisualRun` without corrupting the `source_range` / `cluster` indices.

## 3. PDF export — strict PDF/A-1b compliance

**Shipped (D3.7):** `crates/format-pdf` — box tree to a single-page PDF, Y-axis
inversion, full `Type0` / `CIDFontType2` / `Identity-H` font embedding.

**Deferred:**

- **PDF/A-1b conformance.** `Command::ExportPdf` accepts a `conformance` field
  but ignores it. Genuine /A-1b requires an `OutputIntent` referencing an
  embedded sRGB **ICC profile** (a binary blob — needs an explicit exception to
  CLAUDE.md's no-blobs rule), an XMP metadata packet, and a document ID.
- **veraPDF CI gate.** /A-1b cannot be honestly *claimed* without
  `veraPDF --profile 1b` validating the output in CI (PHASE_3_RENDER_RTL.md
  §10). Compliance is unverifiable until this gate exists.
- **Stream compression.** Content and `FontFile2` streams are uncompressed;
  with full font embedding the output is large. Needs `FlateDecode`.
- **`/ToUnicode` CMap.** Without it, text copy / extraction from the PDF is
  broken. Also `/W` glyph widths and font subsetting (smaller files).

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

**Shipped:** `layout_paragraph` stacks every line at a single fixed
`line_height` taken from the render config.

**Deferred:** line height should be computed per line as
`max(run ascent + run descent)` over every `VisualRun` on it, so a line
carrying a larger span (a bigger font size, a taller script) grows to fit
instead of clipping or overlapping its neighbours. The `rich-text` golden
currently sidesteps this by configuring a `line_height` generous enough for
its largest span. Needs: per-run ascent/descent from `LoadedFont::metrics`, a
max-reduce per line in `layout_paragraph`, and `LineBox.height` / `baseline`
derived from that instead of the config constant.

## 6. Paragraph base direction — auto-detection

**Shipped (global RTL base):** the editor seeds every document with one RTL
base direction (`ts/src/index.ts` → `RenderPage { base_direction: 'RTL' }`).
Per-line BiDi resolves mixed Arabic / English runs correctly within that base.

**Deferred:** UAX #9 first-strong auto-detection. `ShapingDirection` is
`Ltr | Rtl` only and the base is global — so an English-only paragraph
right-aligns like an Arabic one instead of detecting LTR from its first strong
character. Needs: an `Auto` mode (a `ShapingDirection::Auto` variant or a
resolve-before-layout pass), first-strong detection per **paragraph** (base
direction is a paragraph property, not per line), and a UI / keyboard toggle
to override the detected direction. Until then the Arabic-first RTL default
stands.

## 7. Selection rendering — discontinuous BiDi rectangles

**Shipped (D4.6 pragmatic subset):** per-line bounding selection rectangles —
the engine emits one `Rect` per line spanning the selected range's leftmost to
rightmost caret slot (`selection_rects_geom` in `engine-wasm`). Exact for
LTR-only, RTL-only, and whole-line selections.

**Deferred:** per-BiDi-visual-segment rectangles. A contiguous logical
selection that crosses an LTR↔RTL boundary mid-line is visually discontinuous —
the selected characters scatter into separate visual segments, and the single
bounding rect over-covers the unselected gap between them. A faithful render
emits one rect per visual segment. Needs: clipping the selected byte range
against each line's `VisualRun`s and emitting a rect per run-clipped sub-span.
The `SelectionOverlay` already renders an N-rect list, so only the engine side
changes.

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
