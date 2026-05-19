//! `layout` — paragraph + page box layout on top of `text-pipeline`.
//!
//! Phase 1 week 14: A4 page + single-paragraph greedy line breaking +
//! Latin/Kashida justification.

pub mod line_box;
pub mod page;
pub mod paragraph;

pub use line_box::{LineBox, PaintedGlyph};
pub use page::{A4Page, Margins};
pub use paragraph::{ParagraphConfig, layout_paragraph};
