//! `engine` — document model + undo stack.
//!
//! Phase 1 weeks 15–18: plain-text paragraphs, in-place text insertion,
//! cheap snapshots via `im::Vector` for undo/redo.

use im::Vector;

pub mod html;
pub mod numbering;

/// Top-level document block (Phase 5 PR 1). Tables sit alongside
/// paragraphs in the body; future block variants (Phase 7 floating
/// images, Phase 8 footnotes) extend this enum.
///
/// Sprint 12 (#11) — `Paragraph` grew past clippy's
/// `large_enum_variant` threshold once the shadow direct_overrides
/// field landed (Paragraph now carries a full ParaProperties + a
/// shadow ParaProperties + style_id + everything from prior phases).
/// Boxing `Paragraph` here would touch every `Block::Paragraph(p)`
/// match site across nine crates; the trade-off is not worth the
/// memory savings for the typical 50-page document the engine
/// targets (the persistent `im::Vector` shares structurally between
/// snapshots anyway). Allowing the lint here is the documented
/// pragmatic choice.
#[allow(clippy::large_enum_variant)]
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
    pub headers: std::collections::HashMap<String, Vec<Paragraph>>,
    /// Mirror of `headers` for `<w:footerReference>`. Phase 2 audit
    /// (gap D.1 follow-up) — was `Vec<String>`; widened to full
    /// `Paragraph` so headers/footers carry style spans, inline
    /// objects, hyperlinks, revisions and `Field` overlays. The
    /// paginator's per-page field evaluator stamps PAGE/NUMPAGES on
    /// the laid-out copies these paragraphs produce.
    pub footers: std::collections::HashMap<String, Vec<Paragraph>>,
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
    /// Phase 2 audit — typed `word/settings.xml` flags. Currently only
    /// `even_and_odd_headers`; grows as more settings get modelled.
    pub settings: DocumentSettings,
    /// Sprint 12 (#11) — parsed `word/styles.xml` entries keyed by
    /// `w:styleId`. Sprint 12 ships paragraph styles only; character
    /// styles (`<w:rStyle>`) are deliberately out of scope. The
    /// reader populates this from `<w:style w:type="paragraph">`
    /// entries; the cascade walker
    /// (`DocumentTree::resolve_style_cascade`) folds a `style_id`
    /// chain through `based_on` into a flat `ParaProperties`.
    pub styles: std::collections::HashMap<String, ParagraphStyle>,
    /// Sprint 12 (#11) — document-wide `<w:docDefaults>`. Sits at
    /// the bottom of every paragraph's resolved cascade. The
    /// resolver merges `defaults → style chain → direct_overrides`
    /// in document order.
    pub style_defaults: ParaProperties,
    /// Issue #29 — `<w:docDefaults><w:rPrDefault>` run properties. The
    /// base of the run cascade: `style_run_defaults → pStyle-chain
    /// <w:rPr> → direct span formatting`, folded at span-materialize
    /// time (engine-wasm `build_style_spans`), never baked into spans.
    pub style_run_defaults: SpanStyle,
    /// Issue #21 — flips when `modify_style` mutates the style table so
    /// the `.docx` writer regenerates `word/styles.xml` (mirror of
    /// `NumberingDefinitions.dirty`). Never set by reads.
    pub styles_dirty: bool,
    /// Sprint 13 (#12) — in-memory mirror of `word/numbering.xml`.
    /// Drives marker resolution + the synthesis path the
    /// `Command::ToggleList { Bullet | Number }` handler invokes.
    /// `.dirty` flips to `true` only when synth_list_definition
    /// actually appends new entries; the writer then regenerates
    /// the part, otherwise the OPC passthrough byte-identical.
    pub numbering: numbering::NumberingDefinitions,
}

/// Sprint 12 (#11) — one `<w:style w:type="paragraph">` entry,
/// modelled in the engine so the live editor can apply / re-resolve
/// styles without the format-docx crate's `StyleTable`. Character
/// styles + table styles are deliberately out of scope.
#[derive(Debug, Clone, Default)]
pub struct ParagraphStyle {
    pub id: String,
    /// Human-readable name from `<w:name w:val>`. Drives the styles
    /// dropdown label; falls back to `id` when absent.
    pub name: String,
    /// `<w:basedOn w:val>` — parent style id. The cascade walker
    /// folds the chain root-first.
    pub based_on: Option<String>,
    /// `<w:pPr>` overrides this style contributes (folded onto the
    /// root-most ancestor's already-folded baseline).
    pub para: ParaProperties,
    /// `<w:rPr>` overrides this style contributes — applied to spans
    /// during cascade resolution since issue #29 (closed).
    pub run: SpanStyle,
}

/// Phase 8a — author + date + body for one entry of `word/comments.xml`.
/// Body is currently the joined plain text of every `<w:p>` inside the
/// comment (rich formatting + reply threading deferred to Phase 8c).
#[derive(Debug, Clone, Default)]
pub struct CommentDef {
    pub author: String,
    pub date: String,
    pub paragraphs: Vec<String>,
    /// Sprint 9 — round-tripped through `word/commentsExtended.xml`.
    /// The reader populates from `<w15:commentEx w15:done="1"/>`; the
    /// writer regenerates `commentsExtended.xml` when ANY comment
    /// carries `resolved = true`, otherwise the OPC passthrough keeps
    /// the original part byte-identical.
    pub resolved: bool,
    /// Sprint 9 — `w14:paraId` of this comment's first paragraph as
    /// captured by the comments.xml reader. `<w15:commentEx>` keys its
    /// entries by this id; without one, a synthesized comment cannot
    /// round-trip its resolved bit. Engine-minted comments leave this
    /// `None` until the comments.xml writer learns to mint paraIds —
    /// tracked as Core Engine tech-debt.
    pub first_para_id: Option<String>,
    /// Issue #27 — threaded replies. `Some(id)` marks this comment as
    /// a reply to the comment with that `w:id`; `None` marks a
    /// top-level comment. Round-trips through
    /// `word/commentsExtended.xml` `<w15:commentEx w15:paraIdParent>`
    /// (the reader maps the parent paraId back to its comment id via
    /// `first_para_id`).
    pub parent_id: Option<u32>,
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
///
/// **OOXML reference values.** A page size landed verbatim from a Word
/// `<w:sectPr><w:pgSz w:w="11906" w:h="16838"/></w:sectPr>` is ISO 216 A4
/// (210 × 297 mm). The canonical twips → pt math:
///
/// - `11906 twips / 20 = 595.3 pt`  ← page width
/// - `16838 twips / 20 = 841.9 pt`  ← page height
/// - `1440 twips / 20 = 72.0 pt`    ← Word default 1-inch margins
///
/// Aspect ratio 841.9 / 595.3 = 1.4143, matching ISO 216's `1 : √2`.
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
    /// ISO 216 A4 with 1-inch (72 pt) margins.
    ///
    /// Dimensions land exactly on the OOXML canonical twips:
    /// `<w:pgSz w:w="11906" w:h="16838"/>` and
    /// `<w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"
    /// w:header="720" w:footer="720"/>` — what `Word.exe` itself stamps
    /// on a freshly-created `Document1.docx`. Header / footer offsets
    /// default to 0.5 inch (720 twips / 36 pt), Word's stock value.
    pub const fn a4() -> Self {
        Self::from_twips(11906, 16838, 1440, 1440, 1440, 1440, 720, 720)
    }

    /// Build a `PageGeometry` from OOXML twips directly. 1 twip = 1/20 pt.
    /// Used by the `<w:pgSz>` / `<w:pgMar>` parser to preserve exact
    /// integer round-trip; in-code default constructors call this with
    /// canonical Word values so the model never drifts off-spec by
    /// floating-point rounding.
    #[allow(clippy::too_many_arguments)]
    pub const fn from_twips(
        w_twips: i32,
        h_twips: i32,
        top_twips: i32,
        right_twips: i32,
        bottom_twips: i32,
        left_twips: i32,
        header_twips: i32,
        footer_twips: i32,
    ) -> Self {
        Self {
            width: (w_twips as f32) / 20.0,
            height: (h_twips as f32) / 20.0,
            margin_top: (top_twips as f32) / 20.0,
            margin_right: (right_twips as f32) / 20.0,
            margin_bottom: (bottom_twips as f32) / 20.0,
            margin_left: (left_twips as f32) / 20.0,
            header_offset: (header_twips as f32) / 20.0,
            footer_offset: (footer_twips as f32) / 20.0,
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

/// `<w:headerReference>` / `<w:footerReference>` discriminator — the
/// `w:type` attribute. `Default` is what every page uses unless a more
/// specific variant is requested and selected by the paginator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum HeaderFooterRole {
    #[default]
    Default,
    /// `w:type="first"` — only used when `Section.title_pg` is `true`.
    First,
    /// `w:type="even"` — only used when
    /// `DocumentSettings.even_and_odd_headers` is `true` and the page
    /// number is even.
    Even,
}

/// Per-role header / footer references. The reader fills the slots based
/// on each `<w:headerReference w:type="…" r:id="…"/>` in a section;
/// missing roles stay `None` and fall back to `Default` at paint time.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HeaderFooterRefs {
    pub default: Option<String>,
    pub first: Option<String>,
    pub even: Option<String>,
}

impl HeaderFooterRefs {
    pub fn is_empty(&self) -> bool {
        self.default.is_none() && self.first.is_none() && self.even.is_none()
    }

    /// Set the slot for `role`; replaces any existing value.
    pub fn set(&mut self, role: HeaderFooterRole, rid: String) {
        match role {
            HeaderFooterRole::Default => self.default = Some(rid),
            HeaderFooterRole::First => self.first = Some(rid),
            HeaderFooterRole::Even => self.even = Some(rid),
        }
    }

    /// Look up the rId for `role`, falling back to `Default` if the
    /// requested role is unset (OOXML §17.10.3 — the default header
    /// stands in for any unset variant). Returns `None` when even the
    /// default is missing.
    pub fn resolve(&self, role: HeaderFooterRole) -> Option<&str> {
        let primary = match role {
            HeaderFooterRole::Default => self.default.as_deref(),
            HeaderFooterRole::First => self.first.as_deref(),
            HeaderFooterRole::Even => self.even.as_deref(),
        };
        primary.or(self.default.as_deref())
    }
}

/// Audit gap A.H2 — `<w:sectPr><w:cols/>` descriptor. Holds the column
/// count and inter-column gutter for a section; equal-width snake flow
/// is the only supported layout this sprint (uneven `<w:col>` child
/// widths fall back to equal partitioning). Gutter is layout pixels
/// converted from twips at parse time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColumnSpec {
    pub count: u8,
    pub gutter_pt: f32,
}

impl ColumnSpec {
    /// Word's stock single-column body — `<w:cols>` absent or
    /// `w:num="1"`; gutter is irrelevant when `count == 1`.
    pub const fn single() -> Self {
        Self {
            count: 1,
            gutter_pt: 0.0,
        }
    }

    /// Build from raw twips. `<w:cols w:space>` defaults to 720 twips
    /// (½ inch / 36 pt) per OOXML when absent; callers pass the parsed
    /// value through unchanged. `num == 0` collapses to single column
    /// — defensive against malformed files.
    pub fn from_twips(num: u8, space_twips: i32) -> Self {
        Self {
            count: num.max(1),
            gutter_pt: (space_twips as f32) / 20.0,
        }
    }

    pub fn is_multi(self) -> bool {
        self.count > 1
    }
}

impl Default for ColumnSpec {
    fn default() -> Self {
        Self::single()
    }
}

/// One OOXML `<w:sectPr>` worth of state. A section spans a contiguous
/// half-open block range `[start, end)`; the page geometry is applied to
/// every page the paginator emits while flowing those blocks. The
/// reference structs carry the relationship ids the reader captured —
/// the header / footer XML parts live in the archive's `other_entries`
/// for the passthrough writer.
#[derive(Debug, Clone, Default)]
pub struct Section {
    pub geometry: PageGeometry,
    /// First top-level block (inclusive) covered by this section.
    pub start_block: u32,
    /// One past the last top-level block (exclusive).
    pub end_block: u32,
    /// `<w:headerReference>` table, keyed by `w:type`.
    pub header_refs: HeaderFooterRefs,
    /// `<w:footerReference>` table, keyed by `w:type`.
    pub footer_refs: HeaderFooterRefs,
    /// `<w:titlePg/>` — when `true`, the first page of this section uses
    /// the `First` header / footer slot instead of `Default`.
    pub title_pg: bool,
    /// Audit gap A.H2 — `<w:cols>` descriptor. `Default` is the implicit
    /// single-column body; multi-column sections snake-flow inside the
    /// section's page geometry.
    pub columns: ColumnSpec,
    /// Audit gap A.M11 — `<w:pgNumType>` descriptor. Controls section-
    /// relative `PAGE` field rendering (start value + number format).
    /// `Default` keeps the doc-wide absolute page count.
    pub page_num: PageNumType,
    /// Audit gap A.M12 — `<w:sectPr><w:type w:val>`. Default `NextPage`
    /// forces a page break at section start; `Continuous` flows the
    /// new section directly below the previous one on the SAME page.
    /// `EvenPage` / `OddPage` round-trip but degrade to `NextPage`
    /// (parity routing is paginator work deferred to a later sprint).
    pub section_type: SectionType,
}

/// Audit gap A.M11 — `<w:pgNumType>` descriptor.
///
/// `start: Some(n)` restarts the section's page numbering at `n`; the
/// paginator's PAGE-field evaluator uses `(current_doc_page -
/// section_first_page + n)` instead of the absolute count. `None`
/// keeps doc-wide numbering. `format` picks the glyph set: decimal,
/// lower/upper roman, lower/upper letter — anything else falls back
/// to decimal so an unrecognised `w:fmt` doesn't crash the paginator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PageNumType {
    pub start: Option<u32>,
    pub format: PageNumFormat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PageNumFormat {
    #[default]
    Decimal,
    LowerRoman,
    UpperRoman,
    LowerLetter,
    UpperLetter,
}

impl PageNumFormat {
    /// Audit gap A.M11 — render a 1-based page number under this
    /// format. Roman conversion clamps to the 1..=3999 range (above
    /// that the classical Roman system has no glyphs — beyond Word's
    /// supported range too). Letter format cycles A-Z / AA-ZZ / ...
    pub fn render(self, n: u32) -> String {
        match self {
            PageNumFormat::Decimal => n.to_string(),
            PageNumFormat::LowerRoman => to_roman(n).to_lowercase(),
            PageNumFormat::UpperRoman => to_roman(n),
            PageNumFormat::LowerLetter => to_letter(n, false),
            PageNumFormat::UpperLetter => to_letter(n, true),
        }
    }
}

fn to_roman(mut n: u32) -> String {
    if n == 0 || n > 3999 {
        return n.to_string();
    }
    let table: &[(u32, &str)] = &[
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];
    let mut out = String::new();
    for &(v, s) in table {
        while n >= v {
            out.push_str(s);
            n -= v;
        }
    }
    out
}

fn to_letter(n: u32, upper: bool) -> String {
    if n == 0 {
        return n.to_string();
    }
    let base = if upper { b'A' } else { b'a' };
    /* Word's `lowerLetter` / `upperLetter`: 1..=26 → A..Z; 27..=52 →
    AA..ZZ (NOT base-26 — letters REPEAT). Match that quirk. */
    let count = ((n - 1) / 26) + 1;
    let letter = base + ((n - 1) % 26) as u8;
    let mut out = String::with_capacity(count as usize);
    for _ in 0..count {
        out.push(letter as char);
    }
    out
}

/// Audit gap A.M12 — `<w:sectPr><w:type>` discriminator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SectionType {
    /// Section starts on a fresh page (the default when `<w:type>` is
    /// absent).
    #[default]
    NextPage,
    /// New section flows in-line on the same page — the paginator does
    /// NOT flush before swapping geometry. Used for layouts like
    /// "1-column title, then 2-column body on the same page".
    Continuous,
    /// Round-trips through reader / writer but degrades to `NextPage`
    /// in the paginator until parity-aware page routing lands.
    EvenPage,
    OddPage,
}

impl Section {
    /// Width of one column in this section's page geometry, in layout
    /// pixels. For a single-column section that's just `content_width`.
    pub fn column_width_pt(&self) -> f32 {
        let cw = self.geometry.content_width();
        let n = self.columns.count.max(1) as f32;
        if n <= 1.0 {
            return cw;
        }
        let gutters = (n - 1.0) * self.columns.gutter_pt;
        ((cw - gutters) / n).max(0.0)
    }

    /// Distance (in layout pixels) from the section's content-area
    /// leading edge to the leading edge of column `idx`.
    pub fn column_x_offset_pt(&self, idx: u8) -> f32 {
        let cw = self.column_width_pt();
        (idx.min(self.columns.count.saturating_sub(1)) as f32) * (cw + self.columns.gutter_pt)
    }
}

/// Document-wide flags pulled from `word/settings.xml`. Phase 2 — only
/// the header/footer parity toggle is modelled; later phases grow the
/// struct as more setting elements get typed support.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DocumentSettings {
    /// `<w:evenAndOddHeaders/>` — when `true`, even-numbered pages render
    /// the `Even` header / footer instead of the `Default` slot.
    pub even_and_odd_headers: bool,
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

/// A selectable font family (Backlog #9 / core-engine issue #23). `engine-wasm`
/// resolves it to a loaded font face when building layout style spans; the pure
/// document model just stores the choice.
///
/// The three named variants are the engine's seed faces and keep their
/// canonical asymmetric id ↔ display mappings ("liberation" ↔ "Liberation
/// Sans"). [`Custom`](FontFamily::Custom) is the dynamic, string-backed slot:
/// `id` is the FontStack resolution id + toolbar id (e.g. `"cairo"`), `display`
/// is the verbatim `.docx`/CSS family name. Carrying both makes a document
/// round-trip byte-identically while still letting the layout engine resolve
/// the loaded face. `Custom` holds owned `String`s, so the enum is `Clone` but
/// **not** `Copy` — pass it by reference.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum FontFamily {
    Amiri,
    LiberationSans,
    NotoNaskhArabic,
    Custom { id: String, display: String },
}

impl FontFamily {
    /// The FontStack resolution id + toolbar id. Named faces keep their
    /// canonical lowercase-hyphenated id; a [`Custom`](FontFamily::Custom)
    /// face returns its stored id verbatim.
    pub fn id(&self) -> &str {
        match self {
            FontFamily::Amiri => "amiri",
            FontFamily::LiberationSans => "liberation",
            FontFamily::NotoNaskhArabic => "noto-naskh",
            FontFamily::Custom { id, .. } => id,
        }
    }

    /// Human-facing family name for `.docx` `<w:rFonts>` and CSS
    /// `font-family`. A custom face returns the verbatim display string so a
    /// document round-trips byte-identically.
    pub fn display_name(&self) -> &str {
        match self {
            FontFamily::Amiri => "Amiri",
            FontFamily::LiberationSans => "Liberation Sans",
            FontFamily::NotoNaskhArabic => "Noto Naskh Arabic",
            FontFamily::Custom { display, .. } => display,
        }
    }

    /// Parse a toolbar / resolution id (e.g. `"amiri"`, `"cairo"`) into a
    /// family. Unknown ids become a [`Custom`](FontFamily::Custom) face whose
    /// display name is humanized from the id. Empty input yields `None`.
    pub fn from_id(id: &str) -> Option<FontFamily> {
        let id = id.trim();
        if id.is_empty() {
            return None;
        }
        Some(match id.to_ascii_lowercase().as_str() {
            "amiri" => FontFamily::Amiri,
            "liberation" => FontFamily::LiberationSans,
            "noto-naskh" => FontFamily::NotoNaskhArabic,
            _ => FontFamily::Custom {
                id: id.to_string(),
                display: humanize_font_id(id),
            },
        })
    }

    /// Parse a display / `.docx` / CSS family name (e.g. `"Amiri"`,
    /// `"Liberation Sans"`, `"Cairo"`) into a family. Unknown names become a
    /// [`Custom`](FontFamily::Custom) face that preserves the **verbatim**
    /// display string and derives a resolution id by slugifying it. Empty
    /// input yields `None`.
    ///
    /// The trimmed, unquoted form is used **only** to match a seed face and to
    /// derive the Custom id — the stored `display` is the caller's untouched
    /// input. This is load-bearing for `.docx` byte-identity: the docx reader
    /// (`format-docx`'s `family_from_docx`) passes the raw `<w:rFonts>`
    /// attribute value, which must round-trip byte-for-byte (surrounding
    /// whitespace and any literal quote characters included). The CSS parser,
    /// where quoting/padding are syntax rather than data, pre-cleans the token
    /// before calling (see `engine::html`'s `family_from_name`).
    pub fn from_display_name(name: &str) -> Option<FontFamily> {
        let key = name.trim().trim_matches(['"', '\'']).trim();
        if key.is_empty() {
            return None;
        }
        Some(match key.to_ascii_lowercase().as_str() {
            "amiri" => FontFamily::Amiri,
            "liberation sans" | "liberation" => FontFamily::LiberationSans,
            "noto naskh arabic" | "noto-naskh" => FontFamily::NotoNaskhArabic,
            _ => FontFamily::Custom {
                id: slugify_font_name(key),
                display: name.to_string(),
            },
        })
    }
}

/// Humanize a font id into a display name: split on `-`, upper-case the first
/// letter of each word. `"cairo"` → `"Cairo"`, `"noto-naskh-arabic"` →
/// `"Noto Naskh Arabic"`.
fn humanize_font_id(id: &str) -> String {
    id.split('-')
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Slugify a display name into a resolution id: lowercase, collapse whitespace
/// runs to a single `-`. `"Cairo"` → `"cairo"`, `"Times New Roman"` →
/// `"times-new-roman"`.
fn slugify_font_name(name: &str) -> String {
    name.trim()
        .to_ascii_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("-")
}

/// Underline decoration style — OOXML `<w:u w:val="…"/>` variants the
/// engine carries through layout + render. `Single` matches the legacy
/// boolean-true behaviour; `None` matches boolean-false. The renderer
/// approximates `Dotted` / `Dashed` / `Wavy` with patterned fill rects
/// (Canvas2D backend has no native dash array on the underline path).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum UnderlineStyle {
    #[default]
    None,
    Single,
    Double,
    Dotted,
    Dashed,
    Wavy,
}

impl UnderlineStyle {
    /// `true` when the variant should paint any stroke at all. `None`
    /// is the only variant that suppresses painting.
    pub fn is_visible(self) -> bool {
        !matches!(self, UnderlineStyle::None)
    }
}

/// Audit gap A.M1 — `<w:vertAlign>` super/subscript positioning. The
/// renderer shrinks the run's font and shifts the baseline up
/// (`Superscript`) or down (`Subscript`); `Baseline` is the implicit
/// default and a no-op.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum VertAlign {
    #[default]
    Baseline,
    Superscript,
    Subscript,
}

impl VertAlign {
    /// `true` when the variant alters baseline / font size at paint time.
    pub fn is_shifted(self) -> bool {
        !matches!(self, VertAlign::Baseline)
    }
}

/// Inline style for a run of characters: font size, colour, the
/// bold / italic / underline / strikethrough flags, a background (highlight)
/// colour, and a font family. All are carried through layout and render.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SpanStyle {
    pub font_size: Option<f32>,
    pub color: Option<[u8; 4]>,
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub underline: Option<UnderlineStyle>,
    pub strike: Option<bool>,
    pub bg_color: Option<[u8; 4]>,
    pub font_family: Option<FontFamily>,
    /// `<w:caps/>` — display every character of the run as its uppercase
    /// equivalent. Applied as a `to_uppercase` transform at shape time so
    /// glyph metrics + BiDi + line breaking all see the visible string.
    /// `caps` wins over `small_caps` when both are set (OOXML §17.3.2.7).
    pub caps: Option<bool>,
    /// `<w:smallCaps/>` — display lowercase characters as reduced-height
    /// uppercase glyphs while leaving originally-uppercase characters at
    /// full size. The engine's best-effort approximation uppercases the
    /// originally-lowercase substrings and shrinks their font_size to
    /// ~80% of the run's nominal size.
    pub small_caps: Option<bool>,
    /// Audit gap A.M1 — `<w:vertAlign w:val="superscript|subscript"/>`.
    /// `None` ⇒ baseline (the no-op default); explicit `Some(Baseline)`
    /// is preserved so a run-style override can defeat an inherited
    /// super/subscript from the style cascade.
    pub vert_align: Option<VertAlign>,
    /// Audit gap A.M2 — verbatim font family name from `<w:rFonts w:ascii>`
    /// when the engine cannot resolve it to a loaded face. Round-tripped
    /// back into the writer so a save preserves the author's
    /// "Cambria" / "Times New Roman" / etc. even though we render with
    /// the fallback. `None` when the engine successfully resolves the
    /// name into [`font_family`].
    pub raw_font_family: Option<String>,
    /// Audit gap A.M2 — `<w:rFonts w:asciiTheme="…"/>` (and the
    /// per-script `hAnsiTheme` / `cstheme`). Round-tripped verbatim. Lost
    /// theme bindings break Word's "Update Style" — preserve at all
    /// costs even though our font picker ignores them.
    pub font_theme: Option<String>,
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
            caps: patch.caps.or(self.caps),
            small_caps: patch.small_caps.or(self.small_caps),
            vert_align: patch.vert_align.or(self.vert_align),
            raw_font_family: patch.raw_font_family.or(self.raw_font_family),
            font_theme: patch.font_theme.or(self.font_theme),
        }
    }
}

/// A styled byte range `[start, end)` within a paragraph.
#[derive(Debug, Clone, PartialEq)]
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

/// Phase 2 audit (gap D.1) — complex field overlay on a paragraph byte
/// range. The cached display text lives in the paragraph's `text` field
/// at `[start, end)`; `instruction` is the OOXML field code (`PAGE`,
/// `NUMPAGES`, `DATE`, `TIME`, etc.) lifted verbatim from the
/// `<w:instrText>` element(s) between the field's `begin` and
/// `separate` fldChars.
///
/// Evaluation lives at paginate / paint time, not parse time: a `PAGE`
/// field's actual page number is not knowable until the containing
/// paragraph has been placed on a page. The reader preserves whatever
/// cached value the source `.docx` shipped (Word stamps the
/// last-rendered value as the cached text); the paginator overrides it
/// with the live value before flushing each page via [`Field::evaluate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    /// Byte offset where the cached display text starts (inclusive).
    pub start: u32,
    /// One past the end of the cached display text.
    pub end: u32,
    /// Field code — the unparsed `<w:instrText>` content. Trimmed of
    /// surrounding whitespace; switches like `\* MERGEFORMAT` are
    /// preserved (the evaluator parses the leading keyword).
    pub instruction: String,
}

impl Field {
    /// Extract the leading keyword from `instruction` — the part Word
    /// uses to dispatch field types. `"PAGE \* MERGEFORMAT"` → `"PAGE"`;
    /// `"DATE"` → `"DATE"`. Returns an uppercase owned `String` so
    /// callers can `match` on it without re-allocating.
    pub fn keyword(&self) -> String {
        self.instruction
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_ascii_uppercase()
    }

    /// Compute the live display string for this field given the page
    /// context. Returns `None` for instructions the engine does not
    /// evaluate (the renderer keeps the cached text in that case).
    /// `current_page` and `total_pages` are 1-based.
    pub fn evaluate(&self, current_page: u32, total_pages: u32) -> Option<String> {
        match self.keyword().as_str() {
            "PAGE" => Some(current_page.to_string()),
            "NUMPAGES" => Some(total_pages.to_string()),
            _ => None,
        }
    }
}

/// Phase 8b — kind of tracked-change revision.
///
/// - `Insert` — `<w:ins>` wraps text that a reviewer added.
/// - `Delete` — `<w:del>` wraps text that the original document carried
///   but a reviewer marked for removal. The deleted text rides in the
///   paragraph's `text` field alongside live content; the renderer
///   applies markup styling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevisionKind {
    Insert,
    Delete,
    /// Sprint 14 (#14) — `<w:rPrChange>` tracked formatting change.
    /// The original `SpanStyle` (pre-mutation) lives on
    /// [`Revision::prev_attrs`] so accept/reject can restore it.
    /// Payload-free here to keep `RevisionKind: Copy`, which a dozen
    /// existing match sites rely on.
    FormatChange,
}

/// Phase 8b — one `<w:ins>` / `<w:del>` / `<w:rPrChange>` overlay on a
/// paragraph's byte range. `author` + `date` carry the OOXML
/// `w:author` / `w:date` attributes so the TS shell can surface them
/// on hover. `id` carries the `w:id` attribute Word's accept/reject
/// UI uses to address an individual change; `None` for revisions the
/// engine synthesised (writer assigns a fresh sequential id at
/// emission time) or for source files that omit the attribute.
#[derive(Debug, Clone, PartialEq)]
pub struct Revision {
    pub start: u32,
    pub end: u32,
    pub kind: RevisionKind,
    pub author: String,
    pub date: String,
    pub id: Option<u32>,
    /// Sprint 14 (#14) — pre-mutation `SpanStyle` snapshot for a
    /// `RevisionKind::FormatChange` so reject can restore the
    /// original look. `None` for `Insert` / `Delete` revisions where
    /// the attribute is irrelevant.
    pub prev_attrs: Option<SpanStyle>,
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
/// Audit gap A.M3 — one `<w:pPr><w:tabs><w:tab/>` entry. `position`
/// is layout pt at scale=1 (twip → pt at parse); `kind` controls the
/// alignment of content at the stop. Phase-5 line builder honours
/// `Left` precisely; `Center` / `Right` / `Decimal` round-trip
/// faithfully on the writer but render as `Left` for now (proper
/// alignment requires a measure-then-place pass deferred to a later
/// sprint).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct TabStop {
    pub position_pt: f32,
    pub kind: TabKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TabKind {
    /// Tab cursor jumps to `position_pt`; content lands right of it.
    /// Word's default and what the line builder honours.
    #[default]
    Left,
    Center,
    Right,
    Decimal,
    /// `<w:clear>` — explicit "no tab at this position", used to defeat
    /// an inherited tab stop from the style cascade.
    Clear,
}

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
    /// Audit gap A.M4 — `<w:pPr><w:pBdr>` border strokes painted around
    /// the paragraph bounding rectangle. Mirror of the table-cell
    /// border model (top/left/bottom/right edges plus the unused
    /// inside_h/inside_v slots `CellBorders` ships with). `None` ⇒ no
    /// border (the implicit default). Renderer reuses the cell-border
    /// drawing primitive at paragraph-rect bounds.
    pub borders: Option<CellBorders>,
    /// Audit gap A.M3 — `<w:pPr><w:tabs>` custom tab stops in
    /// document order. Empty list ⇒ fall back to the 0.5-inch default
    /// grid the line builder uses. Position is layout pt at scale=1
    /// (1 twip = 1/20 pt; reader converts at parse time).
    pub tab_stops: Vec<TabStop>,
    /// Audit gap A.M17 — `<w:pPr><w:numPr>` numbering binding inherited
    /// via the pStyle chain. Carries the resolved (num_id, ilvl) when
    /// the paragraph's style cascade specifies a list binding. The
    /// document parser folds this into `Paragraph.list_item` when no
    /// direct `<w:pPr><w:numPr>` appears on the paragraph itself.
    pub list_item: Option<ListItem>,
    /// Sprint 6 (UI Edition) — `<w:pPr><w:shd w:fill>` paragraph
    /// background fill. `None` ⇒ transparent (the implicit default).
    /// Mirror of the cell-shading model; the renderer paints a filled
    /// rect at the paragraph's bounding rectangle before drawing the
    /// `<w:pBdr>` strokes.
    pub shading: Option<[u8; 4]>,
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
            shading: patch.shading.or(self.shading),
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
            /* Audit gap A.M4 — `<w:pBdr>` overlay: patch's borders win
            when set; otherwise inherit. */
            borders: patch.borders.or(self.borders),
            /* Audit gap A.M3 — `<w:tabs>` overlay: patch's stops
            REPLACE the parent's (Word's documented behaviour — child
            `<w:tabs>` is not additive, it shadows the cascade).
            Empty patch inherits. */
            tab_stops: if patch.tab_stops.is_empty() {
                self.tab_stops
            } else {
                patch.tab_stops
            },
            /* Audit gap A.M17 — list binding cascades: patch wins
            when set, otherwise inherit. */
            list_item: patch.list_item.or(self.list_item),
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
    /// Issue #50 — the numbering level's `<w:ind>` (`LvlDef.indent`),
    /// stamped by the marker resolver alongside [`Self::resolved_marker`].
    /// Transient render geometry: the layout boundary falls back to it when
    /// the paragraph carries no direct indent, so interactively-toggled
    /// lists indent without mutating `props`. The `.docx` writer never
    /// serializes it — level indents already live in `numbering.xml`.
    pub resolved_list_indent: Option<Indent>,
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
    /// Phase 8b — tracked-change overlays. Each `<w:ins>` / `<w:del>`
    /// in the source paragraph produces one entry. The paragraph's
    /// `text` retains deleted text alongside live content so the
    /// renderer can show markup; the passthrough writer round-trips
    /// the wrappers byte-identical from `source_xml`.
    pub revisions: Vec<Revision>,
    /// Phase 2 audit (gap D.1) — complex-field overlays. Each
    /// `<w:fldChar fldCharType="begin">`/`separate`/`end` triplet the
    /// reader sees produces one entry covering the cached display
    /// text's byte range. The paginator overrides the rendered string
    /// at flush time for `PAGE` / `NUMPAGES`; other instructions
    /// (`DATE`, `TIME`, …) render their cached value.
    pub fields: Vec<Field>,
    /// Sprint 12 (#11) — `<w:pPr><w:pStyle w:val>` paragraph-style id.
    /// `Some` when the paragraph references an entry in
    /// `DocumentTree.styles`; the cascade walker
    /// (`DocumentTree::resolve_style_cascade`) folds the style's
    /// properties into the bottom of the resolved `props`, with
    /// [`Self::direct_overrides`] layered on top.
    pub style_id: Option<String>,
    /// Sprint 12 (#11) — shadow holding ONLY fields the user
    /// explicitly set on this paragraph (or a `<w:pPr>` that the
    /// reader saw directly on the `<w:p>` element). Resolved `props`
    /// = `style_cascade(style_id) ∪ direct_overrides`. On a style
    /// change, `direct_overrides` is preserved verbatim — that is the
    /// whole point of the shadow approach (a user's manual bold
    /// survives a style switch).
    pub direct_overrides: ParaProperties,
}

impl Paragraph {
    /// Resolved style at byte offset `at` (default if no span covers it).
    pub fn style_at(&self, at: u32) -> SpanStyle {
        self.spans
            .iter()
            .find(|s| at >= s.start && at < s.end)
            .map_or(SpanStyle::default(), |s| s.style.clone())
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
                style = style.merged_with(patch.clone());
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
            resolved_list_indent: self.resolved_list_indent,
            dirty: true,
            source_xml: None,
            inline_objects: Vec::new(),
            hyperlinks: Vec::new(),
            revisions: Vec::new(),
            fields: Vec::new(),
            style_id: None,
            direct_overrides: ParaProperties::default(),
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
                    style: run.style.clone(),
                });
            }
        }
        Paragraph {
            text,
            spans,
            props: self.props.clone(),
            list_item: self.list_item,
            resolved_marker: self.resolved_marker.clone(),
            resolved_list_indent: self.resolved_list_indent,
            dirty: true,
            source_xml: None,
            inline_objects: Vec::new(),
            hyperlinks: Vec::new(),
            revisions: Vec::new(),
            fields: Vec::new(),
            style_id: None,
            direct_overrides: ParaProperties::default(),
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
                    style: run.style.clone(),
                });
            }
            if run.end > at {
                right.push(StyleRun {
                    start: run.start.max(at) - at,
                    end: run.end - at,
                    style: run.style.clone(),
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
                resolved_list_indent: self.resolved_list_indent,
                dirty: true,
                source_xml: None,
                inline_objects: Vec::new(),
                hyperlinks: Vec::new(),
                revisions: Vec::new(),
                fields: Vec::new(),
                style_id: None,
                direct_overrides: ParaProperties::default(),
            },
            Paragraph {
                text: self.text[at as usize..].to_owned(),
                spans: right,
                props: self.props.clone(),
                list_item: self.list_item,
                resolved_marker: self.resolved_marker.clone(),
                resolved_list_indent: self.resolved_list_indent,
                dirty: true,
                source_xml: None,
                inline_objects: Vec::new(),
                hyperlinks: Vec::new(),
                revisions: Vec::new(),
                fields: Vec::new(),
                style_id: None,
                direct_overrides: ParaProperties::default(),
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
                style: run.style.clone(),
            });
        }
        Paragraph {
            text,
            spans,
            props: self.props.clone(),
            list_item: self.list_item,
            resolved_marker: self.resolved_marker.clone(),
            resolved_list_indent: self.resolved_list_indent,
            dirty: true,
            source_xml: None,
            inline_objects: Vec::new(),
            hyperlinks: Vec::new(),
            revisions: Vec::new(),
            fields: Vec::new(),
            style_id: None,
            direct_overrides: ParaProperties::default(),
        }
    }

    /// Byte offset of the UAX-#29 extended grapheme cluster boundary
    /// immediately before `o` (clamped to 0). Audit gap B.H1 — stepping
    /// by Unicode scalar (`char`) bisects Arabic harakat, Devanagari
    /// conjuncts, emoji ZWJ sequences; grapheme stepping keeps each
    /// user-perceived character atomic so Backspace removes a whole
    /// cluster instead of leaving an orphaned combining mark.
    pub fn prev_offset(&self, o: u32) -> u32 {
        use unicode_segmentation::UnicodeSegmentation;
        let o = (o as usize).min(self.text.len());
        self.text[..o]
            .grapheme_indices(true)
            .next_back()
            .map_or(0, |(i, _)| i as u32)
    }

    /// Byte offset of the UAX-#29 extended grapheme cluster boundary
    /// immediately after `o` (clamped to len). See [`Self::prev_offset`]
    /// for the symmetric rationale.
    pub fn next_offset(&self, o: u32) -> u32 {
        use unicode_segmentation::UnicodeSegmentation;
        let o = (o as usize).min(self.text.len());
        self.text[o..]
            .grapheme_indices(true)
            .nth(1)
            .map_or(self.text.len() as u32, |(rel_i, _)| (o + rel_i) as u32)
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
#[derive(Debug, Clone, PartialEq, Default)]
pub struct BorderStroke {
    pub style: BorderStyle,
    pub size_eighth_pt: u16,
    pub color: Option<[u8; 4]>,
}

/// Per-edge border strokes for a `<w:tcBorders>` or `<w:tblBorders>`.
/// `inside_h` / `inside_v` only apply when carried at the table level
/// (`<w:tblBorders>`); cell-level borders ignore them.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CellBorders {
    pub top: Option<BorderStroke>,
    pub left: Option<BorderStroke>,
    pub bottom: Option<BorderStroke>,
    pub right: Option<BorderStroke>,
    pub inside_h: Option<BorderStroke>,
    pub inside_v: Option<BorderStroke>,
}

/// `<w:tblCellMar>` (table default) or `<w:tcMar>` (per-cell override)
/// cell padding. Per-edge `Option<i32>` because OOXML lets each edge
/// override independently — a `<w:tcMar>` carrying only `<w:left>` and
/// `<w:right>` inherits top/bottom from the table default, which itself
/// can also leave edges unset. The layout solver collapses the
/// inherit chain via [`CellMargins::resolve_edges`] →
/// [`ResolvedCellMargins`] (every edge populated with Word stock as
/// the final fallback).
///
/// `default()` is all-`None` — meaning every edge inherits.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CellMargins {
    pub top_twips: Option<i32>,
    pub left_twips: Option<i32>,
    pub bottom_twips: Option<i32>,
    pub right_twips: Option<i32>,
}

/// Fully-resolved per-edge padding the layout solver consumes. Every
/// edge is populated; the [`CellMargins::resolve_edges`] resolver
/// walks cell override → table default → Word stock for each edge
/// independently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedCellMargins {
    pub top_twips: i32,
    pub left_twips: i32,
    pub bottom_twips: i32,
    pub right_twips: i32,
}

impl CellMargins {
    /// Word's stock cell padding (0 / 108 / 0 / 108 twips). Used by the
    /// layout solver when neither `<w:tcMar>` nor `<w:tblCellMar>`
    /// specify an explicit value — matches what `winword.exe` emits on
    /// a freshly-inserted table.
    pub const fn word_default() -> ResolvedCellMargins {
        ResolvedCellMargins {
            top_twips: 0,
            left_twips: 108,
            bottom_twips: 0,
            right_twips: 108,
        }
    }

    /// Per-edge resolver — for each edge, return the cell override if
    /// set, otherwise the table default, otherwise Word's stock value.
    /// Crucially, each edge is resolved INDEPENDENTLY: a cell
    /// `<w:tcMar>` setting only `<w:left>` and `<w:right>` correctly
    /// inherits top/bottom from the table default (or Word stock if
    /// the table also leaves them unset).
    pub fn resolve_edges(cell: Option<&Self>, table: &Self) -> ResolvedCellMargins {
        let stock = Self::word_default();
        let pick = |c: Option<i32>, t: Option<i32>, s: i32| -> i32 { c.or(t).unwrap_or(s) };
        ResolvedCellMargins {
            top_twips: pick(
                cell.and_then(|c| c.top_twips),
                table.top_twips,
                stock.top_twips,
            ),
            left_twips: pick(
                cell.and_then(|c| c.left_twips),
                table.left_twips,
                stock.left_twips,
            ),
            bottom_twips: pick(
                cell.and_then(|c| c.bottom_twips),
                table.bottom_twips,
                stock.bottom_twips,
            ),
            right_twips: pick(
                cell.and_then(|c| c.right_twips),
                table.right_twips,
                stock.right_twips,
            ),
        }
    }
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
    /// Phase 2 audit (gap B.1) — `<w:tcMar>` per-cell padding override.
    /// `None` ⇒ inherit from the table's `<w:tblCellMar>`; an explicit
    /// `Some` value wins per-edge as resolved by
    /// [`CellMargins::resolve_edges`].
    pub cell_margins: Option<CellMargins>,
}

/// Audit gap A.M8 — `<w:tblLayout w:type>`. `Autofit` (Word's
/// default) measures cell content and distributes column widths to
/// fit the available band; `Fixed` honours `<w:tblGrid>` verbatim
/// regardless of content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TableLayout {
    #[default]
    Autofit,
    Fixed,
}

#[derive(Debug, Clone, Default)]
pub struct TableProperties {
    pub width: Option<CellWidth>,
    pub alignment: Option<Alignment>,
    pub indent_twips: i32,
    pub borders: Option<CellBorders>,
    pub cell_margins: CellMargins,
    pub table_style_id: Option<String>,
    /// Audit gap A.M8 — `<w:tblLayout w:type="autofit|fixed"/>`.
    /// Default `Autofit` matches Word's behaviour when the element
    /// is absent.
    pub layout: TableLayout,
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
            settings: DocumentSettings::default(),
            styles: std::collections::HashMap::new(),
            style_defaults: ParaProperties::default(),
            style_run_defaults: SpanStyle::default(),
            styles_dirty: false,
            numbering: numbering::NumberingDefinitions::default(),
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
            resolved_list_indent: None,
            dirty: false,
            source_xml: None,
            inline_objects: Vec::new(),
            hyperlinks: Vec::new(),
            revisions: Vec::new(),
            fields: Vec::new(),
            style_id: None,
            direct_overrides: ParaProperties::default(),
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
            settings: DocumentSettings::default(),
            styles: std::collections::HashMap::new(),
            style_defaults: ParaProperties::default(),
            style_run_defaults: SpanStyle::default(),
            styles_dirty: false,
            numbering: numbering::NumberingDefinitions::default(),
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
                resolved_list_indent: None,
                dirty: false,
                source_xml: None,
                inline_objects: Vec::new(),
                hyperlinks: Vec::new(),
                revisions: Vec::new(),
                fields: Vec::new(),
                style_id: None,
                direct_overrides: ParaProperties::default(),
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
            settings: DocumentSettings::default(),
            styles: std::collections::HashMap::new(),
            style_defaults: ParaProperties::default(),
            style_run_defaults: SpanStyle::default(),
            styles_dirty: false,
            numbering: numbering::NumberingDefinitions::default(),
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
            settings: DocumentSettings::default(),
            styles: std::collections::HashMap::new(),
            style_defaults: ParaProperties::default(),
            style_run_defaults: SpanStyle::default(),
            styles_dirty: false,
            numbering: numbering::NumberingDefinitions::default(),
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
            settings: DocumentSettings::default(),
            styles: std::collections::HashMap::new(),
            style_defaults: ParaProperties::default(),
            style_run_defaults: SpanStyle::default(),
            styles_dirty: false,
            numbering: numbering::NumberingDefinitions::default(),
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
            settings: DocumentSettings::default(),
            styles: std::collections::HashMap::new(),
            style_defaults: ParaProperties::default(),
            style_run_defaults: SpanStyle::default(),
            styles_dirty: false,
            numbering: numbering::NumberingDefinitions::default(),
        }
    }

    /// Phase 6b — attach the parsed header / footer parts collected from
    /// `word/header*.xml` / `word/footer*.xml`, keyed by their relationship
    /// id (`r:id`). Consumed by the paginator when a section's
    /// `header_ref` / `footer_ref` resolves.
    pub fn with_header_footer_parts(
        mut self,
        headers: std::collections::HashMap<String, Vec<Paragraph>>,
        footers: std::collections::HashMap<String, Vec<Paragraph>>,
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
                header_refs: HeaderFooterRefs::default(),
                footer_refs: HeaderFooterRefs::default(),
                title_pg: false,
                columns: ColumnSpec::single(),
                page_num: PageNumType::default(),
                section_type: SectionType::default(),
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

    /// Sprint 10 — walk `path` and return a borrowed reference to the
    /// **innermost** `CellProperties` the caret is sitting inside, or
    /// `None` when the path never enters a table cell. Backs the
    /// `Event::SelectionChanged.cell_properties` field that drives
    /// `CellPropertiesDialog` prefill.
    pub fn innermost_cell_props_at(&self, path: &BlockPath) -> Option<&CellProperties> {
        let mut last: Option<&CellProperties> = None;
        let mut i = 0usize;
        let first = path.steps.first()?;
        let PathStep::Block(n) = first else {
            return None;
        };
        let mut current: &Block = self.blocks.get(*n as usize)?;
        i += 1;
        while i < path.steps.len() {
            let Block::Table(t) = current else {
                return last;
            };
            let PathStep::Cell { row, col } = path.steps[i] else {
                return last;
            };
            let cell = t.rows.get(row as usize)?.cells.get(col as usize)?;
            last = Some(&cell.props);
            i += 1;
            let Some(PathStep::Block(b_idx)) = path.steps.get(i) else {
                return last;
            };
            current = cell.blocks.get(*b_idx as usize)?;
            i += 1;
        }
        last
    }

    /// Sprint 10 — locate the `Section` covering top-level block
    /// `block_idx`. Falls back to a synthesised default-A4 section
    /// when the document carries no `<w:sectPr>` (the pre-Phase-6
    /// behaviour). The caller can read the returned section's
    /// geometry directly into the `SelectionChanged` event's
    /// `section_geometry` field.
    pub fn section_for_block(&self, block_idx: u32) -> Section {
        if let Some(s) = self
            .sections
            .iter()
            .find(|s| block_idx >= s.start_block && block_idx < s.end_block)
        {
            return s.clone();
        }
        Section {
            geometry: PageGeometry::a4(),
            start_block: 0,
            end_block: self.blocks.len() as u32,
            header_refs: HeaderFooterRefs::default(),
            footer_refs: HeaderFooterRefs::default(),
            title_pg: false,
            columns: ColumnSpec::single(),
            page_num: PageNumType::default(),
            section_type: SectionType::default(),
        }
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

    /// Sprint 8 (UI Edition) — total character count across every
    /// paragraph in the document, including paragraphs nested in
    /// table cells. Counts Unicode scalars (`char`s), not bytes —
    /// matches Word's "Characters (no spaces)" minus the no-space
    /// filter. Cheap O(n) walk.
    pub fn character_count(&self) -> u32 {
        let mut n = 0u32;
        walk_paragraphs(&self.blocks, &mut |p| {
            n = n.saturating_add(p.text.chars().count() as u32);
        });
        n
    }

    /// Sprint 11 (#17) — total word count via UAX-#29 word
    /// segmentation. Replaces the Sprint 8 whitespace-split
    /// fallback so CJK / Thai / Khmer (scripts without inter-word
    /// whitespace) report a meaningful count.
    ///
    /// `icu_segmenter::WordSegmenter::new_auto` shares its data
    /// tables with `text-pipeline`'s `LineSegmenter::new_auto`, so
    /// the wasm artifact does not grow beyond the icu data already
    /// linked for line breaking. We filter `WordType::Word` so
    /// punctuation and inter-word whitespace runs don't count as
    /// words.
    pub fn word_count(&self) -> u32 {
        let mut n = 0u32;
        walk_paragraphs(&self.blocks, &mut |p| {
            n = n.saturating_add(count_uax_words(&p.text) as u32);
        });
        n
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

    /// Sprint 9 — flatten the whole document to plain text.
    ///
    /// Paragraphs join with `\n`. Tables emit one tab-separated row per
    /// `TableRow` (cells joined with `\t`, in their visual order); the
    /// table itself sits on its own line, with a blank-line separator
    /// before and after. Inline objects (images, footnote refs) render
    /// as the placeholder marker `[image]` / `[footnote N]` so the
    /// caller never silently drops them.
    pub fn to_plain_text(&self) -> String {
        let mut out = String::new();
        for block in self.blocks.iter() {
            match block {
                Block::Paragraph(p) => {
                    push_paragraph_plain(p, &mut out);
                    out.push('\n');
                }
                Block::Table(t) => push_table_plain(t, &mut out),
            }
        }
        /* Drop the trailing newline so single-paragraph docs are not
        terminated by an empty line. */
        if out.ends_with('\n') {
            out.pop();
        }
        out
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
    /// Sprint 14 (#14) — track-changes-aware text insertion.
    ///
    /// Boundary math:
    /// - **Inside an existing Insert by same author** → existing
    ///   Insert grows via the offset shift; NO new revision added
    ///   (prevents per-keystroke fragmentation).
    /// - **Inside an existing Delete** → split the Delete around the
    ///   insertion point and stamp a fresh Insert in the gap (typing
    ///   inside a `<w:del>` logically replaces deleted text).
    /// - **Adjacent to an Insert by same author** (cursor at its
    ///   right edge) → extend the existing Insert end (merge
    ///   keystrokes).
    /// - Otherwise → add a fresh `Insert` revision over `[at,
    ///   at+len)`.
    pub fn tracked_insert_text(
        &self,
        at: LogicalPos,
        text: &str,
        author: String,
        date: String,
    ) -> Self {
        if text.is_empty() {
            return self.clone();
        }
        /* Empty doc: fall through to plain insert_text + stamp the
        Insert revision on paragraph 0. */
        let mut doc = self.insert_text(at.clone(), text);
        let len = text.len() as u32;
        /* Resolve the path the insert actually landed on (insert_text
        clamps to the document end when the original path is stale). */
        let target_path = if doc.paragraph_at_path(&at.path).is_some() {
            at.path
        } else {
            doc.path_to_last_top_paragraph()
                .unwrap_or(BlockPath::top(0))
        };
        let mut blocks = doc.blocks.clone();
        let off_input = at.offset;
        let _ = mutate_paragraph_in_top(&mut blocks, &target_path, |para| {
            /* `insert_text` clamps `at.offset` to `para.text.len()`
            BEFORE inserting; mirror that clamp so revision math
            uses the same byte position the insertion actually
            landed at. */
            let pre_text_len = (para.text.len() as u32).saturating_sub(len);
            let off = off_input.min(pre_text_len);

            /* Detect boundary state BEFORE shifting revisions so the
            classifier sees the pre-insert geometry. */
            let inside_insert_same_author = para.revisions.iter().any(|r| {
                r.kind == RevisionKind::Insert && r.start < off && off < r.end && r.author == author
            });
            let inside_delete = para
                .revisions
                .iter()
                .any(|r| r.kind == RevisionKind::Delete && r.start <= off && off < r.end);

            /* Shift trailing revisions by `len`. Mirrors the span-shift
            in `insert_text`: revisions starting at or after `off`
            slide right; revisions containing `off` grow (end +=
            len). Inline `objects` + `hyperlinks` shifts are deferred
            to a future sprint — Sprint 14 keeps the surface bounded. */
            for r in &mut para.revisions {
                if r.start >= off {
                    r.start += len;
                }
                if r.end > off {
                    r.end += len;
                }
            }

            /* If we landed inside a Delete, the shift above grew the
            Delete to span both halves. Split it back into the two
            halves around the new Insert. */
            if inside_delete {
                let mut split: Vec<Revision> = Vec::with_capacity(para.revisions.len() + 1);
                for r in para.revisions.drain(..) {
                    let was_split =
                        r.kind == RevisionKind::Delete && r.start <= off && off + len < r.end;
                    if !was_split {
                        split.push(r);
                        continue;
                    }
                    /* Left half [r.start, off) keeps Delete kind. */
                    if off > r.start {
                        split.push(Revision {
                            start: r.start,
                            end: off,
                            kind: RevisionKind::Delete,
                            author: r.author.clone(),
                            date: r.date.clone(),
                            id: None,
                            prev_attrs: None,
                        });
                    }
                    /* Right half [off + len, r.end) — note r.end was
                    already shifted by +len above, so it correctly
                    covers the post-insert remainder. */
                    if r.end > off + len {
                        split.push(Revision {
                            start: off + len,
                            end: r.end,
                            kind: RevisionKind::Delete,
                            author: r.author,
                            date: r.date,
                            id: None,
                            prev_attrs: None,
                        });
                    }
                }
                para.revisions = split;
            }

            /* Add (or merge-grow) the new Insert revision unless we're
            already inside an Insert by the same author (the offset-
            shift already extended its end). */
            if !inside_insert_same_author {
                let new_end = off + len;
                let merged = para
                    .revisions
                    .iter_mut()
                    .find(|r| r.kind == RevisionKind::Insert && r.end == off && r.author == author);
                if let Some(left) = merged {
                    left.end = new_end;
                    left.date = date.clone();
                } else {
                    para.revisions.push(Revision {
                        start: off,
                        end: new_end,
                        kind: RevisionKind::Insert,
                        author: author.clone(),
                        date: date.clone(),
                        id: None,
                        prev_attrs: None,
                    });
                }
            }
            para.dirty = true;
        });
        doc.blocks = blocks;
        doc
    }

    /// Sprint 14 (#14) — track-changes-aware delete.
    ///
    /// Boundary math:
    /// - **Range entirely inside a same-author Insert** → shrink the
    ///   Insert AND remove the text. Inserts never originated in the
    ///   source; deleting one's own pending insertion is a no-revision
    ///   undo of that pending edit.
    /// - **Range outside any Insert** → preserve the text, mark a
    ///   fresh `Delete` revision covering the range. Adjacent
    ///   same-author Delete gets merged.
    /// - Mixed cases (range straddles Insert + non-Insert) fall back
    ///   to the marker-only behaviour for v1 (text preserved, Delete
    ///   stamped over the whole range; the overlapped Insert remains).
    pub fn tracked_delete_range(
        &self,
        start: LogicalPos,
        end: LogicalPos,
        author: String,
        date: String,
    ) -> Self {
        let (start, end) = order_positions(start, end);
        if start == end || !same_parent(&start.path, &end.path) {
            return self.clone();
        }
        let Some(s_idx) = start.path.last_block_index() else {
            return self.clone();
        };
        let Some(e_idx) = end.path.last_block_index() else {
            return self.clone();
        };
        if s_idx != e_idx {
            /* Cross-paragraph tracked-delete falls back to the
            mark-only flow per-paragraph; v1 limitation. */
            return self.clone();
        }
        let target_path = start.path.clone();
        let s_off = start.offset;
        let e_off = end.offset;
        let mut blocks = self.blocks.clone();
        let _ = mutate_paragraph_in_top(&mut blocks, &target_path, |para| {
            /* Range entirely inside a same-author Insert? If so, undo
            the Insert (remove text + shrink the Insert overlay). */
            let owning_insert = para.revisions.iter().position(|r| {
                r.kind == RevisionKind::Insert
                    && r.author == author
                    && r.start <= s_off
                    && e_off <= r.end
            });
            if let Some(idx) = owning_insert {
                let s = (s_off as usize).min(para.text.len());
                let e = (e_off as usize).min(para.text.len());
                let removed_len = (e - s) as u32;
                if e > s {
                    para.text.replace_range(s..e, "");
                }
                /* Shrink the owning Insert by removed_len; shift
                trailing revisions left by removed_len. */
                let owning = &mut para.revisions[idx];
                owning.end -= removed_len;
                let owning_empty = owning.end <= owning.start;
                /* Now shift everything else after e_off. */
                for (i, r) in para.revisions.iter_mut().enumerate() {
                    if i == idx {
                        continue;
                    }
                    if r.start >= e_off {
                        r.start = r.start.saturating_sub(removed_len);
                    }
                    if r.end > e_off {
                        r.end = r.end.saturating_sub(removed_len);
                    }
                }
                if owning_empty {
                    para.revisions.remove(idx);
                }
                /* Shift spans + their byte-offset relatives. */
                for s in &mut para.spans {
                    if s.start >= e_off {
                        s.start = s.start.saturating_sub(removed_len);
                    }
                    if s.end > e_off {
                        s.end = s.end.saturating_sub(removed_len);
                    }
                }
                para.dirty = true;
                return;
            }
            /* Marker-only delete: stamp a fresh Delete over the range
            (text preserved). Merge with adjacent same-author Delete. */
            let new_end = e_off;
            let merged_left = para
                .revisions
                .iter_mut()
                .find(|r| r.kind == RevisionKind::Delete && r.end == s_off && r.author == author);
            if let Some(left) = merged_left {
                left.end = new_end;
                left.date = date.clone();
            } else {
                para.revisions.push(Revision {
                    start: s_off,
                    end: new_end,
                    kind: RevisionKind::Delete,
                    author: author.clone(),
                    date: date.clone(),
                    id: None,
                    prev_attrs: None,
                });
            }
            para.dirty = true;
        });
        Self {
            blocks,
            sections: self.sections.clone(),
            headers: self.headers.clone(),
            footers: self.footers.clone(),
            media: self.media.clone(),
            footnotes: self.footnotes.clone(),
            comment_defs: self.comment_defs.clone(),
            comment_ranges: self.comment_ranges.clone(),
            settings: self.settings.clone(),
            styles: self.styles.clone(),
            style_defaults: self.style_defaults.clone(),
            style_run_defaults: self.style_run_defaults.clone(),
            styles_dirty: self.styles_dirty,
            numbering: self.numbering.clone(),
        }
    }

    /// Sprint 14 (#14) — track-changes-aware format-change stamp.
    /// Records a `FormatChange` revision over the range carrying the
    /// pre-mutation `SpanStyle` snapshot (so reject can restore it).
    /// Caller still applies the formatting via the existing path —
    /// this helper only adds the overlay.
    pub fn tracked_format_change(
        &self,
        start: LogicalPos,
        end: LogicalPos,
        prev_attrs: SpanStyle,
        author: String,
        date: String,
    ) -> Self {
        let (start, end) = order_positions(start, end);
        if start == end || !same_parent(&start.path, &end.path) {
            return self.clone();
        }
        let Some(s_idx) = start.path.last_block_index() else {
            return self.clone();
        };
        let Some(e_idx) = end.path.last_block_index() else {
            return self.clone();
        };
        let parent = start.path.parent();
        let s_off = start.offset;
        let e_off = end.offset;
        let mut blocks = self.blocks.clone();
        let single_paragraph = s_idx == e_idx;
        for idx in s_idx..=e_idx {
            let child_path = parent.clone().push(PathStep::Block(idx));
            let author_local = author.clone();
            let date_local = date.clone();
            let prev_local = prev_attrs.clone();
            let _ = mutate_paragraph_in_top(&mut blocks, &child_path, |para| {
                let r_start = if single_paragraph { s_off } else { 0 };
                let r_end = if single_paragraph {
                    e_off
                } else {
                    para.text.len() as u32
                };
                if r_end <= r_start {
                    return;
                }
                para.revisions.push(Revision {
                    start: r_start,
                    end: r_end,
                    kind: RevisionKind::FormatChange,
                    author: author_local,
                    date: date_local,
                    id: None,
                    prev_attrs: Some(prev_local),
                });
                para.dirty = true;
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
            settings: self.settings.clone(),
            styles: self.styles.clone(),
            style_defaults: self.style_defaults.clone(),
            style_run_defaults: self.style_run_defaults.clone(),
            styles_dirty: self.styles_dirty,
            numbering: self.numbering.clone(),
        }
    }

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
                resolved_list_indent: None,
                dirty: true,
                source_xml: None,
                inline_objects: Vec::new(),
                hyperlinks: Vec::new(),
                revisions: Vec::new(),
                fields: Vec::new(),
                style_id: None,
                direct_overrides: ParaProperties::default(),
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
                settings: self.settings.clone(),
                styles: self.styles.clone(),
                style_defaults: self.style_defaults.clone(),
                style_run_defaults: self.style_run_defaults.clone(),
                styles_dirty: self.styles_dirty,
                numbering: self.numbering.clone(),
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
            settings: self.settings.clone(),
            styles: self.styles.clone(),
            style_defaults: self.style_defaults.clone(),
            style_run_defaults: self.style_run_defaults.clone(),
            styles_dirty: self.styles_dirty,
            numbering: self.numbering.clone(),
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
            let styled = p.apply_style(lo, hi, patch.clone());
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
            settings: self.settings.clone(),
            styles: self.styles.clone(),
            style_defaults: self.style_defaults.clone(),
            style_run_defaults: self.style_run_defaults.clone(),
            styles_dirty: self.styles_dirty,
            numbering: self.numbering.clone(),
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
            settings: self.settings.clone(),
            styles: self.styles.clone(),
            style_defaults: self.style_defaults.clone(),
            style_run_defaults: self.style_run_defaults.clone(),
            styles_dirty: self.styles_dirty,
            numbering: self.numbering.clone(),
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
                    /* Sprint 12 (#11) — shadow direct_overrides so a
                    subsequent ApplyStyle preserves this user edit. */
                    para.direct_overrides.alignment = Some(align);
                });
            }
        } else {
            let _ = mutate_paragraph_in_top(&mut blocks, &start.path, |para| {
                para.props.alignment = Some(align);
                para.direct_overrides.alignment = Some(align);
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
            settings: self.settings.clone(),
            styles: self.styles.clone(),
            style_defaults: self.style_defaults.clone(),
            style_run_defaults: self.style_run_defaults.clone(),
            styles_dirty: self.styles_dirty,
            numbering: self.numbering.clone(),
        }
    }

    /// Set paragraph base direction (`<w:bidi>`) on every paragraph the
    /// range spans. Mirrors [`Self::set_alignment`] but writes
    /// `props.direction` instead. The direction defines logical text
    /// flow + punctuation placement; alignment is a separate concern
    /// (visual anchoring). Word ties them with the
    /// writing-direction-relative `Start` / `End` alignment tokens —
    /// flipping direction automatically swaps which visual edge those
    /// resolve to, no alignment rewrite needed.
    pub fn set_direction(
        &self,
        start: LogicalPos,
        end: LogicalPos,
        direction: TextDirection,
    ) -> Self {
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
                    para.props.direction = Some(direction);
                    para.direct_overrides.direction = Some(direction);
                });
            }
        } else {
            let _ = mutate_paragraph_in_top(&mut blocks, &start.path, |para| {
                para.props.direction = Some(direction);
                para.direct_overrides.direction = Some(direction);
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
            settings: self.settings.clone(),
            styles: self.styles.clone(),
            style_defaults: self.style_defaults.clone(),
            style_run_defaults: self.style_run_defaults.clone(),
            styles_dirty: self.styles_dirty,
            numbering: self.numbering.clone(),
        }
    }

    /// Sprint 2 (UI Edition) — set `Section.columns` for the section
    /// containing the top-level block step at `pos`. `count == 0`
    /// collapses to single column (matches `ColumnSpec::from_twips`
    /// defensive clamping). The paginator picks the new geometry up
    /// on the next reflow.
    pub fn set_section_columns_at(&self, pos: LogicalPos, count: u8, gutter_pt: f32) -> Self {
        let block_idx = pos
            .path
            .steps
            .iter()
            .find_map(|s| match s {
                PathStep::Block(n) => Some(*n),
                PathStep::Cell { .. } => None,
            })
            .unwrap_or(0);
        let mut sections = self.sections.clone();
        let section_idx = sections
            .iter()
            .position(|s| s.start_block <= block_idx && block_idx < s.end_block)
            .or_else(|| sections.len().checked_sub(1));
        if let Some(idx) = section_idx
            && let Some(s) = sections.get_mut(idx)
        {
            s.columns = ColumnSpec {
                count: count.max(1),
                gutter_pt,
            };
        }
        Self {
            blocks: self.blocks.clone(),
            sections,
            headers: self.headers.clone(),
            footers: self.footers.clone(),
            media: self.media.clone(),
            footnotes: self.footnotes.clone(),
            comment_defs: self.comment_defs.clone(),
            comment_ranges: self.comment_ranges.clone(),
            settings: self.settings.clone(),
            styles: self.styles.clone(),
            style_defaults: self.style_defaults.clone(),
            style_run_defaults: self.style_run_defaults.clone(),
            styles_dirty: self.styles_dirty,
            numbering: self.numbering.clone(),
        }
    }

    /// Sprint 2 (UI Edition) — set `ParaProperties.page_break_before`
    /// on the paragraph identified by `pos`. The flag is already
    /// honoured by the paginator (renders from `<w:pageBreakBefore>`
    /// on `.docx` load); this method is what the editor calls when the
    /// user inserts a page break via `Ctrl+Enter`.
    pub fn set_page_break_before(&self, pos: LogicalPos, value: bool) -> Self {
        let mut blocks = self.blocks.clone();
        let _ = mutate_paragraph_in_top(&mut blocks, &pos.path, |para| {
            para.props.page_break_before = value;
        });
        Self {
            blocks,
            sections: self.sections.clone(),
            headers: self.headers.clone(),
            footers: self.footers.clone(),
            media: self.media.clone(),
            footnotes: self.footnotes.clone(),
            comment_defs: self.comment_defs.clone(),
            comment_ranges: self.comment_ranges.clone(),
            settings: self.settings.clone(),
            styles: self.styles.clone(),
            style_defaults: self.style_defaults.clone(),
            style_run_defaults: self.style_run_defaults.clone(),
            styles_dirty: self.styles_dirty,
            numbering: self.numbering.clone(),
        }
    }

    /* ===========================================================
    Sprint 7 (UI Edition) — review mutators.
    Track changes RECORDING (gating new edits as `<w:ins>`/`<w:del>`)
    is a separate Core Engine task and is NOT implemented here; the
    bridge `ToggleTrackChanges` surfaces an Error. The mutators
    below operate on revisions and comments already present in the
    `DocumentTree`.
    =========================================================== */

    /// Accept a tracked-change revision identified by (top-level
    /// `block`, byte `start`, byte `end`). Semantics:
    ///   - `Insert + Accept` → keep the inserted text, drop the overlay
    ///   - `Delete + Accept` → drop the deleted text + drop the overlay
    pub fn accept_revision_at(&self, block: u32, start: u32, end: u32) -> Self {
        self.apply_revision_decision(block, start, end, /* accept = */ true)
    }

    /// Reject a tracked-change revision identified by (top-level
    /// `block`, byte `start`, byte `end`). Semantics:
    ///   - `Insert + Reject` → drop the inserted text + drop the overlay
    ///   - `Delete + Reject` → keep the original text, drop the overlay
    pub fn reject_revision_at(&self, block: u32, start: u32, end: u32) -> Self {
        self.apply_revision_decision(block, start, end, /* accept = */ false)
    }

    fn apply_revision_decision(&self, block: u32, start: u32, end: u32, accept: bool) -> Self {
        let mut blocks = self.blocks.clone();
        let path = BlockPath::top(block);
        let _ = mutate_paragraph_in_top(&mut blocks, &path, |para| {
            let Some(idx) = para
                .revisions
                .iter()
                .position(|r| r.start == start && r.end == end)
            else {
                return;
            };
            /* Remove the matched revision FIRST so the offset-shift
             * helper does not also `retain`-drop it (which would make
             * any post-shift index lookup brittle). */
            let rev = para.revisions.remove(idx);
            let delete_text = match (rev.kind, accept) {
                (RevisionKind::Insert, false) => true, // Reject Insert
                (RevisionKind::Delete, true) => true,  // Accept Delete
                _ => false,                            // text stays live
            };
            if delete_text {
                let s = (rev.start as usize).min(para.text.len());
                let e = (rev.end as usize).min(para.text.len());
                if s < e {
                    let removed_len = (e - s) as u32;
                    para.text.replace_range(s..e, "");
                    shift_paragraph_offsets_after(para, rev.start, removed_len);
                }
            }
            para.dirty = true;
        });
        Self {
            blocks,
            sections: self.sections.clone(),
            headers: self.headers.clone(),
            footers: self.footers.clone(),
            media: self.media.clone(),
            footnotes: self.footnotes.clone(),
            comment_defs: self.comment_defs.clone(),
            comment_ranges: self.comment_ranges.clone(),
            settings: self.settings.clone(),
            styles: self.styles.clone(),
            style_defaults: self.style_defaults.clone(),
            style_run_defaults: self.style_run_defaults.clone(),
            styles_dirty: self.styles_dirty,
            numbering: self.numbering.clone(),
        }
    }

    /// Sprint 7 (UI Edition) — append a new comment anchored to a
    /// logical range. Picks a fresh `id` (max existing + 1) and
    /// installs both a `CommentDef` (with `paragraphs = [text]`)
    /// and a matching `CommentRange`. Returns `(new_doc, id)`.
    pub fn insert_comment(
        &self,
        start: LogicalPos,
        end: LogicalPos,
        text: String,
        author: String,
        date: String,
    ) -> (Self, u32) {
        let (start, end) = order_positions(start, end);
        let new_id = self
            .comment_defs
            .keys()
            .max()
            .copied()
            .unwrap_or(0)
            .saturating_add(1);
        let mut comment_defs = self.comment_defs.clone();
        comment_defs.insert(
            new_id,
            CommentDef {
                author,
                date,
                paragraphs: vec![text],
                resolved: false,
                /* Engine-minted comments have no paraId yet — their
                resolved state survives only in-memory until the
                comments.xml writer learns to mint paraIds. */
                first_para_id: None,
                parent_id: None,
            },
        );
        let mut comment_ranges = self.comment_ranges.clone();
        comment_ranges.push(CommentRange {
            id: new_id,
            start,
            end,
        });
        let doc = Self {
            blocks: self.blocks.clone(),
            sections: self.sections.clone(),
            headers: self.headers.clone(),
            footers: self.footers.clone(),
            media: self.media.clone(),
            footnotes: self.footnotes.clone(),
            comment_defs,
            comment_ranges,
            settings: self.settings.clone(),
            styles: self.styles.clone(),
            style_defaults: self.style_defaults.clone(),
            style_run_defaults: self.style_run_defaults.clone(),
            styles_dirty: self.styles_dirty,
            numbering: self.numbering.clone(),
        };
        (doc, new_id)
    }

    /// Issue #27 — append a threaded reply to an existing comment.
    /// Mints the next `w:id` (max existing + 1, same discipline as
    /// [`Self::insert_comment`]) and installs a `CommentDef` with
    /// `parent_id = Some(parent_id)`. The reply's `CommentRange` is
    /// CLONED from the parent's range (same `start` / `end`) — Word
    /// anchors replies on the parent's span, and the snapshot loop
    /// (which iterates `comment_ranges`) then surfaces the reply
    /// without any special-casing. A parent that carries no range
    /// (orphan def) still accepts the reply; no range is pushed in
    /// that case, mirroring the parent's own anchor-less state.
    ///
    /// Returns `None` when `parent_id` names no existing comment —
    /// the wasm layer maps that to `Event::Error`.
    pub fn reply_to_comment(
        &self,
        parent_id: u32,
        text: String,
        author: String,
        date: String,
    ) -> Option<(Self, u32)> {
        if !self.comment_defs.contains_key(&parent_id) {
            return None;
        }
        let new_id = self
            .comment_defs
            .keys()
            .max()
            .copied()
            .unwrap_or(0)
            .saturating_add(1);
        let mut comment_defs = self.comment_defs.clone();
        comment_defs.insert(
            new_id,
            CommentDef {
                author,
                date,
                paragraphs: vec![text],
                resolved: false,
                /* Engine-minted replies have no paraId yet — the
                comments.xml writer mints one at save time. */
                first_para_id: None,
                parent_id: Some(parent_id),
            },
        );
        let mut comment_ranges = self.comment_ranges.clone();
        if let Some(parent_range) = self.comment_ranges.iter().find(|r| r.id == parent_id) {
            comment_ranges.push(CommentRange {
                id: new_id,
                start: parent_range.start.clone(),
                end: parent_range.end.clone(),
            });
        }
        let doc = Self {
            blocks: self.blocks.clone(),
            sections: self.sections.clone(),
            headers: self.headers.clone(),
            footers: self.footers.clone(),
            media: self.media.clone(),
            footnotes: self.footnotes.clone(),
            comment_defs,
            comment_ranges,
            settings: self.settings.clone(),
            styles: self.styles.clone(),
            style_defaults: self.style_defaults.clone(),
            style_run_defaults: self.style_run_defaults.clone(),
            styles_dirty: self.styles_dirty,
            numbering: self.numbering.clone(),
        };
        Some((doc, new_id))
    }

    /// Sprint 7 (UI Edition) — remove a comment by id from both
    /// `comment_defs` and `comment_ranges`.
    ///
    /// Issue #27 — deletion CASCADES through the reply thread: every
    /// comment whose `parent_id` chain (walked transitively) reaches
    /// the deleted id is removed too, along with its ranges. Deleting
    /// a reply leaves its parent untouched.
    pub fn delete_comment(&self, id: u32) -> Self {
        /* Transitive closure of the thread rooted at `id`. Fixpoint
        loop — reply chains are short (Word nests one level, but a
        chain of replies-to-replies still terminates because each
        pass only ever adds ids). */
        let mut doomed: std::collections::HashSet<u32> = std::collections::HashSet::new();
        doomed.insert(id);
        loop {
            let before = doomed.len();
            for (cid, def) in &self.comment_defs {
                if let Some(pid) = def.parent_id
                    && doomed.contains(&pid)
                {
                    doomed.insert(*cid);
                }
            }
            if doomed.len() == before {
                break;
            }
        }
        let mut comment_defs = self.comment_defs.clone();
        comment_defs.retain(|cid, _| !doomed.contains(cid));
        let mut comment_ranges = self.comment_ranges.clone();
        comment_ranges.retain(|r| !doomed.contains(&r.id));
        Self {
            blocks: self.blocks.clone(),
            sections: self.sections.clone(),
            headers: self.headers.clone(),
            footers: self.footers.clone(),
            media: self.media.clone(),
            footnotes: self.footnotes.clone(),
            comment_defs,
            comment_ranges,
            settings: self.settings.clone(),
            styles: self.styles.clone(),
            style_defaults: self.style_defaults.clone(),
            style_run_defaults: self.style_run_defaults.clone(),
            styles_dirty: self.styles_dirty,
            numbering: self.numbering.clone(),
        }
    }

    /// Sprint 7 (UI Edition) — set the in-memory `resolved` flag on
    /// the comment with the given id. No `commentsExtended.xml`
    /// round-trip yet — see Core Engine backlog.
    pub fn set_comment_resolved(&self, id: u32, resolved: bool) -> Self {
        let mut comment_defs = self.comment_defs.clone();
        if let Some(cd) = comment_defs.get_mut(&id) {
            cd.resolved = resolved;
        }
        Self {
            blocks: self.blocks.clone(),
            sections: self.sections.clone(),
            headers: self.headers.clone(),
            footers: self.footers.clone(),
            media: self.media.clone(),
            footnotes: self.footnotes.clone(),
            comment_defs,
            comment_ranges: self.comment_ranges.clone(),
            settings: self.settings.clone(),
            styles: self.styles.clone(),
            style_defaults: self.style_defaults.clone(),
            style_run_defaults: self.style_run_defaults.clone(),
            styles_dirty: self.styles_dirty,
            numbering: self.numbering.clone(),
        }
    }

    /// Sprint 6 (UI Edition) — set `<w:pPr><w:ind>` (paragraph
    /// indentation) on every paragraph the range spans. Values in pt
    /// (1 pt = 20 twips). `first_line_pt > 0` populates
    /// `first_line_twips`; `first_line_pt < 0` populates
    /// `hanging_twips` with `|first_line_pt| * 20` (Word's mutually-
    /// exclusive `<w:firstLine>` vs `<w:hanging>` semantics).
    ///
    /// `start_pt` / `end_pt` may be **negative** — a negative `<w:start>` /
    /// `<w:end>` is a Word/Google-Docs *outdent* that pulls the leading /
    /// trailing edge into the page margin. ECMA-376 defines `w:start` /
    /// `w:end` as `ST_SignedTwipsMeasure`, so negative twips round-trip
    /// faithfully through the `.docx` writer. `first_line_pt` stays signed
    /// only through the `firstLine` / `hanging` split — both of those are
    /// `ST_TwipsMeasure` (unsigned), so the magnitude is always stored
    /// non-negative.
    pub fn set_paragraph_indent(
        &self,
        start: LogicalPos,
        end: LogicalPos,
        start_pt: f32,
        end_pt: f32,
        first_line_pt: f32,
    ) -> Self {
        let (start, end) = order_positions(start, end);
        /* No `.max(0.0)` floor: negative start/end are first-class outdents
        (Bug B — "the grey area"). The Ruler clamps the drag to the page
        edge so the model never receives an outdent larger than the margin. */
        let start_twips = (start_pt * 20.0).round() as i32;
        let end_twips = (end_pt * 20.0).round() as i32;
        let (first_line_twips, hanging_twips) = if first_line_pt >= 0.0 {
            ((first_line_pt * 20.0).round() as i32, 0)
        } else {
            (0, (-first_line_pt * 20.0).round() as i32)
        };
        let mut blocks = self.blocks.clone();
        let apply = |para: &mut Paragraph| {
            let new_ind = Indent {
                start_twips,
                end_twips,
                first_line_twips,
                hanging_twips,
            };
            para.props.indent = new_ind;
            /* Sprint 12 (#11) — shadow into direct_overrides. */
            para.direct_overrides.indent = new_ind;
        };
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
                let _ = mutate_paragraph_in_top(&mut blocks, &child_path, apply);
            }
        } else {
            let _ = mutate_paragraph_in_top(&mut blocks, &start.path, apply);
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
            settings: self.settings.clone(),
            styles: self.styles.clone(),
            style_defaults: self.style_defaults.clone(),
            style_run_defaults: self.style_run_defaults.clone(),
            styles_dirty: self.styles_dirty,
            numbering: self.numbering.clone(),
        }
    }

    /// Sprint 12 (#11) — resolve the paragraph cascade for `style_id`
    /// into a flat `ParaProperties`. Cycle-safe (visited set) +
    /// depth-capped at [`MAX_STYLE_CHAIN`] entries, matching ECMA-376
    /// §17.7.4.5 implementation guidance. Used both by
    /// [`Self::recompute_paragraph_props`] (on every style mutation)
    /// and by the reader's first-pass cascade.
    pub fn resolve_style_cascade(&self, style_id: Option<&str>) -> ParaProperties {
        let mut out = self.style_defaults.clone();
        let Some(leaf) = style_id else {
            return out;
        };
        let mut visited: std::collections::HashSet<&str> = std::collections::HashSet::new();
        let mut chain: Vec<&ParagraphStyle> = Vec::new();
        let mut current: Option<&str> = Some(leaf);
        while let Some(id) = current {
            if chain.len() >= MAX_STYLE_CHAIN || !visited.insert(id) {
                break;
            }
            let Some(def) = self.styles.get(id) else {
                break;
            };
            chain.push(def);
            current = def.based_on.as_deref();
        }
        for def in chain.iter().rev() {
            out = out.clone().merged_with(def.para.clone());
        }
        out
    }

    /// Issue #29 — the RUN half of the cascade: fold
    /// `style_run_defaults → pStyle chain <w:rPr>` (root → leaf) into
    /// the `SpanStyle` a run inherits before its direct formatting.
    /// Same cycle / depth guards as [`Self::resolve_style_cascade`].
    pub fn resolve_style_run_cascade(&self, style_id: Option<&str>) -> SpanStyle {
        resolve_run_cascade(&self.styles, &self.style_run_defaults, style_id)
    }

    /// Sprint 12 (#11) — apply `style_id` to every paragraph the range
    /// spans. The user's pre-existing `direct_overrides` are
    /// preserved; only `props` (the resolved view) is recomputed so
    /// downstream rendering picks up the cascade. Empty `style_id`
    /// detaches the paragraph from any style (resolved view falls
    /// back to `style_defaults ∪ direct_overrides`).
    pub fn set_paragraph_style(
        &self,
        start: LogicalPos,
        end: LogicalPos,
        style_id: Option<String>,
    ) -> Self {
        let (start, end) = order_positions(start, end);
        let mut blocks = self.blocks.clone();
        let styles_for_apply = self.styles.clone();
        let defaults_for_apply = self.style_defaults.clone();
        let apply = |para: &mut Paragraph| {
            para.style_id = style_id.clone();
            recompute_paragraph_props(para, &styles_for_apply, &defaults_for_apply);
        };
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
                let _ = mutate_paragraph_in_top(&mut blocks, &child_path, apply);
            }
        } else {
            let _ = mutate_paragraph_in_top(&mut blocks, &start.path, apply);
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
            settings: self.settings.clone(),
            styles: self.styles.clone(),
            style_defaults: self.style_defaults.clone(),
            style_run_defaults: self.style_run_defaults.clone(),
            styles_dirty: self.styles_dirty,
            numbering: self.numbering.clone(),
        }
    }

    /// Issue #21 — mutate an existing style definition in-place and
    /// re-cascade every styled paragraph (body + table cells). Patch
    /// semantics: `None` leaves a half untouched; `Some(patch)` folds
    /// over the current definition via `merged_with`. `based_on` is the
    /// three-state knob (leave / clear / re-parent). Direct overrides
    /// survive verbatim — the #11 re-application discipline. Flips
    /// `styles_dirty` so the writer regenerates `word/styles.xml`.
    /// Unknown `style_id` is a no-op clone.
    pub fn modify_style(
        &self,
        style_id: &str,
        para_patch: Option<ParaProperties>,
        run_patch: Option<SpanStyle>,
        based_on: Option<Option<String>>,
        display_name: Option<String>,
    ) -> Self {
        if !self.styles.contains_key(style_id) {
            return self.clone();
        }
        let mut styles = self.styles.clone();
        if let Some(def) = styles.get_mut(style_id) {
            if let Some(pp) = para_patch {
                def.para = def.para.clone().merged_with(pp);
            }
            if let Some(rp) = run_patch {
                def.run = def.run.clone().merged_with(rp);
            }
            if let Some(b) = based_on {
                def.based_on = b;
            }
            if let Some(n) = display_name {
                def.name = n;
            }
        }
        /* Re-resolve every paragraph's cascaded para props against the
        mutated table (the run half folds at span-materialize time, so
        it re-cascades for free). Recomputing unstyled paragraphs too
        is a harmless idempotent fold — cheaper than chain-membership
        bookkeeping and immune to basedOn re-parenting edge cases. */
        fn recompute_block(
            b: &mut Block,
            styles: &std::collections::HashMap<String, ParagraphStyle>,
            defaults: &ParaProperties,
        ) {
            match b {
                Block::Paragraph(p) => recompute_paragraph_props(p, styles, defaults),
                Block::Table(t) => {
                    for row in &mut t.rows {
                        for cell in &mut row.cells {
                            for cb in &mut cell.blocks {
                                recompute_block(cb, styles, defaults);
                            }
                        }
                    }
                }
            }
        }
        let mut blocks = self.blocks.clone();
        for i in 0..blocks.len() {
            let mut b = blocks[i].clone();
            recompute_block(&mut b, &styles, &self.style_defaults);
            blocks.set(i, b);
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
            settings: self.settings.clone(),
            styles,
            style_defaults: self.style_defaults.clone(),
            style_run_defaults: self.style_run_defaults.clone(),
            styles_dirty: true,
            numbering: self.numbering.clone(),
        }
    }

    /// Sprint 11 (#13) — replace `<w:pPr><w:tabs>` on every paragraph
    /// the range spans with `stops`. Empty `stops` clears the
    /// paragraph's custom tab grid (it falls back to the default
    /// 0.5-inch grid the line builder ships). One commit per call,
    /// so the Ruler's drag-end dispatch produces exactly one undo
    /// entry per tab-stop edit (matches Word's "release commits"
    /// behaviour).
    pub fn set_tab_stops(&self, start: LogicalPos, end: LogicalPos, stops: Vec<TabStop>) -> Self {
        let (start, end) = order_positions(start, end);
        let mut blocks = self.blocks.clone();
        let apply = |para: &mut Paragraph| {
            para.props.tab_stops = stops.clone();
            /* Sprint 12 (#11) — shadow into direct_overrides. */
            para.direct_overrides.tab_stops = stops.clone();
        };
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
                let _ = mutate_paragraph_in_top(&mut blocks, &child_path, apply);
            }
        } else {
            let _ = mutate_paragraph_in_top(&mut blocks, &start.path, apply);
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
            settings: self.settings.clone(),
            styles: self.styles.clone(),
            style_defaults: self.style_defaults.clone(),
            style_run_defaults: self.style_run_defaults.clone(),
            styles_dirty: self.styles_dirty,
            numbering: self.numbering.clone(),
        }
    }

    /// Sprint 6 (UI Edition) — set `<w:pPr><w:spacing w:line>` as an
    /// `Auto` (multiplier) line height. 240 twips = single (1.0×).
    /// Pass `multiplier <= 0.0` to clear (`line_height: None`).
    pub fn set_line_spacing(&self, start: LogicalPos, end: LogicalPos, multiplier: f32) -> Self {
        let target = if multiplier > 0.0 {
            Some(LineHeight::Auto {
                twips: (multiplier * 240.0).round() as i32,
            })
        } else {
            None
        };
        let (start, end) = order_positions(start, end);
        let mut blocks = self.blocks.clone();
        let apply = |para: &mut Paragraph| {
            para.props.line_height = target;
            /* Sprint 12 (#11) — shadow into direct_overrides. */
            para.direct_overrides.line_height = target;
        };
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
                let _ = mutate_paragraph_in_top(&mut blocks, &child_path, apply);
            }
        } else {
            let _ = mutate_paragraph_in_top(&mut blocks, &start.path, apply);
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
            settings: self.settings.clone(),
            styles: self.styles.clone(),
            style_defaults: self.style_defaults.clone(),
            style_run_defaults: self.style_run_defaults.clone(),
            styles_dirty: self.styles_dirty,
            numbering: self.numbering.clone(),
        }
    }

    /// Sprint 6 (UI Edition) — set `<w:pPr><w:shd>` (paragraph
    /// shading) on every paragraph the range spans. `None` clears.
    pub fn set_paragraph_shading(
        &self,
        start: LogicalPos,
        end: LogicalPos,
        color: Option<[u8; 4]>,
    ) -> Self {
        let (start, end) = order_positions(start, end);
        let mut blocks = self.blocks.clone();
        let apply = |para: &mut Paragraph| {
            para.props.shading = color;
            /* Sprint 12 (#11) — shadow into direct_overrides. */
            para.direct_overrides.shading = color;
        };
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
                let _ = mutate_paragraph_in_top(&mut blocks, &child_path, apply);
            }
        } else {
            let _ = mutate_paragraph_in_top(&mut blocks, &start.path, apply);
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
            settings: self.settings.clone(),
            styles: self.styles.clone(),
            style_defaults: self.style_defaults.clone(),
            style_run_defaults: self.style_run_defaults.clone(),
            styles_dirty: self.styles_dirty,
            numbering: self.numbering.clone(),
        }
    }

    /// Issue #50 — re-stamp `resolved_marker` / `resolved_list_indent`
    /// across the top-level paragraphs after a structural mutation
    /// (split / merge / splice). `Paragraph::split_at` / `concat` clone
    /// the stamped fields onto both outputs, so without a refresh an
    /// Enter inside a numbered list shows the head's marker twice
    /// instead of renumbering the tail. Cheap early-out when the
    /// document carries no list paragraphs.
    fn with_list_markers_refreshed(mut self) -> Self {
        let has_lists = self
            .blocks
            .iter()
            .any(|b| b.as_paragraph().is_some_and(|p| p.list_item.is_some()));
        if !has_lists {
            return self;
        }
        /* Two passes so `im::Vector` structural sharing survives the hot
        edit path: compute the expected stamps immutably, then path-copy
        ONLY the paragraphs whose stamps actually changed. An Enter inside
        a bullet list changes nothing ("•" stays "•"); a numbered-list
        edit touches only the renumbered tail — a blanket `iter_mut()`
        would thaw every chunk of the shared tree (and un-share every
        UndoStack snapshot) on each split/merge/paste. */
        let (para_indices, items): (Vec<usize>, Vec<Option<ListItem>>) = self
            .blocks
            .iter()
            .enumerate()
            .filter_map(|(idx, b)| b.as_paragraph().map(|p| (idx, p.list_item)))
            .unzip();
        let expected = numbering::compute_markers(&items, &self.numbering);
        for (idx, (marker, indent)) in para_indices.into_iter().zip(expected) {
            let stale = self.blocks[idx]
                .as_paragraph()
                .is_some_and(|p| p.resolved_marker != marker || p.resolved_list_indent != indent);
            if stale && let Some(p) = self.blocks.get_mut(idx).and_then(|b| b.as_paragraph_mut()) {
                p.resolved_marker = marker;
                p.resolved_list_indent = indent;
            }
        }
        self
    }

    /// Sprint 5 (UI Edition) — clear `list_item` on every paragraph
    /// the range spans. The engine has no numbering synthesizer
    /// today, so this is the only list mutation that is safe to
    /// expose: removing list membership cannot introduce a dangling
    /// `num_id`. Adding list membership is filed as a Core Engine
    /// task (see project backlog).
    /// Sprint 13 (#12) — set `Paragraph.list_item = Some(ListItem {
    /// num_id, ilvl: 0 })` on every paragraph the range spans, then
    /// re-resolve markers for the whole document. `num_id` is the
    /// idempotent return from
    /// [`numbering::NumberingDefinitions::synth_list_definition`] —
    /// reuses an existing matching template when one is present so
    /// repeated toggles do not inflate `numbering.xml`.
    ///
    /// The synth runs against a CLONED numbering store; only if it
    /// flips `.dirty` does the new store replace the existing one
    /// (preserves passthrough byte-identity in the no-op reuse
    /// case).
    pub fn toggle_list_on_range(
        &self,
        start: LogicalPos,
        end: LogicalPos,
        kind: numbering::ListSynthesisKind,
    ) -> Self {
        let (start, end) = order_positions(start, end);
        let mut next_numbering = self.numbering.clone();
        let num_id = next_numbering.synth_list_definition(kind);
        let mut blocks = self.blocks.clone();
        let apply = |para: &mut Paragraph| {
            para.list_item = Some(ListItem { num_id, ilvl: 0 });
            /* resolved_marker is re-stamped by the document-wide
            resolver below — clearing now keeps it consistent if the
            resolver bails on a malformed cascade. */
            para.resolved_marker = None;
        };
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
                let _ = mutate_paragraph_in_top(&mut blocks, &child_path, apply);
            }
        } else {
            let _ = mutate_paragraph_in_top(&mut blocks, &start.path, apply);
        }
        /* Document-wide marker refresh — counters reset at the top
        because the toggled range might appear in the middle. */
        let mut paragraph_refs: Vec<&mut Paragraph> = blocks
            .iter_mut()
            .filter_map(|b| b.as_paragraph_mut())
            .collect();
        numbering::resolve_markers_in_place(&mut paragraph_refs, &next_numbering);
        Self {
            blocks,
            sections: self.sections.clone(),
            headers: self.headers.clone(),
            footers: self.footers.clone(),
            media: self.media.clone(),
            footnotes: self.footnotes.clone(),
            comment_defs: self.comment_defs.clone(),
            comment_ranges: self.comment_ranges.clone(),
            settings: self.settings.clone(),
            styles: self.styles.clone(),
            style_defaults: self.style_defaults.clone(),
            style_run_defaults: self.style_run_defaults.clone(),
            styles_dirty: self.styles_dirty,
            numbering: next_numbering,
        }
    }

    pub fn clear_list_item_on_range(&self, start: LogicalPos, end: LogicalPos) -> Self {
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
                    para.list_item = None;
                    para.resolved_marker = None;
                    para.resolved_list_indent = None;
                });
            }
        } else {
            let _ = mutate_paragraph_in_top(&mut blocks, &start.path, |para| {
                para.list_item = None;
                para.resolved_marker = None;
                para.resolved_list_indent = None;
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
            settings: self.settings.clone(),
            styles: self.styles.clone(),
            style_defaults: self.style_defaults.clone(),
            style_run_defaults: self.style_run_defaults.clone(),
            styles_dirty: self.styles_dirty,
            numbering: self.numbering.clone(),
        }
        .with_list_markers_refreshed()
    }

    /// Sprint 4 (UI Edition) — set `<w:pgMar>` (top/right/bottom/left
    /// in points) on the section containing the top-level block step
    /// at `pos`. Header/footer offsets are preserved.
    pub fn set_section_margins_at(
        &self,
        pos: LogicalPos,
        top_pt: f32,
        right_pt: f32,
        bottom_pt: f32,
        left_pt: f32,
    ) -> Self {
        let block_idx = pos
            .path
            .steps
            .iter()
            .find_map(|s| match s {
                PathStep::Block(n) => Some(*n),
                PathStep::Cell { .. } => None,
            })
            .unwrap_or(0);
        let mut sections = self.sections.clone();
        let section_idx = sections
            .iter()
            .position(|s| s.start_block <= block_idx && block_idx < s.end_block)
            .or_else(|| sections.len().checked_sub(1));
        if let Some(idx) = section_idx
            && let Some(s) = sections.get_mut(idx)
        {
            s.geometry.margin_top = top_pt.max(0.0);
            s.geometry.margin_right = right_pt.max(0.0);
            s.geometry.margin_bottom = bottom_pt.max(0.0);
            s.geometry.margin_left = left_pt.max(0.0);
        }
        Self {
            blocks: self.blocks.clone(),
            sections,
            headers: self.headers.clone(),
            footers: self.footers.clone(),
            media: self.media.clone(),
            footnotes: self.footnotes.clone(),
            comment_defs: self.comment_defs.clone(),
            comment_ranges: self.comment_ranges.clone(),
            settings: self.settings.clone(),
            styles: self.styles.clone(),
            style_defaults: self.style_defaults.clone(),
            style_run_defaults: self.style_run_defaults.clone(),
            styles_dirty: self.styles_dirty,
            numbering: self.numbering.clone(),
        }
    }

    /// Sprint 4 (UI Edition) — force the orientation of the section
    /// containing `pos`. `landscape == true` swaps width and height
    /// when width <= height; `landscape == false` swaps the other
    /// way. Margins are NOT rotated — Word treats `<w:pgMar>` as
    /// edge-labelled, not paper-relative.
    pub fn set_section_orientation_at(&self, pos: LogicalPos, landscape: bool) -> Self {
        let block_idx = pos
            .path
            .steps
            .iter()
            .find_map(|s| match s {
                PathStep::Block(n) => Some(*n),
                PathStep::Cell { .. } => None,
            })
            .unwrap_or(0);
        let mut sections = self.sections.clone();
        let section_idx = sections
            .iter()
            .position(|s| s.start_block <= block_idx && block_idx < s.end_block)
            .or_else(|| sections.len().checked_sub(1));
        if let Some(idx) = section_idx
            && let Some(s) = sections.get_mut(idx)
        {
            let is_landscape = s.geometry.width > s.geometry.height;
            if landscape != is_landscape {
                let (w, h) = (s.geometry.width, s.geometry.height);
                s.geometry.width = h;
                s.geometry.height = w;
            }
        }
        Self {
            blocks: self.blocks.clone(),
            sections,
            headers: self.headers.clone(),
            footers: self.footers.clone(),
            media: self.media.clone(),
            footnotes: self.footnotes.clone(),
            comment_defs: self.comment_defs.clone(),
            comment_ranges: self.comment_ranges.clone(),
            settings: self.settings.clone(),
            styles: self.styles.clone(),
            style_defaults: self.style_defaults.clone(),
            style_run_defaults: self.style_run_defaults.clone(),
            styles_dirty: self.styles_dirty,
            numbering: self.numbering.clone(),
        }
    }

    /// Sprint 3 (UI Edition) — insert a brand-new inline image at
    /// `pos`. Picks a fresh `rel_id` (collision-free against
    /// `media`), inserts a U+FFFC sentinel in the paragraph text at
    /// the byte offset, shifts existing styled spans + inline
    /// objects + hyperlinks + revisions + fields rightward by the
    /// sentinel's UTF-8 length, and appends a new
    /// [`InlineKind::Image`] entry pointing at the registered blob.
    ///
    /// Width/height are passed in EMU (English Metric Units —
    /// 914_400 per inch) so the model unit matches what `.docx`
    /// readers and the renderer already expect.
    pub fn insert_inline_image_at(
        &self,
        pos: LogicalPos,
        blob: ImageBlob,
        width_emu: i64,
        height_emu: i64,
    ) -> Self {
        let mut media = self.media.clone();
        /* Find a rel_id not already in the media map. Walk a counter
         * past any existing `nge_img_*` keys so removal-then-insert
         * cycles never collide. */
        let mut counter = media.len() as u32 + 1;
        let rel_id = loop {
            let candidate = format!("nge_img_{counter}");
            if !media.contains_key(&candidate) {
                break candidate;
            }
            counter = counter.saturating_add(1);
        };
        media.insert(rel_id.clone(), blob);

        let mut blocks = self.blocks.clone();
        let target = if self.paragraph_at_path(&pos.path).is_some() {
            pos.path.clone()
        } else {
            self.path_to_last_top_paragraph()
                .unwrap_or(BlockPath::top(0))
        };
        const SENTINEL: char = '\u{FFFC}';
        let sentinel_len = SENTINEL.len_utf8() as u32;
        let off = pos.offset;
        let rel_id_for_inline = rel_id.clone();
        let _ = mutate_paragraph_in_top(&mut blocks, &target, |para| {
            let offset = (off as usize).min(para.text.len());
            para.text.insert(offset, SENTINEL);
            let off = offset as u32;
            for s in &mut para.spans {
                if s.start >= off {
                    s.start += sentinel_len;
                }
                if s.end >= off {
                    s.end += sentinel_len;
                }
            }
            for io in &mut para.inline_objects {
                if io.at >= off {
                    io.at += sentinel_len;
                }
            }
            for h in &mut para.hyperlinks {
                if h.start >= off {
                    h.start += sentinel_len;
                }
                if h.end >= off {
                    h.end += sentinel_len;
                }
            }
            for r in &mut para.revisions {
                if r.start >= off {
                    r.start += sentinel_len;
                }
                if r.end >= off {
                    r.end += sentinel_len;
                }
            }
            for f in &mut para.fields {
                if f.start >= off {
                    f.start += sentinel_len;
                }
                if f.end >= off {
                    f.end += sentinel_len;
                }
            }
            para.inline_objects.push(InlineObject {
                at: off,
                kind: InlineKind::Image {
                    rel_id: rel_id_for_inline.clone(),
                    width_emu,
                    height_emu,
                },
            });
            para.inline_objects.sort_by_key(|i| i.at);
            para.dirty = true;
        });

        Self {
            blocks,
            sections: self.sections.clone(),
            headers: self.headers.clone(),
            footers: self.footers.clone(),
            media,
            footnotes: self.footnotes.clone(),
            comment_defs: self.comment_defs.clone(),
            comment_ranges: self.comment_ranges.clone(),
            settings: self.settings.clone(),
            styles: self.styles.clone(),
            style_defaults: self.style_defaults.clone(),
            style_run_defaults: self.style_run_defaults.clone(),
            styles_dirty: self.styles_dirty,
            numbering: self.numbering.clone(),
        }
    }

    /// Sprint 2 (UI Edition) — set `<w:pPr><w:pBdr>` on every
    /// paragraph the range spans. Mirrors [`Self::set_alignment`] but
    /// writes `props.borders`. Pass `None` to clear the borders.
    pub fn set_paragraph_borders(
        &self,
        start: LogicalPos,
        end: LogicalPos,
        borders: Option<CellBorders>,
    ) -> Self {
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
                    para.props.borders = borders.clone();
                });
            }
        } else {
            let _ = mutate_paragraph_in_top(&mut blocks, &start.path, |para| {
                para.props.borders = borders.clone();
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
            settings: self.settings.clone(),
            styles: self.styles.clone(),
            style_defaults: self.style_defaults.clone(),
            style_run_defaults: self.style_run_defaults.clone(),
            styles_dirty: self.styles_dirty,
            numbering: self.numbering.clone(),
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
                settings: self.settings.clone(),
                styles: self.styles.clone(),
                style_defaults: self.style_defaults.clone(),
                style_run_defaults: self.style_run_defaults.clone(),
                styles_dirty: self.styles_dirty,
                numbering: self.numbering.clone(),
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
            settings: self.settings.clone(),
            styles: self.styles.clone(),
            style_defaults: self.style_defaults.clone(),
            style_run_defaults: self.style_run_defaults.clone(),
            styles_dirty: self.styles_dirty,
            numbering: self.numbering.clone(),
        }
        .with_list_markers_refreshed()
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
                settings: self.settings.clone(),
                styles: self.styles.clone(),
                style_defaults: self.style_defaults.clone(),
                style_run_defaults: self.style_run_defaults.clone(),
                styles_dirty: self.styles_dirty,
                numbering: self.numbering.clone(),
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
            settings: self.settings.clone(),
            styles: self.styles.clone(),
            style_defaults: self.style_defaults.clone(),
            style_run_defaults: self.style_run_defaults.clone(),
            styles_dirty: self.styles_dirty,
            numbering: self.numbering.clone(),
        }
        .with_list_markers_refreshed()
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
                    settings: self.settings.clone(),
                    styles: self.styles.clone(),
                    style_defaults: self.style_defaults.clone(),
                    style_run_defaults: self.style_run_defaults.clone(),
                    styles_dirty: self.styles_dirty,
                    numbering: self.numbering.clone(),
                }
                .with_list_markers_refreshed(),
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
                settings: self.settings.clone(),
                styles: self.styles.clone(),
                style_defaults: self.style_defaults.clone(),
                style_run_defaults: self.style_run_defaults.clone(),
                styles_dirty: self.styles_dirty,
                numbering: self.numbering.clone(),
            }
            .with_list_markers_refreshed(),
            caret,
        )
    }

    /// Like [`Self::slice`] but preserves the top-level block sequence —
    /// tables that fall between the start and end paragraphs survive
    /// the slice as `Block::Table` entries, so the rich clipboard can
    /// round-trip a selection that crosses a table without dropping
    /// the cell content. The first and last paragraphs are clipped to
    /// their respective endpoint offsets; tables between them are
    /// cloned verbatim. Cross-container ranges (cell ↔ body) fall back
    /// to the start paragraph's tail only — full cross-container
    /// slicing lands with §IV.6 of UX_BEHAVIOR_SPEC.
    pub fn slice_blocks(&self, start: LogicalPos, end: LogicalPos) -> Vec<Block> {
        let (start, end) = order_positions(start, end);
        if self.paragraph_count() == 0 {
            return Vec::new();
        }
        if start.path == end.path {
            let Some(p) = self.paragraph_at_path(&start.path) else {
                return Vec::new();
            };
            let head = p.split_at(end.offset).0;
            return vec![Block::Paragraph(head.split_at(start.offset).1)];
        }
        if !same_parent(&start.path, &end.path) {
            let Some(p) = self.paragraph_at_path(&start.path) else {
                return Vec::new();
            };
            return vec![Block::Paragraph(p.split_at(start.offset).1)];
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
        let mut out: Vec<Block> = Vec::with_capacity((ep_idx - sp_idx + 1) as usize);
        if let Some(p) = container
            .get(sp_idx as usize)
            .and_then(|b| b.as_paragraph())
        {
            out.push(Block::Paragraph(p.split_at(start.offset).1));
        }
        for idx in (sp_idx + 1)..ep_idx {
            if let Some(b) = container.get(idx as usize) {
                out.push(b.clone());
            }
        }
        if let Some(p) = container
            .get(ep_idx as usize)
            .and_then(|b| b.as_paragraph())
        {
            out.push(Block::Paragraph(p.split_at(end.offset).0));
        }
        out
    }

    /// Like [`Self::insert_rich`] but accepts pre-styled blocks — the
    /// rich clipboard's table-and-paragraph payload. Splits the target
    /// paragraph at `at`, splices the head + first input block, then
    /// inserts the middle blocks (tables and paragraphs alike), then
    /// the last input block + the target's tail. Returns the new tree
    /// and the caret at the end of the inserted content.
    pub fn insert_rich_blocks(&self, at: LogicalPos, blocks_in: &[Block]) -> (Self, LogicalPos) {
        if blocks_in.is_empty() {
            return (self.clone(), at.clone());
        }
        /* When the input is all paragraphs, defer to the existing
        paragraph-only insert — it merges head+first and last+tail
        intra-paragraph so consecutive `\n`-joined paragraphs splice
        with no inter-paragraph break. */
        if blocks_in.iter().all(|b| matches!(b, Block::Paragraph(_))) {
            let paras: Vec<Paragraph> = blocks_in
                .iter()
                .filter_map(|b| b.as_paragraph().cloned())
                .collect();
            return self.insert_rich(at, &paras);
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
        let Some(target_para) = self.paragraph_at_path(&target_path).cloned() else {
            return (self.clone(), at.clone());
        };
        let (head, tail) = target_para.split_at(at.offset);
        /* Replace the target paragraph with the head + first input
        block (merged inline if that first block is a paragraph; else
        head stays its own paragraph and the input block follows). */
        let (first_replace, first_is_para) = match &blocks_in[0] {
            Block::Paragraph(p) => (Block::Paragraph(head.concat(p)), true),
            _ => (Block::Paragraph(head), false),
        };
        replace_block_in_top(&mut blocks, &target_path, first_replace);
        let mut after_path = target_path.clone();
        if !first_is_para {
            insert_block_after_path_in_top(&mut blocks, &after_path, blocks_in[0].clone());
            after_path = bump_last_block_index(&after_path);
        }
        /* Middle blocks (everything between the first and last input
        block) splice in verbatim. */
        let last_idx = blocks_in.len() - 1;
        for b in &blocks_in[1..last_idx] {
            insert_block_after_path_in_top(&mut blocks, &after_path, b.clone());
            after_path = bump_last_block_index(&after_path);
        }
        /* Last input block + the target's tail. When both are
        paragraphs, merge them inline (the typical Word paste shape).
        When the last input is a table, the tail becomes its own
        trailing paragraph below the table. */
        let (last_block, caret_offset) = match (blocks_in.get(last_idx), last_idx == 0) {
            (Some(Block::Paragraph(p)), true) => {
                /* Single-input-paragraph case is handled by the
                paragraph fast-path above; this branch is unreachable
                in practice but stays for completeness. */
                (Block::Paragraph(p.concat(&tail)), p.text.len() as u32)
            }
            (Some(Block::Paragraph(p)), false) => {
                let offset = p.text.len() as u32;
                (Block::Paragraph(p.concat(&tail)), offset)
            }
            (Some(Block::Table(t)), _) => {
                /* Splice the table, then append the tail as a fresh
                paragraph BELOW it so the caret has a logical home. */
                insert_block_after_path_in_top(&mut blocks, &after_path, Block::Table(t.clone()));
                after_path = bump_last_block_index(&after_path);
                (Block::Paragraph(tail.clone()), 0)
            }
            _ => (Block::Paragraph(tail.clone()), 0),
        };
        /* Splice the final block. When the last input block was a
        table, `last_block` is the synthesised trailing paragraph the
        match above produced and `after_path` already points to that
        table; otherwise `after_path` still points to the merged
        head+first paragraph (or the last middle block) and we
        append the merged last+tail behind it. */
        insert_block_after_path_in_top(&mut blocks, &after_path, last_block);
        after_path = bump_last_block_index(&after_path);
        let caret = LogicalPos {
            path: after_path,
            offset: caret_offset,
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
                settings: self.settings.clone(),
                styles: self.styles.clone(),
                style_defaults: self.style_defaults.clone(),
                style_run_defaults: self.style_run_defaults.clone(),
                styles_dirty: self.styles_dirty,
                numbering: self.numbering.clone(),
            }
            .with_list_markers_refreshed(),
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
        /* OOXML mandates a `<w:p>` after every `<w:tbl>` boundary
        (the body's last child must be a paragraph). Beyond spec
        compliance, the trailing paragraph is the caret's escape
        hatch — without one, pressing Down at the bottom row has
        nowhere to go and traps the caret inside the table. Splice
        one in unless the next block is already a Paragraph. */
        let needs_trailing = blocks
            .get(insert_at + 1)
            .is_none_or(|b| !matches!(b, Block::Paragraph(_)));
        if needs_trailing {
            blocks.insert(insert_at + 1, Block::Paragraph(Paragraph::default()));
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
            settings: self.settings.clone(),
            styles: self.styles.clone(),
            style_defaults: self.style_defaults.clone(),
            style_run_defaults: self.style_run_defaults.clone(),
            styles_dirty: self.styles_dirty,
            numbering: self.numbering.clone(),
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
            settings: self.settings.clone(),
            styles: self.styles.clone(),
            style_defaults: self.style_defaults.clone(),
            style_run_defaults: self.style_run_defaults.clone(),
            styles_dirty: self.styles_dirty,
            numbering: self.numbering.clone(),
        }
    }

    /// Insert a fresh row at `at` (`at` is the index the new row will
    /// occupy; existing rows at that index and below shift down). When
    /// `at >= row_count` the row is appended. Cell count matches the
    /// existing rows' cell count.
    ///
    /// Sprint 2 (UI Edition) hotfix: caller chooses Before vs After
    /// at the bridge boundary (see [`bridge::InsertSide`]); the engine
    /// receives a single resolved insert position in `usize` and
    /// performs no signed arithmetic of its own.
    pub fn insert_row(&self, table_path: BlockPath, at: usize) -> Self {
        self.mutate_table(table_path, |t| {
            /* Prefer the grid width: a merged first row has FEWER cells
            than the table has logical columns, and a fresh row must
            always come in unmerged at full width (Word behaviour). */
            let cols = if t.grid.is_empty() {
                t.rows.first().map(|r| r.cells.len()).unwrap_or(1)
            } else {
                t.grid.len()
            };
            let new_row = TableRow {
                props: RowProperties::default(),
                cells: (0..cols).map(|_| default_table_cell()).collect(),
            };
            let insert_at = at.min(t.rows.len());
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

    /// Insert a column at `at` (new column occupies that index; rows
    /// to the right shift). When `at >= column_count` the column is
    /// appended. Sprint 2 (UI Edition) hotfix — same rationale as
    /// [`Self::insert_row`].
    pub fn insert_column(&self, table_path: BlockPath, at: usize) -> Self {
        self.mutate_table(table_path, |t| {
            let insert_at = at.min(t.grid.len());
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
            /* Vertical: rows r0+1..=r1 collapse to Word's on-disk shape —
            ONE cell per continuation row spanning the merged columns
            (`gridSpan = span`, `vMerge` continue), horizontal partners
            physically removed exactly like the top row. Anything else
            double-counts grid columns in the layout cursor walk and
            diverges from what the .docx reader produces for the same
            merge authored in Word. */
            for r in (r0 + 1)..=r1.min(rcount - 1) {
                let row = &mut t.rows[r as usize];
                if (c0 as usize) >= row.cells.len() {
                    continue;
                }
                row.cells[c0 as usize].props.v_merge = VMergeRole::Continue;
                row.cells[c0 as usize].props.grid_span = span.max(1);
                let drop_count = (c1.min(row.cells.len() as u32 - 1) - c0) as usize;
                for _ in 0..drop_count {
                    if (c0 as usize + 1) < row.cells.len() {
                        row.cells.remove(c0 as usize + 1);
                    }
                }
            }
        })
    }

    /// Inverse of [`merge_cells`]: reset the owner's spans and physically
    /// restore the horizontally-merged-away partner cells (fresh default
    /// cells — Word keeps the merged content in the first cell), then walk
    /// the vertical continuation run below the owner and restore each of
    /// those rows the same way. Continuation cells are matched by their
    /// starting *grid column* (cursor walk over per-row `grid_span`s), not
    /// by cell index — preceding cells in a row may themselves span.
    pub fn split_cell(&self, table_path: BlockPath, row: u32, col: u32) -> Self {
        fn restore_row(row: &mut TableRow, idx: usize, span: usize) {
            row.cells[idx].props.grid_span = 1;
            row.cells[idx].props.v_merge = VMergeRole::None;
            for k in 0..span.saturating_sub(1) {
                row.cells.insert(idx + 1 + k, default_table_cell());
            }
        }
        /* Cell index in `row` whose starting grid column == `grid_col`. */
        fn cell_at_grid_col(row: &TableRow, grid_col: usize) -> Option<usize> {
            let mut cursor = 0usize;
            for (i, c) in row.cells.iter().enumerate() {
                match cursor.cmp(&grid_col) {
                    std::cmp::Ordering::Equal => return Some(i),
                    std::cmp::Ordering::Greater => return None,
                    std::cmp::Ordering::Less => cursor += c.props.grid_span.max(1) as usize,
                }
            }
            None
        }
        self.mutate_table(table_path, |t| {
            let Some(owner_row) = t.rows.get(row as usize) else {
                return;
            };
            let Some(owner) = owner_row.cells.get(col as usize) else {
                return;
            };
            let span = owner.props.grid_span.max(1) as usize;
            let was_restart = matches!(owner.props.v_merge, VMergeRole::Restart);
            let owner_grid_col: usize = owner_row.cells[..col as usize]
                .iter()
                .map(|c| c.props.grid_span.max(1) as usize)
                .sum();
            restore_row(&mut t.rows[row as usize], col as usize, span);
            if was_restart {
                for r in (row as usize + 1)..t.rows.len() {
                    let Some(i) = cell_at_grid_col(&t.rows[r], owner_grid_col) else {
                        break;
                    };
                    if !matches!(t.rows[r].cells[i].props.v_merge, VMergeRole::Continue) {
                        break;
                    }
                    restore_row(&mut t.rows[r], i, span);
                }
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
            settings: self.settings.clone(),
            styles: self.styles.clone(),
            style_defaults: self.style_defaults.clone(),
            style_run_defaults: self.style_run_defaults.clone(),
            styles_dirty: self.styles_dirty,
            numbering: self.numbering.clone(),
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

/* ---- Sprint 9 plain-text flattening helpers ------------------------- */

/// Flatten one paragraph's text + inline objects into the running plain-
/// text buffer (no trailing newline). U+FFFC anchors render as marker
/// strings so the caller never silently drops an image / footnote ref.
fn push_paragraph_plain(p: &Paragraph, out: &mut String) {
    let mut cursor: usize = 0;
    for obj in &p.inline_objects {
        let at = obj.at as usize;
        if at > p.text.len() {
            break;
        }
        if at > cursor {
            out.push_str(&p.text[cursor..at]);
        }
        match &obj.kind {
            InlineKind::Image { .. } => out.push_str("[image]"),
            InlineKind::FootnoteRef { display_number, .. } => {
                out.push_str(&format!("[footnote {display_number}]"));
            }
        }
        /* Skip the 3-byte U+FFFC sentinel. */
        cursor = at.saturating_add(3).min(p.text.len());
    }
    if cursor < p.text.len() {
        out.push_str(&p.text[cursor..]);
    }
}

/// Flatten a table — one tab-separated row per `TableRow`, with a
/// leading + trailing blank line so the surrounding paragraphs do not
/// glue against the table. `VMergeRole::Continue` cells emit an empty
/// column so the row width matches the document grid.
fn push_table_plain(t: &Table, out: &mut String) {
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    for row in &t.rows {
        let mut first = true;
        for cell in &row.cells {
            if !first {
                out.push('\t');
            }
            first = false;
            if cell.props.v_merge == VMergeRole::Continue {
                continue;
            }
            let cell_text = flatten_blocks_plain(&cell.blocks);
            /* Within a cell, newlines + tabs would break the row layout —
            collapse them to spaces. */
            for ch in cell_text.chars() {
                match ch {
                    '\n' | '\r' | '\t' => out.push(' '),
                    other => out.push(other),
                }
            }
        }
        out.push('\n');
    }
}

fn flatten_blocks_plain(blocks: &[Block]) -> String {
    let mut s = String::new();
    for (i, b) in blocks.iter().enumerate() {
        match b {
            Block::Paragraph(p) => {
                if i > 0 {
                    s.push('\n');
                }
                push_paragraph_plain(p, &mut s);
            }
            Block::Table(inner) => push_table_plain(inner, &mut s),
        }
    }
    s
}

/// Sprint 12 (#11) — depth cap on `<w:basedOn>` style chains, matching
/// ECMA-376 §17.7.4.5 implementation guidance. Anything beyond is
/// almost certainly a malformed stylesheet; we silently clamp.
pub const MAX_STYLE_CHAIN: usize = 10;

/// Sprint 12 (#11) — recompute the resolved `props` view on a single
/// paragraph from its `style_id` cascade ∪ `direct_overrides`.
/// `style_defaults` is the document's `<w:docDefaults>` snapshot
/// (sits at the bottom of every cascade).
/// Issue #29 — free-fn form of [`DocumentTree::resolve_style_run_cascade`]
/// for callers holding only the style table (span materialization in
/// engine-wasm). Same cycle / depth guards as the para half.
pub fn resolve_run_cascade(
    styles: &std::collections::HashMap<String, ParagraphStyle>,
    run_defaults: &SpanStyle,
    style_id: Option<&str>,
) -> SpanStyle {
    let mut out = run_defaults.clone();
    let Some(leaf) = style_id else {
        return out;
    };
    let mut visited: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut chain: Vec<&ParagraphStyle> = Vec::new();
    let mut current: Option<&str> = Some(leaf);
    while let Some(id) = current {
        if chain.len() >= MAX_STYLE_CHAIN || !visited.insert(id) {
            break;
        }
        let Some(def) = styles.get(id) else {
            break;
        };
        chain.push(def);
        current = def.based_on.as_deref();
    }
    for def in chain.iter().rev() {
        out = out.merged_with(def.run.clone());
    }
    out
}

pub fn recompute_paragraph_props(
    para: &mut Paragraph,
    styles: &std::collections::HashMap<String, ParagraphStyle>,
    style_defaults: &ParaProperties,
) {
    let mut resolved = style_defaults.clone();
    /* Walk the style chain leaf → root with the same cycle / depth
    guard as `DocumentTree::resolve_style_cascade` (kept here as a
    free fn so callers without a borrowed `DocumentTree` can still
    invoke it — e.g. the reader's per-paragraph fold loop). */
    if let Some(leaf) = para.style_id.as_deref() {
        let mut visited: std::collections::HashSet<&str> = std::collections::HashSet::new();
        let mut chain: Vec<&ParagraphStyle> = Vec::new();
        let mut current: Option<&str> = Some(leaf);
        while let Some(id) = current {
            if chain.len() >= MAX_STYLE_CHAIN || !visited.insert(id) {
                break;
            }
            let Some(def) = styles.get(id) else {
                break;
            };
            chain.push(def);
            current = def.based_on.as_deref();
        }
        for def in chain.iter().rev() {
            resolved = resolved.merged_with(def.para.clone());
        }
    }
    para.props = resolved.merged_with(para.direct_overrides.clone());
}

/// Sprint 11 — UAX-#29 word count for one paragraph's text. Shares
/// the `WordSegmenter::new_auto` thread-local with the line-break
/// path so the icu data tables compile in exactly once; subsequent
/// calls are pure boundary walks.
fn count_uax_words(text: &str) -> usize {
    use icu_segmenter::options::WordBreakInvariantOptions;
    use icu_segmenter::{WordSegmenter, WordSegmenterBorrowed};
    thread_local! {
        static SEGMENTER: WordSegmenterBorrowed<'static> =
            WordSegmenter::new_auto(WordBreakInvariantOptions::default());
    }
    SEGMENTER.with(|seg| {
        seg.segment_str(text)
            .iter_with_word_type()
            .filter(|(_, ty)| ty.is_word_like())
            .count()
    })
}

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

/// Sprint 8 (UI Edition) helper — depth-first walk over every
/// `Block::Paragraph` in the tree, including paragraphs nested
/// inside `Block::Table` cells. Used by the count-style helpers
/// that need to visit every text-bearing node regardless of
/// container.
fn walk_paragraphs<F: FnMut(&Paragraph)>(blocks: &Vector<Block>, f: &mut F) {
    for b in blocks.iter() {
        match b {
            Block::Paragraph(p) => f(p),
            Block::Table(t) => {
                for row in &t.rows {
                    for cell in &row.cells {
                        for nested in &cell.blocks {
                            match nested {
                                Block::Paragraph(p) => f(p),
                                Block::Table(_) => {
                                    /* Nested tables — uncommon;
                                     * defer until a real corpus
                                     * exercises them. */
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Sprint 7 (UI Edition) helper — shift every byte-offset-bearing
/// field on `para` LEFT by `removed_len`, for every value at or
/// after `from`. Mirrors the rightward shift performed by
/// `insert_inline_image_at` in reverse. Used when a tracked-change
/// revision is rejected (Insert) or accepted (Delete) and the
/// covered text range is sliced out.
fn shift_paragraph_offsets_after(para: &mut Paragraph, from: u32, removed_len: u32) {
    let shift = |v: &mut u32| {
        if *v >= from + removed_len {
            *v -= removed_len;
        } else if *v > from {
            *v = from;
        }
    };
    for s in &mut para.spans {
        shift(&mut s.start);
        shift(&mut s.end);
    }
    para.spans.retain(|s| s.start < s.end);
    for io in &mut para.inline_objects {
        shift(&mut io.at);
    }
    for h in &mut para.hyperlinks {
        shift(&mut h.start);
        shift(&mut h.end);
    }
    para.hyperlinks.retain(|h| h.start < h.end);
    for r in &mut para.revisions {
        shift(&mut r.start);
        shift(&mut r.end);
    }
    para.revisions.retain(|r| r.start < r.end);
    for f in &mut para.fields {
        shift(&mut f.start);
        shift(&mut f.end);
    }
    para.fields.retain(|f| f.start < f.end);
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

    /* ---- issue #23: dynamic, string-backed FontFamily ------------- */

    #[test]
    fn font_family_named_ids_and_display_names() {
        assert_eq!(FontFamily::Amiri.id(), "amiri");
        assert_eq!(FontFamily::LiberationSans.id(), "liberation");
        assert_eq!(FontFamily::NotoNaskhArabic.id(), "noto-naskh");
        assert_eq!(FontFamily::Amiri.display_name(), "Amiri");
        assert_eq!(FontFamily::LiberationSans.display_name(), "Liberation Sans");
        assert_eq!(
            FontFamily::NotoNaskhArabic.display_name(),
            "Noto Naskh Arabic"
        );
    }

    #[test]
    fn font_family_from_id_resolves_named_and_custom() {
        assert_eq!(FontFamily::from_id("amiri"), Some(FontFamily::Amiri));
        assert_eq!(
            FontFamily::from_id("noto-naskh"),
            Some(FontFamily::NotoNaskhArabic)
        );
        // Unknown id → Custom, display humanized from the id.
        assert_eq!(
            FontFamily::from_id("cairo"),
            Some(FontFamily::Custom {
                id: "cairo".into(),
                display: "Cairo".into()
            })
        );
        assert_eq!(
            FontFamily::from_id("times-new-roman"),
            Some(FontFamily::Custom {
                id: "times-new-roman".into(),
                display: "Times New Roman".into()
            })
        );
        assert_eq!(FontFamily::from_id(""), None);
        assert_eq!(FontFamily::from_id("   "), None);
    }

    #[test]
    fn font_family_from_display_name_resolves_named_and_custom() {
        assert_eq!(
            FontFamily::from_display_name("Liberation Sans"),
            Some(FontFamily::LiberationSans)
        );
        assert_eq!(
            FontFamily::from_display_name("\"Amiri\""),
            Some(FontFamily::Amiri)
        );
        // Unknown display name → Custom, verbatim display + slugified id.
        assert_eq!(
            FontFamily::from_display_name("Cairo"),
            Some(FontFamily::Custom {
                id: "cairo".into(),
                display: "Cairo".into()
            })
        );
        // Verbatim display is preserved even when slugify→humanize would not
        // recover the original casing — this is what guards docx byte-identity.
        let dejavu = FontFamily::from_display_name("DejaVu Sans").unwrap();
        assert_eq!(dejavu.display_name(), "DejaVu Sans");
        assert_eq!(dejavu.id(), "dejavu-sans");
        assert_eq!(FontFamily::from_display_name("   "), None);
    }

    #[test]
    fn font_family_from_display_name_preserves_verbatim_display() {
        // The display is stored VERBATIM (surrounding whitespace + quotes
        // intact) while the resolution id is derived from the trimmed/unquoted
        // key. This is what guards .docx `<w:rFonts>` byte-identity — the docx
        // reader passes the raw attribute value and must not see it normalized.
        let f = FontFamily::from_display_name("Calibri ").unwrap();
        assert_eq!(f.display_name(), "Calibri ", "trailing space must survive");
        assert_eq!(f.id(), "calibri", "id is the slugified, trimmed key");

        // A name that literally contains quote characters is preserved too.
        let q = FontFamily::from_display_name("\"Weird\" Font").unwrap();
        assert_eq!(q.display_name(), "\"Weird\" Font");
        // Leading/trailing quote+space are stripped only for the match key.
        let amiri = FontFamily::from_display_name("  \"Amiri\"  ").unwrap();
        assert_eq!(amiri, FontFamily::Amiri);
    }

    #[test]
    fn font_family_custom_id_display_round_trip_via_methods() {
        // A custom face's id and display survive the accessor methods intact —
        // the layout/render boundary reads id(), the docx/CSS writer reads
        // display_name().
        let f = FontFamily::Custom {
            id: "cairo".into(),
            display: "Cairo".into(),
        };
        assert_eq!(FontFamily::from_id(f.id()), Some(f.clone()));
    }

    /* ---- Sprint 9: plain-text flattening -------------------------- */

    #[test]
    fn plain_text_two_paragraphs() {
        let d = DocumentTree::from_text("hello");
        let d = d.split_paragraph(LogicalPos {
            path: BlockPath::top(0),
            offset: 5,
        });
        let d = d.insert_text(
            LogicalPos {
                path: BlockPath::top(1),
                offset: 0,
            },
            "world",
        );
        assert_eq!(d.to_plain_text(), "hello\nworld");
    }

    #[test]
    fn plain_text_image_marker_replaces_sentinel() {
        let mut d = DocumentTree::default();
        d.blocks.push_back(Block::Paragraph(Paragraph {
            text: "a\u{FFFC}b".into(),
            inline_objects: vec![InlineObject {
                at: 1,
                kind: InlineKind::Image {
                    rel_id: "rId1".into(),
                    width_emu: 0,
                    height_emu: 0,
                },
            }],
            ..Default::default()
        }));
        assert_eq!(d.to_plain_text(), "a[image]b");
    }

    #[test]
    fn plain_text_table_tab_separates_cells() {
        let mut d = DocumentTree::default();
        let cell = |s: &str| TableCell {
            props: CellProperties::default(),
            blocks: vec![Block::Paragraph(Paragraph {
                text: s.into(),
                ..Default::default()
            })],
        };
        d.blocks.push_back(Block::Table(Table {
            grid: vec![6765, 6765],
            props: TableProperties::default(),
            rows: vec![
                TableRow {
                    props: RowProperties::default(),
                    cells: vec![cell("a"), cell("b")],
                },
                TableRow {
                    props: RowProperties::default(),
                    cells: vec![cell("c"), cell("d")],
                },
            ],
            dirty: true,
            source_xml: None,
        }));
        assert_eq!(d.to_plain_text(), "a\tb\nc\td");
    }

    /* ---- Sprint 12 (#11): style cascade + shadow direct_overrides --- */

    /// Issue #21 — ModifyStyle mutates the definition and re-cascades
    /// every dependent paragraph, preserving direct overrides; #29 —
    /// the run half resolves through the chain.
    #[test]
    fn modify_style_recascades_and_flips_dirty() {
        let d = doc_with_heading_style();
        let start = LogicalPos::new(BlockPath::top(0), 0);
        let end = LogicalPos::new(BlockPath::top(0), 5);
        let d = d.set_paragraph_style(start, end, Some("Heading1".into()));
        assert_eq!(
            d.blocks[0].as_paragraph().unwrap().props.alignment,
            Some(Alignment::Center)
        );
        assert!(!d.styles_dirty, "reads never flip the writer gate");
        let d = d.modify_style(
            "Heading1",
            Some(ParaProperties {
                alignment: Some(Alignment::End),
                ..Default::default()
            }),
            Some(SpanStyle {
                bold: Some(true),
                font_size: Some(16.0),
                ..Default::default()
            }),
            None,
            None,
        );
        assert!(d.styles_dirty, "ModifyStyle must arm styles.xml regen");
        let p = d.blocks[0].as_paragraph().unwrap();
        assert_eq!(
            p.props.alignment,
            Some(Alignment::End),
            "dependent paragraph re-cascaded"
        );
        let run = d.resolve_style_run_cascade(Some("Heading1"));
        assert_eq!(run.bold, Some(true));
        assert_eq!(run.font_size, Some(16.0));
        /* Unknown ids are a no-op clone. */
        let same = d.modify_style("Nope", None, None, None, None);
        assert_eq!(same.styles.len(), d.styles.len());
    }

    /// Issue #21 — a direct override survives a style mutation (the #11
    /// re-application discipline).
    #[test]
    fn modify_style_preserves_direct_overrides() {
        let d = doc_with_heading_style();
        let start = LogicalPos::new(BlockPath::top(0), 0);
        let end = LogicalPos::new(BlockPath::top(0), 5);
        let d = d.set_paragraph_style(start.clone(), end.clone(), Some("Heading1".into()));
        /* User sets an explicit alignment on top of the style. */
        let mut d = d;
        {
            let mut blocks = d.blocks.clone();
            let mut b = blocks[0].clone();
            if let Block::Paragraph(p) = &mut b {
                p.direct_overrides.alignment = Some(Alignment::Justify);
                recompute_paragraph_props(p, &d.styles, &d.style_defaults);
            }
            blocks.set(0, b);
            d.blocks = blocks;
        }
        let d = d.modify_style(
            "Heading1",
            Some(ParaProperties {
                alignment: Some(Alignment::End),
                ..Default::default()
            }),
            None,
            None,
            None,
        );
        assert_eq!(
            d.blocks[0].as_paragraph().unwrap().props.alignment,
            Some(Alignment::Justify),
            "direct override outranks the mutated style"
        );
    }

    fn doc_with_heading_style() -> DocumentTree {
        let mut d = DocumentTree::from_text("hello");
        d.styles.insert(
            "Heading1".into(),
            ParagraphStyle {
                id: "Heading1".into(),
                name: "Heading 1".into(),
                based_on: None,
                para: ParaProperties {
                    alignment: Some(Alignment::Center),
                    ..Default::default()
                },
                run: SpanStyle::default(),
            },
        );
        d
    }

    #[test]
    fn apply_style_sets_style_id_and_props() {
        let d = doc_with_heading_style();
        let p0 = LogicalPos {
            path: BlockPath::top(0),
            offset: 0,
        };
        let d = d.set_paragraph_style(p0.clone(), p0, Some("Heading1".into()));
        let p = d.nth_paragraph(0).expect("paragraph 0");
        assert_eq!(p.style_id.as_deref(), Some("Heading1"));
        assert_eq!(
            p.props.alignment,
            Some(Alignment::Center),
            "style cascade should fold into resolved props"
        );
    }

    #[test]
    fn direct_override_survives_subsequent_style_change() {
        /* Apply a direct alignment (Right) first; then apply
        Heading1 (which sets Center). The shadow approach must keep
        Right because direct_overrides win over the style cascade. */
        let d = doc_with_heading_style();
        let p0 = LogicalPos {
            path: BlockPath::top(0),
            offset: 0,
        };
        let d = d.set_alignment(p0.clone(), p0.clone(), Alignment::End);
        let p = d.nth_paragraph(0).unwrap();
        assert_eq!(p.direct_overrides.alignment, Some(Alignment::End));
        let d = d.set_paragraph_style(p0.clone(), p0, Some("Heading1".into()));
        let p = d.nth_paragraph(0).unwrap();
        assert_eq!(p.style_id.as_deref(), Some("Heading1"));
        assert_eq!(
            p.props.alignment,
            Some(Alignment::End),
            "direct_overrides must win over style cascade"
        );
    }

    #[test]
    fn detach_style_falls_back_to_direct_overrides_only() {
        let d = doc_with_heading_style();
        let p0 = LogicalPos {
            path: BlockPath::top(0),
            offset: 0,
        };
        let d = d.set_paragraph_style(p0.clone(), p0.clone(), Some("Heading1".into()));
        assert_eq!(
            d.nth_paragraph(0).unwrap().props.alignment,
            Some(Alignment::Center)
        );
        let d = d.set_paragraph_style(p0.clone(), p0, None);
        assert_eq!(d.nth_paragraph(0).unwrap().style_id, None);
        assert_eq!(
            d.nth_paragraph(0).unwrap().props.alignment,
            None,
            "detached style + no direct_overrides → default"
        );
    }

    #[test]
    fn style_cascade_walks_based_on_chain() {
        let mut d = DocumentTree::from_text("x");
        d.styles.insert(
            "Base".into(),
            ParagraphStyle {
                id: "Base".into(),
                name: "Base".into(),
                based_on: None,
                para: ParaProperties {
                    alignment: Some(Alignment::Center),
                    ..Default::default()
                },
                run: SpanStyle::default(),
            },
        );
        d.styles.insert(
            "Child".into(),
            ParagraphStyle {
                id: "Child".into(),
                name: "Child".into(),
                based_on: Some("Base".into()),
                /* Child contributes direction; alignment inherits from Base. */
                para: ParaProperties {
                    direction: Some(TextDirection::Rtl),
                    ..Default::default()
                },
                run: SpanStyle::default(),
            },
        );
        let p0 = LogicalPos {
            path: BlockPath::top(0),
            offset: 0,
        };
        let d = d.set_paragraph_style(p0.clone(), p0, Some("Child".into()));
        let p = d.nth_paragraph(0).unwrap();
        assert_eq!(p.props.alignment, Some(Alignment::Center));
        assert_eq!(p.props.direction, Some(TextDirection::Rtl));
    }

    /* ---- Sprint 14 (#14): track-changes recording ----------------- */

    fn tracked_doc() -> DocumentTree {
        DocumentTree::from_text("hello")
    }

    fn pos0(off: u32) -> LogicalPos {
        LogicalPos {
            path: BlockPath::top(0),
            offset: off,
        }
    }

    #[test]
    fn tracked_insert_outside_revision_adds_insert_revision() {
        let d = tracked_doc();
        let d = d.tracked_insert_text(pos0(0), "X", "Alice".into(), "2026-01-01".into());
        let p = d.nth_paragraph(0).unwrap();
        assert_eq!(p.text, "Xhello");
        assert_eq!(p.revisions.len(), 1);
        assert_eq!(p.revisions[0].kind, RevisionKind::Insert);
        assert_eq!(p.revisions[0].start, 0);
        assert_eq!(p.revisions[0].end, 1);
        assert_eq!(p.revisions[0].author, "Alice");
    }

    #[test]
    fn tracked_insert_grows_adjacent_same_author_insert() {
        let d = tracked_doc();
        let d = d.tracked_insert_text(pos0(5), "A", "Alice".into(), "t1".into());
        /* Cursor now at offset 6; same author types another char. */
        let d = d.tracked_insert_text(pos0(6), "B", "Alice".into(), "t2".into());
        let p = d.nth_paragraph(0).unwrap();
        assert_eq!(p.text, "helloAB");
        assert_eq!(
            p.revisions.len(),
            1,
            "adjacent same-author Inserts must merge — got {:?}",
            p.revisions
        );
        assert_eq!(p.revisions[0].start, 5);
        assert_eq!(p.revisions[0].end, 7);
    }

    #[test]
    fn tracked_insert_inside_existing_insert_grows_it_no_new_revision() {
        let d = tracked_doc();
        let d = d.tracked_insert_text(pos0(5), "AAA", "Alice".into(), "t1".into());
        /* Type INSIDE the Insert at offset 6 (between AAA). */
        let d = d.tracked_insert_text(pos0(6), "Z", "Alice".into(), "t2".into());
        let p = d.nth_paragraph(0).unwrap();
        assert_eq!(p.text, "helloAZAA");
        assert_eq!(
            p.revisions.len(),
            1,
            "inside-Insert keystroke must grow the Insert, not split"
        );
        assert_eq!(p.revisions[0].kind, RevisionKind::Insert);
        assert_eq!(p.revisions[0].start, 5);
        assert_eq!(p.revisions[0].end, 9);
    }

    #[test]
    fn tracked_insert_inside_delete_splits_delete_and_adds_insert() {
        let d = tracked_doc();
        /* First mark "hello" entirely as a tracked Delete. */
        let d = d.tracked_delete_range(pos0(0), pos0(5), "Alice".into(), "t1".into());
        assert_eq!(
            d.nth_paragraph(0).unwrap().revisions.len(),
            1,
            "single Delete after mark"
        );
        /* Now type inside the Delete at offset 2 (between "he" and "llo"). */
        let d = d.tracked_insert_text(pos0(2), "X", "Alice".into(), "t2".into());
        let p = d.nth_paragraph(0).unwrap();
        assert_eq!(p.text, "heXllo");
        /* Expected: [0, 2) Delete (he) + [2, 3) Insert (X) + [3, 6) Delete (llo). */
        let kinds: Vec<_> = p
            .revisions
            .iter()
            .map(|r| (r.kind, r.start, r.end))
            .collect();
        assert!(
            kinds.contains(&(RevisionKind::Delete, 0, 2)),
            "left Delete half missing: {kinds:?}"
        );
        assert!(
            kinds.contains(&(RevisionKind::Delete, 3, 6)),
            "right Delete half missing: {kinds:?}"
        );
        assert!(
            kinds.contains(&(RevisionKind::Insert, 2, 3)),
            "Insert in gap missing: {kinds:?}"
        );
    }

    #[test]
    fn tracked_delete_marker_only_preserves_text() {
        let d = tracked_doc();
        let d = d.tracked_delete_range(pos0(0), pos0(3), "Alice".into(), "t1".into());
        let p = d.nth_paragraph(0).unwrap();
        /* Marker-only delete: text remains. */
        assert_eq!(p.text, "hello");
        assert_eq!(p.revisions.len(), 1);
        assert_eq!(p.revisions[0].kind, RevisionKind::Delete);
        assert_eq!(p.revisions[0].start, 0);
        assert_eq!(p.revisions[0].end, 3);
    }

    #[test]
    fn tracked_delete_inside_own_insert_shrinks_insert_removes_text() {
        let d = tracked_doc();
        /* Type 3 chars at offset 5 — Insert overlay covers [5, 8). */
        let d = d.tracked_insert_text(pos0(5), "ABC", "Alice".into(), "t1".into());
        assert_eq!(d.nth_paragraph(0).unwrap().text, "helloABC");
        /* Backspace one char (delete [7, 8)). Range is fully inside
        the same-author Insert → uninsert: text shrinks AND Insert
        end shifts left by 1. */
        let d = d.tracked_delete_range(pos0(7), pos0(8), "Alice".into(), "t2".into());
        let p = d.nth_paragraph(0).unwrap();
        assert_eq!(p.text, "helloAB");
        assert_eq!(p.revisions.len(), 1);
        assert_eq!(p.revisions[0].kind, RevisionKind::Insert);
        assert_eq!(p.revisions[0].start, 5);
        assert_eq!(p.revisions[0].end, 7);
    }

    #[test]
    fn tracked_delete_merges_adjacent_same_author_delete() {
        let d = tracked_doc();
        let d = d.tracked_delete_range(pos0(0), pos0(2), "Alice".into(), "t1".into());
        let d = d.tracked_delete_range(pos0(2), pos0(4), "Alice".into(), "t2".into());
        let p = d.nth_paragraph(0).unwrap();
        assert_eq!(
            p.revisions.len(),
            1,
            "adjacent same-author Deletes must merge"
        );
        assert_eq!(p.revisions[0].start, 0);
        assert_eq!(p.revisions[0].end, 4);
    }

    #[test]
    fn tracked_format_change_carries_prev_attrs() {
        let d = tracked_doc();
        let prev = SpanStyle {
            bold: Some(false),
            ..Default::default()
        };
        let d =
            d.tracked_format_change(pos0(0), pos0(5), prev.clone(), "Alice".into(), "t1".into());
        let p = d.nth_paragraph(0).unwrap();
        let rev = p
            .revisions
            .iter()
            .find(|r| r.kind == RevisionKind::FormatChange);
        let rev = rev.expect("FormatChange revision present");
        assert_eq!(rev.start, 0);
        assert_eq!(rev.end, 5);
        assert_eq!(rev.prev_attrs, Some(prev));
    }

    #[test]
    fn undo_of_tracked_insert_restores_snapshot_no_counter_delete() {
        /* UndoStack is snapshot-based: undo restores the prior tree,
        which had no revisions. No counter-Delete should appear. */
        let initial = tracked_doc();
        let mut stack = UndoStack::new(initial.clone(), 100);
        let after_insert = initial.tracked_insert_text(pos0(0), "X", "Alice".into(), "t1".into());
        stack.push(after_insert);
        assert_eq!(stack.current().nth_paragraph(0).unwrap().text, "Xhello");
        assert_eq!(stack.current().nth_paragraph(0).unwrap().revisions.len(), 1);
        stack.undo();
        let p = stack.current().nth_paragraph(0).unwrap();
        assert_eq!(p.text, "hello");
        assert!(
            p.revisions.is_empty(),
            "undo of tracked insert must NOT leave a counter-Delete revision"
        );
    }

    /* ---- Sprint 11 (#17): UAX-#29 word_count ---------------------- */

    #[test]
    fn word_count_latin_matches_word_like_split() {
        let d = DocumentTree::from_text("Hello, brave new world!");
        assert_eq!(d.word_count(), 4);
    }

    #[test]
    fn word_count_cjk_segments_chars() {
        /* Mandarin "我喜欢编程" (I like programming) — 5 Han chars.
        Whitespace-split would return 1; UAX-#29 with dictionary
        segmentation returns a CJK-meaningful count > 1. The exact
        count varies with the segmenter's dictionary; assert > 1 so
        the test survives dictionary updates without sacrificing
        regression coverage for "CJK reports a meaningful value". */
        let d = DocumentTree::from_text("我喜欢编程");
        assert!(
            d.word_count() > 1,
            "CJK word_count should segment, got {}",
            d.word_count()
        );
    }

    #[test]
    fn word_count_punctuation_excluded() {
        /* Five word-like tokens; commas + period must NOT count. */
        let d = DocumentTree::from_text("one, two, three, four, five.");
        assert_eq!(d.word_count(), 5);
    }

    /* ---- Sprint 10: section + cell read-back helpers --------------- */

    #[test]
    fn section_for_block_returns_default_a4_when_no_sections() {
        let d = DocumentTree::from_text("hello");
        let s = d.section_for_block(0);
        assert!((s.geometry.width - 595.3).abs() < 0.5);
        assert!((s.geometry.height - 841.9).abs() < 0.5);
        assert_eq!(s.columns.count, 1);
    }

    #[test]
    fn section_for_block_picks_matching_section() {
        let mut d = DocumentTree::from_text("a");
        let mut narrow = PageGeometry::a4();
        narrow.margin_left = 36.0;
        d.sections = vec![
            Section {
                geometry: narrow,
                start_block: 0,
                end_block: 1,
                ..Default::default()
            },
            Section {
                geometry: PageGeometry::a4(),
                start_block: 1,
                end_block: 5,
                ..Default::default()
            },
        ];
        assert!((d.section_for_block(0).geometry.margin_left - 36.0).abs() < 0.1);
        assert!((d.section_for_block(2).geometry.margin_left - 72.0).abs() < 0.1);
    }

    #[test]
    fn innermost_cell_props_returns_none_outside_table() {
        let d = DocumentTree::from_text("hello");
        let path = BlockPath::top(0);
        assert!(d.innermost_cell_props_at(&path).is_none());
    }

    #[test]
    fn innermost_cell_props_finds_cell_at_top_level_table() {
        let mut d = DocumentTree::default();
        let mut cell = TableCell {
            props: CellProperties::default(),
            blocks: vec![Block::Paragraph(Paragraph {
                text: "x".into(),
                ..Default::default()
            })],
        };
        cell.props.shading = Some([0xff, 0, 0, 0xff]);
        d.blocks.push_back(Block::Table(Table {
            grid: vec![6765],
            props: TableProperties::default(),
            rows: vec![TableRow {
                props: RowProperties::default(),
                cells: vec![cell],
            }],
            dirty: true,
            source_xml: None,
        }));
        let path = BlockPath {
            steps: vec![
                PathStep::Block(0),
                PathStep::Cell { row: 0, col: 0 },
                PathStep::Block(0),
            ],
        };
        let props = d.innermost_cell_props_at(&path).expect("cell resolved");
        assert_eq!(props.shading, Some([0xff, 0, 0, 0xff]));
    }

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
            red.clone(),
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
            big.clone(),
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
        let span = doc.nth_paragraph(0).unwrap().spans[0].clone();
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
            resolved_list_indent: None,
            dirty: false,
            source_xml: None,
            inline_objects: Vec::new(),
            hyperlinks: Vec::new(),
            revisions: Vec::new(),
            fields: Vec::new(),
            style_id: None,
            direct_overrides: ParaProperties::default(),
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
            resolved_list_indent: None,
            dirty: false,
            source_xml: None,
            inline_objects: Vec::new(),
            hyperlinks: Vec::new(),
            revisions: Vec::new(),
            fields: Vec::new(),
            style_id: None,
            direct_overrides: ParaProperties::default(),
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
            resolved_list_indent: None,
            dirty: false,
            source_xml: None,
            inline_objects: Vec::new(),
            hyperlinks: Vec::new(),
            revisions: Vec::new(),
            fields: Vec::new(),
            style_id: None,
            direct_overrides: ParaProperties::default(),
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
        /* "a"=1 byte, "م"=2 bytes, "b"=1 byte → grapheme boundaries 0,1,3,4. */
        let p = Paragraph {
            text: "aمb".into(),
            spans: Vec::new(),
            props: ParaProperties::default(),
            list_item: None,
            resolved_marker: None,
            resolved_list_indent: None,
            dirty: false,
            source_xml: None,
            inline_objects: Vec::new(),
            hyperlinks: Vec::new(),
            revisions: Vec::new(),
            fields: Vec::new(),
            style_id: None,
            direct_overrides: ParaProperties::default(),
        };
        assert_eq!(p.next_offset(0), 1);
        assert_eq!(p.next_offset(1), 3);
        assert_eq!(p.prev_offset(4), 3);
        assert_eq!(p.prev_offset(3), 1);
    }

    /// Audit gap B.H1 — `prev_offset` / `next_offset` step by UAX-#29
    /// extended grapheme cluster, not Unicode scalar. The Arabic letter
    /// `ي` plus FATHATAN diacritic `ً` forms one user-perceived
    /// character (two `char`s, four UTF-8 bytes); Backspace must
    /// remove the cluster atomically instead of leaving an orphaned
    /// combining mark behind.
    #[test]
    fn prev_next_offset_step_grapheme_cluster() {
        /* Byte map for "aيًb":
          0: 'a'                              (1 byte)
          1: 'ي'         start of cluster    (2 bytes)
          3: ARABIC FATHATAN U+064B          (2 bytes, combining)
          5: 'b'                              (1 byte)
          6: end
        The يً cluster spans bytes 1..5. */
        let p = Paragraph {
            text: "aيًb".into(),
            spans: Vec::new(),
            props: ParaProperties::default(),
            list_item: None,
            resolved_marker: None,
            resolved_list_indent: None,
            dirty: false,
            source_xml: None,
            inline_objects: Vec::new(),
            hyperlinks: Vec::new(),
            revisions: Vec::new(),
            fields: Vec::new(),
            style_id: None,
            direct_overrides: ParaProperties::default(),
        };
        /* Forward from 'a' jumps over the whole يً cluster, not just 'ي'. */
        assert_eq!(p.next_offset(1), 5, "forward must skip the FATHATAN");
        /* Backward from 'b' jumps over both letters of the cluster. */
        assert_eq!(p.prev_offset(5), 1, "backward must skip the FATHATAN");
        /* Edges still pin / clamp. */
        assert_eq!(p.prev_offset(0), 0);
        assert_eq!(p.next_offset(6), 6);
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
                underline: Some(UnderlineStyle::Single),
                ..Default::default()
            },
        );
        let style = doc.nth_paragraph(0).unwrap().style_at(2);
        assert_eq!(style.bold, Some(true));
        assert_eq!(style.italic, Some(true));
        assert_eq!(style.underline, Some(UnderlineStyle::Single));
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
    fn set_direction_marks_spanned_paragraphs() {
        let d = DocumentTree::from_paragraphs(["a".into(), "b".into(), "c".into()]);
        let d = d.set_direction(
            LogicalPos {
                path: BlockPath::top(0),
                offset: 0,
            },
            LogicalPos {
                path: BlockPath::top(1),
                offset: 0,
            },
            TextDirection::Rtl,
        );
        assert_eq!(
            d.nth_paragraph(0).unwrap().props.direction,
            Some(TextDirection::Rtl)
        );
        assert_eq!(
            d.nth_paragraph(1).unwrap().props.direction,
            Some(TextDirection::Rtl)
        );
        /* outside the range — untouched */
        assert_eq!(d.nth_paragraph(2).unwrap().props.direction, None);
    }

    #[test]
    fn set_paragraph_indent_allows_negative_outdent() {
        /* Bug B — negative `<w:start>` / `<w:end>` are first-class outdents
        (ECMA-376 ST_SignedTwipsMeasure). The `.max(0.0)` floor used to
        clobber them to zero, making the grey-margin drag a no-op. */
        let d = DocumentTree::from_paragraphs(["a".into(), "b".into()]);
        let d = d.set_paragraph_indent(
            LogicalPos {
                path: BlockPath::top(0),
                offset: 0,
            },
            LogicalPos {
                path: BlockPath::top(0),
                offset: 0,
            },
            -18.0, // start: 18 pt outdent
            -6.0,  // end: 6 pt outdent
            -12.0, // first-line: negative ⇒ hanging
        );
        let ind = d.nth_paragraph(0).unwrap().props.indent;
        assert_eq!(ind.start_twips, -360, "negative start survives (−18 pt)");
        assert_eq!(ind.end_twips, -120, "negative end survives (−6 pt)");
        /* first_line stays signed only through the firstLine/hanging split:
        the magnitude is stored non-negative in `hanging_twips`. */
        assert_eq!(ind.first_line_twips, 0);
        assert_eq!(
            ind.hanging_twips, 240,
            "negative first-line ⇒ hanging (12 pt)"
        );
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
                style: bold.clone(),
            }],
            props: ParaProperties::default(),
            list_item: None,
            resolved_marker: None,
            resolved_list_indent: None,
            dirty: false,
            source_xml: None,
            inline_objects: Vec::new(),
            hyperlinks: Vec::new(),
            revisions: Vec::new(),
            fields: Vec::new(),
            style_id: None,
            direct_overrides: ParaProperties::default(),
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
            resolved_list_indent: None,
            dirty: false,
            source_xml: None,
            inline_objects: Vec::new(),
            hyperlinks: Vec::new(),
            revisions: Vec::new(),
            fields: Vec::new(),
            style_id: None,
            direct_overrides: ParaProperties::default(),
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
                resolved_list_indent: None,
                dirty: false,
                source_xml: None,
                inline_objects: Vec::new(),
                hyperlinks: Vec::new(),
                revisions: Vec::new(),
                fields: Vec::new(),
                style_id: None,
                direct_overrides: ParaProperties::default(),
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
                resolved_list_indent: None,
                dirty: false,
                source_xml: None,
                inline_objects: Vec::new(),
                hyperlinks: Vec::new(),
                revisions: Vec::new(),
                fields: Vec::new(),
                style_id: None,
                direct_overrides: ParaProperties::default(),
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
        /* 3 blocks: "hello" + table + auto-appended trailing empty
        paragraph (the OOXML-mandated escape paragraph). */
        assert_eq!(d.blocks.len(), 3);
        let t = d.blocks[1].as_table().expect("Block::Table");
        assert_eq!(t.rows.len(), 2);
        assert_eq!(t.rows[0].cells.len(), 3);
        assert_eq!(t.grid.len(), 3);
        assert!(t.dirty, "synthesised tables must regen on save");
        assert!(t.source_xml.is_none());
        assert!(
            d.blocks[2]
                .as_paragraph()
                .is_some_and(|p| p.text.is_empty()),
            "trailing escape paragraph"
        );
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

    /// Sprint 2 (UI Edition) hotfix — prepending a row at index 0
    /// must NOT underflow the wire `u32` or be silently coerced to
    /// "insert after row 0". `insert_row(path, 0)` lands the new row
    /// at index 0; the original row shifts to index 1.
    /// Issue #25 — `set_line_spacing` stores Word's Auto multiplier as
    /// 240-ths twips on props AND direct_overrides; `<= 0` clears.
    #[test]
    fn set_line_spacing_stores_auto_multiplier_and_clears() {
        let d = DocumentTree::from_text("hi");
        let start = LogicalPos::new(BlockPath::top(0), 0);
        let end = LogicalPos::new(BlockPath::top(0), 2);
        let d = d.set_line_spacing(start.clone(), end.clone(), 1.15);
        let p = d.blocks[0].as_paragraph().unwrap();
        assert_eq!(p.props.line_height, Some(LineHeight::Auto { twips: 276 }));
        assert_eq!(
            p.direct_overrides.line_height,
            Some(LineHeight::Auto { twips: 276 })
        );
        let d = d.set_line_spacing(start, end, 0.0);
        assert_eq!(d.blocks[0].as_paragraph().unwrap().props.line_height, None);
    }

    /// Issue #50 — ToggleList must stamp BOTH the marker text and the
    /// numbering level's indent; the indent is what layout consumes to
    /// park the bullet in the hanging gutter instead of underneath the
    /// first text glyph.
    #[test]
    fn toggle_list_stamps_marker_and_level_indent() {
        let d = DocumentTree::from_text("alpha");
        let pos = LogicalPos::new(BlockPath::top(0), 0);
        let d = d.toggle_list_on_range(pos.clone(), pos, numbering::ListSynthesisKind::Bullet);
        let p = d.blocks[0].as_paragraph().unwrap();
        assert_eq!(p.resolved_marker.as_deref(), Some("\u{2022}"));
        assert_eq!(
            p.resolved_list_indent,
            Some(Indent {
                start_twips: 720,
                end_twips: 0,
                first_line_twips: 0,
                hanging_twips: 360,
            }),
            "level-0 stock indent must ride along with the marker"
        );
    }

    /// Issue #50 — Enter inside a numbered list renumbers the tail;
    /// `Paragraph::split_at` alone clones "1." onto both halves.
    #[test]
    fn split_inside_numbered_list_renumbers_the_tail() {
        let d = DocumentTree::from_text("onetwo");
        let start = LogicalPos::new(BlockPath::top(0), 0);
        let end = LogicalPos::new(BlockPath::top(0), 6);
        let d = d.toggle_list_on_range(start, end, numbering::ListSynthesisKind::Number);
        assert_eq!(
            d.blocks[0]
                .as_paragraph()
                .unwrap()
                .resolved_marker
                .as_deref(),
            Some("1.")
        );
        let d = d.split_paragraph(LogicalPos::new(BlockPath::top(0), 3));
        let markers: Vec<Option<&str>> = d
            .blocks
            .iter()
            .filter_map(|b| b.as_paragraph())
            .map(|p| p.resolved_marker.as_deref())
            .collect();
        assert_eq!(
            markers,
            vec![Some("1."), Some("2.")],
            "split must renumber, not duplicate the head's marker"
        );
    }

    /// Issue #50 — merging across the break (delete_range) and clearing
    /// list membership both re-resolve the remaining markers.
    #[test]
    fn merge_and_clear_renumber_following_list_items() {
        /* three numbered paragraphs: one / two / three */
        let d = DocumentTree::from_text("one");
        let d = d.split_paragraph(LogicalPos::new(BlockPath::top(0), 3));
        let d = d.insert_text(LogicalPos::new(BlockPath::top(1), 0), "two");
        let d = d.split_paragraph(LogicalPos::new(BlockPath::top(1), 3));
        let d = d.insert_text(LogicalPos::new(BlockPath::top(2), 0), "three");
        let start = LogicalPos::new(BlockPath::top(0), 0);
        let end = LogicalPos::new(BlockPath::top(2), 5);
        let d = d.toggle_list_on_range(start, end, numbering::ListSynthesisKind::Number);
        /* delete across the first paragraph break — "one" and "two"
        merge; the survivors renumber 1. / 2. */
        let d = d.delete_range(
            LogicalPos::new(BlockPath::top(0), 3),
            LogicalPos::new(BlockPath::top(1), 0),
        );
        let markers: Vec<Option<String>> = d
            .blocks
            .iter()
            .filter_map(|b| b.as_paragraph())
            .map(|p| p.resolved_marker.clone())
            .collect();
        assert_eq!(markers.len(), 2);
        assert_eq!(markers[0].as_deref(), Some("1."));
        assert_eq!(markers[1].as_deref(), Some("2."));
        /* clearing the first item renumbers the rest from 1. */
        let p0 = LogicalPos::new(BlockPath::top(0), 0);
        let d = d.clear_list_item_on_range(p0.clone(), p0);
        let p = d.blocks[0].as_paragraph().unwrap();
        assert!(p.resolved_marker.is_none() && p.resolved_list_indent.is_none());
        assert_eq!(
            d.blocks[1]
                .as_paragraph()
                .unwrap()
                .resolved_marker
                .as_deref(),
            Some("1."),
            "the surviving list item restarts at 1."
        );
    }

    #[test]
    fn insert_row_at_zero_prepends_no_underflow() {
        let d = DocumentTree::new().insert_table(BlockPath::top(0), 2, 2);
        let before = d.blocks[0].as_table().unwrap().clone();
        let d = d.insert_row(BlockPath::top(0), 0);
        let t = d.blocks[0].as_table().unwrap();
        assert_eq!(t.rows.len(), 3, "row was not inserted");
        assert_eq!(
            t.rows[1].cells.len(),
            before.rows[0].cells.len(),
            "row 1 should be the original row 0 after prepend",
        );
        /* Appending at `at == row_count` lands at the end. */
        let d = d.insert_row(BlockPath::top(0), 3);
        let t = d.blocks[0].as_table().unwrap();
        assert_eq!(t.rows.len(), 4);
    }

    /// Sprint 2 (UI Edition) hotfix — prepending a column at index 0
    /// is the mirror invariant for [`insert_row_at_zero_prepends_no_underflow`].
    #[test]
    fn insert_column_at_zero_prepends_no_underflow() {
        let d = DocumentTree::new().insert_table(BlockPath::top(0), 2, 2);
        let d = d.insert_column(BlockPath::top(0), 0);
        let t = d.blocks[0].as_table().unwrap();
        assert_eq!(t.grid.len(), 3);
        assert!(t.rows.iter().all(|r| r.cells.len() == 3));
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

    /// A rectangular merge must produce Word's on-disk shape: every
    /// member row collapses to ONE spanning cell (partners physically
    /// removed), so per-row grid-column accounting stays exact for
    /// cells to the right of the merge.
    #[test]
    fn merge_cells_rectangular_collapses_continuation_rows() {
        let d = DocumentTree::from_text("hi").insert_table(BlockPath::top(1), 3, 3);
        let d = d.merge_cells(BlockPath::top(1), 0, 0, 1, 1);
        let t = d.blocks[1].as_table().unwrap();
        assert_eq!(t.rows[0].cells.len(), 2, "top row collapses 3 → 2 cells");
        assert_eq!(t.rows[0].cells[0].props.grid_span, 2);
        assert_eq!(t.rows[0].cells[0].props.v_merge, VMergeRole::Restart);
        assert_eq!(t.rows[0].cells[1].props.grid_span.max(1), 1);
        assert_eq!(
            t.rows[1].cells.len(),
            2,
            "continuation row collapses 3 → 2 cells"
        );
        assert_eq!(t.rows[1].cells[0].props.grid_span, 2);
        assert_eq!(t.rows[1].cells[0].props.v_merge, VMergeRole::Continue);
        assert_eq!(
            t.rows[1].cells[1].props.v_merge,
            VMergeRole::None,
            "cell right of the merge is untouched"
        );
        assert_eq!(t.rows[2].cells.len(), 3, "row below the merge untouched");
    }

    /// `split_cell` is the true inverse of `merge_cells`: it restores
    /// the horizontally-removed partners in the owner row AND in every
    /// vertical continuation row, matched by starting grid column.
    #[test]
    fn split_cell_restores_merged_partners() {
        let d = DocumentTree::from_text("hi").insert_table(BlockPath::top(1), 3, 3);
        let d = d.merge_cells(BlockPath::top(1), 0, 0, 1, 1);
        let d = d.split_cell(BlockPath::top(1), 0, 0);
        let t = d.blocks[1].as_table().unwrap();
        for (r, row) in t.rows.iter().enumerate() {
            assert_eq!(row.cells.len(), 3, "row {r} must be back to 3 cells");
            for (c, cell) in row.cells.iter().enumerate() {
                assert_eq!(cell.props.grid_span.max(1), 1, "cell {r},{c} span reset");
                assert_eq!(
                    cell.props.v_merge,
                    VMergeRole::None,
                    "cell {r},{c} vMerge cleared"
                );
            }
        }
    }

    /// Horizontal-only merge + split round-trips the row shape.
    #[test]
    fn split_cell_restores_horizontal_only_merge() {
        let d = DocumentTree::from_text("hi").insert_table(BlockPath::top(1), 1, 3);
        let d = d.merge_cells(BlockPath::top(1), 0, 0, 0, 2);
        assert_eq!(d.blocks[1].as_table().unwrap().rows[0].cells.len(), 1);
        let d = d.split_cell(BlockPath::top(1), 0, 0);
        let t = d.blocks[1].as_table().unwrap();
        assert_eq!(t.rows[0].cells.len(), 3);
        assert!(
            t.rows[0]
                .cells
                .iter()
                .all(|c| c.props.grid_span.max(1) == 1 && c.props.v_merge == VMergeRole::None)
        );
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
            settings: DocumentSettings::default(),
            styles: std::collections::HashMap::new(),
            style_defaults: ParaProperties::default(),
            style_run_defaults: SpanStyle::default(),
            styles_dirty: false,
            numbering: numbering::NumberingDefinitions::default(),
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
        /* 3 blocks: "before" + table + auto-trailing empty paragraph
        (the OOXML-mandated escape paragraph `insert_table` appends). */
        assert_eq!(d.blocks.len(), 3);
        let d = d.delete_table(BlockPath::top(1));
        /* 2 blocks remain: "before" + the trailing empty paragraph
        (delete_table removes only the Table block at idx 1). */
        assert_eq!(d.blocks.len(), 2);
        assert!(d.blocks[0].as_paragraph().is_some());
        assert!(
            d.blocks[1]
                .as_paragraph()
                .is_some_and(|p| p.text.is_empty())
        );
    }

    /* ---- issue #27: threaded comment replies ----------------------- */

    /// Anchor a top-level comment on a non-trivial range so the reply's
    /// cloned range is distinguishable from a default.
    fn doc_with_comment() -> (DocumentTree, u32) {
        let doc = DocumentTree::from_text("hello world");
        doc.insert_comment(
            LogicalPos::new(BlockPath::top(0), 2),
            LogicalPos::new(BlockPath::top(0), 7),
            "root comment".into(),
            "Alice".into(),
            "2026-07-01T00:00:00Z".into(),
        )
    }

    #[test]
    fn reply_to_comment_mints_id_sets_parent_and_clones_range() {
        let (doc, parent) = doc_with_comment();
        let (doc, reply) = doc
            .reply_to_comment(
                parent,
                "reply body".into(),
                "Bob".into(),
                "2026-07-02T00:00:00Z".into(),
            )
            .expect("parent exists");
        assert_eq!(reply, parent + 1, "next sequential id minted");
        let def = doc.comment_defs.get(&reply).expect("reply def");
        assert_eq!(def.parent_id, Some(parent));
        assert_eq!(def.paragraphs, vec!["reply body".to_string()]);
        assert_eq!(def.author, "Bob");
        assert!(!def.resolved);
        assert!(def.first_para_id.is_none());
        /* The reply's range is a clone of the parent's span. */
        let pr = doc
            .comment_ranges
            .iter()
            .find(|r| r.id == parent)
            .expect("parent range");
        let rr = doc
            .comment_ranges
            .iter()
            .find(|r| r.id == reply)
            .expect("reply range");
        assert_eq!(rr.start, pr.start);
        assert_eq!(rr.end, pr.end);
    }

    #[test]
    fn reply_to_unknown_parent_returns_none() {
        let (doc, _parent) = doc_with_comment();
        assert!(
            doc.reply_to_comment(999, "x".into(), "Bob".into(), "d".into())
                .is_none()
        );
    }

    #[test]
    fn delete_comment_cascades_to_transitive_replies() {
        let (doc, parent) = doc_with_comment();
        let (doc, reply) = doc
            .reply_to_comment(parent, "reply".into(), "Bob".into(), "d".into())
            .expect("parent exists");
        let (doc, nested) = doc
            .reply_to_comment(reply, "reply to reply".into(), "Carol".into(), "d".into())
            .expect("reply exists");
        assert_eq!(doc.comment_defs.len(), 3);
        assert_eq!(doc.comment_ranges.len(), 3);
        let doc = doc.delete_comment(parent);
        assert!(doc.comment_defs.is_empty(), "cascade removed the thread");
        assert!(doc.comment_ranges.is_empty());
        let _ = nested;
    }

    #[test]
    fn deleting_a_reply_leaves_the_parent() {
        let (doc, parent) = doc_with_comment();
        let (doc, reply) = doc
            .reply_to_comment(parent, "reply".into(), "Bob".into(), "d".into())
            .expect("parent exists");
        let doc = doc.delete_comment(reply);
        assert!(doc.comment_defs.contains_key(&parent));
        assert!(!doc.comment_defs.contains_key(&reply));
        assert!(doc.comment_ranges.iter().any(|r| r.id == parent));
        assert!(!doc.comment_ranges.iter().any(|r| r.id == reply));
    }
}
