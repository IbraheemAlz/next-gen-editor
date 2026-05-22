//! Text shaping via `rustybuzz` (pure-Rust HarfBuzz subset).
//!
//! Takes a logical string + base direction + font, returns visually-ordered
//! glyphs with positions (advance / offset) in scaled pixels.

use crate::fonts::LoadedFont;
use rustybuzz::{Direction, UnicodeBuffer};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShapingDirection {
    Ltr,
    Rtl,
}

#[derive(Debug, Clone)]
pub struct ShapedGlyph {
    pub glyph_id: u32,
    /// Byte offset into the original logical string the glyph maps to.
    pub cluster: u32,
    pub x_advance: f32,
    pub y_advance: f32,
    pub x_offset: f32,
    pub y_offset: f32,
}

#[derive(Debug, Clone)]
pub struct ShapedRun {
    pub glyphs: Vec<ShapedGlyph>,
    pub direction: ShapingDirection,
    /// Sum of `x_advance` across all glyphs, in scaled pixels.
    pub total_advance: f32,
}

/// Shape `text` with `font` for the given base direction at the given pixel size.
///
/// Returns glyphs in **visual order** (left-to-right on screen for both LTR and RTL),
/// so a caller can pen them left-to-right with positive advances.
pub fn shape_text(
    font: &LoadedFont,
    text: &str,
    direction: ShapingDirection,
    px_size: f32,
) -> ShapedRun {
    let face = font.face_rustybuzz();

    let mut buffer = UnicodeBuffer::new();
    buffer.push_str(text);
    buffer.set_direction(match direction {
        ShapingDirection::Ltr => Direction::LeftToRight,
        ShapingDirection::Rtl => Direction::RightToLeft,
    });
    /* Infer script + language from the input codepoints unless caller has set them. */
    buffer.guess_segment_properties();

    let glyph_buf = rustybuzz::shape(face, &[], buffer);
    let infos = glyph_buf.glyph_infos();
    let positions = glyph_buf.glyph_positions();

    let upem = face.units_per_em() as f32;
    let scale = px_size / upem;

    let mut glyphs = Vec::with_capacity(infos.len());
    let mut total = 0.0_f32;
    for (info, pos) in infos.iter().zip(positions.iter()) {
        let g = ShapedGlyph {
            glyph_id: info.glyph_id,
            cluster: info.cluster,
            x_advance: pos.x_advance as f32 * scale,
            y_advance: pos.y_advance as f32 * scale,
            x_offset: pos.x_offset as f32 * scale,
            y_offset: pos.y_offset as f32 * scale,
        };
        total += g.x_advance;
        glyphs.push(g);
    }

    ShapedRun {
        glyphs,
        direction,
        total_advance: total,
    }
}
