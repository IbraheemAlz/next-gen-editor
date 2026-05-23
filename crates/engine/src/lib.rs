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
    /// Phase 6 — `<w:sectPr>`. One `Section` per OOXML section, in document
    /// order. Each owns a half-open `[start, end)` block range and the page
    /// geometry the paginator uses for that range. Empty `Vec` ⇒ the engine
    /// applies a single implicit A4 section over the whole document (the
    /// pre-Phase-6 behaviour).
    pub sections: Vec<Section>,
    /// Phase 6b — parsed header parts keyed by the OOXML relationship id
    /// (`r:id` from `<w:headerReference>`). Each value is the flat
    /// per-paragraph plain text the header reader extracted. The
    /// paginator looks each `Section`'s `header_ref` up here and renders
    /// the result in the top margin band.
    pub headers: std::collections::HashMap<String, Vec<String>>,
    /// Mirror of `headers` for `<w:footerReference>`.
    pub footers: std::collections::HashMap<String, Vec<String>>,
    /// Phase 7 — image blobs keyed by their relationship id (`r:id`). The
    /// archive reader fills this from `word/media/*` for every image rel
    /// the document references. Inline images look up by the `rel_id`
    /// their [`InlineKind::Image`] carries.
    pub media: std::collections::HashMap<String, ImageBlob>,
    /// Phase 8a — parsed `word/footnotes.xml` entries keyed by the OOXML
    /// `w:id`. The value is the footnote body's plain text per paragraph;
    /// the paginator looks an `InlineKind::FootnoteRef.id` up here when
    /// it lays out the page's footnote band.
    pub footnotes: std::collections::HashMap<u32, Vec<String>>,
    /// Phase 8a — parsed `word/comments.xml` entries keyed by `w:id`.
    /// Plain text + author / date metadata for the sidebar UI.
    pub comment_defs: std::collections::HashMap<u32, CommentDef>,
    /// Phase 8a — comment range overlays. Each entry is the byte-range
    /// span of one `<w:commentRangeStart>` / `<w:commentRangeEnd>` pair
    /// expressed in `LogicalPos` so a comment can span across paragraph
    /// (and table-cell) boundaries.
    pub comment_ranges: Vec<CommentRange>,
}

/// Phase 8a — author + date + body for one entry of `word/comments.xml`.
/// Body is currently the joined plain text of every `<w:p>` inside the
/// comment (rich formatting + reply threading deferred to Phase 8c).
#[derive(Debug, Clone, Default)]
pub struct CommentDef {
    pub author: String,
    pub date: String,
    pub paragraphs: Vec<String>,
}

/// Phase 8a — one `<w:commentRangeStart>` / `<w:commentRangeEnd>` overlay
/// on a logical position range. `id` matches a key in
/// [`DocumentTree::comment_defs`].
#[derive(Debug, Clone)]
pub struct CommentRange {
    pub id: u32,
    pub start: LogicalPos,
    pub end: LogicalPos,
}

/// Page geometry for a [`Section`]. Dimensions are layout pixels at 1 pt/unit
/// (matching `layout::A4Page`). The reader converts twips → pt (× 1/20) and
/// the renderer / paginator consume these values directly.
#[derive(Debug, Clone, Copy)]
pub struct PageGeometry {
    pub width: f32,
    pub height: f32,
    pub margin_top: f32,
    pub margin_right: f32,
    pub margin_bottom: f32,
    pub margin_left: f32,
    /// Distance from the top edge of the page to the top edge of the header
    /// content area. Optional in OOXML; defaults to half the top margin.
    pub header_offset: f32,
    /// Distance from the bottom edge of the page to the bottom edge of the
    /// footer content area.
    pub footer_offset: f32,
}

impl PageGeometry {
    /// ISO 216 A4 with 1-inch (72 pt) margins — the legacy `A4Page::a4()`.
    pub const fn a4() -> Self {
        Self {
            width: 595.0,
            height: 842.0,
            margin_top: 72.0,
            margin_right: 72.0,
            margin_bottom: 72.0,
            margin_left: 72.0,
            header_offset: 36.0,
            footer_offset: 36.0,
        }
    }

    pub fn content_width(&self) -> f32 {
        self.width - self.margin_left - self.margin_right
    }

    pub fn content_height(&self) -> f32 {
        self.height - self.margin_top - self.margin_bottom
    }
}

impl Default for PageGeometry {
    fn default() -> Self {
        Self::a4()
    }
}

/// One OOXML `<w:sectPr>` worth of state. A section spans a contiguous
/// half-open block range `[start, end)`; the page geometry is applied to
/// every page the paginator emits while flowing those blocks. `header_ref`
/// / `footer_ref` carry the relationship ids the reader captured — the
/// header / footer XML parts live in the archive's `other_entries` for the
/// passthrough writer.
#[derive(Debug, Clone, Default)]
pub struct Section {
    pub geometry: PageGeometry,
    /// First top-level block (inclusive) covered by this section.
    pub start_block: u32,
    /// One past the last top-level block (exclusive).
    pub end_block: u32,
    /// `r:id` of the `word/header*.xml` part referenced by `<w:headerReference>`.
    pub header_ref: Option<String>,
    /// `r:id` of the `word/footer*.xml` part referenced by `<w:footerReference>`.
    pub footer_ref: Option<String>,
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

    /// Parent container path (every step except the last).
    pub fn parent(&self) -> Self {
        let mut steps = self.steps.clone();
        steps.pop();
        Self { steps }
    }

    /// The path's final `Block`-step index, when one terminates the
    /// path. `None` when the path is empty or its last step is `Cell`.
    pub fn last_block_index(&self) -> Option<u32> {
        match self.steps.last()? {
            PathStep::Block(n) => Some(*n),
            PathStep::Cell { .. } => None,
        }
    }

    /// `true` when this path is a prefix of `descendant` (or equal).
    pub fn is_ancestor_of(&self, descendant: &Self) -> bool {
        if self.steps.len() > descendant.steps.len() {
            return false;
        }
        self.steps
            .iter()
            .zip(descendant.steps.iter())
            .all(|(a, b)| a == b)
    }

    /// Compare two paths in document order (depth-first walk). Used to
    /// canonicalize selection endpoints before edit/range operations.
    pub fn cmp_doc_order(&self, other: &Self) -> core::cmp::Ordering {
        use core::cmp::Ordering;
        let n = self.steps.len().min(other.steps.len());
        for i in 0..n {
            let ord = match (&self.steps[i], &other.steps[i]) {
                (PathStep::Block(a), PathStep::Block(b)) => a.cmp(b),
                (PathStep::Cell { row: r1, col: c1 }, PathStep::Cell { row: r2, col: c2 }) => {
                    r1.cmp(r2).then_with(|| c1.cmp(c2))
                }
                /* Shape mismatch in well-formed paths is unreachable
                (a Cell step always follows a Block step that descends
                into a table). Compare by surface index when it
                happens — keeps doc-order stable. */
                (PathStep::Block(a), PathStep::Cell { row: b, .. }) => a.cmp(b),
                (PathStep::Cell { row: a, .. }, PathStep::Block(b)) => a.cmp(b),
            };
            if ord != Ordering::Equal {
                return ord;
            }
        }
        self.steps.len().cmp(&other.steps.len())
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

/// Phase 7 — one EMU is **1/914400 of an inch**. 914400 EMU = 1 in = 72 pt;
/// dividing by 12700 converts straight to PostScript points, which is the
/// layout unit at scale=1. The paginator then multiplies by `scale` for the
/// device-pixel canvas.
pub const EMU_PER_PT: i64 = 12700;

/// Convert EMUs to layout points (the engine's 1 pt/unit space).
pub fn emu_to_pt(emu: i64) -> f32 {
    (emu as f32) / (EMU_PER_PT as f32)
}

/// Kind of inline object anchored in a paragraph's text. Phase 7 ships
/// inline images; Phase 8a adds footnote references.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InlineKind {
    /// `<w:drawing><wp:inline><a:graphic><pic:pic>` — a DrawingML picture.
    /// `rel_id` is the OOXML relationship id from the `<a:blip r:embed=...>`
    /// pointing to the `word/media/*` archive entry. `width_emu` /
    /// `height_emu` come from `<wp:extent cx="..." cy="..."/>`.
    Image {
        rel_id: String,
        width_emu: i64,
        height_emu: i64,
    },
    /// `<w:footnoteReference w:id="N"/>` — Phase 8a. `id` is the OOXML
    /// footnote id; `display_number` is the 1-based ordinal the renderer
    /// paints as the superscript marker (assigned at parse time in
    /// document order, skipping the sentinel `id=0` / `id=-1`
    /// separator / continuation entries `word/footnotes.xml` ships with).
    FootnoteRef { id: u32, display_number: u32 },
}

/// A non-text inline node anchored at a single byte offset in a paragraph.
/// The paragraph text carries one U+FFFC (OBJECT REPLACEMENT CHARACTER) at
/// `at`; layout looks the object up here when it sees the sentinel and
/// reserves the right physical size in the line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineObject {
    pub at: u32,
    pub kind: InlineKind,
}

/// A hyperlink overlay on a contiguous byte range of a paragraph. Display
/// styling (blue + underline if no explicit `<w:rPr>`) is applied at layout
/// time; clicks are out of scope for Phase 7 (the model is read-only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hyperlink {
    pub start: u32,
    pub end: u32,
    /// External URL (`Target` from the `r:id`'s rel entry). Internal
    /// document anchors (`<w:hyperlink w:anchor>`) are not modelled in
    /// this initial cut.
    pub target: String,
}

/// Phase 7 — a media blob stashed for the renderer to decode.
///
/// `content_type` is the MIME type the OOXML rels claimed (`image/png`,
/// `image/jpeg`, ...). The bytes are the raw archive entry contents — no
/// re-encoding, so format round-trips byte-identical through the writer
/// (writer-side media emission is a follow-up sprint).
#[derive(Debug, Clone)]
pub struct ImageBlob {
    pub content_type: String,
    pub data: Vec<u8>,
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
    /// Phase 7 — non-text inline objects anchored in the paragraph's text.
    /// Each one corresponds to a U+FFFC OBJECT REPLACEMENT CHARACTER in
    /// `text` at `inline_objects[i].at`. Sorted by `at`.
    pub inline_objects: Vec<InlineObject>,
    /// Phase 7 — hyperlink overlays on the paragraph's text. Multiple
    /// hyperlinks may exist; they do not overlap.
    pub hyperlinks: Vec<Hyperlink>,
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
            inline_objects: Vec::new(),
            hyperlinks: Vec::new(),
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
            inline_objects: Vec::new(),
            hyperlinks: Vec::new(),
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
                inline_objects: Vec::new(),
                hyperlinks: Vec::new(),
            },
            Paragraph {
                text: self.text[at as usize..].to_owned(),
                spans: right,
                props: self.props.clone(),
                list_item: self.list_item,
                resolved_marker: self.resolved_marker.clone(),
                dirty: true,
                source_xml: None,
                inline_objects: Vec::new(),
                hyperlinks: Vec::new(),
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
            inline_objects: Vec::new(),
            hyperlinks: Vec::new(),
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
LogicalPos — BlockPath addressing (Phase 5 PR 4).
`path` walks the block tree to a `Block::Paragraph`; `offset` is the
caret's byte offset inside that paragraph's UTF-8 text. Cross-cell
ranges work as long as the endpoints share a parent container; full
cross-container linear semantics ship with Phase 5c (the engine
currently clamps to the deeper endpoint's container).
==================================================================== */

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LogicalPos {
    pub path: BlockPath,
    /// Byte offset within the paragraph (UTF-8).
    pub offset: u32,
}

impl LogicalPos {
    pub fn new(path: BlockPath, offset: u32) -> Self {
        Self { path, offset }
    }

    /// Path to the Nth top-level paragraph, skipping tables — the
    /// canonical compat shim for callers that still address the doc
    /// paragraph-flat (RFC §4: `BlockPath::root_paragraph(n)`).
    pub fn at_top_paragraph(doc: &DocumentTree, n: u32, offset: u32) -> Option<Self> {
        let path = doc.path_to_top_paragraph(n)?;
        Some(Self { path, offset })
    }
}

impl DocumentTree {
    pub fn new() -> Self {
        Self {
            blocks: Vector::new(),
            sections: Vec::new(),
            headers: std::collections::HashMap::new(),
            footers: std::collections::HashMap::new(),
            media: std::collections::HashMap::new(),
            footnotes: std::collections::HashMap::new(),
            comment_defs: std::collections::HashMap::new(),
            comment_ranges: Vec::new(),
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
            inline_objects: Vec::new(),
            hyperlinks: Vec::new(),
        }));
        Self {
            blocks,
            sections: Vec::new(),
            headers: std::collections::HashMap::new(),
            footers: std::collections::HashMap::new(),
            media: std::collections::HashMap::new(),
            footnotes: std::collections::HashMap::new(),
            comment_defs: std::collections::HashMap::new(),
            comment_ranges: Vec::new(),
        }
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
                inline_objects: Vec::new(),
                hyperlinks: Vec::new(),
            }));
        }
        Self {
            blocks,
            sections: Vec::new(),
            headers: std::collections::HashMap::new(),
            footers: std::collections::HashMap::new(),
            media: std::collections::HashMap::new(),
            footnotes: std::collections::HashMap::new(),
            comment_defs: std::collections::HashMap::new(),
            comment_ranges: Vec::new(),
        }
    }

    /// Build a document from pre-styled paragraphs — the `.docx` reader (run
    /// properties → spans) and the HTML paste path both produce these.
    pub fn from_rich_paragraphs<I: IntoIterator<Item = Paragraph>>(paras: I) -> Self {
        let mut blocks = Vector::new();
        for p in paras {
            blocks.push_back(Block::Paragraph(p));
        }
        Self {
            blocks,
            sections: Vec::new(),
            headers: std::collections::HashMap::new(),
            footers: std::collections::HashMap::new(),
            media: std::collections::HashMap::new(),
            footnotes: std::collections::HashMap::new(),
            comment_defs: std::collections::HashMap::new(),
            comment_ranges: Vec::new(),
        }
    }

    /// Build a document from a pre-mixed block sequence — the `.docx` reader
    /// (with tables) produces these. Phase 5 PR 1 entry point.
    pub fn from_blocks<I: IntoIterator<Item = Block>>(blocks_in: I) -> Self {
        let mut blocks = Vector::new();
        for b in blocks_in {
            blocks.push_back(b);
        }
        Self {
            blocks,
            sections: Vec::new(),
            headers: std::collections::HashMap::new(),
            footers: std::collections::HashMap::new(),
            media: std::collections::HashMap::new(),
            footnotes: std::collections::HashMap::new(),
            comment_defs: std::collections::HashMap::new(),
            comment_ranges: Vec::new(),
        }
    }

    /// Phase 6 — build a document from a pre-mixed block sequence plus the
    /// section table the `.docx` reader collected from `<w:sectPr>` elements.
    /// Trims sections that fall outside the block range so the paginator
    /// never indexes off the end.
    pub fn from_blocks_with_sections<I: IntoIterator<Item = Block>>(
        blocks_in: I,
        sections_in: Vec<Section>,
    ) -> Self {
        let mut blocks = Vector::new();
        for b in blocks_in {
            blocks.push_back(b);
        }
        let len = blocks.len() as u32;
        let sections: Vec<Section> = sections_in
            .into_iter()
            .filter_map(|mut s| {
                s.start_block = s.start_block.min(len);
                s.end_block = s.end_block.min(len);
                if s.end_block <= s.start_block {
                    return None;
                }
                Some(s)
            })
            .collect();
        Self {
            blocks,
            sections,
            headers: std::collections::HashMap::new(),
            footers: std::collections::HashMap::new(),
            media: std::collections::HashMap::new(),
            footnotes: std::collections::HashMap::new(),
            comment_defs: std::collections::HashMap::new(),
            comment_ranges: Vec::new(),
        }
    }

    /// Phase 6b — attach the parsed header / footer parts collected from
    /// `word/header*.xml` / `word/footer*.xml`, keyed by their relationship
    /// id (`r:id`). Consumed by the paginator when a section's
    /// `header_ref` / `footer_ref` resolves.
    pub fn with_header_footer_parts(
        mut self,
        headers: std::collections::HashMap<String, Vec<String>>,
        footers: std::collections::HashMap<String, Vec<String>>,
    ) -> Self {
        self.headers = headers;
        self.footers = footers;
        self
    }

    /// Resolved section coverage — returns one effective `Section` per
    /// top-level block. When `self.sections` is empty (the pre-Phase-6
    /// case) the helper synthesises a single implicit A4 section over the
    /// whole document.
    pub fn effective_sections(&self) -> Vec<Section> {
        if self.sections.is_empty() {
            return vec![Section {
                geometry: PageGeometry::a4(),
                start_block: 0,
                end_block: self.blocks.len() as u32,
                header_ref: None,
                footer_ref: None,
            }];
        }
        self.sections.clone()
    }

    /* ============================================================
    Phase 5 PR 4 — `BlockPath` walk helpers
    The paragraph-flat shim (`nth_paragraph` / `paragraph_count` /
    `paragraph_text`) is kept as a compatibility surface for tests
    and round-trip callers; the canonical position type is now
    `LogicalPos { path, offset }`, addressed through the helpers
    below.
    ============================================================ */

    /// Resolve a `BlockPath` to its terminal `Block`. The path's first
    /// step is a top-level `Block(n)`; subsequent `Cell` / `Block`
    /// pairs descend into table cells.
    pub fn block_at(&self, path: &BlockPath) -> Option<&Block> {
        let first = path.steps.first()?;
        let PathStep::Block(n) = first else {
            return None;
        };
        let block = self.blocks.get(*n as usize)?;
        block_at_descend(block, &path.steps[1..])
    }

    /// Resolve a `BlockPath` to its terminal `Paragraph`; `None` when
    /// the path is empty or terminates at a `Table`.
    pub fn paragraph_at_path(&self, path: &BlockPath) -> Option<&Paragraph> {
        self.block_at(path)?.as_paragraph()
    }

    /// Resolve a `BlockPath` to a borrowed reference to its terminal
    /// `Table`; `None` when the path does not terminate at one.
    pub fn table_at_path(&self, path: &BlockPath) -> Option<&Table> {
        self.block_at(path)?.as_table()
    }

    /// Path to the Nth top-level paragraph (skipping tables). Compat
    /// shim for callers that still index paragraph-flat — RFC §4
    /// `BlockPath::root_paragraph(n)`.
    pub fn path_to_top_paragraph(&self, n: u32) -> Option<BlockPath> {
        let mut seen = 0u32;
        for (i, b) in self.blocks.iter().enumerate() {
            if matches!(b, Block::Paragraph(_)) {
                if seen == n {
                    return Some(BlockPath::top(i as u32));
                }
                seen += 1;
            }
        }
        None
    }

    /// Path to the document's last top-level paragraph (skipping
    /// tables). `None` for empty / tables-only documents.
    pub fn path_to_last_top_paragraph(&self) -> Option<BlockPath> {
        let mut last: Option<u32> = None;
        for (i, b) in self.blocks.iter().enumerate() {
            if matches!(b, Block::Paragraph(_)) {
                last = Some(i as u32);
            }
        }
        last.map(BlockPath::top)
    }

    /* ============================================================
    Phase 5 PR 1 — paragraph-flat shim
    Treats `Block::Table` as inert. Kept as a compatibility helper;
    every interactive path now uses the `BlockPath` helpers above.
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
        let Some(path) = self.path_to_last_top_paragraph() else {
            return LogicalPos {
                path: BlockPath::top(0),
                offset: 0,
            };
        };
        let offset = self
            .paragraph_at_path(&path)
            .map(|p| p.text.len() as u32)
            .unwrap_or(0);
        LogicalPos { path, offset }
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
                inline_objects: Vec::new(),
                hyperlinks: Vec::new(),
            }));
            return Self {
                blocks,
                sections: self.sections.clone(),
                headers: self.headers.clone(),
                footers: self.footers.clone(),
                media: self.media.clone(),
                footnotes: self.footnotes.clone(),
                comment_defs: self.comment_defs.clone(),
                comment_ranges: self.comment_ranges.clone(),
            };
        }
        let target = if self.paragraph_at_path(&at.path).is_some() {
            at.path.clone()
        } else {
            /* Path no longer addresses a paragraph (clamped after a
            structural edit). Fall back to the document end. */
            self.path_to_last_top_paragraph()
                .unwrap_or(BlockPath::top(0))
        };
        let off = at.offset;
        let mutated = mutate_paragraph_in_top(&mut blocks, &target, |para| {
            let offset = (off as usize).min(para.text.len());
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
        });
        if mutated.is_none() {
            return self.clone();
        }
        Self {
            blocks,
            sections: self.sections.clone(),
            headers: self.headers.clone(),
            footers: self.footers.clone(),
            media: self.media.clone(),
            footnotes: self.footnotes.clone(),
            comment_defs: self.comment_defs.clone(),
            comment_ranges: self.comment_ranges.clone(),
        }
    }

    /// Apply a style `patch` over the logical range `[start, end)`. Splits and
    /// merges spans on every covered paragraph; unaffected paragraphs are
    /// structurally shared. PR 4: full range support only when `start` and
    /// `end` share a parent container (`same_parent`); cross-container
    /// ranges clamp to the `start` endpoint's paragraph.
    pub fn apply_style(&self, start: LogicalPos, end: LogicalPos, patch: SpanStyle) -> Self {
        let (start, end) = order_positions(start, end);
        if !same_parent(&start.path, &end.path) {
            return self.apply_style_single(start, end, patch);
        }
        let Some(start_idx) = start.path.last_block_index() else {
            return self.clone();
        };
        let Some(end_idx) = end.path.last_block_index() else {
            return self.clone();
        };
        let Some(container) = parent_container_snapshot(self, &start.path) else {
            return self.clone();
        };
        let mut blocks = self.blocks.clone();
        let parent = start.path.parent();
        for idx in start_idx..=end_idx {
            let Some(Block::Paragraph(p)) = container.get(idx as usize) else {
                continue;
            };
            let lo = if idx == start_idx { start.offset } else { 0 };
            let hi = if idx == end_idx {
                end.offset
            } else {
                p.text.len() as u32
            };
            let styled = p.apply_style(lo, hi, patch);
            let child_path = parent.clone().push(PathStep::Block(idx));
            replace_block_in_top(&mut blocks, &child_path, Block::Paragraph(styled));
        }
        Self {
            blocks,
            sections: self.sections.clone(),
            headers: self.headers.clone(),
            footers: self.footers.clone(),
            media: self.media.clone(),
            footnotes: self.footnotes.clone(),
            comment_defs: self.comment_defs.clone(),
            comment_ranges: self.comment_ranges.clone(),
        }
    }

    fn apply_style_single(&self, start: LogicalPos, end: LogicalPos, patch: SpanStyle) -> Self {
        let Some(p) = self.paragraph_at_path(&start.path) else {
            return self.clone();
        };
        let hi = if start.path == end.path {
            end.offset
        } else {
            p.text.len() as u32
        };
        let styled = p.apply_style(start.offset, hi, patch);
        let mut blocks = self.blocks.clone();
        replace_block_in_top(&mut blocks, &start.path, Block::Paragraph(styled));
        Self {
            blocks,
            sections: self.sections.clone(),
            headers: self.headers.clone(),
            footers: self.footers.clone(),
            media: self.media.clone(),
            footnotes: self.footnotes.clone(),
            comment_defs: self.comment_defs.clone(),
            comment_ranges: self.comment_ranges.clone(),
        }
    }

    /// Set `align` on every paragraph the logical range `[start, end)` spans
    /// (Backlog #9). Paragraphs outside the range are structurally shared.
    /// `start`/`end` are expected in document order. PR 4: same-parent
    /// ranges spread across siblings; cross-container ranges align only
    /// `start`'s paragraph.
    pub fn set_alignment(&self, start: LogicalPos, end: LogicalPos, align: Alignment) -> Self {
        let (start, end) = order_positions(start, end);
        let mut blocks = self.blocks.clone();
        if same_parent(&start.path, &end.path) {
            let Some(start_idx) = start.path.last_block_index() else {
                return self.clone();
            };
            let Some(end_idx) = end.path.last_block_index() else {
                return self.clone();
            };
            let parent = start.path.parent();
            for idx in start_idx..=end_idx {
                let child_path = parent.clone().push(PathStep::Block(idx));
                let _ = mutate_paragraph_in_top(&mut blocks, &child_path, |para| {
                    para.props.alignment = Some(align);
                });
            }
        } else {
            let _ = mutate_paragraph_in_top(&mut blocks, &start.path, |para| {
                para.props.alignment = Some(align);
            });
        }
        Self {
            blocks,
            sections: self.sections.clone(),
            headers: self.headers.clone(),
            footers: self.footers.clone(),
            media: self.media.clone(),
            footnotes: self.footnotes.clone(),
            comment_defs: self.comment_defs.clone(),
            comment_ranges: self.comment_ranges.clone(),
        }
    }

    /// Delete the logical range `[start, end)`. A range spanning paragraphs
    /// merges the partial first and last paragraphs and drops those between.
    /// PR 4: same-parent cross-paragraph ranges work end-to-end; cross-
    /// container ranges (cell ↔ body) clamp to the `start` paragraph.
    pub fn delete_range(&self, start: LogicalPos, end: LogicalPos) -> Self {
        let (start, end) = order_positions(start, end);
        if self.paragraph_count() == 0 {
            return self.clone();
        }
        if start.path == end.path {
            let mut blocks = self.blocks.clone();
            let _ = mutate_paragraph_in_top(&mut blocks, &start.path, |para| {
                *para = para.delete_text(start.offset, end.offset);
            });
            return Self {
                blocks,
                sections: self.sections.clone(),
                headers: self.headers.clone(),
                footers: self.footers.clone(),
                media: self.media.clone(),
                footnotes: self.footnotes.clone(),
                comment_defs: self.comment_defs.clone(),
                comment_ranges: self.comment_ranges.clone(),
            };
        }
        if !same_parent(&start.path, &end.path) {
            /* Cross-container delete clamps to the start endpoint —
            full cross-container linear semantics land with Phase 5c. */
            let end_in_start_container = LogicalPos {
                path: start.path.clone(),
                offset: self
                    .paragraph_at_path(&start.path)
                    .map(|p| p.text.len() as u32)
                    .unwrap_or(start.offset),
            };
            return self.delete_range(start, end_in_start_container);
        }
        let Some(sp_idx) = start.path.last_block_index() else {
            return self.clone();
        };
        let Some(ep_idx) = end.path.last_block_index() else {
            return self.clone();
        };
        let Some(container) = parent_container_snapshot(self, &start.path) else {
            return self.clone();
        };
        let head = container
            .get(sp_idx as usize)
            .and_then(|b| b.as_paragraph())
            .map(|p| p.split_at(start.offset).0)
            .unwrap_or_default();
        let tail = container
            .get(ep_idx as usize)
            .and_then(|b| b.as_paragraph())
            .map(|p| p.split_at(end.offset).1)
            .unwrap_or_default();
        let merged = head.concat(&tail);
        let mut blocks = self.blocks.clone();
        let parent = start.path.parent();
        /* Drop every block strictly after sp up to and including ep,
        then replace sp with the merged paragraph. */
        for idx in ((sp_idx + 1)..=ep_idx).rev() {
            let child = parent.clone().push(PathStep::Block(idx));
            delete_block_at_path(&mut blocks, &child);
        }
        let sp_path = parent.push(PathStep::Block(sp_idx));
        replace_block_in_top(&mut blocks, &sp_path, Block::Paragraph(merged));
        Self {
            blocks,
            sections: self.sections.clone(),
            headers: self.headers.clone(),
            footers: self.footers.clone(),
            media: self.media.clone(),
            footnotes: self.footnotes.clone(),
            comment_defs: self.comment_defs.clone(),
            comment_ranges: self.comment_ranges.clone(),
        }
    }

    /// Split the paragraph at `at`, the break falling between the two halves.
    pub fn split_paragraph(&self, at: LogicalPos) -> Self {
        let count = self.paragraph_count();
        let mut blocks = self.blocks.clone();
        if count == 0 {
            blocks.push_back(Block::Paragraph(Paragraph::default()));
            blocks.push_back(Block::Paragraph(Paragraph::default()));
            return Self {
                blocks,
                sections: self.sections.clone(),
                headers: self.headers.clone(),
                footers: self.footers.clone(),
                media: self.media.clone(),
                footnotes: self.footnotes.clone(),
                comment_defs: self.comment_defs.clone(),
                comment_ranges: self.comment_ranges.clone(),
            };
        }
        let Some(p) = self.paragraph_at_path(&at.path) else {
            return self.clone();
        };
        let (left, right) = p.split_at(at.offset);
        replace_block_in_top(&mut blocks, &at.path, Block::Paragraph(left));
        insert_block_after_path_in_top(&mut blocks, &at.path, Block::Paragraph(right));
        Self {
            blocks,
            sections: self.sections.clone(),
            headers: self.headers.clone(),
            footers: self.footers.clone(),
            media: self.media.clone(),
            footnotes: self.footnotes.clone(),
            comment_defs: self.comment_defs.clone(),
            comment_ranges: self.comment_ranges.clone(),
        }
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
            doc = doc.insert_text(cur.clone(), line);
            let after = LogicalPos {
                path: cur.path.clone(),
                offset: cur.offset + line.len() as u32,
            };
            if i + 1 < lines.len() {
                /* A newline follows this line — break the paragraph so the
                next line lands in a fresh one; the remainder of the original
                paragraph rides along on the tail. */
                doc = doc.split_paragraph(after.clone());
                /* Advance the path to the inserted sibling — its last
                Block step bumps by 1; other steps unchanged. */
                cur = LogicalPos {
                    path: bump_last_block_index(&cur.path),
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
    /// **Tables in the spanned range are silently dropped** —
    /// clipboard fragments stay paragraph-only. Cross-container ranges
    /// clamp to the start endpoint's container until Phase 5c.
    pub fn slice(&self, start: LogicalPos, end: LogicalPos) -> Vec<Paragraph> {
        let (start, end) = order_positions(start, end);
        if self.paragraph_count() == 0 {
            return Vec::new();
        }
        if start.path == end.path {
            let Some(p) = self.paragraph_at_path(&start.path) else {
                return Vec::new();
            };
            let head = p.split_at(end.offset).0;
            return vec![head.split_at(start.offset).1];
        }
        if !same_parent(&start.path, &end.path) {
            let Some(p) = self.paragraph_at_path(&start.path) else {
                return Vec::new();
            };
            return vec![p.split_at(start.offset).1];
        }
        let Some(sp_idx) = start.path.last_block_index() else {
            return Vec::new();
        };
        let Some(ep_idx) = end.path.last_block_index() else {
            return Vec::new();
        };
        let Some(container) = parent_container_snapshot(self, &start.path) else {
            return Vec::new();
        };
        let mut out: Vec<Paragraph> = Vec::with_capacity((ep_idx - sp_idx + 1) as usize);
        if let Some(p) = container
            .get(sp_idx as usize)
            .and_then(|b| b.as_paragraph())
        {
            out.push(p.split_at(start.offset).1);
        }
        for idx in (sp_idx + 1)..ep_idx {
            if let Some(p) = container.get(idx as usize).and_then(|b| b.as_paragraph()) {
                out.push(p.clone());
            }
        }
        if let Some(p) = container
            .get(ep_idx as usize)
            .and_then(|b| b.as_paragraph())
        {
            out.push(p.split_at(end.offset).0);
        }
        out
    }

    /// Insert pre-styled `paras` at `at`; returns the new tree and the caret
    /// at the end of the inserted content. The caller deletes any active
    /// selection first. Drives HTML paste (Backlog #12).
    pub fn insert_rich(&self, at: LogicalPos, paras: &[Paragraph]) -> (Self, LogicalPos) {
        if paras.is_empty() {
            return (self.clone(), at.clone());
        }
        let mut blocks = self.blocks.clone();
        if self.paragraph_count() == 0 {
            blocks.push_back(Block::Paragraph(Paragraph::default()));
        }
        let target_path = if self.paragraph_at_path(&at.path).is_some() {
            at.path.clone()
        } else {
            self.path_to_last_top_paragraph()
                .unwrap_or(BlockPath::top(0))
        };
        let Some(target_para) = self
            .paragraph_at_path(&target_path)
            .cloned()
            .or_else(|| Some(Paragraph::default()))
        else {
            return (self.clone(), at.clone());
        };
        let (head, tail) = target_para.split_at(at.offset);
        if paras.len() == 1 {
            let caret = LogicalPos {
                path: target_path.clone(),
                offset: (head.text.len() + paras[0].text.len()) as u32,
            };
            replace_block_in_top(
                &mut blocks,
                &target_path,
                Block::Paragraph(head.concat(&paras[0]).concat(&tail)),
            );
            return (
                Self {
                    blocks,
                    sections: self.sections.clone(),
                    headers: self.headers.clone(),
                    footers: self.footers.clone(),
                    media: self.media.clone(),
                    footnotes: self.footnotes.clone(),
                    comment_defs: self.comment_defs.clone(),
                    comment_ranges: self.comment_ranges.clone(),
                },
                caret,
            );
        }
        let lastp = &paras[paras.len() - 1];
        replace_block_in_top(
            &mut blocks,
            &target_path,
            Block::Paragraph(head.concat(&paras[0])),
        );
        let mut last_path = target_path.clone();
        for p in &paras[1..paras.len() - 1] {
            insert_block_after_path_in_top(&mut blocks, &last_path, Block::Paragraph(p.clone()));
            last_path = bump_last_block_index(&last_path);
        }
        insert_block_after_path_in_top(
            &mut blocks,
            &last_path,
            Block::Paragraph(lastp.concat(&tail)),
        );
        let final_path = bump_last_block_index(&last_path);
        let caret = LogicalPos {
            path: final_path,
            offset: lastp.text.len() as u32,
        };
        (
            Self {
                blocks,
                sections: self.sections.clone(),
                headers: self.headers.clone(),
                footers: self.footers.clone(),
                media: self.media.clone(),
                footnotes: self.footnotes.clone(),
                comment_defs: self.comment_defs.clone(),
                comment_ranges: self.comment_ranges.clone(),
            },
            caret,
        )
    }

    /// Extract the text of the logical range `[start, end)`. Paragraphs the
    /// range spans are joined by `\n`. Used for clipboard copy.
    pub fn text_range(&self, start: LogicalPos, end: LogicalPos) -> String {
        let (start, end) = order_positions(start, end);
        if self.paragraph_count() == 0 {
            return String::new();
        }
        if start.path == end.path {
            let Some(p) = self.paragraph_at_path(&start.path) else {
                return String::new();
            };
            let lo = (start.offset as usize).min(p.text.len());
            let hi = (end.offset as usize).min(p.text.len());
            if lo >= hi {
                return String::new();
            }
            return p.text[lo..hi].to_string();
        }
        if !same_parent(&start.path, &end.path) {
            let Some(p) = self.paragraph_at_path(&start.path) else {
                return String::new();
            };
            let lo = (start.offset as usize).min(p.text.len());
            return p.text[lo..].to_string();
        }
        let Some(sp_idx) = start.path.last_block_index() else {
            return String::new();
        };
        let Some(ep_idx) = end.path.last_block_index() else {
            return String::new();
        };
        let Some(container) = parent_container_snapshot(self, &start.path) else {
            return String::new();
        };
        let mut out = String::new();
        for idx in sp_idx..=ep_idx {
            let Some(para) = container.get(idx as usize).and_then(|b| b.as_paragraph()) else {
                continue;
            };
            let len = para.text.len();
            let lo = if idx == sp_idx {
                (start.offset as usize).min(len)
            } else {
                0
            };
            let hi = if idx == ep_idx {
                (end.offset as usize).min(len)
            } else {
                len
            };
            if idx > sp_idx {
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
        /* Default column width — evenly divide A4 content width
        (9020 twips ≈ 6.26 in) so a fresh table fits the page on
        insert. Phase 5c will switch to `<w:tblLayout w:type="autofit"/>`
        once auto-fit lands; until then a literal grid that matches
        the page is the pragmatic default. */
        let per_col = (DEFAULT_A4_CONTENT_TWIPS / cols as i32).max(720);
        let grid: Vec<i32> = vec![per_col; cols];
        let mut row_vec: Vec<TableRow> = Vec::with_capacity(rows.max(1) as usize);
        for _ in 0..rows.max(1) {
            let mut cells = Vec::with_capacity(cols);
            for _ in 0..cols {
                cells.push(default_table_cell());
            }
            row_vec.push(TableRow {
                props: RowProperties::default(),
                cells,
            });
        }
        let table = Table {
            grid,
            /* Word-style default outer borders — 0.5 pt single black on
            every edge so a freshly inserted table is visible without
            the user opening the borders panel. */
            props: TableProperties {
                borders: Some(default_word_borders()),
                ..TableProperties::default()
            },
            rows: row_vec,
            /* Engine-synthesised — no source bytes, fully regenerated on
            save. */
            dirty: true,
            source_xml: None,
        };
        let mut blocks = self.blocks.clone();
        let insert_at = (idx as usize).min(blocks.len());
        blocks.insert(insert_at, Block::Table(table));
        Self {
            blocks,
            sections: self.sections.clone(),
            headers: self.headers.clone(),
            footers: self.footers.clone(),
            media: self.media.clone(),
            footnotes: self.footnotes.clone(),
            comment_defs: self.comment_defs.clone(),
            comment_ranges: self.comment_ranges.clone(),
        }
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
        Self {
            blocks,
            sections: self.sections.clone(),
            headers: self.headers.clone(),
            footers: self.footers.clone(),
            media: self.media.clone(),
            footnotes: self.footnotes.clone(),
            comment_defs: self.comment_defs.clone(),
            comment_ranges: self.comment_ranges.clone(),
        }
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
                cells: (0..cols).map(|_| default_table_cell()).collect(),
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
            /* Re-divide the A4 content width across the new column
            count so an inserted column shrinks the existing ones
            instead of pushing the table past the right margin. */
            let new_cols = t.grid.len() + 1;
            let per_col = (DEFAULT_A4_CONTENT_TWIPS / new_cols.max(1) as i32).max(720);
            t.grid.insert(insert_at, per_col);
            for w in t.grid.iter_mut() {
                *w = per_col;
            }
            for row in &mut t.rows {
                let cell_at = insert_at.min(row.cells.len());
                row.cells.insert(cell_at, default_table_cell());
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
        Self {
            blocks,
            sections: self.sections.clone(),
            headers: self.headers.clone(),
            footers: self.footers.clone(),
            media: self.media.clone(),
            footnotes: self.footnotes.clone(),
            comment_defs: self.comment_defs.clone(),
            comment_ranges: self.comment_ranges.clone(),
        }
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

/// Default `<w:sz>` for a synthesised cell border — 4 eighths of a
/// point ≈ 0.5 pt single black line. Word's out-of-the-box border
/// weight; matches what `<w:tblBorders>` emits on `Normal.dotx`.
const DEFAULT_BORDER_SIZE_EIGHTH_PT: u16 = 4;

/// A4 content width in twips, sized to match the engine's layout-px
/// universe (1 layout px ≡ 1 CSS px ≡ 15 twips at 96 DPI — the same
/// conversion factor `twips_to_layout_px` uses). The page is 595 ×
/// 842 layout-px with 72 layout-px margins, leaving 451 layout-px of
/// content → 451 × 15 = 6765 twips. Used to seed a fresh table's
/// grid so the table fits inside the page margins on insert. Phase
/// 5c will switch to `<w:tblLayout w:type="autofit"/>`.
const DEFAULT_A4_CONTENT_TWIPS: i32 = 6765;

/// Word-style default cell-edge stroke — single 0.5 pt black.
pub fn default_word_stroke() -> BorderStroke {
    BorderStroke {
        style: BorderStyle::Single,
        size_eighth_pt: DEFAULT_BORDER_SIZE_EIGHTH_PT,
        color: Some([0, 0, 0, 255]),
    }
}

/// All-edges Word default border set. Used for both cell-level and
/// table-level borders on engine-synthesised tables (`InsertTable` /
/// `InsertRow` / `InsertColumn`) so freshly inserted tables paint
/// without the user opening the borders picker first.
pub fn default_word_borders() -> CellBorders {
    let s = default_word_stroke();
    CellBorders {
        top: Some(s.clone()),
        left: Some(s.clone()),
        bottom: Some(s.clone()),
        right: Some(s.clone()),
        inside_h: Some(s.clone()),
        inside_v: Some(s),
    }
}

/// Construct a default table cell — one empty paragraph (so layout
/// has something to measure) + Word-default 0.5 pt single-line
/// borders on every edge. Without the placeholder paragraph cells
/// collapse to zero height; without the borders the table is
/// invisible until the user dresses it up.
pub fn default_table_cell() -> TableCell {
    TableCell {
        props: CellProperties {
            borders: Some(default_word_borders()),
            ..CellProperties::default()
        },
        blocks: vec![Block::Paragraph(Paragraph::default())],
    }
}

/// Continue a `block_at` walk from one `Block` through any remaining
/// `Cell + Block` step pairs.
fn block_at_descend<'a>(block: &'a Block, steps: &[PathStep]) -> Option<&'a Block> {
    if steps.is_empty() {
        return Some(block);
    }
    let Block::Table(t) = block else {
        return None;
    };
    let PathStep::Cell { row, col } = steps[0] else {
        return None;
    };
    let cell = t.rows.get(row as usize)?.cells.get(col as usize)?;
    let PathStep::Block(n) = *steps.get(1)? else {
        return None;
    };
    let next = cell.blocks.get(n as usize)?;
    block_at_descend(next, &steps[2..])
}

/// Mutate the paragraph addressed by `path` in `top` (the
/// `im::Vector` top-level container). `f` runs against the cloned
/// paragraph in place; the containing block (and any intervening
/// table) is cloned + spliced back so the structural-sharing
/// invariant holds. Returns `Some(())` on success.
fn mutate_paragraph_in_top<F>(top: &mut Vector<Block>, path: &BlockPath, f: F) -> Option<()>
where
    F: FnOnce(&mut Paragraph),
{
    let first = path.steps.first()?;
    let PathStep::Block(n) = first else {
        return None;
    };
    let n = *n as usize;
    let mut block = top.get(n)?.clone();
    if path.steps.len() == 1 {
        let Block::Paragraph(ref mut p) = block else {
            return None;
        };
        f(p);
        p.dirty = true;
        p.source_xml = None;
        top.set(n, block);
        return Some(());
    }
    let Block::Table(ref mut t) = block else {
        return None;
    };
    let PathStep::Cell { row, col } = path.steps[1] else {
        return None;
    };
    let row_box = t.rows.get_mut(row as usize)?;
    let cell = row_box.cells.get_mut(col as usize)?;
    mutate_paragraph_in_vec(&mut cell.blocks, &path.steps[2..], f)?;
    /* A mutation inside a cell dirties the containing table so the
    writer regenerates it (PR 3 passthrough invariant). */
    t.dirty = true;
    t.source_xml = None;
    top.set(n, block);
    Some(())
}

#[allow(clippy::ptr_arg)]
fn mutate_paragraph_in_vec<F>(blocks: &mut Vec<Block>, steps: &[PathStep], f: F) -> Option<()>
where
    F: FnOnce(&mut Paragraph),
{
    let first = steps.first()?;
    let PathStep::Block(n) = first else {
        return None;
    };
    let n = *n as usize;
    let block = blocks.get_mut(n)?;
    if steps.len() == 1 {
        let Block::Paragraph(p) = block else {
            return None;
        };
        f(p);
        p.dirty = true;
        p.source_xml = None;
        return Some(());
    }
    let Block::Table(t) = block else {
        return None;
    };
    let PathStep::Cell { row, col } = steps[1] else {
        return None;
    };
    let row_box = t.rows.get_mut(row as usize)?;
    let cell = row_box.cells.get_mut(col as usize)?;
    mutate_paragraph_in_vec(&mut cell.blocks, &steps[2..], f)?;
    t.dirty = true;
    t.source_xml = None;
    Some(())
}

/// Replace the block at `path` with `replacement` in `top`. Used by
/// `split_paragraph` etc. to splice freshly built paragraphs into the
/// container structurally.
fn replace_block_in_top(
    top: &mut Vector<Block>,
    path: &BlockPath,
    replacement: Block,
) -> Option<()> {
    let first = path.steps.first()?;
    let PathStep::Block(n) = first else {
        return None;
    };
    let n = *n as usize;
    if path.steps.len() == 1 {
        top.set(n, replacement);
        return Some(());
    }
    let mut block = top.get(n)?.clone();
    let Block::Table(ref mut t) = block else {
        return None;
    };
    let PathStep::Cell { row, col } = path.steps[1] else {
        return None;
    };
    let cell = t.rows.get_mut(row as usize)?.cells.get_mut(col as usize)?;
    replace_block_in_vec(&mut cell.blocks, &path.steps[2..], replacement)?;
    t.dirty = true;
    t.source_xml = None;
    top.set(n, block);
    Some(())
}

#[allow(clippy::ptr_arg)]
fn replace_block_in_vec(
    blocks: &mut Vec<Block>,
    steps: &[PathStep],
    replacement: Block,
) -> Option<()> {
    let first = steps.first()?;
    let PathStep::Block(n) = first else {
        return None;
    };
    let n = *n as usize;
    if steps.len() == 1 {
        if n >= blocks.len() {
            return None;
        }
        blocks[n] = replacement;
        return Some(());
    }
    let block = blocks.get_mut(n)?;
    let Block::Table(t) = block else {
        return None;
    };
    let PathStep::Cell { row, col } = steps[1] else {
        return None;
    };
    let cell = t.rows.get_mut(row as usize)?.cells.get_mut(col as usize)?;
    replace_block_in_vec(&mut cell.blocks, &steps[2..], replacement)?;
    t.dirty = true;
    t.source_xml = None;
    Some(())
}

/// Insert `inserted` immediately after `path` in its parent container.
fn insert_block_after_path_in_top(
    top: &mut Vector<Block>,
    path: &BlockPath,
    inserted: Block,
) -> Option<()> {
    let first = path.steps.first()?;
    let PathStep::Block(n) = first else {
        return None;
    };
    let n = *n as usize;
    if path.steps.len() == 1 {
        if n >= top.len() {
            top.push_back(inserted);
        } else {
            top.insert(n + 1, inserted);
        }
        return Some(());
    }
    let mut block = top.get(n)?.clone();
    let Block::Table(ref mut t) = block else {
        return None;
    };
    let PathStep::Cell { row, col } = path.steps[1] else {
        return None;
    };
    let cell = t.rows.get_mut(row as usize)?.cells.get_mut(col as usize)?;
    insert_block_after_path_in_vec(&mut cell.blocks, &path.steps[2..], inserted)?;
    t.dirty = true;
    t.source_xml = None;
    top.set(n, block);
    Some(())
}

fn insert_block_after_path_in_vec(
    blocks: &mut Vec<Block>,
    steps: &[PathStep],
    inserted: Block,
) -> Option<()> {
    let first = steps.first()?;
    let PathStep::Block(n) = first else {
        return None;
    };
    let n = *n as usize;
    if steps.len() == 1 {
        let at = (n + 1).min(blocks.len());
        blocks.insert(at, inserted);
        return Some(());
    }
    let block = blocks.get_mut(n)?;
    let Block::Table(t) = block else {
        return None;
    };
    let PathStep::Cell { row, col } = steps[1] else {
        return None;
    };
    let cell = t.rows.get_mut(row as usize)?.cells.get_mut(col as usize)?;
    insert_block_after_path_in_vec(&mut cell.blocks, &steps[2..], inserted)?;
    t.dirty = true;
    t.source_xml = None;
    Some(())
}

/// Resolve `path`'s parent container into an owned snapshot
/// (`Vec<Block>` cloned out of the document). Used by range methods
/// that need to walk the paragraphs between two same-container
/// endpoints; structural sharing is preserved by mutating through
/// the dedicated splice helpers above (`replace_block_in_*`,
/// `insert_block_after_path_in_*`), not by writing this snapshot
/// back.
pub fn parent_container_snapshot(doc: &DocumentTree, path: &BlockPath) -> Option<Vec<Block>> {
    if path.steps.len() == 1 {
        return Some(doc.blocks.iter().cloned().collect());
    }
    if path.steps.len() < 3 {
        return None;
    }
    let n = path.steps.len();
    let grandparent = BlockPath {
        steps: path.steps[..n - 2].to_vec(),
    };
    let block = doc.block_at(&grandparent)?;
    let Block::Table(t) = block else {
        return None;
    };
    let PathStep::Cell { row, col } = path.steps[n - 2] else {
        return None;
    };
    let cell = t.rows.get(row as usize)?.cells.get(col as usize)?;
    Some(cell.blocks.clone())
}

/// Same parent container? Two paragraph paths share a container
/// when every step but the last is identical.
pub fn same_parent(a: &BlockPath, b: &BlockPath) -> bool {
    a.steps.len() == b.steps.len() && a.parent() == b.parent()
}

/// Path with its last `Block` step's index bumped by 1. Used by
/// `split_paragraph` etc. to compute the path of the inserted
/// sibling.
pub fn bump_last_block_index(path: &BlockPath) -> BlockPath {
    let mut steps = path.steps.clone();
    if let Some(PathStep::Block(n)) = steps.last_mut() {
        *n += 1;
    }
    BlockPath { steps }
}

/// Order two positions in document order. The first returned position
/// is always `<=` the second when compared by `(path, offset)`.
pub fn order_positions(a: LogicalPos, b: LogicalPos) -> (LogicalPos, LogicalPos) {
    use core::cmp::Ordering;
    let ord = a.path.cmp_doc_order(&b.path);
    let swap = match ord {
        Ordering::Less => false,
        Ordering::Greater => true,
        Ordering::Equal => a.offset > b.offset,
    };
    if swap { (b, a) } else { (a, b) }
}

/// Delete the block at `path` from its parent container.
fn delete_block_at_path(top: &mut Vector<Block>, path: &BlockPath) -> Option<()> {
    let first = path.steps.first()?;
    let PathStep::Block(n) = first else {
        return None;
    };
    let n = *n as usize;
    if path.steps.len() == 1 {
        if n >= top.len() {
            return None;
        }
        top.remove(n);
        return Some(());
    }
    let mut block = top.get(n)?.clone();
    let Block::Table(ref mut t) = block else {
        return None;
    };
    let PathStep::Cell { row, col } = path.steps[1] else {
        return None;
    };
    let cell = t.rows.get_mut(row as usize)?.cells.get_mut(col as usize)?;
    delete_block_in_vec(&mut cell.blocks, &path.steps[2..])?;
    t.dirty = true;
    t.source_xml = None;
    top.set(n, block);
    Some(())
}

fn delete_block_in_vec(blocks: &mut Vec<Block>, steps: &[PathStep]) -> Option<()> {
    let first = steps.first()?;
    let PathStep::Block(n) = first else {
        return None;
    };
    let n = *n as usize;
    if steps.len() == 1 {
        if n >= blocks.len() {
            return None;
        }
        blocks.remove(n);
        return Some(());
    }
    let block = blocks.get_mut(n)?;
    let Block::Table(t) = block else {
        return None;
    };
    let PathStep::Cell { row, col } = steps[1] else {
        return None;
    };
    let cell = t.rows.get_mut(row as usize)?.cells.get_mut(col as usize)?;
    delete_block_in_vec(&mut cell.blocks, &steps[2..])?;
    t.dirty = true;
    t.source_xml = None;
    Some(())
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
        let d = d.insert_text(
            LogicalPos {
                path: BlockPath::top(0),
                offset: 0,
            },
            "hello",
        );
        assert_eq!(d.paragraph_text(0), Some("hello"));
    }

    #[test]
    fn insert_mid_paragraph() {
        let d = DocumentTree::from_text("hello world");
        let d = d.insert_text(
            LogicalPos {
                path: BlockPath::top(0),
                offset: 5,
            },
            ",",
        );
        assert_eq!(d.paragraph_text(0), Some("hello, world"));
    }

    #[test]
    fn apply_style_creates_span() {
        let doc = DocumentTree::from_text("hello world");
        let doc = doc.apply_style(
            LogicalPos {
                path: BlockPath::top(0),
                offset: 0,
            },
            LogicalPos {
                path: BlockPath::top(0),
                offset: 5,
            },
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
            LogicalPos {
                path: BlockPath::top(0),
                offset: 0,
            },
            LogicalPos {
                path: BlockPath::top(0),
                offset: 8,
            },
            red,
        );
        let doc = doc.apply_style(
            LogicalPos {
                path: BlockPath::top(0),
                offset: 4,
            },
            LogicalPos {
                path: BlockPath::top(0),
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
            LogicalPos {
                path: BlockPath::top(0),
                offset: 2,
            },
            LogicalPos {
                path: BlockPath::top(0),
                offset: 4,
            },
            SpanStyle {
                font_size: None,
                color: Some([1, 2, 3, 255]),
                ..Default::default()
            },
        );
        let doc = doc.insert_text(
            LogicalPos {
                path: BlockPath::top(0),
                offset: 0,
            },
            "XX",
        );
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
            inline_objects: Vec::new(),
            hyperlinks: Vec::new(),
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
            inline_objects: Vec::new(),
            hyperlinks: Vec::new(),
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
            inline_objects: Vec::new(),
            hyperlinks: Vec::new(),
        };
        assert_eq!(p.word_bounds(0), (0, 0));
    }

    #[test]
    fn delete_within_paragraph() {
        let d = DocumentTree::from_text("hello world");
        let d = d.delete_range(
            LogicalPos {
                path: BlockPath::top(0),
                offset: 5,
            },
            LogicalPos {
                path: BlockPath::top(0),
                offset: 11,
            },
        );
        assert_eq!(d.paragraph_text(0), Some("hello"));
    }

    #[test]
    fn delete_merges_paragraphs() {
        let d = DocumentTree::from_paragraphs(["abc".to_string(), "def".to_string()]);
        let d = d.delete_range(
            LogicalPos {
                path: BlockPath::top(0),
                offset: 3,
            },
            LogicalPos {
                path: BlockPath::top(1),
                offset: 0,
            },
        );
        assert_eq!(d.paragraph_count(), 1);
        assert_eq!(d.paragraph_text(0), Some("abcdef"));
    }

    #[test]
    fn delete_clips_spans() {
        let doc = DocumentTree::from_text("hello world");
        let doc = doc.apply_style(
            LogicalPos {
                path: BlockPath::top(0),
                offset: 0,
            },
            LogicalPos {
                path: BlockPath::top(0),
                offset: 5,
            },
            SpanStyle {
                font_size: Some(20.0),
                color: None,
                ..Default::default()
            },
        );
        let doc = doc.delete_range(
            LogicalPos {
                path: BlockPath::top(0),
                offset: 3,
            },
            LogicalPos {
                path: BlockPath::top(0),
                offset: 5,
            },
        );
        assert_eq!(doc.paragraph_text(0), Some("hel world"));
        let spans = &doc.nth_paragraph(0).unwrap().spans;
        assert_eq!(spans.len(), 1);
        assert_eq!((spans[0].start, spans[0].end), (0, 3));
    }

    #[test]
    fn split_paragraph_in_two() {
        let d = DocumentTree::from_text("hello world");
        let d = d.split_paragraph(LogicalPos {
            path: BlockPath::top(0),
            offset: 5,
        });
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
            inline_objects: Vec::new(),
            hyperlinks: Vec::new(),
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
                LogicalPos {
                    path: BlockPath::top(0),
                    offset: 0
                },
                LogicalPos {
                    path: BlockPath::top(0),
                    offset: 5
                },
            ),
            "hello"
        );
        assert_eq!(
            d.text_range(
                LogicalPos {
                    path: BlockPath::top(0),
                    offset: 6
                },
                LogicalPos {
                    path: BlockPath::top(1),
                    offset: 6
                },
            ),
            "world\nsecond"
        );
        /* reversed args normalize to document order */
        assert_eq!(
            d.text_range(
                LogicalPos {
                    path: BlockPath::top(0),
                    offset: 5
                },
                LogicalPos {
                    path: BlockPath::top(0),
                    offset: 0
                },
            ),
            "hello"
        );
    }

    #[test]
    fn apply_style_bold_italic_underline() {
        let doc = DocumentTree::from_text("hello world");
        /* Apply bold over [0,5). */
        let doc = doc.apply_style(
            LogicalPos {
                path: BlockPath::top(0),
                offset: 0,
            },
            LogicalPos {
                path: BlockPath::top(0),
                offset: 5,
            },
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
            LogicalPos {
                path: BlockPath::top(0),
                offset: 0,
            },
            LogicalPos {
                path: BlockPath::top(0),
                offset: 5,
            },
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

        let d2 = initial.insert_text(
            LogicalPos {
                path: BlockPath::top(0),
                offset: 3,
            },
            "def",
        );
        undo.push(d2.clone());
        assert_eq!(undo.current().paragraph_text(0), Some("abcdef"));

        let d3 = d2.insert_text(
            LogicalPos {
                path: BlockPath::top(0),
                offset: 6,
            },
            "ghi",
        );
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
            LogicalPos {
                path: BlockPath::top(0),
                offset: 0,
            },
            LogicalPos {
                path: BlockPath::top(1),
                offset: 0,
            },
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
            LogicalPos {
                path: BlockPath::top(0),
                offset: 0,
            },
            LogicalPos {
                path: BlockPath::top(0),
                offset: 0,
            },
            Alignment::End,
        );
        /* insertion clones the paragraph in place — alignment rides along */
        let d = d.insert_text(
            LogicalPos {
                path: BlockPath::top(0),
                offset: 0,
            },
            "X",
        );
        assert_eq!(d.paragraph_text(0), Some("Xhello world"));
        assert_eq!(
            d.nth_paragraph(0).unwrap().props.alignment,
            Some(Alignment::End)
        );
        /* a style change preserves alignment */
        let d = d.apply_style(
            LogicalPos {
                path: BlockPath::top(0),
                offset: 0,
            },
            LogicalPos {
                path: BlockPath::top(0),
                offset: 3,
            },
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
            LogicalPos {
                path: BlockPath::top(0),
                offset: 0,
            },
            LogicalPos {
                path: BlockPath::top(0),
                offset: 1,
            },
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
            LogicalPos {
                path: BlockPath::top(0),
                offset: 0,
            },
            LogicalPos {
                path: BlockPath::top(0),
                offset: 0,
            },
            Alignment::Center,
        );
        let d = d.split_paragraph(LogicalPos {
            path: BlockPath::top(0),
            offset: 5,
        });
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
            LogicalPos {
                path: BlockPath::top(0),
                offset: 0,
            },
            LogicalPos {
                path: BlockPath::top(0),
                offset: 0,
            },
            Alignment::Center,
        );
        let d = d.set_alignment(
            LogicalPos {
                path: BlockPath::top(1),
                offset: 0,
            },
            LogicalPos {
                path: BlockPath::top(1),
                offset: 0,
            },
            Alignment::End,
        );
        /* deleting the paragraph break merges the two */
        let d = d.delete_range(
            LogicalPos {
                path: BlockPath::top(0),
                offset: 3,
            },
            LogicalPos {
                path: BlockPath::top(1),
                offset: 0,
            },
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
        let (d, caret) = d.insert_multiline(
            LogicalPos {
                path: BlockPath::top(0),
                offset: 2,
            },
            "XY",
        );
        assert_eq!(d.paragraph_count(), 1);
        assert_eq!(d.paragraph_text(0), Some("abXYcd"));
        assert_eq!(
            caret,
            LogicalPos {
                path: BlockPath::top(0),
                offset: 4
            }
        );
    }

    #[test]
    fn insert_multiline_splits_into_paragraphs() {
        let d = DocumentTree::from_text("abcd");
        let (d, caret) = d.insert_multiline(
            LogicalPos {
                path: BlockPath::top(0),
                offset: 2,
            },
            "L0\nL1\nL2",
        );
        assert_eq!(d.paragraph_count(), 3);
        /* the original paragraph splits around the caret; the tail rides the
        last pasted line's paragraph */
        assert_eq!(d.paragraph_text(0), Some("abL0"));
        assert_eq!(d.paragraph_text(1), Some("L1"));
        assert_eq!(d.paragraph_text(2), Some("L2cd"));
        assert_eq!(
            caret,
            LogicalPos {
                path: BlockPath::top(2),
                offset: 2
            }
        );
    }

    #[test]
    fn insert_multiline_normalizes_crlf_and_cr() {
        let d = DocumentTree::from_text("");
        let (d, _) = d.insert_multiline(
            LogicalPos {
                path: BlockPath::top(0),
                offset: 0,
            },
            "a\r\nb\rc",
        );
        assert_eq!(d.paragraph_count(), 3);
        assert_eq!(d.paragraph_text(0), Some("a"));
        assert_eq!(d.paragraph_text(1), Some("b"));
        assert_eq!(d.paragraph_text(2), Some("c"));
    }

    #[test]
    fn insert_multiline_trailing_newline_makes_empty_paragraph() {
        let d = DocumentTree::from_text("xy");
        let (d, caret) = d.insert_multiline(
            LogicalPos {
                path: BlockPath::top(0),
                offset: 2,
            },
            "Z\n",
        );
        assert_eq!(d.paragraph_count(), 2);
        assert_eq!(d.paragraph_text(0), Some("xyZ"));
        assert_eq!(d.paragraph_text(1), Some(""));
        assert_eq!(
            caret,
            LogicalPos {
                path: BlockPath::top(1),
                offset: 0
            }
        );
    }

    #[test]
    fn insert_multiline_into_second_paragraph() {
        let d = DocumentTree::from_paragraphs(["first".to_string(), "second".to_string()]);
        let (d, caret) = d.insert_multiline(
            LogicalPos {
                path: BlockPath::top(1),
                offset: 3,
            },
            "A\nB",
        );
        assert_eq!(d.paragraph_count(), 3);
        assert_eq!(d.paragraph_text(0), Some("first"));
        assert_eq!(d.paragraph_text(1), Some("secA"));
        assert_eq!(d.paragraph_text(2), Some("Bond"));
        assert_eq!(
            caret,
            LogicalPos {
                path: BlockPath::top(2),
                offset: 1
            }
        );
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
            inline_objects: Vec::new(),
            hyperlinks: Vec::new(),
        };
        let doc = DocumentTree::from_rich_paragraphs([para]);
        /* Slice "lo wor" (bytes 3-9) — the bold span clips to 3-6, local. */
        let cut = doc.slice(
            LogicalPos {
                path: BlockPath::top(0),
                offset: 3,
            },
            LogicalPos {
                path: BlockPath::top(0),
                offset: 9,
            },
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
            inline_objects: Vec::new(),
            hyperlinks: Vec::new(),
        }];
        let (out, caret) = doc.insert_rich(
            LogicalPos {
                path: BlockPath::top(0),
                offset: 6,
            },
            &frag,
        );
        assert_eq!(out.paragraph_count(), 1);
        assert_eq!(out.paragraph_text(0), Some("hello BRAVE world"));
        assert_eq!(
            caret,
            LogicalPos {
                path: BlockPath::top(0),
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
                inline_objects: Vec::new(),
                hyperlinks: Vec::new(),
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
                inline_objects: Vec::new(),
                hyperlinks: Vec::new(),
            },
        ];
        let (out, caret) = doc.insert_rich(
            LogicalPos {
                path: BlockPath::top(0),
                offset: 2,
            },
            &frag,
        );
        assert_eq!(out.paragraph_count(), 2);
        assert_eq!(out.paragraph_text(0), Some("ABone"));
        assert_eq!(out.paragraph_text(1), Some("twoCD"));
        assert_eq!(
            caret,
            LogicalPos {
                path: BlockPath::top(1),
                offset: 3
            }
        );
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

    /// PR 4 visibility fix — a freshly inserted table must paint:
    /// each cell carries an empty paragraph placeholder so layout has
    /// something to measure, and Word-default 0.5pt single black
    /// borders on every cell + the table outer perimeter so the user
    /// sees the table on the canvas immediately.
    #[test]
    fn insert_table_seeds_placeholder_paragraph_and_default_borders() {
        let d = DocumentTree::new().insert_table(BlockPath::top(0), 3, 3);
        let t = d.blocks[0].as_table().expect("Block::Table");
        for (r, row) in t.rows.iter().enumerate() {
            for (c, cell) in row.cells.iter().enumerate() {
                assert!(
                    !cell.blocks.is_empty(),
                    "cell ({r},{c}) needs a placeholder paragraph for layout"
                );
                let p = cell.blocks[0]
                    .as_paragraph()
                    .expect("cell placeholder is a Paragraph");
                assert!(p.text.is_empty(), "placeholder is the empty paragraph");
                let borders = cell
                    .props
                    .borders
                    .as_ref()
                    .expect("cell needs default Word borders");
                assert!(borders.top.is_some());
                assert!(borders.bottom.is_some());
                assert!(borders.left.is_some());
                assert!(borders.right.is_some());
            }
        }
        let outer = t.props.borders.as_ref().expect("outer borders");
        assert!(outer.top.is_some() && outer.bottom.is_some());
    }

    /// PR 4 follow-up — Bug 4. A fresh table's grid must fit the A4
    /// content area: total grid twips ≤ DEFAULT_A4_CONTENT_TWIPS so
    /// the layout pass produces a table width ≤ page content width.
    /// 451 layout-px content × 15 twips/layout-px = 6765 twips.
    #[test]
    fn insert_table_grid_fits_a4_content_width() {
        for cols in 1u32..=8u32 {
            let d = DocumentTree::new().insert_table(BlockPath::top(0), 1, cols);
            let t = d.blocks[0].as_table().unwrap();
            assert_eq!(t.grid.len(), cols as usize);
            let total: i32 = t.grid.iter().sum();
            assert!(
                total <= DEFAULT_A4_CONTENT_TWIPS,
                "{cols}-col grid totals {total} twips, exceeds A4 content {DEFAULT_A4_CONTENT_TWIPS}"
            );
            assert!(
                total >= DEFAULT_A4_CONTENT_TWIPS - cols as i32,
                "{cols}-col grid totals {total} twips, leaves >1 twip/col slack"
            );
        }
    }

    /// Inserting a column re-divides the grid so total stays under
    /// the A4 content area instead of pushing the table past the
    /// right margin.
    #[test]
    fn insert_column_redivides_grid_within_content_width() {
        let d = DocumentTree::new().insert_table(BlockPath::top(0), 1, 3);
        let d = d.insert_column(BlockPath::top(0), 0);
        let t = d.blocks[0].as_table().unwrap();
        assert_eq!(t.grid.len(), 4);
        let total: i32 = t.grid.iter().sum();
        assert!(total <= DEFAULT_A4_CONTENT_TWIPS);
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
        let d = DocumentTree {
            blocks,
            sections: Vec::new(),
            headers: std::collections::HashMap::new(),
            footers: std::collections::HashMap::new(),
            media: std::collections::HashMap::new(),
            footnotes: std::collections::HashMap::new(),
            comment_defs: std::collections::HashMap::new(),
            comment_ranges: Vec::new(),
        };
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
