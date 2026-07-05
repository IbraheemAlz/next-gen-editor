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
    /// Source byte offset where this line begins. Normally redundant with
    /// `runs[0].source_range.start`, but load-bearing for EMPTY lines that
    /// carry no runs — a trailing/doubled soft break (`U+2028`) emits a
    /// runless placeholder line, and the caret/hit-test geometry needs its
    /// true source offset to resolve a caret at `offset == text.len()` onto
    /// it (otherwise an empty line reports byte 0 and the caret snaps back
    /// to the paragraph's first line).
    pub source_start: u32,
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
    /// Audit gap C.M2 — `<w:trPr><w:cantSplit/>` toggle. When `true`,
    /// the row never splits across a page boundary; the paginator
    /// flushes the page and pushes the row whole on the next. When
    /// `false`, the paginator MAY split the row's cell paragraphs
    /// mid-row (deferred — current implementation keeps every row
    /// atomic).
    pub cant_split: bool,
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
    /// Phase 8a — footnote band. Each entry is the laid-out body of one
    /// `<w:footnote>` whose `<w:footnoteReference>` lands somewhere in
    /// this page's body content. Painted above the bottom margin in
    /// emission order, separated from the body by a thin horizontal
    /// rule. Origins are relative to the band's top-left.
    pub footnotes: Vec<FootnoteEntry>,
    /// Issue #74 — the header/footer slot this page resolved at flush
    /// time. Enter-header/footer derives the double-clicked page's
    /// role from this instead of re-deriving parity in a second place.
    pub hf_role: HeaderRole,
    /// Issue #43 — the FORMATTED (displayed) page number, exactly what
    /// a PAGE field on this page renders (pgNumType-rebased). The
    /// field-resolution reshape pass and Even/Odd filler logic read it.
    pub page_number: u32,
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

/// Phase 8a — one laid-out footnote inside a [`PageBox::footnotes`] band.
/// The marker text (e.g. `"1"`) is painted at the entry's leading edge so
/// the reader can tie body text to footnote.
#[derive(Debug, Clone)]
pub struct FootnoteEntry {
    pub id: u32,
    pub marker: String,
    pub paragraph: ParagraphBox,
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
