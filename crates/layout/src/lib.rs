//! `layout` — paragraph + page box layout on top of `text-pipeline`.
//!
//! Phase 3: a hierarchical box model (`boxes`) — `PageBox` → `ParagraphBox` →
//! `LineBox` → `VisualRun` → `PositionedGlyph` — with parent-relative origins.
//! `layout_paragraph` owns all geometry; the renderer is a pure tree walk.

pub mod boxes;
pub mod floats;
pub mod page;
pub mod paginate;
pub mod paragraph;
pub mod watchdog;

pub use boxes::{
    CellAnchorRef, FloatAnchorRef, FloatBox, FloatGlyph, FloatOffsetPx, FloatSpec, FontId,
    HeaderFooterBox, LayoutBlock, LayoutField, LineBox, MarkerBox, PageBox, ParagraphBox, Point,
    PositionedGlyph, Size, StyleSpan, TableBox, TableCellBox, TableRowBox, TextAttrs, VisualRun,
};
pub use floats::{ColumnLayout, resolve_page_floats};
pub use page::{A4Page, Margins};
pub use paginate::{
    HeaderBands, HeaderRole, PageGeometry as PaginatePageGeometry, Paginator,
    split_paragraph_at_line,
};
pub use paragraph::{InlineObjectInfo, ParagraphConfig, layout_paragraph};
pub use watchdog::{
    BlockFingerprint, DegradeReason, DegradeStage, FastPathMismatch, LayoutDegradation, Watchdog,
    geometry_fingerprint, verify_prefix,
};
