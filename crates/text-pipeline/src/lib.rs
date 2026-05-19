//! `text-pipeline` — Unicode BiDi + shaping + line-break + Kashida justify.
//!
//! Phase 1 weeks 5–6: `fonts` (load + metrics + raster via swash).
//! Phase 1 weeks 7–9: `shape` (rustybuzz integration).
//! BiDi + segmentation + Kashida land weeks 10+ per PHASE_3_RENDER_RTL.md §3.

pub mod fonts;
pub mod shape;

pub use fonts::{FontError, FontMetrics, GlyphMetrics, LoadedFont, RasterizedGlyph};
pub use shape::{ShapedGlyph, ShapedRun, ShapingDirection, shape_text};
