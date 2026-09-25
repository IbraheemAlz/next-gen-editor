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

/// Issue #82 — one available horizontal range of a line band, in
/// paragraph-box-relative px (the same space as `LineBox::origin.x`).
/// A band with no float cutouts has exactly one segment spanning the
/// content box; a float that cuts into the band splits it into the
/// ranges left and right of the object (plus its wrap distances). This is
/// the model that unifies column bounds and float cutouts (and, for the
/// text-frame epic #83, frame cutouts): the line composer fills segments
/// in reading order and never sees the object itself.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LineSegment {
    pub x0: f32,
    pub x1: f32,
}

impl LineSegment {
    pub fn width(&self) -> f32 {
        (self.x1 - self.x0).max(0.0)
    }
}

/// Issue #82 — which side(s) of a floating object text may flow on
/// (`<wp:wrapSquare wrapText="…">`, ECMA-376 §20.4.3.7 `ST_WrapText`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WrapSide {
    #[default]
    Both,
    Left,
    Right,
    Largest,
}

/// Issue #82 — the wrap contract a floating object declares, in layout
/// px: the wrap kind (`<wp:wrapNone>` / `Square` / `Tight` / `Through` /
/// `TopAndBottom`), the side rule, the four wrap distances (`distT` /
/// `distB` / `distL` / `distR`, already scaled) and — for tight / through
/// wrap — the `<wp:wrapPolygon>` vertices in Word's 21600-unit shape
/// space (`(21600, 21600)` is the object's bottom-right corner). `None`
/// polygon on a tight / through object falls back to the bounding box
/// and is reported (`DegradeReason::WrapPolygonFallback`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FloatWrap {
    pub kind: engine::WrapKind,
    pub side: WrapSide,
    pub dist_top: f32,
    pub dist_bottom: f32,
    pub dist_left: f32,
    pub dist_right: f32,
    pub polygon: Option<Vec<Point>>,
}

impl FloatWrap {
    /// `true` when this object cuts text at all — every kind except
    /// `WrapKind::None` (behind / in front of text).
    pub fn cuts_text(&self) -> bool {
        !matches!(self.kind, engine::WrapKind::None)
    }
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
    /// Audit gap A.M1 — `<w:vertAlign>` baseline shift in layout pt.
    /// Positive lifts the run above the line baseline (superscript);
    /// negative drops it below (subscript). The renderer subtracts
    /// this from each glyph's pen Y when emitting paint commands.
    /// `0.0` for the baseline (the default — no per-glyph offset).
    pub baseline_shift_px: f32,
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
    /// Audit gap A.M1 — `<w:vertAlign>` baseline shift in layout pt
    /// (positive = up, the renderer subtracts from the glyph baseline).
    /// Set by `build_style_spans` for `Superscript` (positive shift,
    /// ~30 % of base px) / `Subscript` (negative). `0.0` for baseline
    /// runs — paint takes the fast path with no per-glyph offset.
    /// `px_size` already reflects the ~65 % shrink so the shaper
    /// produces small glyphs directly; the shift only re-anchors the
    /// pen Y. Keeping shift + shrink on the layout span (vs the
    /// renderer) means line-height math sees the smaller cap-height
    /// and the line doesn't grow visibly when a single superscript
    /// gets inserted into a body run.
    pub baseline_shift_px: f32,
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
    /// Phase 8a / issue #80 — when set, this glyph is the FIRST glyph of
    /// a note marker (`"1"`, `"iv"`, …): the marker text itself. The
    /// marker's digits are shaped into real glyphs at layout time (this
    /// glyph plus `synthetic` continuation glyphs sharing its cluster), so
    /// the renderer paints them like any run; the field is metadata for
    /// the paginator and hit-testing.
    pub inline_footnote_marker: Option<String>,
    /// Issue #80 — the note this marker REFERENCES (body text only). `None`
    /// on the self-mark heading a note body and on every non-marker glyph.
    /// The paginator reserves the note's band space when the line carrying
    /// this glyph is placed.
    pub inline_note_anchor: Option<engine::NoteAnchor>,
    /// Phase 7 — physical height of the anchored inline object in layout
    /// pixels. Folded into the line's ascent so the line grows to host
    /// the image without clipping.
    pub inline_object_height: f32,
    /// Issue #69 — when set, this glyph is the U+FFFC sentinel of a
    /// FLOATING object (`<wp:anchor>`). The glyph itself reserves no
    /// width (`x_advance == 0`) and does not grow the line; the page
    /// assembler resolves the object's rectangle from the spec against
    /// the page / column / paragraph / line the glyph lands on
    /// (`crate::floats::resolve_page_floats`). Boxed — floats are rare
    /// and glyphs are cloned by the million.
    pub float: Option<Box<FloatGlyph>>,
    /// Issue #81 — `Some` on a TAB glyph whose stop carries a
    /// `<w:tab w:leader>`: the renderer fills the tab's advance with the
    /// leader pattern (TOC dot leaders). Geometry-neutral — the
    /// advance is the tab's own, so fingerprints never see it.
    pub leader: Option<TabLeaderKind>,
}

/// Issue #81 — the fill a leadered tab paints across its advance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TabLeaderKind {
    Dot,
    Hyphen,
    Underscore,
    Heavy,
    MiddleDot,
}

/// Issue #69 — the floating-object payload a sentinel glyph carries into
/// pagination. Sizes are layout px (already scaled); the positioning
/// spec is frame-relative and resolved once the anchor's page is known.
#[derive(Debug, Clone, PartialEq)]
pub struct FloatGlyph {
    /// Archive relationship id of the image blob to paint.
    pub rel_id: String,
    /// Object extent in layout px.
    pub width: f32,
    pub height: f32,
    pub spec: FloatSpec,
    /// Issue #82 — the wrap contract (kind, side, distances, polygon)
    /// the page assembler turns into line cutouts.
    pub wrap: FloatWrap,
}

/// Issue #69 — one positioning axis of a float in layout units. Mirrors
/// `engine::FloatOffset` with EMUs already converted to px and the
/// percentage already normalised to a `0.0..=1.0` fraction of the frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FloatOffsetPx {
    Px(f32),
    Align(engine::FloatAlign),
    /// Fraction of the reference frame's extent (`0.5` ⇒ 50 %).
    Fraction(f32),
}

/// Issue #69 — resolved-unit twin of `engine::FloatAnchor`: everything
/// the page assembler needs to place the object, nothing the writer
/// needs (wrap XML, docPr) — those stay on the document model.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FloatSpec {
    pub h_frame: engine::HRelativeFrom,
    pub h_offset: FloatOffsetPx,
    pub v_frame: engine::VRelativeFrom,
    pub v_offset: FloatOffsetPx,
    /// `Some((x, y))` ⇒ `simplePos="1"`: page-absolute placement in px
    /// that overrides both axes.
    pub simple_pos: Option<Point>,
    /// `relativeHeight` — z-order among floats (higher paints later).
    pub z_order: u32,
    /// `behindDoc` — paint under the text.
    pub behind_doc: bool,
    /// `hidden` — resolved into a [`FloatBox`] for hit-testing but never
    /// painted.
    pub hidden: bool,
}

/// Issue #69 — which laid-out paragraph on a page owns a float's anchor,
/// so hit-testing can map the float back to an engine block path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloatAnchorRef {
    /// `page.blocks[block]` (a paragraph, or a table when `cell` is set:
    /// then `rows[row].cells[col].content[inner]`).
    Body {
        block: usize,
        cell: Option<CellAnchorRef>,
    },
    /// A paragraph inside the page's header band.
    Header,
    /// A paragraph inside the page's footer band.
    Footer,
}

/// Issue #69 — table-cell coordinates of a float anchored inside a cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellAnchorRef {
    pub row: usize,
    pub col: usize,
    pub inner: usize,
}

/// Issue #69 — one positioned floating object on a page. `origin` is
/// **page-relative** (the page's top-left corner, NOT the content area —
/// floats are placed against page / margin / column frames, so the
/// renderer adds only the page top). `frame_origin` is the top-left of
/// the reference frame the object's offsets were measured from, so an
/// interactive drag can convert a new page position back into
/// frame-relative EMU offsets (`Command::MoveImage`).
#[derive(Debug, Clone, PartialEq)]
pub struct FloatBox {
    pub origin: Point,
    pub size: Size,
    pub rel_id: String,
    /// Sentinel byte offset in the anchor paragraph's source text.
    pub at: u32,
    pub anchor: FloatAnchorRef,
    pub z_order: u32,
    pub behind_doc: bool,
    pub hidden: bool,
    pub frame_origin: Point,
    /// Issue #82 — the wrap contract carried from the sentinel glyph, so
    /// the wrap plan (`crate::wrap::derive_plan`) can register this
    /// object's cutouts against every paragraph it overlaps.
    pub wrap: FloatWrap,
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
    /// Source byte offset where this line begins. Normally redundant with
    /// `runs[0].source_range.start`, but load-bearing for EMPTY lines that
    /// carry no runs — a trailing/doubled soft break (`U+2028`) emits a
    /// runless placeholder line, and the caret/hit-test geometry needs its
    /// true source offset to resolve a caret at `offset == text.len()` onto
    /// it (otherwise an empty line reports byte 0 and the caret snaps back
    /// to the paragraph's first line).
    pub source_start: u32,
    /// Issue #82 — the available horizontal ranges of the band this line
    /// sits in, after float cutouts, sorted left → right in
    /// paragraph-box-relative px. **Empty ⇒ the whole content box** — the
    /// pre-#82 shape, which allocates nothing and is what every paragraph
    /// without a nearby float still produces (fingerprints unchanged by
    /// construction). When non-empty, [`Self::segment`] indexes the range
    /// this line's runs were composed into; sibling lines filling the
    /// other ranges of the same band share `origin.y`, `baseline` and
    /// `height` (one band, one baseline).
    pub segments: Vec<LineSegment>,
    /// Issue #82 — index into [`Self::segments`] (0 when `segments` is
    /// empty).
    pub segment: usize,
}

impl LineBox {
    /// Issue #82 — the segment this line was composed into, when the
    /// band was cut by a float. `None` for a full-width line.
    pub fn segment_range(&self) -> Option<LineSegment> {
        self.segments.get(self.segment).copied()
    }
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
    /// Audit gap A.M4 — `<w:pPr><w:pBdr>` border strokes painted
    /// around the paragraph's bounding rectangle. Renderer pulls
    /// strokes from `top` / `left` / `bottom` / `right` and reuses
    /// the cell-border drawing primitive. `None` ⇒ no border (the
    /// default — fast path skips stroke emission entirely).
    pub borders: Option<engine::CellBorders>,
    /// Sprint 6 (UI Edition) — `<w:pPr><w:shd>` paragraph shading.
    /// Renderer paints a filled rect at the paragraph's bounding
    /// rectangle BEFORE `borders` (so the strokes draw on top of the
    /// fill). `None` ⇒ no shading (fast path skips emission).
    pub shading: Option<[u8; 4]>,
    /// Issue #87 — `<w:keepNext/>`: keep this paragraph's last line on
    /// the same page as the next block's first line. The paginator
    /// honours it as an *optional* constraint: a chain of keep-next
    /// blocks moves to the next page together when the following block
    /// does not fit, and the constraint is released (with a
    /// `KeepChainDropped` note) when the chain is already at a page top
    /// or the watchdog reaches stage (a). A split paragraph's head never
    /// keeps (its keep is with its own tail); the tail inherits the flag.
    /// Not yet wired from `engine::ParaProperties::keep_next` — the
    /// engine adapter leaves it `false` until the golden corpus is
    /// re-verified with keep-with-next enabled.
    pub keep_next: bool,
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
    /// Audit gap A.M9 — `<w:trPr><w:tblHeader/>` toggle. When `true`,
    /// the paginator clones this row at the top of every page the table
    /// continues onto after a split. Header rows still pay their own
    /// budget on the original page.
    pub header: bool,
    /// Audit gap C.M2 — `<w:trPr><w:cantSplit/>` toggle. Issue #155: a
    /// row that does not fit the rest of the page is cut at a line
    /// boundary (Word's default "allow row to break across pages") unless
    /// this flag is set — then it moves whole to the next page; a flagged
    /// row taller than a whole page is placed atomically and clips
    /// (`DegradeReason::OversizeLine`, issue #91), Word's reading of the
    /// flag. The layout pass also sets it for `<w:trHeight
    /// w:hRule="exact">` rows: an exact-height row never breaks.
    pub cant_split: bool,
    /// Issue #91 — index of the model row (`engine::Table::rows`) this
    /// box renders. Equal to the row's position in an unsplit table; a
    /// continuation fragment re-bases its rows and a repeated header
    /// clone keeps its source header's index, so hit-testing maps every
    /// fragment row back to the right model row.
    pub source_row: u32,
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
    /// Issue #91 — index of the model cell block `content[0]` renders.
    /// 0 for an unsplit cell; the continuation of a row split inside its
    /// cells starts mid-cell (a paragraph cut at a line boundary keeps
    /// its own index, like a body paragraph split across pages).
    pub content_offset: u32,
}

/// Which header/footer slot a page resolved (issue #74). Lives here
/// (not `paginate`) because [`PageBox`] carries it; `paginate`
/// re-exports for its historical path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HeaderRole {
    #[default]
    Default,
    First,
    Even,
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
    /// Phase 3 (#39) — `<w:pgMar w:header>`: distance from the page's top
    /// edge to the header band's top, in the page's own units (already
    /// scaled with `size` / `margins`). Previously parsed + threaded to
    /// the paginator but dropped here, leaving the renderer on a
    /// hardcoded fraction of the margin.
    pub header_offset: f32,
    /// `<w:pgMar w:footer>`: distance from the page's bottom edge to the
    /// footer band's bottom.
    pub footer_offset: f32,
    /// Phase 8a / issue #80 — footnote band. Each entry is the laid-out
    /// body (or the page's share of a split body) of one `<w:footnote>`
    /// whose `<w:footnoteReference>` lands in this page's body content —
    /// plus, at the head of the band, the continuation of a note split
    /// off the previous page. Painted in band order below the body at
    /// [`NoteBand::y`], separated from it by a rule.
    pub footnotes: NoteBand,
    /// Issue #80 — endnote band: the notes `<w:endnotePr><w:pos>` collects
    /// at this section's / the document's end. Flows beneath the body's
    /// last line (never page-bottom anchored) and continues onto the
    /// following pages like a footnote continuation.
    pub endnotes: NoteBand,
    /// Issue #74 — the header/footer slot this page resolved at flush
    /// time. Enter-header/footer derives the double-clicked page's
    /// role from this instead of re-deriving parity in a second place.
    pub hf_role: HeaderRole,
    /// Issue #43 — the FORMATTED (displayed) page number, exactly what
    /// a PAGE field on this page renders (pgNumType-rebased). The
    /// field-resolution reshape pass and Even/Odd filler logic read it.
    pub page_number: u32,
    /// Issue #69 — every floating object whose anchor landed on this
    /// page, already positioned (page-relative). Resolved by the
    /// paginator at flush time from the sentinel glyphs in `blocks` and
    /// the bands; the renderer paints `behind_doc` entries under the
    /// content and the rest over it, each group in `z_order`. Empty for
    /// documents without floats — the fingerprint / fast-path checks
    /// treat an empty list as "no geometry", so fixed-geometry documents
    /// keep their pinned values.
    pub floats: Vec<FloatBox>,
}

impl PageBox {
    /// Phase 3 (#39) — page-relative Y of the header band's top. THE
    /// single source of band placement: `render::scene` paints with it
    /// and engine-wasm's story hit-testing/caret geometry reads the
    /// same method, so paint and hit-test cannot diverge.
    pub fn header_band_top(&self) -> f32 {
        self.header_offset
    }

    /// Page-relative Y of the footer band's top: the band's laid-out
    /// content bottom-anchors at `footer_offset` above the page's
    /// bottom edge (Word's "Footer from Bottom" semantics). Overflow
    /// expansion for bands taller than the margin is out of scope —
    /// tracked as a follow-up.
    pub fn footer_band_top(&self) -> f32 {
        let content_h = self
            .footer
            .as_ref()
            .map_or(0.0, HeaderFooterBox::content_height);
        self.size.height - self.footer_offset - content_h
    }
}

/// Issue #80 — one note band on a page: the entries in band order plus
/// where the band sits. `y` is page-relative (top edge of the FIRST
/// entry; the separator rule paints above it). THE single source of band
/// placement: paint, PDF and note hit-testing all read it, so pixels and
/// caret geometry cannot diverge.
#[derive(Debug, Clone, Default)]
pub struct NoteBand {
    pub entries: Vec<FootnoteEntry>,
    /// Page-relative Y of `entries[0]`'s top. `0.0` when empty.
    pub y: f32,
    /// `true` when the band opens with a note continued from the
    /// previous page: the separator paints as the full-width
    /// continuation rule instead of the short one.
    pub continuation: bool,
}

impl NoteBand {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Total laid-out height of the band's entries (no separator gap).
    pub fn content_height(&self) -> f32 {
        self.entries
            .iter()
            .map(|e| e.origin.y + e.content_height())
            .fold(0.0, f32::max)
    }

    /// Every paragraph box in the band, recursing through table cells.
    pub fn for_each_paragraph<'a>(&'a self, f: &mut impl FnMut(&'a ParagraphBox)) {
        for e in &self.entries {
            e.for_each_paragraph(f);
        }
    }

    /// Mutable twin of [`Self::for_each_paragraph`].
    pub fn for_each_paragraph_mut(&mut self, f: &mut impl FnMut(&mut ParagraphBox)) {
        for e in &mut self.entries {
            e.for_each_paragraph_mut(f);
        }
    }
}

/// Phase 8a / issue #80 — one laid-out note inside a [`NoteBand`]: the
/// note's block model laid out at the content width, with the self-mark
/// shaped into the first paragraph. `origin` is band-relative; block
/// origins are entry-relative.
#[derive(Debug, Clone)]
pub struct FootnoteEntry {
    /// The note's OOXML `w:id` (a key into the engine's story map).
    pub id: u32,
    pub kind: engine::NoteKind,
    /// The displayed number (`"1"`, `"iv"`, …) — informational; the
    /// glyphs already carry it.
    pub marker: String,
    pub origin: Point,
    pub blocks: Vec<LayoutBlock>,
    /// Story-block index of `blocks[0]` inside the note body — `0` for a
    /// whole note or a head; a continuation resumes at the block the
    /// split cut (a mid-paragraph cut repeats that block's index, the
    /// fragment's glyph clusters being offsets into the full text).
    /// Note hit-testing maps entry block `j` to story block
    /// `first_block_index + j`.
    pub first_block_index: u32,
    /// This entry continues a note whose head sat on the previous page.
    pub continued_from_previous: bool,
    /// This entry is the head of a note that continues on the next page
    /// (its blocks may end with the continuation-notice story).
    pub continues_on_next: bool,
}

impl FootnoteEntry {
    /// Deepest block bottom edge, entry-relative.
    pub fn content_height(&self) -> f32 {
        self.blocks
            .iter()
            .map(|b| b.origin().y + b.size().height)
            .fold(0.0, f32::max)
    }

    /// Every paragraph box in the entry, recursing through table cells.
    pub fn for_each_paragraph<'a>(&'a self, f: &mut impl FnMut(&'a ParagraphBox)) {
        for_each_paragraph_in_blocks(&self.blocks, f);
    }

    /// Mutable twin of [`Self::for_each_paragraph`].
    pub fn for_each_paragraph_mut(&mut self, f: &mut impl FnMut(&mut ParagraphBox)) {
        for_each_paragraph_in_blocks_mut(&mut self.blocks, f);
    }
}

/// A laid-out header / footer band. Issue #72 widened it from a flat
/// `Vec<ParagraphBox>` to the body's own [`LayoutBlock`] list so a
/// `<w:tbl>` inside a header part lays out, paints and hit-tests with
/// the same table machinery the body uses.
#[derive(Debug, Clone)]
pub struct HeaderFooterBox {
    /// Each laid-out block in the band. Origins are relative to the
    /// band's top-left.
    pub blocks: Vec<LayoutBlock>,
    /// Issue #43 — the OOXML relationship id of the part this band was
    /// laid from. The per-page field-resolution reshape re-derives the
    /// SOURCE blocks from it (`doc.headers[rid]`), so a page's band can
    /// re-lay with that page's resolved PAGE/NUMPAGES text without any
    /// page→section→role bookkeeping. `None` for synthetic test bands.
    pub source_rid: Option<String>,
}

impl HeaderFooterBox {
    /// Phase 3 (#39) — the band content's laid-out height: the deepest
    /// block bottom edge, band-relative. Feeds
    /// [`PageBox::footer_band_top`]'s bottom-anchoring.
    pub fn content_height(&self) -> f32 {
        self.blocks
            .iter()
            .map(|b| match b {
                LayoutBlock::Paragraph(p) => p.origin.y + p.size.height,
                LayoutBlock::Table(t) => t.origin.y + t.size.height,
            })
            .fold(0.0, f32::max)
    }

    /// Every paragraph box in the band, recursing through table cells —
    /// the iteration shape the field evaluator and PDF exporter share.
    pub fn for_each_paragraph<'a>(&'a self, f: &mut impl FnMut(&'a ParagraphBox)) {
        for_each_paragraph_in_blocks(&self.blocks, f);
    }

    /// Mutable twin of [`Self::for_each_paragraph`] — the paginator's
    /// per-page field stamping walks this.
    pub fn for_each_paragraph_mut(&mut self, f: &mut impl FnMut(&mut ParagraphBox)) {
        for_each_paragraph_in_blocks_mut(&mut self.blocks, f);
    }
}

/// Recursive paragraph walk over a laid-out block list (skips
/// vertically-merged continuation cells, whose content is a clone of
/// the merge origin's).
pub fn for_each_paragraph_in_blocks<'a>(
    blocks: &'a [LayoutBlock],
    f: &mut impl FnMut(&'a ParagraphBox),
) {
    for b in blocks {
        match b {
            LayoutBlock::Paragraph(p) => f(p),
            LayoutBlock::Table(t) => {
                for row in &t.rows {
                    for cell in &row.cells {
                        if matches!(cell.v_merge, engine::VMergeRole::Continue) {
                            continue;
                        }
                        for_each_paragraph_in_blocks(&cell.content, f);
                    }
                }
            }
        }
    }
}

/// Mutable twin of [`for_each_paragraph_in_blocks`].
pub fn for_each_paragraph_in_blocks_mut(
    blocks: &mut [LayoutBlock],
    f: &mut impl FnMut(&mut ParagraphBox),
) {
    for b in blocks {
        match b {
            LayoutBlock::Paragraph(p) => f(p),
            LayoutBlock::Table(t) => {
                for row in &mut t.rows {
                    for cell in &mut row.cells {
                        if matches!(cell.v_merge, engine::VMergeRole::Continue) {
                            continue;
                        }
                        for_each_paragraph_in_blocks_mut(&mut cell.content, f);
                    }
                }
            }
        }
    }
}
