# Render / layout rules

Phase 3 invariants for the box model, the Canvas2D backend, and PDF export.

## Coordinate spaces

- The box tree is `PageBox → ParagraphBox → LineBox → VisualRun →
  PositionedGlyph`. Every box's `origin` is **parent-relative**. Absolute
  positions are reached by accumulating down the tree:
  `page.margins + para.origin + line.origin + run pen + glyph.x_offset`.
- `layout_paragraph` owns **all** geometry. Line stacking (`origin.y`) and the
  alignment offset (`origin.x`) are baked into `LineBox.origin` — the renderer
  is a pure traversal and never re-derives alignment.
- `PositionedGlyph` stores advances/offsets only, no absolute `x`; position is
  the run pen plus the cumulative `x_advance`. The pen advances for glyph id 0
  (`.notdef`) too, even though it is not drawn.
- Issues #359 / #104 / #249 — a `StyleSpan` carries an optional
  complex-script set (`StyleSpan::cs`: size, shift, weight, slant,
  family; `None` when equal to the Latin set). `build_line` and the
  width probe segment by script AND complex-script class
  (`segment_by_script_class`) and shape each piece with
  `StyleSpan::face_for(complex)` — keep the two in step, and never read
  `span.px_size` / `span.bold` / `span.font_family` directly for a piece.

## Font substitution and line pitch (issue #329)

- `FontStack::resolve_family` matches a document's family before the
  per-script fallback: exact id → name (id key or the face's `name`-table
  family, `family_key`-folded) → `text_pipeline::SUBSTITUTIONS`, keyed by
  (family, `ScriptClass`) — Arial Latin is Liberation Sans, Arial Arabic
  is Noto Naskh Arabic. A substitute must cover the class. Add rows to
  the table (and the face to `fonts.json` `substitutes`), never a special
  case in layout; `every_shipped_substitute_resolves_through_its_name_table`
  pins the shipped faces against it.
- **Line pitch has two models.** A document read from a Word package
  (`DocumentEnvelope::is_captured`, `StyleContext::word_line_metrics`)
  lays out with Word's font-derived pitch (`layout::FontLinePitch`): a
  line is the largest `ascent + line gap` plus the largest `descent` of
  its runs' faces under `LoadedFont::line_metrics` (Windows Word's rule:
  win extent + GDI external leading, typo metrics when
  `USE_TYPO_METRICS`), × the `auto` multiple / floored by `atLeast`, the
  extra space ABOVE the text; a runless line takes the paragraph mark's
  face. A substituted run whose row names `metrics_from` measures that
  face (`VisualRun::metrics_font`). Everything else (the seeded page,
  `.txt` / `.html`, the `RenderPage` harness) keeps the configured
  `line_height` — its goldens stay put. `exact` is the same in both.
- The rule is the Windows one on purpose (Arabic documents are authored
  there); Mac Word uses `hhea`. Whether Windows Word honours
  `USE_TYPO_METRICS` is unverified (Amiri: 1.758 em typo vs 2.760 em win)
  — `word_line_metrics` is the one place to flip it.

## Canvas2D backend

- **`put_image_data` ignores the canvas clip path** (and the transform), per
  the Canvas2D spec. Glyphs are blitted with `put_image_data`, so `ctx.clip()`
  does **not** skip off-region glyphs — cull glyph runs by a bounding-box test
  in the Rust loop instead. The clip only bounds `fill_rect` / `stroke_rect`.
- A clip covering the whole page is a no-op: every run intersects it, nothing
  is culled, so a full repaint is byte-identical to the unclipped path. Pure
  render refactors must keep the visual-diff goldens at 0.000 %.

## Layout self-defense (issue #87)

**An imperfect layout that terminates beats a perfect one that hangs.**
Every convergence loop in `crates/layout` (and the layout helpers in
`engine-wasm`) is bounded twice: by construction — each re-push consumes
content (a paragraph tail has fewer lines, a table continuation fewer
rows) or the oversize guard clips — and by the `layout::watchdog`
backstop, which counts *churn rounds* (the same content re-placed on a
fresh page without consuming any of it; a fresh page is the most room
the paginator can ever offer, so a repeat is a proof of non-progress)
and escalates: **(a) drop optional constraints** (keep-with-next chains,
repeated table header rows), **(b) freeze** — pin the block atomically
at the cursor, clipping, **(c) force-validate** — the page cap: append
the rest without page breaks and accept the state. Progress (a smaller
re-push, a new top-level block) resets counter and stage. A strict
switch (`Paginator::with_strict_watchdog`) turns every recovery into a
hard failure so CI catches a new loop instead of a silent recovery.

- Never add a retry / re-push / negotiation loop without a local
  termination rule *and* a `DegradeReason` for the escape hatch. The
  escape hatch must paint something; it must never drop content.
- Every degradation is reported: the paginator's notes ride
  `Event::Painted.layout_degraded` (and the synthetic side-channel).
  A degraded paint is telemetry, never a blocking error.
- **Verified fast paths.** An incremental relayout (`LazyLayoutState` /
  `ExpandLayout` bands, the paragraph layout LRU, any future
  single-paragraph tier) is a *prediction*. Run the real layout, check
  its invariants against the previous result (`layout::verify_prefix`:
  page count, page geometry, per-block origin + size, end position;
  `cached_paragraph_is_consistent` at the paragraph tier) and demote to
  a full reflow on mismatch — never trust the prediction. A stale-layout
  bug becomes a perf blip plus a `FastPathMismatch` / `CacheMismatch`
  note, not corruption.
- A band's geometry must be self-consistent where it stops: never cull
  right behind a keep-with-next paragraph (issue #95), and mark pages
  that a pending pass will still rewrite — a continuous section's column
  balance — as provisional (`LazyLayoutInfo::open_from_page`, verified
  with `layout::verify_prefix_open`, issue #93) instead of letting every
  expand demote.
- The nominal path stays output-identical: `geometry_fingerprint` pins
  the pre-watchdog geometry for every paginator and engine fixture. A
  self-defense change that moves a pinned fingerprint moves the goldens.
- **Bound slow progress, not only churn (issue #318).** The watchdog
  catches non-progress; a recursion that re-does a subtree per ancestor
  pass makes progress and still never finishes (nested-table autofit
  was `F(2·depth)` grid layouts). Any per-level re-measure of a subtree
  goes through a memo scoped to ONE top-level layout (engine-wasm
  `TableLayout`: inner table per width, column solve per width,
  intrinsic widths per cell — keyed by the subtree's address in the
  immutably borrowed tree, verified on every hit), and recursion over
  document nesting is capped (`MAX_TABLE_LAYOUT_DEPTH` = 32: deeper
  tables flatten to their paragraphs, `NestingCapped`). A probe pass
  that only needs a size takes it from the size's own computation
  (a nested table's width is its column sum) — never from a full
  layout at a width the final pass will not use.

## PDF export

- PDF user space has its origin **bottom-left, y-up**; the layout engine is
  **top-left, y-down**. Invert every glyph: `pdf_y = page_height - layout_y`.
  X needs no inversion.
- Fonts embed as `Type0` / `CIDFontType2` with `Identity-H` encoding and
  `CIDToGIDMap /Identity`, **subset** to the glyphs the document shows (issue
  #327, the `subsetter` crate — `format-pdf/src/font_program.rs`). The
  subset renumbers glyph ids, so the 2-byte content-stream codes are the
  SUBSET's ids, never the shaped ones: always emit a shown glyph through
  `FontObj::show_code` (it assigns the id on first use — that is how the
  subset learns which glyphs it needs), and key `/W`, `/ToUnicode` and the
  PDF/A-1b `/CIDSet` by code. Subset names carry a deterministic six-letter
  `ABCDEF+` tag. A face the subsetter rejects falls back to full embedding
  with codes == shaped glyph ids (the pre-#327 output).
- A CFF-outline face (`OTTO` `.otf`, `CFF ` table, no `glyf`) embeds as
  `CIDFontType0` + `FontFile3 /Subtype /CIDFontType0C` (the bare CFF program
  the subsetter rewrote CID-keyed with an identity charset) and **no**
  `CIDToGIDMap` (only `CIDFontType2` may carry one) — issue #361,
  `font_program::Outlines`. TrueType output must stay byte-identical when
  touching the CFF branch.
- CFF test fonts are synthesized at test time from the shipped OFL `.ttf`s
  (`format-pdf/src/cff_test_font.rs`) — never commit a font binary. Keep a
  `Notice`/`Copyright` in any synthesized Top DICT: veraPDF 1.30 mis-scales
  CFF widths when the subset's `FontMatrix` directly follows `ROS`.
- Tests that inspect content-stream text decode codes through the font's
  own `/ToUnicode` (`format_pdf::test_support::{to_unicode_cmaps,
  decode_codes}`) — comparing them against `LoadedFont::glyph_id` is wrong
  for a subset font.
- Position every glyph with an explicit text matrix, not the PDF font's
  advances: our `x_advance` carries justification + Kashida adjustments the
  font's intrinsic widths do not.
- **Tagging is opt-in and byte-neutral when off (issue #360).** Only
  `PdfExportOptions::tagged` (always on for `PdfProfile::Ua1`, the tagged
  PDF/A-2u) writes marked content, the structure tree and the PDF/UA
  identification; every other profile must stay byte-identical — sha256
  the tier-a exports before/after (`write_tier_a_exports` in
  `engine-wasm/src/pdf_semantics_tests.rs`). In a tagged export every
  painting operator is inside either a `BDC` with an `/MCID` (real content,
  recorded through `SemCtx` — `tagging.rs`) or an `/Artifact` sequence;
  a new paint pass must pick one (`artifact_begin` / `decoration_pass`),
  or veraPDF's PDF/UA-1 §7.1 check fails. A tagged PDF 1.5+ file is
  re-laid out by `objstm::pack` (object stream + xref stream), so tests
  that search a tagged file for dictionary text go through
  `test_support::searchable_text`.
