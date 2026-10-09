//! `text-pipeline` — Unicode BiDi + shaping + line-break + Kashida justify.
//!
//! Phase 1 weeks 5–6:  `fonts` (load + metrics + raster via swash).
//! Phase 1 weeks 7–9:  `shape` (rustybuzz integration).
//! Phase 1 weeks 10–13:`bidi` + `line_break` + `justify`.
//! Phase 3: `justify_kashida` (priority-band Kashida) + `script` (Unicode
//! script detection + per-script font resolution, §13.A).
//! Issue #329: `substitution` (metric-compatible font substitution table).

pub mod bidi;
pub mod fonts;
pub mod justify;
pub mod justify_kashida;
pub mod line_break;
pub mod script;
pub mod shape;
pub mod substitution;

pub use bidi::{BidiAnalysis, VisualRun, analyze_bidi, first_strong_direction};
pub use fonts::{
    FamilyMatch, FamilyResolution, FontError, FontId, FontMetrics, FontStack, GlyphMetrics,
    LineMetrics, LoadedFont, RasterizedGlyph, Resolved, Synthesis,
};
pub use justify::{Alignment, JustifyMode};
pub use line_break::break_opportunities;
pub use script::{
    Script, complex_script_tag, is_complex_script, script_of, segment_by_script,
    segment_by_script_class,
};
pub use shape::{ShapedGlyph, ShapedRun, ShapingDirection, shape_text};
pub use substitution::{SUBSTITUTIONS, ScriptClass, Substitution, family_key, substitution_for};
