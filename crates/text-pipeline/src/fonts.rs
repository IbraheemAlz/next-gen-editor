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

    /// Glyph id for `ch` via the font's cmap; `None` when unmapped
    /// (`.notdef`). Used to look up the Tatweel (U+0640) for Kashida ink.
    pub fn glyph_id(&self, ch: char) -> Option<u16> {
        let gid = self.face().charmap().map(ch);
        (gid != 0).then_some(gid)
    }

    /// Whether the font's own OS/2 metadata marks it a bold weight (>= 600).
    pub fn is_bold(&self) -> bool {
        self.face().attributes().weight().0 >= 600
    }

    /// Whether the font's own metadata marks it italic or oblique.
    pub fn is_italic(&self) -> bool {
        !matches!(self.face().attributes().style(), swash::Style::Normal)
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

/// Which synthetic styles the renderer must apply because no real font face
/// covered the requested weight / slant (Backlog #1 — faux bold / italic).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Synthesis {
    pub faux_bold: bool,
    pub faux_italic: bool,
}

/// A per-script font resolver (PHASE_3_RENDER_RTL.md §13.A).
///
/// Holds every loaded face plus, for each script, the ids of the faces that
/// cover it. [`resolve`](FontStack::resolve) picks the best face for a script
/// and requested weight/slant, falling through `fallback_chain` and flagging
/// synthetic styling when no real face variant exists.
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

    /// Resolve `script` plus the requested weight/slant to a font id, its
    /// loaded face, and the [`Synthesis`] the renderer must apply. A real face
    /// whose own metadata matches `bold`/`italic` is used verbatim (no faux);
    /// otherwise the best covering face is returned and the missing weight /
    /// slant is flagged for synthesis (Backlog #1). `None` only when the stack
    /// holds no faces at all.
    pub fn resolve(
        &self,
        script: Script,
        bold: bool,
        italic: bool,
    ) -> Option<(&FontId, &LoadedFont, Synthesis)> {
        let preferred = self.by_script.get(&script).into_iter().flatten();
        let chain: Vec<&FontId> = preferred.chain(self.fallback_chain.iter()).collect();
        /* A real variant whose own metadata matches the request — no faux. */
        for id in &chain {
            if let Some((key, face)) = self.faces.get_key_value(*id)
                && face.is_bold() == bold
                && face.is_italic() == italic
            {
                return Some((key, face.as_ref(), Synthesis::default()));
            }
        }
        /* No matching variant — fall back to the first covering face and
        synthesize the missing weight / slant. */
        for id in &chain {
            if let Some((key, face)) = self.faces.get_key_value(*id) {
                return Some((
                    key,
                    face.as_ref(),
                    Synthesis {
                        faux_bold: bold,
                        faux_italic: italic,
                    },
                ));
            }
        }
        None
    }

    /// The loaded face for an exact font id, if the stack holds it.
    pub fn face(&self, id: &str) -> Option<&LoadedFont> {
        self.faces.get(id).map(|f| f.as_ref())
    }
}
