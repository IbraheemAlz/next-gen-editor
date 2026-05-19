//! A laid-out line: glyphs in visual order with absolute x positions, ready
//! for paint.

use text_pipeline::ShapingDirection;

#[derive(Debug, Clone, Copy)]
pub struct PaintedGlyph {
    pub glyph_id: u32,
    /// Byte offset of the source character in the original paragraph text.
    pub source_offset: u32,
    /// Absolute x within the line (line origin at 0, increases left-to-right).
    pub x: f32,
    pub x_offset: f32,
    pub y_offset: f32,
    pub x_advance: f32,
}

#[derive(Debug, Clone, Default)]
pub struct LineBox {
    pub glyphs: Vec<PaintedGlyph>,
    pub direction: Option<ShapingDirection>,
    /// Sum of glyph advances; max position used by paint.
    pub natural_width: f32,
    /// Baseline y relative to paragraph top.
    pub baseline_y: f32,
    /// True when this line ended at a line-break opportunity (not at end of
    /// paragraph); affects justify (last line of a paragraph is not justified).
    pub broken_at_opportunity: bool,
}
