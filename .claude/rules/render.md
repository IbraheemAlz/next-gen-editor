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
- The nominal path stays output-identical: `geometry_fingerprint` pins
  the pre-watchdog geometry for every paginator and engine fixture. A
  self-defense change that moves a pinned fingerprint moves the goldens.

## PDF export

- PDF user space has its origin **bottom-left, y-up**; the layout engine is
  **top-left, y-down**. Invert every glyph: `pdf_y = page_height - layout_y`.
  X needs no inversion.
- Fonts embed as `Type0` / `CIDFontType2` with `Identity-H` encoding and
  `CIDToGIDMap /Identity` — the 2-byte codes in the content stream are the
  shaped glyph ids directly.
- Position every glyph with an explicit text matrix, not the PDF font's
  advances: our `x_advance` carries justification + Kashida adjustments the
  font's intrinsic widths do not.
