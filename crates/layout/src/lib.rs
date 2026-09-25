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
mod table_split;
pub mod watchdog;
pub mod wrap;
#[cfg(test)]
mod wrap_fixtures;

pub use boxes::{
    CellAnchorRef, FloatAnchorRef, FloatBox, FloatGlyph, FloatOffsetPx, FloatSpec, FloatWrap,
    FontId, FootnoteEntry, HeaderFooterBox, LayoutBlock, LayoutField, LineBox, LineSegment,
    MarkerBox, NoteBand, PageBox, ParaFlow, ParagraphBox, Point, PositionedGlyph, Size, StyleSpan,
    TabLeaderKind, TableBox, TableCellBox, TableRowBox, TextAttrs, TextBoxFrame, TextBoxGlyph,
    VisualRun, WrapSide,
};
pub use floats::{ColumnLayout, resolve_page_floats};
pub use page::{A4Page, Margins};
pub use paginate::{
    FOOTNOTE_SEPARATOR_HEIGHT_PT, HeaderBands, HeaderRole, NoteBody,
    PageGeometry as PaginatePageGeometry, Paginator, collect_note_anchors, split_paragraph_at_line,
};
pub use paragraph::{
    InlineObjectInfo, ParagraphConfig, layout_paragraph, layout_paragraph_wrapped,
};
pub use watchdog::{
    BlockFingerprint, DegradeReason, DegradeStage, FastPathMismatch, LayoutDegradation,
    PageRefConvergence, Watchdog, converge_page_refs, geometry_fingerprint, verify_prefix,
};
pub use wrap::{
    FloatKey, WrapConvergence, WrapCutout, WrapPlan, WrapVerdict, cutouts_for_float, derive_plan,
    float_key, next_band_edge, plans_equal, segments_for_band,
};
