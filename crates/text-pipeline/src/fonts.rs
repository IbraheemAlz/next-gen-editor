//! Font loading + glyph metrics + glyph rasterization via `swash`.

use crate::script::Script;
use std::collections::HashMap;
use std::sync::Arc;
use swash::FontRef;
use swash::scale::{Render, ScaleContext, Source};
use swash::zeno::Format;
use thiserror::Error;

/// Font identifier — a key into the engine's font map.
pub type FontId = String;

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

/// Owns the font byte buffer; `swash::FontRef` and `rustybuzz::Face` are
/// rebuilt on each access (both zero-cost; they just hold a slice reference).
pub struct LoadedFont {
    id: String,
    data: Vec<u8>,
    units_per_em: u16,
}

impl LoadedFont {
    pub fn parse(id: String, data: Vec<u8>) -> Result<Self, FontError> {
        let face = FontRef::from_index(&data, 0).ok_or(FontError::Parse)?;
        let upem = face.metrics(&[]).units_per_em;
        /* Also validate the same bytes are a valid rustybuzz Face — we share
        the buffer between swash and rustybuzz at runtime. */
        rustybuzz::Face::from_slice(&data, 0).ok_or(FontError::Parse)?;
        Ok(Self {
            id,
            data,
            units_per_em: upem,
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    /// The raw font-file bytes — used to embed the full face in a PDF.
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    fn face(&self) -> FontRef<'_> {
        FontRef::from_index(&self.data, 0).expect("validated in parse")
    }

    pub fn face_rustybuzz(&self) -> Option<rustybuzz::Face<'_>> {
        rustybuzz::Face::from_slice(&self.data, 0)
    }

    /// Whether the font's cmap maps `ch` to a real (non-`.notdef`) glyph.
    pub fn covers(&self, ch: char) -> bool {
        self.face().charmap().map(ch) != 0
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

    /// Rasterize a glyph by character (does charmap lookup internally).
    pub fn rasterize(&self, ch: char, px_size: f32) -> Result<RasterizedGlyph, FontError> {
        let gid = {
            let face = self.face();
            let gid = face.charmap().map(ch);
            if gid == 0 {
                return Err(FontError::GlyphMissing(ch as u32));
            }
            gid
        };
        self.rasterize_glyph(gid, px_size)
    }

    /// Rasterize a glyph by its glyph id (skipping the charmap lookup;
    /// used after `rustybuzz` shaping returns glyph ids directly).
    pub fn rasterize_glyph(&self, gid: u16, px_size: f32) -> Result<RasterizedGlyph, FontError> {
        let face = self.face();
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

/// A per-script font resolver (PHASE_3_RENDER_RTL.md §13.A).
///
/// Holds every loaded face plus, for each script, the ids of the faces that
/// cover it. [`resolve`](FontStack::resolve) picks the best face for a script,
/// falling through `fallback_chain` when no script-specific face exists.
pub struct FontStack {
    faces: HashMap<FontId, Arc<LoadedFont>>,
    by_script: HashMap<Script, Vec<FontId>>,
    fallback_chain: Vec<FontId>,
}

impl FontStack {
    /// Build a stack from loaded faces, classifying each by the scripts it
    /// covers (probed against a representative codepoint). `primary` seeds the
    /// fallback chain so a script with no dedicated face still resolves.
    pub fn from_faces(faces: HashMap<FontId, Arc<LoadedFont>>, primary: &str) -> Self {
        let mut by_script: HashMap<Script, Vec<FontId>> = HashMap::new();
        for (id, face) in &faces {
            if face.covers('\u{0628}') {
                by_script
                    .entry(Script::Arabic)
                    .or_default()
                    .push(id.clone());
            }
            if face.covers('A') {
                by_script.entry(Script::Latin).or_default().push(id.clone());
            }
        }
        /* Deterministic priority within each script. */
        for ids in by_script.values_mut() {
            ids.sort();
        }
        /* Fallback chain: the primary first, then the rest in id order. */
        let mut fallback_chain: Vec<FontId> = Vec::new();
        if faces.contains_key(primary) {
            fallback_chain.push(primary.to_string());
        }
        let mut others: Vec<FontId> = faces
            .keys()
            .filter(|k| k.as_str() != primary)
            .cloned()
            .collect();
        others.sort();
        fallback_chain.extend(others);
        Self {
            faces,
            by_script,
            fallback_chain,
        }
    }

    /// Resolve `script` to a font id and its loaded face — script-specific
    /// faces first, then the fallback chain. `None` only when the stack holds
    /// no faces at all.
    pub fn resolve(&self, script: Script) -> Option<(&FontId, &LoadedFont)> {
        let preferred = self.by_script.get(&script).into_iter().flatten();
        for id in preferred.chain(self.fallback_chain.iter()) {
            if let Some((key, face)) = self.faces.get_key_value(id) {
                return Some((key, face.as_ref()));
            }
        }
        None
    }

    /// The loaded face for an exact font id, if the stack holds it.
    pub fn face(&self, id: &str) -> Option<&LoadedFont> {
        self.faces.get(id).map(|f| f.as_ref())
    }
}
