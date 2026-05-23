//! `engine` — document model + undo stack.
//!
//! Phase 1 weeks 15–18: plain-text paragraphs, in-place text insertion,
//! cheap snapshots via `im::Vector` for undo/redo.

use im::Vector;

pub mod html;

/// Top-level document block (Phase 5 PR 1). Tables sit alongside
/// paragraphs in the body; future block variants (Phase 7 floating
/// images, Phase 8 footnotes) extend this enum.
#[derive(Debug, Clone)]
pub enum Block {
    Paragraph(Paragraph),
    Table(Table),
}

impl Block {
    pub fn as_paragraph(&self) -> Option<&Paragraph> {
        match self {
            Block::Paragraph(p) => Some(p),
            Block::Table(_) => None,
        }
    }
    pub fn as_paragraph_mut(&mut self) -> Option<&mut Paragraph> {
        match self {
            Block::Paragraph(p) => Some(p),
            Block::Table(_) => None,
        }
    }
    pub fn as_table(&self) -> Option<&Table> {
        match self {
            Block::Table(t) => Some(t),
            Block::Paragraph(_) => None,
        }
    }
    pub fn as_table_mut(&mut self) -> Option<&mut Table> {
        match self {
            Block::Table(t) => Some(t),
            Block::Paragraph(_) => None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct DocumentTree {
    /// Top-level block sequence. Previously a flat `Vector<Paragraph>`;
    /// Phase 5 PR 1 widened it to `Vector<Block>` so tables can appear at
    /// any document position. Still `im::Vector` so undo snapshots clone
    /// in O(1) — table cells use plain `Vec<Block>` instead.
    pub blocks: Vector<Block>,
}

/// Address of a `Block` inside a `DocumentTree`. Walks from the root
/// `blocks: Vector<Block>` down through table cells. Phase 5 PR 3.
///
/// Examples:
/// - `BlockPath { steps: vec![PathStep::Block(2)] }` — the 3rd top-level
///   block.
/// - `BlockPath { steps: vec![PathStep::Block(2), PathStep::Cell{row:1,
///   col:0}, PathStep::Block(0)] }` — the first paragraph in the
///   top-left cell of row 1 of the 3rd top-level block (which must be
///   `Block::Table`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct BlockPath {
    pub steps: Vec<PathStep>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PathStep {
    /// Index into the current `&[Block]` / `&Vector<Block>`.
    Block(u32),
    /// Step from a `Block::Table` into one of its cells.
    Cell { row: u32, col: u32 },
}

impl BlockPath {
    /// Empty path — addresses the root `blocks` container itself.
    pub fn root() -> Self {
        Self::default()
    }
    /// Path to the Nth top-level block.
    pub fn top(idx: u32) -> Self {
        Self {
            steps: vec![PathStep::Block(idx)],
        }
    }
    pub fn push(mut self, step: PathStep) -> Self {
        self.steps.push(step);
        self
    }
}

/// A selectable font family (Backlog #9). `engine-wasm` resolves it to a
/// loaded font face when building layout style spans; the pure document model
/// just stores the choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FontFamily {
    Amiri,
    LiberationSans,
    NotoNaskhArabic,
}

/// Inline style for a run of characters: font size, colour, the
/// bold / italic / underline / strikethrough flags, a background (highlight)
/// colour, and a font family. All are carried through layout and render.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SpanStyle {
    pub font_size: Option<f32>,
    pub color: Option<[u8; 4]>,
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub underline: Option<bool>,
    pub strike: Option<bool>,
    pub bg_color: Option<[u8; 4]>,
    pub font_family: Option<FontFamily>,
}

impl SpanStyle {
    /// Overlay `patch`'s set fields onto `self`.
    pub fn merged_with(self, patch: SpanStyle) -> SpanStyle {
        SpanStyle {
            font_size: patch.font_size.or(self.font_size),
            color: patch.color.or(self.color),
            bold: patch.bold.or(self.bold),
            italic: patch.italic.or(self.italic),
            underline: patch.underline.or(self.underline),
            strike: patch.strike.or(self.strike),
            bg_color: patch.bg_color.or(self.bg_color),
            font_family: patch.font_family.or(self.font_family),
        }
    }
}

/// A styled byte range `[start, end)` within a paragraph.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StyleRun {
    pub start: u32,
    pub end: u32,
    pub style: SpanStyle,
}

/// Paragraph text alignment (Backlog #9). `Start` / `End` are
/// writing-direction-relative — they resolve against the base direction at
/// layout time; `Center` and `Justify` are absolute. Mirrors
/// `text_pipeline::Alignment`; kept here so the pure document model carries no
/// dependency on the text-shaping crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alignment {
    Start,
    End,
    Center,
    Justify,
}

/// Paragraph indentation. OOXML carries these as twips (1/1440 inch); the
/// engine stores them in the same unit and converts to layout pixels at
/// `engine-wasm` boundary so the pure document model has no float-DPI
/// dependency. `first_line` and `hanging` are mutually exclusive in OOXML;
/// the reader sets the matching field and zeroes the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Indent {
    pub start_twips: i32,
    pub end_twips: i32,
    pub first_line_twips: i32,
    pub hanging_twips: i32,
}

/// Per-paragraph vertical spacing. Twips, matching `<w:spacing>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Spacing {
    pub before_twips: i32,
    pub after_twips: i32,
}

/// Explicit paragraph base direction (`<w:bidi/>` for RTL). `None` lets
/// `text_pipeline::first_strong_direction` infer from the first strong
/// character — the current document-wide default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextDirection {
    Ltr,
    Rtl,
}

/// Per-paragraph line-height override (`<w:spacing w:line>` /
/// `w:lineRule>`). `None` inherits the renderer's default line height.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LineHeight {
    /// `w:lineRule="auto"` — `w:line` is a 240-ths multiple of single line
    /// height; we store the integer twips for round-trip, layout converts.
    Auto { twips: i32 },
    /// `w:lineRule="exact"` — fixed twip height; overflow clips.
    Exact { twips: i32 },
    /// `w:lineRule="atLeast"` — minimum; grows for tall glyphs.
    AtLeast { twips: i32 },
}

/// Paragraph-level properties parsed from `<w:pPr>`. Holds every field the
/// engine needs to round-trip a Word paragraph; layout consumes the
/// alignment / indent / spacing / direction / line-height subset.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ParaProperties {
    pub alignment: Option<Alignment>,
    pub indent: Indent,
    pub spacing: Spacing,
    pub direction: Option<TextDirection>,
    pub line_height: Option<LineHeight>,
    pub keep_next: bool,
    pub keep_lines: bool,
    pub page_break_before: bool,
}

impl ParaProperties {
    /// Overlay `patch` onto `self` using OOXML cascade semantics: a child
    /// style with a *set* (non-default) field overrides the parent. Used by
    /// the Phase 3 `format_docx::style_resolver` to fold a basedOn chain
    /// root → leaf and then drop direct `<w:pPr>` on top.
    ///
    /// **Known limitation.** Engine fields are flat (`Indent`, `Spacing` are
    /// non-`Option` structs), so we cannot distinguish "child specified 0"
    /// from "child inherited". A child whose `<w:ind w:start="0"/>` is
    /// intentional will lose to a parent's non-zero start. Real-world
    /// stylesheets virtually never set 0 explicitly, so the trade-off is
    /// acceptable for Phase 3; Phase 4+ may widen to `Option`.
    pub fn merged_with(self, patch: ParaProperties) -> ParaProperties {
        ParaProperties {
            alignment: patch.alignment.or(self.alignment),
            indent: if patch.indent == Indent::default() {
                self.indent
            } else {
                patch.indent
            },
            spacing: if patch.spacing == Spacing::default() {
                self.spacing
            } else {
                patch.spacing
            },
            direction: patch.direction.or(self.direction),
            line_height: patch.line_height.or(self.line_height),
            keep_next: patch.keep_next || self.keep_next,
            keep_lines: patch.keep_lines || self.keep_lines,
            page_break_before: patch.page_break_before || self.page_break_before,
        }
    }
}

/// `<w:numPr>` reference — a paragraph's binding to a numbering definition.
/// `num_id` keys into `word/numbering.xml`'s `<w:num>` entries; `ilvl`
/// (0-indexed) selects the level inside the bound `<w:abstractNum>`. The
/// resolved marker text lives in [`Paragraph::resolved_marker`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListItem {
    pub num_id: u32,
    pub ilvl: u8,
}

#[derive(Debug, Clone, Default)]
pub struct Paragraph {
    pub text: String,
    /// Non-overlapping styled ranges, sorted by `start`; default-styled ranges
    /// are omitted. An empty list is plain text.
    pub spans: Vec<StyleRun>,
    /// Paragraph-level properties (`<w:pPr>`). Default = inherit everything
    /// from the render config / document defaults.
    pub props: ParaProperties,
    /// Phase 4 list membership. `Some` when the paragraph carries a
    /// `<w:numPr>` reference; resolved marker is in [`Self::resolved_marker`].
    pub list_item: Option<ListItem>,
    /// Phase 4 cached list marker (`"1."`, `"a)"`, `"•"`, `"1.1.2."`).
    /// Populated by the numbering resolver after `parse_document_xml` returns,
    /// once the full paragraph sequence is known. `None` for non-list
    /// paragraphs and for list paragraphs whose `num_id` resolves to no
    /// definition (defensive — Word tolerates dangling numIds).
    pub resolved_marker: Option<String>,
    /// Phase 3 passthrough optimisation. `false` on load; flips to `true` the
    /// first time any engine mutation produces a derived paragraph. The writer
    /// emits `source_xml` verbatim when this is `false` and ignores it
    /// otherwise — so unmutated stylesheet-driven paragraphs round-trip
    /// byte-identical.
    pub dirty: bool,
    /// Raw `<w:p>...</w:p>` source bytes captured by the reader (Phase 3).
    /// `None` for paragraphs the engine synthesised (`from_text`, splits,
    /// pastes); `Some` for any paragraph parsed from a real `.docx`.
    pub source_xml: Option<Vec<u8>>,
}

impl Paragraph {
    /// Resolved style at byte offset `at` (default if no span covers it).
    pub fn style_at(&self, at: u32) -> SpanStyle {
        self.spans
            .iter()
            .find(|s| at >= s.start && at < s.end)
            .map_or(SpanStyle::default(), |s| s.style)
    }

    /// Return a copy with `patch` overlaid on the byte range `[start, end)`.
    /// Existing spans are split at the boundaries; every covered sub-range
    /// merges the patch's set fields. Adjacent equal spans are coalesced and
    /// default-only spans dropped, so the representation stays minimal.
    pub fn apply_style(&self, start: u32, end: u32, patch: SpanStyle) -> Paragraph {
        let text_len = self.text.len() as u32;
        let start = start.min(text_len);
        let end = end.min(text_len);
        if start >= end {
            return self.clone();
        }

        /* Every boundary: text extent, the patch range, existing span edges. */
        let mut bounds: Vec<u32> = vec![0, text_len, start, end];
        for s in &self.spans {
            bounds.push(s.start);
            bounds.push(s.end);
        }
        bounds.retain(|&b| b <= text_len);
        bounds.sort_unstable();
        bounds.dedup();

        /* Re-derive each interval's style, merging the patch where covered. */
        let mut spans: Vec<StyleRun> = Vec::new();
        for win in bounds.windows(2) {
            let (a, b) = (win[0], win[1]);
            let mut style = self.style_at(a);
            if a >= start && b <= end {
                style = style.merged_with(patch);
            }
            if style == SpanStyle::default() {
                continue;
            }
            match spans.last_mut() {
                Some(prev) if prev.end == a && prev.style == style => prev.end = b,
                _ => spans.push(StyleRun {
                    start: a,
                    end: b,
                    style,
                }),
            }
        }

        Paragraph {
            text: self.text.clone(),
            spans,
            props: self.props.clone(),
            list_item: self.list_item,
            resolved_marker: self.resolved_marker.clone(),
            dirty: true,
            source_xml: None,
        }
    }

    /// Byte range `[start, end)` of the word containing caret position
    /// `offset` — a whitespace-delimited span (PHASE_4_HEADLESS_UI.md §7,
    /// double-click select). When `offset` sits on whitespace, the run of
    /// whitespace is returned. `offset` is clamped to a char boundary.
    pub fn word_bounds(&self, offset: u32) -> (u32, u32) {
        let text = self.text.as_str();
        let len = text.len();
        if len == 0 {
            return (0, 0);
        }
        let mut off = (offset as usize).min(len);
        while off > 0 && !text.is_char_boundary(off) {
            off -= 1;
        }
        /* Classify by the char to the right; at end-of-text, the char left. */
        let ws = text[off..]
            .chars()
            .next()
            .or_else(|| text[..off].chars().next_back())
            .is_some_and(char::is_whitespace);

        let mut start = off;
        for (i, c) in text[..off].char_indices().rev() {
            if c.is_whitespace() == ws {
                start = i;
            } else {
                break;
            }
        }
        let mut end = off;
        for (i, c) in text[off..].char_indices() {
            if c.is_whitespace() == ws {
                end = off + i + c.len_utf8();
            } else {
                break;
            }
        }
        (start as u32, end as u32)
    }

    /// Return a copy with bytes `[s, e)` removed. Style spans are clipped and
    /// shifted across the deletion.
    pub fn delete_text(&self, s: u32, e: u32) -> Paragraph {
        let len = self.text.len() as u32;
        let s = s.min(len);
        let e = e.min(len);
        if s >= e {
            return self.clone();
        }
        let mut text = self.text.clone();
        text.replace_range(s as usize..e as usize, "");
        let gap = e - s;
        /* Map a pre-delete offset to its post-delete position. */
        let map = |p: u32| -> u32 {
            if p <= s {
                p
            } else if p >= e {
                p - gap
            } else {
                s
            }
        };
        let mut spans = Vec::new();
        for run in &self.spans {
            let (ns, ne) = (map(run.start), map(run.end));
            if ns < ne {
                spans.push(StyleRun {
                    start: ns,
                    end: ne,
                    style: run.style,
                });
            }
        }
        Paragraph {
            text,
            spans,
            props: self.props.clone(),
            list_item: self.list_item,
            resolved_marker: self.resolved_marker.clone(),
            dirty: true,
            source_xml: None,
        }
    }

    /// Split into `[0, at)` and `[at, len)`. Spans straddling `at` are split.
    pub fn split_at(&self, at: u32) -> (Paragraph, Paragraph) {
        let len = self.text.len() as u32;
        let at = at.min(len);
        let mut left = Vec::new();
        let mut right = Vec::new();
        for run in &self.spans {
            if run.start < at {
                left.push(StyleRun {
                    start: run.start,
                    end: run.end.min(at),
                    style: run.style,
                });
            }
            if run.end > at {
                right.push(StyleRun {
                    start: run.start.max(at) - at,
                    end: run.end - at,
                    style: run.style,
                });
            }
        }
        (
            Paragraph {
                text: self.text[..at as usize].to_owned(),
                spans: left,
                props: self.props.clone(),
                list_item: self.list_item,
                resolved_marker: self.resolved_marker.clone(),
                dirty: true,
                source_xml: None,
            },
            Paragraph {
                text: self.text[at as usize..].to_owned(),
                spans: right,
                props: self.props.clone(),
                list_item: self.list_item,
                resolved_marker: self.resolved_marker.clone(),
                dirty: true,
                source_xml: None,
            },
        )
    }

    /// Append `other` to a copy of `self`, shifting `other`'s spans right.
    /// The merged paragraph keeps `self`'s alignment — the surviving
    /// paragraph mark wins when a paragraph break is deleted.
    pub fn concat(&self, other: &Paragraph) -> Paragraph {
        let shift = self.text.len() as u32;
        let mut text = self.text.clone();
        text.push_str(&other.text);
        let mut spans = self.spans.clone();
        for run in &other.spans {
            spans.push(StyleRun {
                start: run.start + shift,
                end: run.end + shift,
                style: run.style,
            });
        }
        Paragraph {
            text,
            spans,
            props: self.props.clone(),
            list_item: self.list_item,
            resolved_marker: self.resolved_marker.clone(),
            dirty: true,
            source_xml: None,
        }
    }

    /// Byte offset of the char boundary immediately before `o` (clamped to 0).
    pub fn prev_offset(&self, o: u32) -> u32 {
        let o = (o as usize).min(self.text.len());
        self.text[..o]
            .char_indices()
            .next_back()
            .map_or(0, |(i, _)| i as u32)
    }

    /// Byte offset of the char boundary immediately after `o` (clamped to len).
    pub fn next_offset(&self, o: u32) -> u32 {
        let o = (o as usize).min(self.text.len());
        self.text[o..]
            .chars()
            .next()
            .map_or(o as u32, |c| (o + c.len_utf8()) as u32)
    }
}

/* ===================================================================
Phase 5 PR 1 — Table model
==================================================================== */

/// Border line style (`<w:val>` on `<w:left>` / `<w:top>` / …).
/// Phase 5 PR 1 ships the common subset; `Other` preserves the
/// original token for round-trip.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum BorderStyle {
    #[default]
    Single,
    Double,
    Dotted,
    Dashed,
    None,
    Other(String),
}

/// One border edge stroke. `size_eighth_pt` is `<w:sz>` (eighths of a
/// point — the OOXML unit); divide by 8 to get points, by 6 to get px
/// at 96 DPI.
#[derive(Debug, Clone, Default)]
pub struct BorderStroke {
    pub style: BorderStyle,
    pub size_eighth_pt: u16,
    pub color: Option<[u8; 4]>,
}

/// Per-edge border strokes for a `<w:tcBorders>` or `<w:tblBorders>`.
/// `inside_h` / `inside_v` only apply when carried at the table level
/// (`<w:tblBorders>`); cell-level borders ignore them.
#[derive(Debug, Clone, Default)]
pub struct CellBorders {
    pub top: Option<BorderStroke>,
    pub left: Option<BorderStroke>,
    pub bottom: Option<BorderStroke>,
    pub right: Option<BorderStroke>,
    pub inside_h: Option<BorderStroke>,
    pub inside_v: Option<BorderStroke>,
}

/// `<w:tblCellMar>` default cell padding.
#[derive(Debug, Clone, Copy, Default)]
pub struct CellMargins {
    pub top_twips: i32,
    pub left_twips: i32,
    pub bottom_twips: i32,
    pub right_twips: i32,
}

/// `<w:tcW>` / `<w:tblW>` width — twips, percent (50-thousandths per
/// OOXML), auto (content-driven), or nil (no width).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellWidth {
    Dxa(i32),
    Pct(u16),
    Auto,
    Nil,
}

/// `<w:vMerge>` — vertical merge role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VMergeRole {
    /// Independent cell.
    #[default]
    None,
    /// Top of a vertical span; renders its content, spans down through
    /// every `Continue` cell directly below.
    Restart,
    /// Placeholder; content is ignored at render time (the `Restart`
    /// cell visually owns the merged block).
    Continue,
}

/// `<w:vAlign>` — vertical alignment of a cell's blocks within the
/// cell bounding box.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VerticalAlign {
    #[default]
    Top,
    Center,
    Bottom,
}

/// `<w:trHeight>` row height. `hRule` decides whether the value is a
/// minimum, exact, or auto-fit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowHeight {
    Auto,
    AtLeast { twips: i32 },
    Exact { twips: i32 },
}

#[derive(Debug, Clone, Default)]
pub struct RowProperties {
    pub height: Option<RowHeight>,
    /// `<w:cantSplit/>` — row cannot break across pages. Phase 5a
    /// treats this as implicit-on for every row (no mid-row pagination
    /// yet). Carried verbatim for round-trip.
    pub cant_split: bool,
    /// `<w:tblHeader/>` — row repeats at the top of every page after
    /// a break. Phase 5a captures but does not honour.
    pub header: bool,
}

#[derive(Debug, Clone, Default)]
pub struct CellProperties {
    pub grid_span: u8,
    pub v_merge: VMergeRole,
    pub width: Option<CellWidth>,
    pub borders: Option<CellBorders>,
    pub shading: Option<[u8; 4]>,
    pub v_align: VerticalAlign,
}

#[derive(Debug, Clone, Default)]
pub struct TableProperties {
    pub width: Option<CellWidth>,
    pub alignment: Option<Alignment>,
    pub indent_twips: i32,
    pub borders: Option<CellBorders>,
    pub cell_margins: CellMargins,
    pub table_style_id: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct TableCell {
    pub props: CellProperties,
    /// Nested block sequence. `Vec`, not `im::Vector`: cells average
    /// 1-2 paragraphs, so persistent-vector overhead is not worth the
    /// structural-sharing win at that size (RFC §1.4).
    pub blocks: Vec<Block>,
}

#[derive(Debug, Clone, Default)]
pub struct TableRow {
    pub props: RowProperties,
    pub cells: Vec<TableCell>,
}

#[derive(Debug, Clone, Default)]
pub struct Table {
    /// `<w:tblGrid>` — column template widths in twips. Length is the
    /// logical column count; cells with `grid_span > 1` consume
    /// multiple template columns.
    pub grid: Vec<i32>,
    pub props: TableProperties,
    pub rows: Vec<TableRow>,
    /// Phase 3 passthrough mirror: `false` on load, `true` after any
    /// mutation. Writer emits `source_xml` verbatim when clean.
    pub dirty: bool,
    /// Raw `<w:tbl>...</w:tbl>` source bytes captured by the reader.
    /// `None` for engine-synthesised tables.
    pub source_xml: Option<Vec<u8>>,
}

/* ===================================================================
LogicalPos — paragraph-flat addressing (Phase 5 PR 1 shim).
Phase 5 PR 3 will widen this to `BlockPath`; for now `para` is the
*paragraph index skipping tables*, matching every Phase 4 caller.
==================================================================== */

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LogicalPos {
    pub para: u32,
    /// Byte offset within the paragraph (UTF-8).
    pub offset: u32,
}

impl DocumentTree {
    pub fn new() -> Self {
        Self {
            blocks: Vector::new(),
        }
    }

    /// Build a single-paragraph document from a plain string.
    pub fn from_text(text: &str) -> Self {
        let mut blocks = Vector::new();
        blocks.push_back(Block::Paragraph(Paragraph {
            text: text.to_owned(),
            spans: Vec::new(),
            props: ParaProperties::default(),
            list_item: None,
            resolved_marker: None,
            dirty: false,
            source_xml: None,
        }));
        Self { blocks }
    }

    /// Build a document from a list of paragraph plain-text bodies.
    pub fn from_paragraphs<I: IntoIterator<Item = String>>(texts: I) -> Self {
        let mut blocks = Vector::new();
        for t in texts {
            blocks.push_back(Block::Paragraph(Paragraph {
                text: t,
                spans: Vec::new(),
                props: ParaProperties::default(),
                list_item: None,
                resolved_marker: None,
                dirty: false,
                source_xml: None,
            }));
        }
        Self { blocks }
    }

    /// Build a document from pre-styled paragraphs — the `.docx` reader (run
    /// properties → spans) and the HTML paste path both produce these.
    pub fn from_rich_paragraphs<I: IntoIterator<Item = Paragraph>>(paras: I) -> Self {
        let mut blocks = Vector::new();
        for p in paras {
            blocks.push_back(Block::Paragraph(p));
        }
        Self { blocks }
    }

    /// Build a document from a pre-mixed block sequence — the `.docx` reader
    /// (with tables) produces these. Phase 5 PR 1 entry point.
    pub fn from_blocks<I: IntoIterator<Item = Block>>(blocks_in: I) -> Self {
        let mut blocks = Vector::new();
        for b in blocks_in {
            blocks.push_back(b);
        }
        Self { blocks }
    }

    /* ============================================================
    Phase 5 PR 1 — paragraph-flat shim
    Treats `Block::Table` as inert. `LogicalPos.para` is an index
    into the paragraph-flat view (skipping tables). PR 3 will widen
    to `BlockPath`.
    ============================================================ */

    /// Number of `Block::Paragraph`s in the doc, skipping tables.
    pub fn paragraph_count(&self) -> u32 {
        self.blocks
            .iter()
            .filter(|b| matches!(b, Block::Paragraph(_)))
            .count() as u32
    }

    /// Total block count (paragraphs + tables).
    pub fn block_count(&self) -> u32 {
        self.blocks.len() as u32
    }

    /// The Nth `Block::Paragraph`, skipping tables. Phase 5 PR 1 shim
    /// that keeps Phase 1-4 callers working unchanged. Phase 5 PR 3
    /// widens callers to `BlockPath`.
    pub fn nth_paragraph(&self, n: u32) -> Option<&Paragraph> {
        self.blocks
            .iter()
            .filter_map(Block::as_paragraph)
            .nth(n as usize)
    }

    pub fn paragraph_text(&self, idx: u32) -> Option<&str> {
        self.nth_paragraph(idx).map(|p| p.text.as_str())
    }

    pub fn end_of_document(&self) -> LogicalPos {
        let count = self.paragraph_count();
        if count == 0 {
            return LogicalPos { para: 0, offset: 0 };
        }
        let last_para = count - 1;
        let offset = self
            .nth_paragraph(last_para)
            .map(|p| p.text.len() as u32)
            .unwrap_or(0);
        LogicalPos {
            para: last_para,
            offset,
        }
    }

    /// Mutate the Nth paragraph in place. Returns the new `Vector<Block>`
    /// or `None` when the index is out of range. Walks blocks once to
    /// find the target, then clones the paragraph and runs `f`.
    fn map_paragraph<F>(blocks: &mut Vector<Block>, n: u32, f: F) -> Option<()>
    where
        F: FnOnce(&mut Paragraph),
    {
        let mut seen = 0u32;
        for i in 0..blocks.len() {
            if matches!(blocks[i], Block::Paragraph(_)) {
                if seen == n {
                    let mut b = blocks[i].clone();
                    if let Block::Paragraph(ref mut p) = b {
                        f(p);
                    }
                    blocks.set(i, b);
                    return Some(());
                }
                seen += 1;
            }
        }
        None
    }

    /// Replace the Nth paragraph with `replacement` (one paragraph).
    fn replace_paragraph(blocks: &mut Vector<Block>, n: u32, replacement: Paragraph) -> Option<()> {
        let bi = Self::find_paragraph_block_idx(blocks, n)?;
        blocks.set(bi, Block::Paragraph(replacement));
        Some(())
    }

    /// Insert `paragraph` immediately *after* the Nth paragraph (or at
    /// the document end when `n + 1 == paragraph_count`).
    fn insert_paragraph_after(
        blocks: &mut Vector<Block>,
        n: u32,
        paragraph: Paragraph,
    ) -> Option<()> {
        let bi = Self::find_paragraph_block_idx(blocks, n)?;
        blocks.insert(bi + 1, Block::Paragraph(paragraph));
        Some(())
    }

    fn find_paragraph_block_idx(blocks: &Vector<Block>, n: u32) -> Option<usize> {
        let mut seen = 0u32;
        for i in 0..blocks.len() {
            if matches!(blocks[i], Block::Paragraph(_)) {
                if seen == n {
                    return Some(i);
                }
                seen += 1;
            }
        }
        None
    }

    /// Insert `text` at `at`. Out-of-range positions are clamped to end of
    /// document. Returns the new tree (the old one is structurally shared via
    /// `im::Vector`).
    pub fn insert_text(&self, at: LogicalPos, text: &str) -> Self {
        if text.is_empty() {
            return self.clone();
        }
        let mut blocks = self.blocks.clone();
        let count = self.paragraph_count();
        if count == 0 {
            blocks.push_back(Block::Paragraph(Paragraph {
                text: text.to_owned(),
                spans: Vec::new(),
                props: ParaProperties::default(),
                list_item: None,
                resolved_marker: None,
                dirty: true,
                source_xml: None,
            }));
            return Self { blocks };
        }
        let para_idx = at.para.min(count - 1);
        Self::map_paragraph(&mut blocks, para_idx, |para| {
            let offset = (at.offset as usize).min(para.text.len());
            para.text.insert_str(offset, text);
            /* Shift styled spans across the insertion point — a span
            containing the point grows, spans wholly after it slide right. */
            let off = offset as u32;
            let len = text.len() as u32;
            for s in &mut para.spans {
                if s.start >= off {
                    s.start += len;
                }
                if s.end > off {
                    s.end += len;
                }
            }
            para.dirty = true;
            para.source_xml = None;
        });
        Self { blocks }
    }

    /// Apply a style `patch` over the logical range `[start, end)`. Splits and
    /// merges spans on every covered paragraph; unaffected paragraphs are
    /// structurally shared.
    pub fn apply_style(&self, start: LogicalPos, end: LogicalPos, patch: SpanStyle) -> Self {
        let count = self.paragraph_count();
        if count == 0 {
            return self.clone();
        }
        let mut blocks = self.blocks.clone();
        let last_idx = count - 1;
        let first = start.para.min(last_idx);
        let last = end.para.min(last_idx);
        for p in first..=last {
            let lo = if p == first { start.offset } else { 0 };
            let para_text_len = self
                .nth_paragraph(p)
                .map(|x| x.text.len() as u32)
                .unwrap_or(0);
            let hi = if p == last { end.offset } else { para_text_len };
            let styled = self
                .nth_paragraph(p)
                .map(|src| src.apply_style(lo, hi, patch));
            if let Some(styled) = styled {
                Self::replace_paragraph(&mut blocks, p, styled);
            }
        }
        Self { blocks }
    }

    /// Set `align` on every paragraph the logical range `[start, end)` spans
    /// (Backlog #9). Paragraphs outside the range are structurally shared.
    /// `start`/`end` are expected in document order.
    pub fn set_alignment(&self, start: LogicalPos, end: LogicalPos, align: Alignment) -> Self {
        let count = self.paragraph_count();
        if count == 0 {
            return self.clone();
        }
        let mut blocks = self.blocks.clone();
        let last = count - 1;
        let first = start.para.min(last);
        let final_para = end.para.min(last);
        for p in first..=final_para {
            Self::map_paragraph(&mut blocks, p, |para| {
                para.props.alignment = Some(align);
                para.dirty = true;
                para.source_xml = None;
            });
        }
        Self { blocks }
    }

    /// Delete the logical range `[start, end)`. A range spanning paragraphs
    /// merges the partial first and last paragraphs and drops those between.
    pub fn delete_range(&self, start: LogicalPos, end: LogicalPos) -> Self {
        let (start, end) = if (start.para, start.offset) <= (end.para, end.offset) {
            (start, end)
        } else {
            (end, start)
        };
        let count = self.paragraph_count();
        if count == 0 {
            return self.clone();
        }
        let mut blocks = self.blocks.clone();
        let last = count - 1;
        let sp = start.para.min(last);
        let ep = end.para.min(last);
        if sp == ep {
            Self::map_paragraph(&mut blocks, sp, |para| {
                *para = para.delete_text(start.offset, end.offset);
            });
        } else {
            let head = self
                .nth_paragraph(sp)
                .map(|p| p.split_at(start.offset).0)
                .unwrap_or_default();
            let tail = self
                .nth_paragraph(ep)
                .map(|p| p.split_at(end.offset).1)
                .unwrap_or_default();
            let merged = head.concat(&tail);
            /* Drop every block strictly between sp's paragraph block and
            ep's paragraph block (inclusive of ep, exclusive of sp), then
            replace sp with the merged paragraph. Intervening tables are
            dropped too — matches Word's "delete across paragraphs eats
            everything between" semantics. */
            let sp_block =
                Self::find_paragraph_block_idx(&blocks, sp).expect("sp exists in `count > 0` arm");
            let ep_block =
                Self::find_paragraph_block_idx(&blocks, ep).expect("ep exists in `count > 0` arm");
            for _ in 0..(ep_block - sp_block) {
                blocks.remove(sp_block + 1);
            }
            Self::replace_paragraph(&mut blocks, sp, merged);
        }
        Self { blocks }
    }

    /// Split the paragraph at `at`, the break falling between the two halves.
    pub fn split_paragraph(&self, at: LogicalPos) -> Self {
        let count = self.paragraph_count();
        let mut blocks = self.blocks.clone();
        if count == 0 {
            blocks.push_back(Block::Paragraph(Paragraph::default()));
            blocks.push_back(Block::Paragraph(Paragraph::default()));
            return Self { blocks };
        }
        let idx = at.para.min(count - 1);
        let (left, right) = self
            .nth_paragraph(idx)
            .map(|p| p.split_at(at.offset))
            .unwrap_or_default();
        Self::replace_paragraph(&mut blocks, idx, left);
        Self::insert_paragraph_after(&mut blocks, idx, right);
        Self { blocks }
    }

    /// Insert `text` at `at`, splitting it into separate paragraphs on every
    /// newline — `\r\n` and bare `\r` are normalized to `\n` first. A `text`
    /// with no newline behaves exactly like [`DocumentTree::insert_text`].
    /// Returns the new tree and the caret position at the end of the last
    /// inserted line (Backlog #12, multi-line paste).
    pub fn insert_multiline(&self, at: LogicalPos, text: &str) -> (Self, LogicalPos) {
        let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
        let lines: Vec<&str> = normalized.split('\n').collect();
        let mut doc = self.clone();
        let mut cur = at;
        for (i, line) in lines.iter().enumerate() {
            doc = doc.insert_text(cur, line);
            let after = LogicalPos {
                para: cur.para,
                offset: cur.offset + line.len() as u32,
            };
            if i + 1 < lines.len() {
                /* A newline follows this line — break the paragraph so the
                next line lands in a fresh one; the remainder of the original
                paragraph rides along on the tail. */
                doc = doc.split_paragraph(after);
                cur = LogicalPos {
                    para: cur.para + 1,
                    offset: 0,
                };
            } else {
                cur = after;
            }
        }
        (doc, cur)
    }

    /// Extract the logical range `[start, end)` as standalone paragraphs,
    /// style spans clipped and shifted to local offsets. Drives rich
    /// clipboard copy — HTML + `.docx`-fragment generation (Backlog #12).
    /// **Tables in the spanned range are silently dropped at Phase 5 PR 1**
    /// — clipboard fragments stay paragraph-only until PR 3 widens the
    /// engine API to `Block`.
    pub fn slice(&self, start: LogicalPos, end: LogicalPos) -> Vec<Paragraph> {
        let (start, end) = if (start.para, start.offset) <= (end.para, end.offset) {
            (start, end)
        } else {
            (end, start)
        };
        let count = self.paragraph_count();
        if count == 0 {
            return Vec::new();
        }
        let last = count - 1;
        let sp = start.para.min(last);
        let ep = end.para.min(last);
        if sp == ep {
            let Some(p) = self.nth_paragraph(sp) else {
                return Vec::new();
            };
            let head = p.split_at(end.offset).0;
            return vec![head.split_at(start.offset).1];
        }
        let mut out = Vec::with_capacity((ep - sp + 1) as usize);
        if let Some(p) = self.nth_paragraph(sp) {
            out.push(p.split_at(start.offset).1);
        }
        for p in (sp + 1)..ep {
            if let Some(para) = self.nth_paragraph(p) {
                out.push(para.clone());
            }
        }
        if let Some(p) = self.nth_paragraph(ep) {
            out.push(p.split_at(end.offset).0);
        }
        out
    }

    /// Insert pre-styled `paras` at `at`; returns the new tree and the caret
    /// at the end of the inserted content. The caller deletes any active
    /// selection first. Drives HTML paste (Backlog #12).
    pub fn insert_rich(&self, at: LogicalPos, paras: &[Paragraph]) -> (Self, LogicalPos) {
        if paras.is_empty() {
            return (self.clone(), at);
        }
        let mut blocks = self.blocks.clone();
        let count = self.paragraph_count();
        if count == 0 {
            blocks.push_back(Block::Paragraph(Paragraph::default()));
        }
        let effective_count = count.max(1);
        let idx = at.para.min(effective_count - 1);
        let target = Self::find_paragraph_block_idx(&blocks, idx).expect("effective_count >= 1");
        let (head, tail) = match &blocks[target] {
            Block::Paragraph(p) => p.split_at(at.offset),
            _ => unreachable!("nth_paragraph index points at a paragraph block"),
        };
        if paras.len() == 1 {
            let caret = LogicalPos {
                para: idx,
                offset: (head.text.len() + paras[0].text.len()) as u32,
            };
            blocks.set(
                target,
                Block::Paragraph(head.concat(&paras[0]).concat(&tail)),
            );
            return (Self { blocks }, caret);
        }
        let lastp = &paras[paras.len() - 1];
        let caret = LogicalPos {
            para: idx + paras.len() as u32 - 1,
            offset: lastp.text.len() as u32,
        };
        blocks.set(target, Block::Paragraph(head.concat(&paras[0])));
        for (k, p) in paras[1..paras.len() - 1].iter().enumerate() {
            blocks.insert(target + 1 + k, Block::Paragraph(p.clone()));
        }
        blocks.insert(
            target + paras.len() - 1,
            Block::Paragraph(lastp.concat(&tail)),
        );
        (Self { blocks }, caret)
    }

    /// Extract the text of the logical range `[start, end)`. Paragraphs the
    /// range spans are joined by `\n`. Used for clipboard copy.
    pub fn text_range(&self, start: LogicalPos, end: LogicalPos) -> String {
        let (start, end) = if (start.para, start.offset) <= (end.para, end.offset) {
            (start, end)
        } else {
            (end, start)
        };
        let count = self.paragraph_count();
        if count == 0 {
            return String::new();
        }
        let last = count - 1;
        let sp = start.para.min(last);
        let ep = end.para.min(last);
        let mut out = String::new();
        for p in sp..=ep {
            let Some(para) = self.nth_paragraph(p) else {
                continue;
            };
            let len = para.text.len();
            let lo = if p == sp {
                (start.offset as usize).min(len)
            } else {
                0
            };
            let hi = if p == ep {
                (end.offset as usize).min(len)
            } else {
                len
            };
            if p > sp {
                out.push('\n');
            }
            if lo < hi {
                out.push_str(&para.text[lo..hi]);
            }
        }
        out
    }

    /* ============================================================
    Phase 5 PR 3 — table mutation commands.
    Every command flips `Table.dirty = true` + drops
    `source_xml`, so the writer regenerates the table from rows
    instead of emitting the captured passthrough bytes.
    ============================================================ */

    /// Insert an empty `rows × cols` table at the *block position* given
    /// by `at` (top-level path only — nested-cell insertion in 5b). The
    /// new table sits at top-level block index `at.steps[0]`; the
    /// existing block at that index slides down by one.
    pub fn insert_table(&self, at: BlockPath, rows: u32, cols: u32) -> Self {
        let idx = top_level_block_index(&at).unwrap_or(self.blocks.len() as u32);
        let cols = cols.max(1) as usize;
        /* Default column width — Word's "auto" cells map to 2880 twips
        (2 inches) when no grid is specified. */
        let grid: Vec<i32> = vec![2880; cols];
        let mut row_vec: Vec<TableRow> = Vec::with_capacity(rows.max(1) as usize);
        for _ in 0..rows.max(1) {
            let mut cells = Vec::with_capacity(cols);
            for _ in 0..cols {
                cells.push(TableCell::default());
            }
            row_vec.push(TableRow {
                props: RowProperties::default(),
                cells,
            });
        }
        let table = Table {
            grid,
            props: TableProperties::default(),
            rows: row_vec,
            /* Engine-synthesised — no source bytes, fully regenerated on
            save. */
            dirty: true,
            source_xml: None,
        };
        let mut blocks = self.blocks.clone();
        let insert_at = (idx as usize).min(blocks.len());
        blocks.insert(insert_at, Block::Table(table));
        Self { blocks }
    }

    /// Delete the table at `at.steps[0]` (top-level only at PR 3).
    pub fn delete_table(&self, at: BlockPath) -> Self {
        let idx = match top_level_block_index(&at) {
            Some(i) => i as usize,
            None => return self.clone(),
        };
        let mut blocks = self.blocks.clone();
        if idx < blocks.len() && matches!(blocks[idx], Block::Table(_)) {
            blocks.remove(idx);
        }
        Self { blocks }
    }

    /// Insert a fresh row at `after_row` (or at the end when
    /// `after_row >= row_count`). Cell count matches the existing
    /// rows' cell count.
    pub fn insert_row(&self, table_path: BlockPath, after_row: u32) -> Self {
        self.mutate_table(table_path, |t| {
            let cols = t
                .rows
                .first()
                .map(|r| r.cells.len())
                .unwrap_or_else(|| t.grid.len().max(1));
            let new_row = TableRow {
                props: RowProperties::default(),
                cells: (0..cols).map(|_| TableCell::default()).collect(),
            };
            let insert_at = (after_row as usize + 1).min(t.rows.len());
            t.rows.insert(insert_at, new_row);
        })
    }

    pub fn delete_row(&self, table_path: BlockPath, row: u32) -> Self {
        self.mutate_table(table_path, |t| {
            let i = row as usize;
            if i < t.rows.len() {
                t.rows.remove(i);
            }
        })
    }

    pub fn insert_column(&self, table_path: BlockPath, after_col: u32) -> Self {
        self.mutate_table(table_path, |t| {
            let insert_at = (after_col as usize + 1).min(t.grid.len());
            t.grid.insert(insert_at, 2880);
            for row in &mut t.rows {
                let cell_at = insert_at.min(row.cells.len());
                row.cells.insert(cell_at, TableCell::default());
            }
        })
    }

    pub fn delete_column(&self, table_path: BlockPath, col: u32) -> Self {
        self.mutate_table(table_path, |t| {
            let c = col as usize;
            if c < t.grid.len() {
                t.grid.remove(c);
            }
            for row in &mut t.rows {
                if c < row.cells.len() {
                    row.cells.remove(c);
                }
            }
        })
    }

    /// Merge the rectangle of cells `(from_row, from_col)..=(to_row,
    /// to_col)` inside the table at `table_path`. The top-left cell
    /// becomes the visual owner: its `grid_span` widens to cover the
    /// column range, every cell directly below in the column range
    /// becomes `VMergeRole::Continue`. Horizontal partners (same row,
    /// columns to the right) are *removed* and their widths summed
    /// into the owner's `grid_span`. PR 3 minimum implementation —
    /// PR 3b extends for non-rectangular merges.
    pub fn merge_cells(
        &self,
        table_path: BlockPath,
        from_row: u32,
        from_col: u32,
        to_row: u32,
        to_col: u32,
    ) -> Self {
        let (r0, r1) = if from_row <= to_row {
            (from_row, to_row)
        } else {
            (to_row, from_row)
        };
        let (c0, c1) = if from_col <= to_col {
            (from_col, to_col)
        } else {
            (to_col, from_col)
        };
        self.mutate_table(table_path, |t| {
            let rcount = t.rows.len() as u32;
            if r0 >= rcount {
                return;
            }
            let span = (c1 - c0 + 1) as u8;
            /* Horizontal collapse: top-row cells in the rectangle's
            column range merge into one cell with `grid_span = span`. */
            if let Some(top_row) = t.rows.get_mut(r0 as usize) {
                let drop_count = (c1.min(top_row.cells.len() as u32 - 1) - c0) as usize;
                if (c0 as usize) < top_row.cells.len() {
                    top_row.cells[c0 as usize].props.grid_span = span;
                    top_row.cells[c0 as usize].props.v_merge = if r0 == r1 {
                        VMergeRole::None
                    } else {
                        VMergeRole::Restart
                    };
                }
                for _ in 0..drop_count {
                    if (c0 as usize + 1) < top_row.cells.len() {
                        top_row.cells.remove(c0 as usize + 1);
                    }
                }
            }
            /* Vertical: every cell in the column range on rows r0+1..=r1
            becomes `Continue`. We don't shift cells; the rendering layer
            already skips `Continue` cells. */
            for r in (r0 + 1)..=r1.min(rcount - 1) {
                let row = &mut t.rows[r as usize];
                for c in c0..=c1.min(row.cells.len() as u32 - 1) {
                    if let Some(cell) = row.cells.get_mut(c as usize) {
                        cell.props.v_merge = VMergeRole::Continue;
                        cell.props.grid_span = span.max(1);
                    }
                }
            }
        })
    }

    pub fn split_cell(&self, table_path: BlockPath, row: u32, col: u32) -> Self {
        self.mutate_table(table_path, |t| {
            if let Some(r) = t.rows.get_mut(row as usize)
                && let Some(cell) = r.cells.get_mut(col as usize)
            {
                cell.props.grid_span = 1;
                cell.props.v_merge = VMergeRole::None;
            }
        })
    }

    pub fn set_cell_shading(
        &self,
        table_path: BlockPath,
        row: u32,
        col: u32,
        color: Option<[u8; 4]>,
    ) -> Self {
        self.mutate_table(table_path, |t| {
            if let Some(r) = t.rows.get_mut(row as usize)
                && let Some(cell) = r.cells.get_mut(col as usize)
            {
                cell.props.shading = color;
            }
        })
    }

    pub fn set_cell_borders(
        &self,
        table_path: BlockPath,
        row: u32,
        col: u32,
        borders: CellBorders,
    ) -> Self {
        self.mutate_table(table_path, |t| {
            if let Some(r) = t.rows.get_mut(row as usize)
                && let Some(cell) = r.cells.get_mut(col as usize)
            {
                cell.props.borders = Some(borders.clone());
            }
        })
    }

    /// Helper — open `table_path`'s `Block::Table`, run `f`, flip dirty,
    /// drop source bytes, write back.
    fn mutate_table<F>(&self, path: BlockPath, f: F) -> Self
    where
        F: FnOnce(&mut Table),
    {
        let idx = match top_level_block_index(&path) {
            Some(i) => i as usize,
            None => return self.clone(),
        };
        let mut blocks = self.blocks.clone();
        if idx >= blocks.len() {
            return self.clone();
        }
        let mut block = blocks[idx].clone();
        let Some(table) = block.as_table_mut() else {
            return self.clone();
        };
        f(table);
        /* Phase 5 PR 3 invariant: every table mutation drops the
        passthrough — the writer must regenerate from rows. */
        table.dirty = true;
        table.source_xml = None;
        blocks.set(idx, block);
        Self { blocks }
    }
}

/// Extract the top-level block index from a `BlockPath` (first step
/// must be `PathStep::Block(N)`; nested-cell paths return `None`
/// at PR 3 — full nested-table mutation is PR 3b).
fn top_level_block_index(path: &BlockPath) -> Option<u32> {
    match path.steps.first()? {
        PathStep::Block(n) => Some(*n),
        PathStep::Cell { .. } => None,
    }
}

/// Bounded undo/redo snapshot stack. Pushing a new snapshot truncates the
/// redo branch (standard editor semantics).
#[derive(Debug, Clone)]
pub struct UndoStack {
    /// Each element is a complete document snapshot. `im::Vector` clones in O(1)
    /// so pushing a snapshot is cheap structurally; only the modified
    /// `Paragraph.text` allocates.
    snapshots: Vec<DocumentTree>,
    /// Index of the current document in `snapshots`. Always `< snapshots.len()`.
    cursor: usize,
    /// Maximum snapshots retained (oldest are dropped on overflow).
    cap: usize,
}

impl UndoStack {
    pub fn new(initial: DocumentTree, cap: usize) -> Self {
        Self {
            snapshots: vec![initial],
            cursor: 0,
            cap,
        }
    }

    pub fn current(&self) -> &DocumentTree {
        &self.snapshots[self.cursor]
    }

    pub fn replace_current(&mut self, doc: DocumentTree) {
        self.snapshots[self.cursor] = doc;
    }

    pub fn push(&mut self, doc: DocumentTree) {
        /* Truncate any redo branch. */
        if self.cursor + 1 < self.snapshots.len() {
            self.snapshots.truncate(self.cursor + 1);
        }
        self.snapshots.push(doc);
        self.cursor = self.snapshots.len() - 1;
        /* Cap from the bottom. */
        while self.snapshots.len() > self.cap {
            self.snapshots.remove(0);
            self.cursor = self.cursor.saturating_sub(1);
        }
    }

    pub fn undo(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        self.cursor -= 1;
        true
    }

    pub fn redo(&mut self) -> bool {
        if self.cursor + 1 >= self.snapshots.len() {
            return false;
        }
        self.cursor += 1;
        true
    }

    pub fn can_undo(&self) -> bool {
        self.cursor > 0
    }

    pub fn can_redo(&self) -> bool {
        self.cursor + 1 < self.snapshots.len()
    }

    pub fn depth(&self) -> u32 {
        self.snapshots.len() as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_into_empty() {
        let d = DocumentTree::new();
        let d = d.insert_text(LogicalPos { para: 0, offset: 0 }, "hello");
        assert_eq!(d.paragraph_text(0), Some("hello"));
    }

    #[test]
    fn insert_mid_paragraph() {
        let d = DocumentTree::from_text("hello world");
        let d = d.insert_text(LogicalPos { para: 0, offset: 5 }, ",");
        assert_eq!(d.paragraph_text(0), Some("hello, world"));
    }

    #[test]
    fn apply_style_creates_span() {
        let doc = DocumentTree::from_text("hello world");
        let doc = doc.apply_style(
            LogicalPos { para: 0, offset: 0 },
            LogicalPos { para: 0, offset: 5 },
            SpanStyle {
                font_size: Some(20.0),
                color: None,
                ..Default::default()
            },
        );
        let spans = &doc.nth_paragraph(0).unwrap().spans;
        assert_eq!(spans.len(), 1);
        assert_eq!(
            spans[0],
            StyleRun {
                start: 0,
                end: 5,
                style: SpanStyle {
                    font_size: Some(20.0),
                    color: None,
                    ..Default::default()
                },
            }
        );
    }

    #[test]
    fn overlapping_styles_split_and_merge() {
        let doc = DocumentTree::from_text("hello world");
        let red = SpanStyle {
            font_size: None,
            color: Some([255, 0, 0, 255]),
            ..Default::default()
        };
        let big = SpanStyle {
            font_size: Some(30.0),
            color: None,
            ..Default::default()
        };
        let doc = doc.apply_style(
            LogicalPos { para: 0, offset: 0 },
            LogicalPos { para: 0, offset: 8 },
            red,
        );
        let doc = doc.apply_style(
            LogicalPos { para: 0, offset: 4 },
            LogicalPos {
                para: 0,
                offset: 11,
            },
            big,
        );
        let spans = &doc.nth_paragraph(0).unwrap().spans;
        /* [0,4) red ; [4,8) red+big ; [8,11) big */
        assert_eq!(spans.len(), 3);
        assert_eq!((spans[0].start, spans[0].end), (0, 4));
        assert_eq!(spans[0].style, red);
        assert_eq!((spans[1].start, spans[1].end), (4, 8));
        assert_eq!(
            spans[1].style,
            SpanStyle {
                font_size: Some(30.0),
                color: Some([255, 0, 0, 255]),
                ..Default::default()
            }
        );
        assert_eq!((spans[2].start, spans[2].end), (8, 11));
        assert_eq!(spans[2].style, big);
    }

    #[test]
    fn insert_shifts_spans() {
        let doc = DocumentTree::from_text("abcdef");
        let doc = doc.apply_style(
            LogicalPos { para: 0, offset: 2 },
            LogicalPos { para: 0, offset: 4 },
            SpanStyle {
                font_size: None,
                color: Some([1, 2, 3, 255]),
                ..Default::default()
            },
        );
        let doc = doc.insert_text(LogicalPos { para: 0, offset: 0 }, "XX");
        let span = doc.nth_paragraph(0).unwrap().spans[0];
        assert_eq!((span.start, span.end), (4, 6));
    }

    #[test]
    fn word_bounds_latin() {
        let p = Paragraph {
            text: "hello world".into(),
            spans: Vec::new(),
            props: ParaProperties::default(),
            list_item: None,
            resolved_marker: None,
            dirty: false,
            source_xml: None,
        };
        assert_eq!(p.word_bounds(2), (0, 5));
        assert_eq!(p.word_bounds(0), (0, 5));
        assert_eq!(p.word_bounds(5), (5, 6)); // on the space
        assert_eq!(p.word_bounds(8), (6, 11));
        assert_eq!(p.word_bounds(11), (6, 11)); // end of text → last word
    }

    #[test]
    fn word_bounds_arabic() {
        /* "مرحبا بالعالم" — 5-char word, space, 7-char word; 2 bytes/char. */
        let p = Paragraph {
            text: "مرحبا بالعالم".into(),
            spans: Vec::new(),
            props: ParaProperties::default(),
            list_item: None,
            resolved_marker: None,
            dirty: false,
            source_xml: None,
        };
        assert_eq!(p.word_bounds(4), (0, 10));
        assert_eq!(p.word_bounds(0), (0, 10));
        assert_eq!(p.word_bounds(12), (11, 25)); // mid-char offset clamps
    }

    #[test]
    fn word_bounds_empty() {
        let p = Paragraph {
            text: String::new(),
            spans: Vec::new(),
            props: ParaProperties::default(),
            list_item: None,
            resolved_marker: None,
            dirty: false,
            source_xml: None,
        };
        assert_eq!(p.word_bounds(0), (0, 0));
    }

    #[test]
    fn delete_within_paragraph() {
        let d = DocumentTree::from_text("hello world");
        let d = d.delete_range(
            LogicalPos { para: 0, offset: 5 },
            LogicalPos {
                para: 0,
                offset: 11,
            },
        );
        assert_eq!(d.paragraph_text(0), Some("hello"));
    }

    #[test]
    fn delete_merges_paragraphs() {
        let d = DocumentTree::from_paragraphs(["abc".to_string(), "def".to_string()]);
        let d = d.delete_range(
            LogicalPos { para: 0, offset: 3 },
            LogicalPos { para: 1, offset: 0 },
        );
        assert_eq!(d.paragraph_count(), 1);
        assert_eq!(d.paragraph_text(0), Some("abcdef"));
    }

    #[test]
    fn delete_clips_spans() {
        let doc = DocumentTree::from_text("hello world");
        let doc = doc.apply_style(
            LogicalPos { para: 0, offset: 0 },
            LogicalPos { para: 0, offset: 5 },
            SpanStyle {
                font_size: Some(20.0),
                color: None,
                ..Default::default()
            },
        );
        let doc = doc.delete_range(
            LogicalPos { para: 0, offset: 3 },
            LogicalPos { para: 0, offset: 5 },
        );
        assert_eq!(doc.paragraph_text(0), Some("hel world"));
        let spans = &doc.nth_paragraph(0).unwrap().spans;
        assert_eq!(spans.len(), 1);
        assert_eq!((spans[0].start, spans[0].end), (0, 3));
    }

    #[test]
    fn split_paragraph_in_two() {
        let d = DocumentTree::from_text("hello world");
        let d = d.split_paragraph(LogicalPos { para: 0, offset: 5 });
        assert_eq!(d.paragraph_count(), 2);
        assert_eq!(d.paragraph_text(0), Some("hello"));
        assert_eq!(d.paragraph_text(1), Some(" world"));
    }

    #[test]
    fn prev_next_offset_utf8() {
        /* "a"=1 byte, "م"=2 bytes, "b"=1 byte → char boundaries 0,1,3,4. */
        let p = Paragraph {
            text: "aمb".into(),
            spans: Vec::new(),
            props: ParaProperties::default(),
            list_item: None,
            resolved_marker: None,
            dirty: false,
            source_xml: None,
        };
        assert_eq!(p.next_offset(0), 1);
        assert_eq!(p.next_offset(1), 3);
        assert_eq!(p.prev_offset(4), 3);
        assert_eq!(p.prev_offset(3), 1);
    }

    #[test]
    fn text_range_within_and_across() {
        let d = DocumentTree::from_paragraphs(["hello world".to_string(), "second".to_string()]);
        assert_eq!(
            d.text_range(
                LogicalPos { para: 0, offset: 0 },
                LogicalPos { para: 0, offset: 5 },
            ),
            "hello"
        );
        assert_eq!(
            d.text_range(
                LogicalPos { para: 0, offset: 6 },
                LogicalPos { para: 1, offset: 6 },
            ),
            "world\nsecond"
        );
        /* reversed args normalize to document order */
        assert_eq!(
            d.text_range(
                LogicalPos { para: 0, offset: 5 },
                LogicalPos { para: 0, offset: 0 },
            ),
            "hello"
        );
    }

    #[test]
    fn apply_style_bold_italic_underline() {
        let doc = DocumentTree::from_text("hello world");
        /* Apply bold over [0,5). */
        let doc = doc.apply_style(
            LogicalPos { para: 0, offset: 0 },
            LogicalPos { para: 0, offset: 5 },
            SpanStyle {
                bold: Some(true),
                ..Default::default()
            },
        );
        assert_eq!(doc.nth_paragraph(0).unwrap().spans.len(), 1);
        assert_eq!(
            doc.nth_paragraph(0).unwrap().spans[0].style.bold,
            Some(true)
        );
        /* Overlay italic + underline on the same range — they merge in. */
        let doc = doc.apply_style(
            LogicalPos { para: 0, offset: 0 },
            LogicalPos { para: 0, offset: 5 },
            SpanStyle {
                italic: Some(true),
                underline: Some(true),
                ..Default::default()
            },
        );
        let style = doc.nth_paragraph(0).unwrap().style_at(2);
        assert_eq!(style.bold, Some(true));
        assert_eq!(style.italic, Some(true));
        assert_eq!(style.underline, Some(true));
        /* Outside the styled range — unstyled. */
        assert_eq!(
            doc.nth_paragraph(0).unwrap().style_at(8),
            SpanStyle::default()
        );
    }

    #[test]
    fn undo_redo_cycle() {
        let initial = DocumentTree::from_text("abc");
        let mut undo = UndoStack::new(initial.clone(), 16);

        let d2 = initial.insert_text(LogicalPos { para: 0, offset: 3 }, "def");
        undo.push(d2.clone());
        assert_eq!(undo.current().paragraph_text(0), Some("abcdef"));

        let d3 = d2.insert_text(LogicalPos { para: 0, offset: 6 }, "ghi");
        undo.push(d3.clone());
        assert_eq!(undo.current().paragraph_text(0), Some("abcdefghi"));

        undo.undo();
        assert_eq!(undo.current().paragraph_text(0), Some("abcdef"));
        undo.undo();
        assert_eq!(undo.current().paragraph_text(0), Some("abc"));
        assert!(!undo.can_undo());

        undo.redo();
        assert_eq!(undo.current().paragraph_text(0), Some("abcdef"));
    }

    #[test]
    fn set_alignment_marks_spanned_paragraphs() {
        let d = DocumentTree::from_paragraphs(["a".into(), "b".into(), "c".into()]);
        let d = d.set_alignment(
            LogicalPos { para: 0, offset: 0 },
            LogicalPos { para: 1, offset: 0 },
            Alignment::Center,
        );
        assert_eq!(
            d.nth_paragraph(0).unwrap().props.alignment,
            Some(Alignment::Center)
        );
        assert_eq!(
            d.nth_paragraph(1).unwrap().props.alignment,
            Some(Alignment::Center)
        );
        /* outside the range — untouched */
        assert_eq!(d.nth_paragraph(2).unwrap().props.alignment, None);
    }

    #[test]
    fn alignment_survives_text_edits() {
        let d = DocumentTree::from_text("hello world");
        let d = d.set_alignment(
            LogicalPos { para: 0, offset: 0 },
            LogicalPos { para: 0, offset: 0 },
            Alignment::End,
        );
        /* insertion clones the paragraph in place — alignment rides along */
        let d = d.insert_text(LogicalPos { para: 0, offset: 0 }, "X");
        assert_eq!(d.paragraph_text(0), Some("Xhello world"));
        assert_eq!(
            d.nth_paragraph(0).unwrap().props.alignment,
            Some(Alignment::End)
        );
        /* a style change preserves alignment */
        let d = d.apply_style(
            LogicalPos { para: 0, offset: 0 },
            LogicalPos { para: 0, offset: 3 },
            SpanStyle {
                bold: Some(true),
                ..Default::default()
            },
        );
        assert_eq!(
            d.nth_paragraph(0).unwrap().props.alignment,
            Some(Alignment::End)
        );
        /* and so does a deletion */
        let d = d.delete_range(
            LogicalPos { para: 0, offset: 0 },
            LogicalPos { para: 0, offset: 1 },
        );
        assert_eq!(
            d.nth_paragraph(0).unwrap().props.alignment,
            Some(Alignment::End)
        );
    }

    #[test]
    fn split_paragraph_inherits_alignment() {
        let d = DocumentTree::from_text("hello world");
        let d = d.set_alignment(
            LogicalPos { para: 0, offset: 0 },
            LogicalPos { para: 0, offset: 0 },
            Alignment::Center,
        );
        let d = d.split_paragraph(LogicalPos { para: 0, offset: 5 });
        assert_eq!(d.paragraph_count(), 2);
        /* both halves carry the original paragraph's alignment */
        assert_eq!(
            d.nth_paragraph(0).unwrap().props.alignment,
            Some(Alignment::Center)
        );
        assert_eq!(
            d.nth_paragraph(1).unwrap().props.alignment,
            Some(Alignment::Center)
        );
    }

    #[test]
    fn merge_keeps_first_paragraph_alignment() {
        let d = DocumentTree::from_paragraphs(["abc".into(), "def".into()]);
        let d = d.set_alignment(
            LogicalPos { para: 0, offset: 0 },
            LogicalPos { para: 0, offset: 0 },
            Alignment::Center,
        );
        let d = d.set_alignment(
            LogicalPos { para: 1, offset: 0 },
            LogicalPos { para: 1, offset: 0 },
            Alignment::End,
        );
        /* deleting the paragraph break merges the two */
        let d = d.delete_range(
            LogicalPos { para: 0, offset: 3 },
            LogicalPos { para: 1, offset: 0 },
        );
        assert_eq!(d.paragraph_count(), 1);
        assert_eq!(d.paragraph_text(0), Some("abcdef"));
        /* the surviving paragraph keeps the first paragraph's alignment */
        assert_eq!(
            d.nth_paragraph(0).unwrap().props.alignment,
            Some(Alignment::Center)
        );
    }

    #[test]
    fn insert_multiline_single_line_is_plain_insert() {
        let d = DocumentTree::from_text("abcd");
        let (d, caret) = d.insert_multiline(LogicalPos { para: 0, offset: 2 }, "XY");
        assert_eq!(d.paragraph_count(), 1);
        assert_eq!(d.paragraph_text(0), Some("abXYcd"));
        assert_eq!(caret, LogicalPos { para: 0, offset: 4 });
    }

    #[test]
    fn insert_multiline_splits_into_paragraphs() {
        let d = DocumentTree::from_text("abcd");
        let (d, caret) = d.insert_multiline(LogicalPos { para: 0, offset: 2 }, "L0\nL1\nL2");
        assert_eq!(d.paragraph_count(), 3);
        /* the original paragraph splits around the caret; the tail rides the
        last pasted line's paragraph */
        assert_eq!(d.paragraph_text(0), Some("abL0"));
        assert_eq!(d.paragraph_text(1), Some("L1"));
        assert_eq!(d.paragraph_text(2), Some("L2cd"));
        assert_eq!(caret, LogicalPos { para: 2, offset: 2 });
    }

    #[test]
    fn insert_multiline_normalizes_crlf_and_cr() {
        let d = DocumentTree::from_text("");
        let (d, _) = d.insert_multiline(LogicalPos { para: 0, offset: 0 }, "a\r\nb\rc");
        assert_eq!(d.paragraph_count(), 3);
        assert_eq!(d.paragraph_text(0), Some("a"));
        assert_eq!(d.paragraph_text(1), Some("b"));
        assert_eq!(d.paragraph_text(2), Some("c"));
    }

    #[test]
    fn insert_multiline_trailing_newline_makes_empty_paragraph() {
        let d = DocumentTree::from_text("xy");
        let (d, caret) = d.insert_multiline(LogicalPos { para: 0, offset: 2 }, "Z\n");
        assert_eq!(d.paragraph_count(), 2);
        assert_eq!(d.paragraph_text(0), Some("xyZ"));
        assert_eq!(d.paragraph_text(1), Some(""));
        assert_eq!(caret, LogicalPos { para: 1, offset: 0 });
    }

    #[test]
    fn insert_multiline_into_second_paragraph() {
        let d = DocumentTree::from_paragraphs(["first".to_string(), "second".to_string()]);
        let (d, caret) = d.insert_multiline(LogicalPos { para: 1, offset: 3 }, "A\nB");
        assert_eq!(d.paragraph_count(), 3);
        assert_eq!(d.paragraph_text(0), Some("first"));
        assert_eq!(d.paragraph_text(1), Some("secA"));
        assert_eq!(d.paragraph_text(2), Some("Bond"));
        assert_eq!(caret, LogicalPos { para: 2, offset: 1 });
    }

    #[test]
    fn slice_single_paragraph_clips_and_shifts_spans() {
        /* "hello world" with bold over "world" (bytes 6-11). */
        let bold = SpanStyle {
            bold: Some(true),
            ..Default::default()
        };
        let para = Paragraph {
            text: "hello world".into(),
            spans: vec![StyleRun {
                start: 6,
                end: 11,
                style: bold,
            }],
            props: ParaProperties::default(),
            list_item: None,
            resolved_marker: None,
            dirty: false,
            source_xml: None,
        };
        let doc = DocumentTree::from_rich_paragraphs([para]);
        /* Slice "lo wor" (bytes 3-9) — the bold span clips to 3-6, local. */
        let cut = doc.slice(
            LogicalPos { para: 0, offset: 3 },
            LogicalPos { para: 0, offset: 9 },
        );
        assert_eq!(cut.len(), 1);
        assert_eq!(cut[0].text, "lo wor");
        assert_eq!(
            cut[0].spans,
            vec![StyleRun {
                start: 3,
                end: 6,
                style: bold,
            }]
        );
    }

    #[test]
    fn insert_rich_single_paragraph_merges_inline() {
        let doc = DocumentTree::from_text("hello world");
        let frag = vec![Paragraph {
            text: "BRAVE ".into(),
            spans: vec![],
            props: ParaProperties::default(),
            list_item: None,
            resolved_marker: None,
            dirty: false,
            source_xml: None,
        }];
        let (out, caret) = doc.insert_rich(LogicalPos { para: 0, offset: 6 }, &frag);
        assert_eq!(out.paragraph_count(), 1);
        assert_eq!(out.paragraph_text(0), Some("hello BRAVE world"));
        assert_eq!(
            caret,
            LogicalPos {
                para: 0,
                offset: 12
            }
        );
    }

    #[test]
    fn insert_rich_multi_paragraph_splices_and_keeps_spans() {
        let doc = DocumentTree::from_text("ABCD");
        let bold = SpanStyle {
            bold: Some(true),
            ..Default::default()
        };
        let frag = vec![
            Paragraph {
                text: "one".into(),
                spans: vec![],
                props: ParaProperties::default(),
                list_item: None,
                resolved_marker: None,
                dirty: false,
                source_xml: None,
            },
            Paragraph {
                text: "two".into(),
                spans: vec![StyleRun {
                    start: 0,
                    end: 3,
                    style: bold,
                }],
                props: ParaProperties::default(),
                list_item: None,
                resolved_marker: None,
                dirty: false,
                source_xml: None,
            },
        ];
        let (out, caret) = doc.insert_rich(LogicalPos { para: 0, offset: 2 }, &frag);
        assert_eq!(out.paragraph_count(), 2);
        assert_eq!(out.paragraph_text(0), Some("ABone"));
        assert_eq!(out.paragraph_text(1), Some("twoCD"));
        assert_eq!(caret, LogicalPos { para: 1, offset: 3 });
        assert_eq!(out.nth_paragraph(1).unwrap().style_at(0).bold, Some(true));
        assert_eq!(out.nth_paragraph(1).unwrap().style_at(3).bold, None);
    }

    /* ---- Phase 5 PR 3: table command suite ------------------------- */

    #[test]
    fn insert_table_synthesises_dirty_block_with_no_source() {
        let d = DocumentTree::from_text("hello");
        let d = d.insert_table(BlockPath::top(1), 2, 3);
        assert_eq!(d.blocks.len(), 2);
        let t = d.blocks[1].as_table().expect("Block::Table");
        assert_eq!(t.rows.len(), 2);
        assert_eq!(t.rows[0].cells.len(), 3);
        assert_eq!(t.grid.len(), 3);
        assert!(t.dirty, "synthesised tables must regen on save");
        assert!(t.source_xml.is_none());
    }

    #[test]
    fn insert_row_appends_with_matching_column_count() {
        let d = DocumentTree::from_text("hi").insert_table(BlockPath::top(1), 1, 2);
        let d = d.insert_row(BlockPath::top(1), 0);
        let t = d.blocks[1].as_table().unwrap();
        assert_eq!(t.rows.len(), 2);
        assert_eq!(t.rows[1].cells.len(), 2);
    }

    #[test]
    fn insert_column_widens_grid_and_every_row() {
        let d = DocumentTree::from_text("hi").insert_table(BlockPath::top(1), 2, 2);
        let d = d.insert_column(BlockPath::top(1), 0);
        let t = d.blocks[1].as_table().unwrap();
        assert_eq!(t.grid.len(), 3);
        assert!(t.rows.iter().all(|r| r.cells.len() == 3));
    }

    #[test]
    fn delete_row_and_column() {
        let d = DocumentTree::from_text("hi").insert_table(BlockPath::top(1), 3, 3);
        let d = d.delete_row(BlockPath::top(1), 1);
        let d = d.delete_column(BlockPath::top(1), 0);
        let t = d.blocks[1].as_table().unwrap();
        assert_eq!(t.rows.len(), 2);
        assert_eq!(t.grid.len(), 2);
        assert!(t.rows.iter().all(|r| r.cells.len() == 2));
    }

    #[test]
    fn merge_cells_horizontal_sets_grid_span_and_drops_partners() {
        let d = DocumentTree::from_text("hi").insert_table(BlockPath::top(1), 1, 3);
        let d = d.merge_cells(BlockPath::top(1), 0, 0, 0, 2);
        let t = d.blocks[1].as_table().unwrap();
        assert_eq!(t.rows[0].cells.len(), 1);
        assert_eq!(t.rows[0].cells[0].props.grid_span, 3);
        assert_eq!(t.rows[0].cells[0].props.v_merge, VMergeRole::None);
    }

    #[test]
    fn merge_cells_vertical_flips_continue_rows() {
        let d = DocumentTree::from_text("hi").insert_table(BlockPath::top(1), 3, 2);
        let d = d.merge_cells(BlockPath::top(1), 0, 0, 2, 0);
        let t = d.blocks[1].as_table().unwrap();
        assert_eq!(t.rows[0].cells[0].props.v_merge, VMergeRole::Restart);
        assert_eq!(t.rows[1].cells[0].props.v_merge, VMergeRole::Continue);
        assert_eq!(t.rows[2].cells[0].props.v_merge, VMergeRole::Continue);
    }

    #[test]
    fn set_cell_shading_and_borders_flip_dirty() {
        let d = DocumentTree::from_text("hi").insert_table(BlockPath::top(1), 1, 1);
        /* The previously-inserted table starts dirty (synthesised); force
        a "clean" reset to exercise the dirty-flip invariant on a
        passthrough-eligible table. */
        let mut blocks = d.blocks.clone();
        if let Some(t) = blocks[1].as_table_mut() {
            t.dirty = false;
            t.source_xml = Some(b"<w:tbl/>".to_vec());
        }
        let d = DocumentTree { blocks };
        let d = d.set_cell_shading(BlockPath::top(1), 0, 0, Some([0xFF, 0, 0, 0xFF]));
        let t = d.blocks[1].as_table().unwrap();
        assert_eq!(t.rows[0].cells[0].props.shading, Some([0xFF, 0, 0, 0xFF]));
        assert!(t.dirty, "shading edit must flip dirty");
        assert!(t.source_xml.is_none(), "shading edit must drop source");

        let d = d.set_cell_borders(
            BlockPath::top(1),
            0,
            0,
            CellBorders {
                top: Some(BorderStroke {
                    style: BorderStyle::Single,
                    size_eighth_pt: 8,
                    color: Some([0, 0, 0xFF, 0xFF]),
                }),
                ..Default::default()
            },
        );
        let t = d.blocks[1].as_table().unwrap();
        assert!(t.rows[0].cells[0].props.borders.is_some());
    }

    #[test]
    fn delete_table_removes_top_level_block() {
        let d = DocumentTree::from_text("before").insert_table(BlockPath::top(1), 1, 1);
        assert_eq!(d.blocks.len(), 2);
        let d = d.delete_table(BlockPath::top(1));
        assert_eq!(d.blocks.len(), 1);
        assert!(d.blocks[0].as_paragraph().is_some());
    }
}
