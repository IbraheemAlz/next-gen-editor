//! `text-pipeline` — Unicode BiDi + shaping + line-break + Kashida justify.
//!
//! Phase 1 week 5–6: font load + glyph metrics + raster (`fonts.rs`).
//! BiDi + shaping + segmentation land weeks 7+ per PHASE_3_RENDER_RTL.md §3.

pub mod fonts;

pub use fonts::{FontError, FontMetrics, GlyphMetrics, LoadedFont, RasterizedGlyph};
