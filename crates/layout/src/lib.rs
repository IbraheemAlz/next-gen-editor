//! `layout` — paragraph + page box layout on top of `text-pipeline`.
//!
//! Phase 3: a hierarchical box model (`boxes`) — `PageBox` → `ParagraphBox` →
//! `LineBox` → `VisualRun` → `PositionedGlyph` — with parent-relative origins.
//! `layout_paragraph` owns all geometry; the renderer is a pure tree walk.

pub mod boxes;
pub mod page;
pub mod paragraph;

pub use boxes::{
    FontId, LayoutBlock, LineBox, MarkerBox, PageBox, ParagraphBox, Point, PositionedGlyph, Size,
    StyleSpan, TableBox, TableCellBox, TableRowBox, TextAttrs, VisualRun,
};
pub use page::{A4Page, Margins};
pub use paragraph::{ParagraphConfig, layout_paragraph};
