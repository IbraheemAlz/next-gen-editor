//! `text-pipeline` — Unicode BiDi + shaping + line-break + Kashida justify.
//!
//! Phase 1 weeks 5–6:  `fonts` (load + metrics + raster via swash).
//! Phase 1 weeks 7–9:  `shape` (rustybuzz integration).
//! Phase 1 weeks 10–13:`bidi` + `line_break` + `justify`.

pub mod bidi;
pub mod fonts;
pub mod justify;
pub mod justify_kashida;
pub mod line_break;
pub mod shape;

pub use bidi::{BidiAnalysis, VisualRun, analyze_bidi};
pub use fonts::{FontError, FontMetrics, GlyphMetrics, LoadedFont, RasterizedGlyph};
pub use justify::{Alignment, JustifyMode};
pub use line_break::break_opportunities;
pub use shape::{ShapedGlyph, ShapedRun, ShapingDirection, shape_text};
