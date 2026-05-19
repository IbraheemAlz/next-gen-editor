//! Font loading + glyph metrics + glyph rasterization via `swash`.

use swash::FontRef;
use swash::scale::{Render, ScaleContext, Source};
use swash::zeno::Format;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum FontError {
    #[error("font parse failed (invalid TTF/OTF/WOFF2)")]
    Parse,
    #[error("glyph missing for U+{0:04X}")]
    GlyphMissing(u32),
    #[error("rasterizer produced no image")]
    NoImage,
}

#[derive(Debug, Clone, Copy)]
pub struct FontMetrics {
    pub units_per_em: u16,
    pub ascent: f32,
    pub descent: f32,
    pub leading: f32,
    pub cap_height: f32,
    pub x_height: f32,
}

#[derive(Debug, Clone, Copy)]
pub struct GlyphMetrics {
    pub advance_width: f32,
}

#[derive(Debug, Clone)]
pub struct RasterizedGlyph {
    pub width: u32,
    pub height: u32,
    /// Horizontal offset from pen position to bitmap's left edge.
    pub left: i32,
    /// Vertical offset from baseline to bitmap's top edge (positive = above).
    pub top: i32,
    /// 8-bit alpha coverage, row-major, top-left origin. `len == width * height`.
    pub alpha: Vec<u8>,
}

/// Owns the font byte buffer; `swash::FontRef` is rebuilt on each access (zero-cost).
pub struct LoadedFont {
    id: String,
    data: Vec<u8>,
    units_per_em: u16,
}

impl LoadedFont {
    pub fn parse(id: String, data: Vec<u8>) -> Result<Self, FontError> {
        let face = FontRef::from_index(&data, 0).ok_or(FontError::Parse)?;
        let upem = face.metrics(&[]).units_per_em;
        Ok(Self {
            id,
            data,
            units_per_em: upem,
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    fn face(&self) -> FontRef<'_> {
        FontRef::from_index(&self.data, 0).expect("validated in parse")
    }

    pub fn metrics(&self, px_size: f32) -> FontMetrics {
        let m = self.face().metrics(&[]).scale(px_size);
        FontMetrics {
            units_per_em: self.units_per_em,
            ascent: m.ascent,
            descent: m.descent,
            leading: m.leading,
            cap_height: m.cap_height,
            x_height: m.x_height,
        }
    }

    pub fn glyph_metrics(&self, ch: char, px_size: f32) -> Result<GlyphMetrics, FontError> {
        let face = self.face();
        let gid = face.charmap().map(ch);
        if gid == 0 {
            return Err(FontError::GlyphMissing(ch as u32));
        }
        let gm = face.glyph_metrics(&[]).scale(px_size);
        Ok(GlyphMetrics {
            advance_width: gm.advance_width(gid),
        })
    }

    pub fn rasterize(&self, ch: char, px_size: f32) -> Result<RasterizedGlyph, FontError> {
        let face = self.face();
        let gid = face.charmap().map(ch);
        if gid == 0 {
            return Err(FontError::GlyphMissing(ch as u32));
        }
        let mut ctx = ScaleContext::new();
        let mut scaler = ctx.builder(face).size(px_size).hint(true).build();
        let image = Render::new(&[Source::Outline])
            .format(Format::Alpha)
            .render(&mut scaler, gid)
            .ok_or(FontError::NoImage)?;
        Ok(RasterizedGlyph {
            width: image.placement.width,
            height: image.placement.height,
            left: image.placement.left,
            top: image.placement.top,
            alpha: image.data,
        })
    }
}
