//! Glyph rasterization cache (PHASE_2_BRIDGE_MEMORY.md §8.4).
//!
//! The Phase 1 paint loop re-rasterized every glyph on every repaint. The
//! atlas memoizes `LoadedFont::rasterize_glyph` keyed by font + glyph id +
//! pixel size.

use crate::scene::FontId;
use lru::LruCache;
use std::num::NonZeroUsize;
use text_pipeline::{LoadedFont, RasterizedGlyph};

const CAPACITY: usize = 4096;

/// Cache key. `px_size` is fixed-point (pt × 100) so the key is `Eq`/`Hash`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GlyphKey {
    pub font_id: FontId,
    pub glyph_id: u16,
    pub px_size: u16,
}

impl GlyphKey {
    pub fn new(font_id: FontId, glyph_id: u16, px_size: f32) -> Self {
        Self {
            font_id,
            glyph_id,
            px_size: (px_size * 100.0).round() as u16,
        }
    }
}

/// LRU cache of rasterized glyph alpha masks.
pub struct GlyphAtlas {
    cache: LruCache<GlyphKey, RasterizedGlyph>,
}

impl GlyphAtlas {
    pub fn new() -> Self {
        Self {
            cache: LruCache::new(NonZeroUsize::new(CAPACITY).expect("CAPACITY is non-zero")),
        }
    }

    /// Number of glyphs currently resident.
    pub fn len(&self) -> usize {
        self.cache.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cache.is_empty()
    }

    /// Rasterized mask for `key`, rasterizing + caching on a miss. Returns
    /// `None` when the font cannot rasterize the glyph.
    pub fn get_or_rasterize(
        &mut self,
        key: &GlyphKey,
        font: &LoadedFont,
        px_size: f32,
    ) -> Option<&RasterizedGlyph> {
        if !self.cache.contains(key) {
            match font.rasterize_glyph(key.glyph_id, px_size) {
                Ok(raster) => {
                    self.cache.put(key.clone(), raster);
                }
                Err(_) => return None,
            }
        }
        self.cache.get(key)
    }
}

impl Default for GlyphAtlas {
    fn default() -> Self {
        Self::new()
    }
}
