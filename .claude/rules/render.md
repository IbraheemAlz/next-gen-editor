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
