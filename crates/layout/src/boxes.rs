//! Hierarchical layout box model (PHASE_3_RENDER_RTL.md §5).
//!
//! `PageBox` → `ParagraphBox` → `LineBox` → `VisualRun` → `PositionedGlyph`.
//! Every box's `origin` is **relative to its parent container**: a `LineBox`
//! origin is relative to its `ParagraphBox`, a `ParagraphBox` origin relative
//! to the `PageBox` content area (the page rect inset by `margins`). The
//! renderer accumulates origins down the tree to reach absolute positions.
//!
//! Pragmatic subset of §5: the boxes carry only data backed by the current
//! pipeline. §5's `Script`, `ParagraphStyleId`, `JustifyInfo`, and
//! `HeaderFooterBox` wait for the styling / script / header-footer subsystems.

use crate::page::Margins;
use std::ops::Range;
use text_pipeline::{Alignment, ShapingDirection};

/// Font identifier — a key into the engine's font map.
pub type FontId = String;

/// A point in a parent-relative coordinate space, in PostScript points.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

/// A 2D extent, in PostScript points.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Size {
    pub width: f32,
    pub height: f32,
}

/// Per-run text attributes the renderer needs — the data the temporary
/// `render::scene::PaintConfig` used to carry out-of-band.
#[derive(Debug, Clone, Copy)]
pub struct TextAttrs {
    pub px_size: f32,
    /// Straight-alpha RGBA fill colour.
    pub color: [u8; 4],
    /// Render this run with synthetic (faux) bold — set when no real bold
    /// face was available (Backlog #1).
    pub faux_bold: bool,
    /// Render this run with synthetic (faux) italic via a shear transform.
    pub faux_italic: bool,
    /// Underline decoration variant (Backlog #1). `None` suppresses the
    /// stroke; the other variants pick the renderer's pattern.
    pub underline: engine::UnderlineStyle,
    /// Draw a strikethrough stroke through the run (Backlog #1).
    pub strike: bool,
    /// Highlight colour painted behind the run's glyphs (Backlog #1).
    pub bg_color: Option<[u8; 4]>,
}

/// A resolved rich-text style span — a paragraph byte range `[start, end)` with
/// its fully-resolved style (paragraph defaults already applied).
/// `layout_paragraph` splits shaping runs at these boundaries.
#[derive(Debug, Clone)]
pub struct StyleSpan {
    pub start: u32,
    pub end: u32,
    pub px_size: f32,
    pub color: [u8; 4],
    /// Requested bold — resolved to a real face or faux synthesis at layout.
    pub bold: bool,
    /// Requested italic — resolved to a real face or faux synthesis.
    pub italic: bool,
    pub underline: engine::UnderlineStyle,
    pub strike: bool,
    pub bg_color: Option<[u8; 4]>,
    /// Resolved font id for an explicit family request; `None` keeps the
    /// per-script default face (Backlog #9).
    pub font_family: Option<String>,
    /// Audit gap A.H3 — uppercase the source bytes of this span before
    /// shaping. Set by `build_style_spans` for `<w:caps>` and `<w:smallCaps>`
    /// spans; the shaper guards against case-changing length deltas
    /// (e.g. German `ß` → "SS") to keep glyph clusters aligned with the
    /// paragraph's source bytes.
    pub caps_transform: bool,
}

/// One shaped glyph, positioned by advance/offset relative to the pen. There is
/// no stored absolute x: it is the run pen plus the cumulative `x_advance` of
/// the prior glyphs in the run.
///
/// Phase 7 dropped `Copy`: inline-image glyphs carry the image's relationship
/// id as a `String`, which is not `Copy`. Callers that previously moved the
/// glyph by value now clone; the storage layout is unchanged.
#[derive(Debug, Clone)]
pub struct PositionedGlyph {
    pub id: u16,
    /// Byte offset into the owning [`VisualRun::source_range`].
    pub cluster: u32,
    pub x_advance: f32,
    pub y_advance: f32,
    pub x_offset: f32,
    pub y_offset: f32,
    /// A synthetic glyph layout inserted — a Kashida Tatweel (U+0640). Drawn
    /// like any glyph, but skipped by caret / hit-test slot emission so the
    /// byte<->glyph map is not corrupted (Backlog #2).
    pub synthetic: bool,
    /// Phase 7 — when set, this glyph anchors an inline image; the
    /// renderer paints the image whose archive relationship id matches
    /// this string at the glyph's pen position with `x_advance` as the
    /// physical width. The text glyph itself is **not** drawn.
    pub inline_image_rel_id: Option<String>,
    /// Phase 8a — when set, this glyph anchors a footnote reference; the
    /// renderer paints the marker text (e.g. `"1"`, `"2"`) as a
    /// superscript at the glyph's pen position with `x_advance` as the
    /// reserved width.
    pub inline_footnote_marker: Option<String>,
    /// Phase 7 — physical height of the anchored inline object in layout
    /// pixels. Folded into the line's ascent so the line grows to host
    /// the image without clipping.
    pub inline_object_height: f32,
}

/// A maximal run of glyphs sharing one font, direction, and style — the unit
/// produced by BiDi reordering. Glyphs are in visual (left-to-right) order.
#[derive(Debug, Clone)]
pub struct VisualRun {
    pub glyphs: Vec<PositionedGlyph>,
    pub font: FontId,
    pub direction: ShapingDirection,
    /// Byte range in the source paragraph text this run was shaped from.
    pub source_range: Range<u32>,
    pub attrs: TextAttrs,
}

/// One laid-out line. `origin` is relative to the parent [`ParagraphBox`];
/// `origin.x` already carries the alignment offset, so the renderer never
/// recomputes alignment.
#[derive(Debug, Clone)]
pub struct LineBox {
    pub origin: Point,
    /// Baseline offset below `origin.y`.
    pub baseline: f32,
    pub height: f32,
    /// Content width — natural, or the justified target.
    pub width: f32,
    pub runs: Vec<VisualRun>,
    pub alignment: Alignment,
}

/// Phase 2 audit (gap D.1) — complex-field overlay propagated from the
/// source [`engine::Field`] into the laid-out paragraph. `evaluated_text`
/// is `None` until the paginator stamps the per-page value (PAGE /
/// NUMPAGES); other instructions keep `None` and the renderer paints
/// the cached display text already baked into the paragraph's glyphs.
#[derive(Debug, Clone)]
pub struct LayoutField {
    /// Byte range `[start, end)` in the source paragraph text that
    /// carries the field's cached display value.
    pub byte_range: Range<u32>,
    /// Field code lifted from the source `<w:instrText>` — kept as the
    /// trimmed source string so re-evaluation rules live in one place
    /// (`engine::Field::keyword` + `evaluate`).
    pub instruction: String,
    /// Per-page evaluated string the paginator stamps before flushing.
    /// `None` ⇒ renderer paints the original cached glyphs.
    pub evaluated_text: Option<String>,
}

/// One laid-out paragraph. `origin` is relative to the [`PageBox`] content area.
#[derive(Debug, Clone)]
pub struct ParagraphBox {
    pub origin: Point,
    pub size: Size,
    pub lines: Vec<LineBox>,
    pub direction: ShapingDirection,
    /// Phase 4 list marker (`"1."`, `"a)"`, `"•"`). `origin` is relative to
    /// this `ParagraphBox`. Positioned in the leading-edge gutter — left
    /// of the first line for LTR, right for RTL. `None` for non-list
    /// paragraphs.
    pub marker: Option<MarkerBox>,
    /// Phase 6 — flat index into the per-document source-paragraph text
    /// table the PDF `/ToUnicode` builder consumes. Engine-wasm fills it at
    /// layout time; the paginator preserves it through paragraph splits
    /// (head and tail share the same id, both lookup the same full source
    /// text — glyph clusters are byte offsets into that full text).
    /// `u32::MAX` ⇒ unset (the layout-only synthetic paragraphs the
    /// composition preview produces, plus older callers).
    pub source_paragraph_id: u32,
    /// Phase 2 audit (gap D.1) — complex-field overlays the paginator
    /// re-evaluates per page. Layout fills this from
    /// [`engine::Paragraph::fields`]; the paginator mutates
    /// `evaluated_text` on the page-owned copy of the `ParagraphBox`.
    pub fields: Vec<LayoutField>,
    /// Phase 2 audit (gap A.12) — line indices after which the
    /// paginator must force `flush_page`. Populated by the layout
    /// builder for every line containing a `\u{000C}` FORM FEED (the
    /// reader's representation of `<w:br w:type="page"/>`). The
    /// paginator's `push_paragraph_split` consults this list before
    /// the budget-based split so a mid-paragraph page break fires
    /// regardless of remaining content height. Indices remap on
    /// paragraph split (head keeps `[i ≤ split_idx]`; tail keeps
    /// `[i > split_idx]` shifted by `split_idx + 1`).
    pub page_break_after_line: Vec<usize>,
}

impl ParagraphBox {
    /// Sentinel for a paragraph not associated with a source-doc text.
    pub const NO_SOURCE_ID: u32 = u32::MAX;
}

/// A shaped list marker living in the paragraph's leading-edge gutter.
/// Holds its own `VisualRun` so the renderer can paint it without
/// special-casing — it's drawn after the line runs in the same scene
/// pass — but it does not participate in line layout, BiDi reordering,
/// or justification.
#[derive(Debug, Clone)]
pub struct MarkerBox {
    /// Relative to the parent [`ParagraphBox`] origin. `origin.y` lands at
    /// the first line's `origin.y`; `origin.x` is the marker's leading edge.
    pub origin: Point,
    pub baseline: f32,
    pub run: VisualRun,
    /// Total advance of `run.glyphs` — pre-computed so the renderer doesn't
    /// have to re-sum.
    pub width: f32,
}

/// Top-level page child — Phase 5 PR 2. The page-build pipeline emits one
/// `LayoutBlock` per `engine::Block`. Tables, like paragraphs, carry their
/// own `origin` relative to the page content area.
#[derive(Debug, Clone)]
pub enum LayoutBlock {
    Paragraph(ParagraphBox),
    Table(TableBox),
}

impl LayoutBlock {
    pub fn origin(&self) -> Point {
        match self {
            LayoutBlock::Paragraph(p) => p.origin,
            LayoutBlock::Table(t) => t.origin,
        }
    }
    pub fn set_origin(&mut self, o: Point) {
        match self {
            LayoutBlock::Paragraph(p) => p.origin = o,
            LayoutBlock::Table(t) => t.origin = o,
        }
    }
    pub fn size(&self) -> Size {
        match self {
            LayoutBlock::Paragraph(p) => p.size,
            LayoutBlock::Table(t) => t.size,
        }
    }
    pub fn as_paragraph(&self) -> Option<&ParagraphBox> {
        match self {
            LayoutBlock::Paragraph(p) => Some(p),
            _ => None,
        }
    }
    pub fn as_table(&self) -> Option<&TableBox> {
        match self {
            LayoutBlock::Table(t) => Some(t),
            _ => None,
        }
    }
}

/// A laid-out table block (Phase 5 PR 2). `origin` is relative to the
/// parent `PageBox` content area (or, for a nested table, to its
/// containing `TableCellBox` origin).
#[derive(Debug, Clone)]
pub struct TableBox {
    pub origin: Point,
    pub size: Size,
    /// Column widths in device px. Length matches the logical column count
    /// from `engine::Table::grid`; cells with `grid_span > 1` consume
    /// multiple entries.
    pub columns: Vec<f32>,
    pub rows: Vec<TableRowBox>,
    /// Outer table border strokes — painted by the renderer over the
    /// entire table rectangle. `None` per-edge ⇒ no stroke (Word's default
    /// table has no borders unless `<w:tblBorders>` says so).
    pub outer_borders: engine::CellBorders,
}

#[derive(Debug, Clone)]
pub struct TableRowBox {
    /// Relative to parent [`TableBox`] origin.
    pub origin: Point,
    pub size: Size,
    pub cells: Vec<TableCellBox>,
}

#[derive(Debug, Clone)]
pub struct TableCellBox {
    /// Relative to parent [`TableRowBox`] origin.
    pub origin: Point,
    pub size: Size,
    pub grid_span: u8,
    pub v_merge: engine::VMergeRole,
    pub borders: engine::CellBorders,
    pub shading: Option<[u8; 4]>,
    /// Recursive content — paragraphs and nested tables. Phase 5 PR 2
    /// recursion bound: practical 8 levels (matches Word). Deeper
    /// nesting silently truncates with a placeholder.
    pub content: Vec<LayoutBlock>,
    /// Phase 2 audit (gap B.1/B.2) — effective inner padding in layout
    /// pixels, already resolved against `<w:tcMar>` / `<w:tblCellMar>`
    /// / Word stock defaults by the layout solver. The renderer
    /// offsets content origin by `(left, top)` and the cell's size
    /// already includes `(left + right, top + bottom)` of padding.
    pub padding_left: f32,
    pub padding_top: f32,
    pub padding_right: f32,
    pub padding_bottom: f32,
}

/// A laid-out page — one element of the box tree the renderer consumes.
/// Phase 6 turned the engine output into `Vec<PageBox>`: the paginator emits
/// one `PageBox` per flow page; sections that change geometry produce a
/// fresh `PageBox` with the new `size` / `margins`.
#[derive(Debug, Clone)]
pub struct PageBox {
    pub size: Size,
    pub margins: Margins,
    pub blocks: Vec<LayoutBlock>,
    /// Phase 6 — paragraph plain text for the section's `<w:headerReference>`,
    /// painted in the top margin band on every page of the section. `None`
    /// for sections with no header reference, or when the header part is
    /// missing from the archive.
    pub header: Option<HeaderFooterBox>,
    /// Mirror of `header`, painted in the bottom margin band.
    pub footer: Option<HeaderFooterBox>,
    /// Phase 8a — footnote band. Each entry is the laid-out body of one
    /// `<w:footnote>` whose `<w:footnoteReference>` lands somewhere in
    /// this page's body content. Painted above the bottom margin in
    /// emission order, separated from the body by a thin horizontal
    /// rule. Origins are relative to the band's top-left.
    pub footnotes: Vec<FootnoteEntry>,
}

/// Phase 8a — one laid-out footnote inside a [`PageBox::footnotes`] band.
/// The marker text (e.g. `"1"`) is painted at the entry's leading edge so
/// the reader can tie body text to footnote.
#[derive(Debug, Clone)]
pub struct FootnoteEntry {
    pub id: u32,
    pub marker: String,
    pub paragraph: ParagraphBox,
}

/// A laid-out header / footer band — paragraph plain text positioned within
/// the page's top or bottom margin. The Phase 6 cut is deliberately small:
/// rich formatting in header / footer paragraphs ships with the Phase 7
/// sprint that promotes them through the same shape / BiDi pipeline the
/// body uses.
#[derive(Debug, Clone)]
pub struct HeaderFooterBox {
    /// Each laid-out paragraph in the band. Origins are relative to the
    /// band's top-left.
    pub paragraphs: Vec<ParagraphBox>,
}
