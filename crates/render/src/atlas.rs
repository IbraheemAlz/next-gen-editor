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

/// Issue #436 — byte budget of the resident masks (64 MiB), in addition to
/// the entry cap: a handful of near-cap glyphs (each up to ~16 MiB at
/// `MAX_RASTER_PX`) used to be able to hold hundreds of MB while the entry
/// count stayed far below [`CAPACITY`]. Least-recently-used masks are
/// evicted until the total fits; the glyph just inserted always stays.
pub const BYTE_BUDGET: usize = 64 * 1024 * 1024;

/// Cache key. `px_size` is fixed-point (pt × 100) so the key is `Eq`/`Hash`.
/// `bold` / `italic` keep faux-styled masks (Backlog #1) distinct from the
/// plain glyph and from each other.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GlyphKey {
    pub font_id: FontId,
    pub glyph_id: u16,
    pub px_size: u16,
    pub bold: bool,
    pub italic: bool,
}

impl GlyphKey {
    pub fn new(font_id: FontId, glyph_id: u16, px_size: f32, bold: bool, italic: bool) -> Self {
        Self {
            font_id,
            glyph_id,
            px_size: (px_size * 100.0).round() as u16,
            bold,
            italic,
        }
    }
}

/// LRU cache of rasterized glyph alpha masks.
pub struct GlyphAtlas {
    cache: LruCache<GlyphKey, RasterizedGlyph>,
    /// Sum of `alpha.len()` over the resident masks.
    bytes: usize,
    byte_budget: usize,
    /// Glyphs neither the mask path nor the outline path would draw
    /// (issue #436) — a hostile size or a font with no outline for them.
    refused: u64,
}

impl GlyphAtlas {
    pub fn new() -> Self {
        Self::with_budgets(CAPACITY, BYTE_BUDGET)
    }

    /// An atlas with explicit entry and byte caps (tests).
    pub fn with_budgets(entries: usize, byte_budget: usize) -> Self {
        Self {
            cache: LruCache::new(NonZeroUsize::new(entries.max(1)).expect("max(1) is non-zero")),
            bytes: 0,
            byte_budget,
            refused: 0,
        }
    }

    /// Total bytes of the resident masks.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// Glyphs refused by both draw paths since creation (issue #436).
    pub fn refused(&self) -> u64 {
        self.refused
    }

    /// Count one glyph the backend could not draw at all.
    pub fn note_refused(&mut self) {
        self.refused = self.refused.saturating_add(1);
    }

    /// Insert `raster` under `key`, keeping `bytes` exact and enforcing both
    /// caps (the entry cap through the LRU, the byte cap here).
    fn insert(&mut self, key: GlyphKey, raster: RasterizedGlyph) {
        self.bytes += raster.alpha.len();
        if let Some((_, evicted)) = self.cache.push(key, raster) {
            self.bytes -= evicted.alpha.len();
        }
        while self.bytes > self.byte_budget && self.cache.len() > 1 {
            match self.cache.pop_lru() {
                Some((_, old)) => self.bytes -= old.alpha.len(),
                None => break,
            }
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
                Ok(mut raster) => {
                    /* Faux styling (Backlog #1): emboldened / sheared masks
                    are cached under their styled key. */
                    if key.bold {
                        raster = crate::synth::embolden(&raster, px_size);
                    }
                    if key.italic {
                        raster = crate::synth::slant(&raster);
                    }
                    self.insert(key.clone(), raster);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn raster(n: usize) -> RasterizedGlyph {
        RasterizedGlyph {
            width: n as u32,
            height: 1,
            left: 0,
            top: 0,
            alpha: vec![0; n],
        }
    }

    fn key(g: u16) -> GlyphKey {
        GlyphKey::new("f".to_string(), g, 12.0, false, false)
    }

    /* Issue #436 — the byte budget evicts least-recently-used masks even
    when the entry cap is nowhere near reached, and the byte count stays
    exact through eviction and replacement. */
    #[test]
    fn byte_budget_evicts_lru_and_stays_exact() {
        let mut a = GlyphAtlas::with_budgets(100, 1000);
        a.insert(key(1), raster(400));
        a.insert(key(2), raster(400));
        assert_eq!((a.len(), a.bytes()), (2, 800));
        a.cache.get(&key(1)); // key 2 is now the LRU
        a.insert(key(3), raster(400));
        assert_eq!((a.len(), a.bytes()), (2, 800));
        assert!(a.cache.contains(&key(1)) && a.cache.contains(&key(3)));
        assert!(!a.cache.contains(&key(2)));
        /* Replacing a key does not double-count. */
        a.insert(key(3), raster(100));
        assert_eq!(a.bytes(), 500);
        /* An entry over the whole budget stays (newest), alone. */
        a.insert(key(4), raster(5000));
        assert_eq!((a.len(), a.bytes()), (1, 5000));
    }

    #[test]
    fn entry_cap_still_applies_and_bytes_follow() {
        let mut a = GlyphAtlas::with_budgets(2, usize::MAX);
        for g in 0..5 {
            a.insert(key(g), raster(10));
        }
        assert_eq!((a.len(), a.bytes()), (2, 20));
    }
}
