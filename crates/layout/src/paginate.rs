//! Paginator — flow a sequence of `LayoutBlock`s onto multiple `PageBox`es.
//!
//! Phase 6. The pre-Phase-6 pipeline stacked every block onto a single
//! infinite-height `PageBox`. This module wraps that flow with a per-section
//! page-budget tracker: when the next block — or a paragraph's next line —
//! cannot fit in the remaining content-height of the current page, the
//! paginator closes the page and opens a fresh one with the same geometry.
//!
//! Splitting policy:
//!
//! - **Paragraphs split at line boundaries.** A paragraph that overflows is
//!   cut into a head (lines that fit on the current page) and a tail
//!   (lines that flow onto the next). The marker stays with the head. Both
//!   chunks carry per-line origins relative to their own paragraph top.
//! - **Tables split at row boundaries.** A table that doesn't fit moves
//!   wholly to the next page; if it still doesn't fit, the paginator emits
//!   the prefix of rows that do and pushes the remainder. Mid-cell
//!   splitting is deferred — cells stay atomic.
//!
//! Section breaks are caller-driven via [`Paginator::start_new_page`]; the
//! paginator itself only knows about overflow.

use crate::boxes::{
    FootnoteEntry, HeaderFooterBox, LayoutBlock, LineBox, PageBox, ParagraphBox, Point, Size,
    TableBox,
};
use crate::page::Margins;
use std::collections::HashMap;

/// Per-role header / footer bands the paginator picks from for each
/// page (Phase 2 audit — C.1 / C.2 / C.3). `default` always covers
/// every page where no more-specific variant applies. `first` is used
/// for the first page of the section when [`Paginator::title_pg`] is
/// `true`; `even` is used for even-numbered pages when
/// [`Paginator::even_and_odd_headers`] is `true`. Missing variants
/// fall back through `default` per OOXML §17.10.3, then to "no band"
/// when even `default` is unset.
#[derive(Debug, Clone, Default)]
pub struct HeaderBands {
    pub default: Option<HeaderFooterBox>,
    pub first: Option<HeaderFooterBox>,
    pub even: Option<HeaderFooterBox>,
}

/// Which header/footer slot the paginator should attach to a given page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderRole {
    Default,
    First,
    Even,
}

impl HeaderBands {
    /// Resolve `role` against the available slots. Falls back to
    /// `Default` when the requested slot is unset; returns `None`
    /// when even `Default` is missing.
    pub fn resolve(&self, role: HeaderRole) -> Option<&HeaderFooterBox> {
        let primary = match role {
            HeaderRole::Default => self.default.as_ref(),
            HeaderRole::First => self.first.as_ref(),
            HeaderRole::Even => self.even.as_ref(),
        };
        primary.or(self.default.as_ref())
    }
}

/// Phase 8a — vertical gap above the footnote separator rule, in layout
/// pt at scale=1. The renderer multiplies by `scale` if it needs device
/// pixels.
pub const FOOTNOTE_SEPARATOR_HEIGHT_PT: f32 = 12.0;

/// Concrete geometry for one paginated page. Mirrors `engine::PageGeometry`
/// without taking a dependency on the engine crate.
#[derive(Debug, Clone, Copy)]
pub struct PageGeometry {
    pub width: f32,
    pub height: f32,
    pub margins: Margins,
    /// Distance from the top of the page to the top of the header band.
    pub header_offset: f32,
    /// Distance from the bottom of the page to the bottom of the footer band.
    pub footer_offset: f32,
}

impl PageGeometry {
    pub fn content_height(&self) -> f32 {
        self.height - self.margins.top - self.margins.bottom
    }
}

/// Page-flow accumulator. Construct with `new`, feed [`LayoutBlock`]s via
/// [`Paginator::push_block`], optionally start a fresh page on a section
/// break with [`Paginator::start_new_page`], and drain the final page with
/// [`Paginator::finish`].
pub struct Paginator {
    geometry: PageGeometry,
    /// Per-role header bands the paginator picks from for each page.
    headers: HeaderBands,
    footers: HeaderBands,
    /// `<w:titlePg/>` — first page of current section uses `First` slot.
    title_pg: bool,
    /// `<w:evenAndOddHeaders/>` — even-numbered pages use `Even` slot.
    even_and_odd_headers: bool,
    /// Set on `new` and after every `start_new_section`; cleared after
    /// the next page flushes. Drives the `First` slot selection when
    /// `title_pg` is on.
    section_first_page_pending: bool,
    /// Accumulating page state.
    cur_blocks: Vec<LayoutBlock>,
    /// Cursor inside the current page's content area, in content-relative
    /// pt (0.0 at the top of the content rect).
    cur_y: f32,
    /// Finished pages.
    pages: Vec<PageBox>,
    /// Phase 8a — pre-laid-out footnote bodies keyed by `w:id`. The
    /// engine builds these once per document with the same paragraph
    /// layout pipeline the body uses; the paginator only does lookups.
    footnote_bodies: HashMap<u32, ParagraphBox>,
    /// Phase 8a — footnote ids already accumulated on the current page
    /// (in emission order; deduped). Drained on `flush_page`.
    cur_footnote_ids: Vec<u32>,
    /// Phase 8a — total height already consumed by the current page's
    /// footnote band, including the separator gap. Subtracted from
    /// the content budget so the body never overruns the band.
    cur_footnote_height: f32,
}

impl Paginator {
    pub fn new(
        geometry: PageGeometry,
        headers: HeaderBands,
        footers: HeaderBands,
        title_pg: bool,
        even_and_odd_headers: bool,
    ) -> Self {
        Self {
            geometry,
            headers,
            footers,
            title_pg,
            even_and_odd_headers,
            section_first_page_pending: true,
            cur_blocks: Vec::new(),
            cur_y: 0.0,
            pages: Vec::new(),
            footnote_bodies: HashMap::new(),
            cur_footnote_ids: Vec::new(),
            cur_footnote_height: 0.0,
        }
    }

    /// Single-slot convenience constructor — wraps the legacy
    /// `Option<HeaderFooterBox>` pair in default-only bands. Useful for
    /// callers that haven't yet plumbed the per-role section model.
    pub fn with_default_bands(
        geometry: PageGeometry,
        header: Option<HeaderFooterBox>,
        footer: Option<HeaderFooterBox>,
    ) -> Self {
        Self::new(
            geometry,
            HeaderBands {
                default: header,
                ..Default::default()
            },
            HeaderBands {
                default: footer,
                ..Default::default()
            },
            false,
            false,
        )
    }

    /// Pick which header/footer role applies to the page that is about
    /// to flush. Precedence (highest first):
    /// 1. Section first page AND `title_pg` → `First`.
    /// 2. `even_and_odd_headers` AND 1-based page number is even →
    ///    `Even`.
    /// 3. Otherwise → `Default`.
    ///
    /// The paginator counts pages document-wide (`self.pages.len() + 1`
    /// is the in-progress page number), matching Word's behaviour: a
    /// `<w:type="even"/>` header lines up with absolute page parity, not
    /// section-relative parity. The `First` check is section-scoped via
    /// `section_first_page_pending`.
    fn page_role(&self) -> HeaderRole {
        let page_no = self.pages.len() + 1;
        if self.section_first_page_pending && self.title_pg {
            HeaderRole::First
        } else if self.even_and_odd_headers && page_no % 2 == 0 {
            HeaderRole::Even
        } else {
            HeaderRole::Default
        }
    }

    /// Phase 8a — install the per-document footnote body table. The
    /// paginator looks each `<w:footnoteReference w:id="N"/>` up here
    /// when it scans a freshly-pushed paragraph and grows the footnote
    /// band before deciding whether the paragraph still fits.
    pub fn with_footnote_bodies(mut self, bodies: HashMap<u32, ParagraphBox>) -> Self {
        self.footnote_bodies = bodies;
        self
    }

    /// Y cursor within the current page's content area (parent-relative).
    /// Callers use this to compute the per-block origin before handing it to
    /// [`Paginator::push_block`].
    pub fn cursor_y(&self) -> f32 {
        self.cur_y
    }

    /// Number of pages already finalised (i.e. excluding the
    /// in-progress page being filled by [`Paginator::push_block`]).
    /// Callers diff this before/after a `push_block` to detect implicit
    /// overflow page breaks and reroute their per-page bookkeeping.
    pub fn page_count_emitted(&self) -> usize {
        self.pages.len()
    }

    pub fn content_width(&self) -> f32 {
        self.geometry.width - self.geometry.margins.left - self.geometry.margins.right
    }

    /// Switch to a new page geometry mid-flow. Closes the current page (even
    /// if it is empty — section breaks always emit a page in Word) and
    /// starts fresh with `new_geom`.
    pub fn start_new_section(
        &mut self,
        new_geom: PageGeometry,
        new_headers: HeaderBands,
        new_footers: HeaderBands,
        title_pg: bool,
    ) {
        self.flush_page();
        self.geometry = new_geom;
        self.headers = new_headers;
        self.footers = new_footers;
        self.title_pg = title_pg;
        /* New section ⇒ next page emitted is its first page; reset
        the flag so `page_role` picks `First` (if `title_pg`) before
        the next flush clears it. */
        self.section_first_page_pending = true;
    }

    /// Update the document-wide `even_and_odd_headers` toggle mid-flow.
    /// Lives outside `start_new_section` because the setting is a
    /// `word/settings.xml` flag, not a per-section property.
    pub fn set_even_and_odd_headers(&mut self, on: bool) {
        self.even_and_odd_headers = on;
    }

    /// Close the in-progress page (without changing geometry) — used when a
    /// caller wants a hard page break independent of overflow.
    pub fn force_page_break(&mut self) {
        self.flush_page();
    }

    /// Add a laid-out block to the current flow. The block's `origin.y` is
    /// rewritten to land at the current cursor; if it overflows, the block
    /// is split (paragraphs at line boundaries, tables at row boundaries)
    /// and the tail re-pushed onto the next page.
    ///
    /// Phase 8a — every block is scanned for `<w:footnoteReference>`
    /// anchors. Each new footnote brings its laid-out body into the
    /// page's bottom band; the band's accumulated height is subtracted
    /// from the body budget so the page never overflows. If the block
    /// plus its new footnote draw exceeds the budget, the new
    /// footnote(s) get rolled back, the page closes, and the block is
    /// re-tried on a fresh page (where its footnotes start a new band).
    pub fn push_block(&mut self, mut block: LayoutBlock, before: f32, after: f32) {
        /* Apply the paragraph's `<w:spacing w:before>` first — the engine
        layer already had this concept; we keep it here so the paginator
        owns every Y-coordinate. */
        self.cur_y += before;

        /* Phase 8a — gather every NEW footnote referenced by this block
        (already-on-page refs don't grow the band) and provisionally
        commit their heights to the budget. We undo the commit if the
        block ends up forced onto a new page. */
        let new_refs: Vec<u32> = collect_footnote_refs(&block)
            .into_iter()
            .filter(|id| !self.cur_footnote_ids.contains(id))
            .collect();
        let (added_height, added_separator) = self.try_consume_footnotes(&new_refs);

        let remaining = self.geometry.content_height() - self.cur_y - self.cur_footnote_height;
        let block_height = block.size().height;

        /* Paragraphs always run through the line-splitter when they
        don't fit — even when the current page is empty — so a single
        oversize paragraph turns into N pages, not one overflowing
        bag of content. Tables stay atomic on an empty page: the
        line-splitter doesn't apply, and a table taller than a full
        page is a rare authoring decision the user took deliberately.
        `push_paragraph_split` carries its own termination guard for
        the pathological single-line-bigger-than-page case. */
        let is_paragraph = matches!(block, LayoutBlock::Paragraph(_));
        let atomic_overflow_ok = !is_paragraph && self.cur_blocks.is_empty();
        if block_height <= remaining || atomic_overflow_ok {
            let mut origin = block.origin();
            origin.x = 0.0;
            origin.y = self.cur_y;
            block.set_origin(origin);
            self.cur_y += block_height + after;
            self.cur_blocks.push(block);
            return;
        }

        /* Overflow. Roll back the provisional footnote commit before
        retrying: the refs belong to the block, the block is going to
        the next page, and they should land in that page's band. */
        self.rollback_footnotes(&new_refs, added_height, added_separator);

        /* Pure block-level (a table on a non-empty page that doesn't
        fit) is the easy case: flush, retry. Paragraphs split at line
        boundaries. */
        match block {
            LayoutBlock::Paragraph(p) => self.push_paragraph_split(p, after),
            LayoutBlock::Table(t) => self.push_table_split(t, after),
        }
    }

    /// Provisional footnote commit. Returns `(extra_height_added,
    /// added_separator)` so [`Self::rollback_footnotes`] can undo it on
    /// an overflow path.
    fn try_consume_footnotes(&mut self, new_refs: &[u32]) -> (f32, bool) {
        if new_refs.is_empty() {
            return (0.0, false);
        }
        let mut extra = 0.0_f32;
        let added_separator = self.cur_footnote_ids.is_empty();
        if added_separator {
            extra += FOOTNOTE_SEPARATOR_HEIGHT_PT;
        }
        for id in new_refs {
            if let Some(body) = self.footnote_bodies.get(id) {
                extra += body.size.height;
            }
            self.cur_footnote_ids.push(*id);
        }
        self.cur_footnote_height += extra;
        (extra, added_separator)
    }

    fn rollback_footnotes(&mut self, new_refs: &[u32], extra: f32, added_separator: bool) {
        if new_refs.is_empty() {
            return;
        }
        /* Remove from the tail — the provisional push appended them in
        order, so the unwind pops the same ids. Defensive `retain`
        guards against duplicates the caller might pass. */
        for id in new_refs.iter().rev() {
            if let Some(pos) = self.cur_footnote_ids.iter().rposition(|x| x == id) {
                self.cur_footnote_ids.remove(pos);
            }
        }
        self.cur_footnote_height -= extra;
        if added_separator && self.cur_footnote_ids.is_empty() {
            /* `extra` already includes the separator; nothing else to do. */
        }
        if self.cur_footnote_height < 0.0 {
            self.cur_footnote_height = 0.0;
        }
    }

    fn push_paragraph_split(&mut self, para: ParagraphBox, after: f32) {
        let remaining = self.geometry.content_height() - self.cur_y - self.cur_footnote_height;
        let (head, tail) = split_paragraph_at_line(&para, remaining);

        match (head, tail) {
            (None, Some(tail)) if self.cur_blocks.is_empty() => {
                /* Pathological case — even the first line of the
                paragraph is taller than a fresh content area. Stuff
                atomically (single oversize line clips the bottom; a
                proper line-internal splitter is deferred). Without
                this guard `push_block` would recurse on the same
                tail on every fresh page → infinite loop. */
                let h = tail.size.height;
                let mut t = tail;
                t.origin = Point {
                    x: 0.0,
                    y: self.cur_y,
                };
                self.cur_y += h + after;
                self.cur_blocks.push(LayoutBlock::Paragraph(t));
            }
            (None, Some(tail)) => {
                /* Not even the first line fits on the *current* page
                but the page already has content — flush, retry on a
                fresh page where the same paragraph gets a full budget. */
                self.flush_page();
                self.push_block(LayoutBlock::Paragraph(tail), 0.0, after);
            }
            (Some(head), tail) => {
                let h = head.size.height;
                let mut head = head;
                head.origin = Point {
                    x: 0.0,
                    y: self.cur_y,
                };
                self.cur_y += h;
                self.cur_blocks.push(LayoutBlock::Paragraph(head));
                if let Some(tail) = tail {
                    self.flush_page();
                    self.push_block(LayoutBlock::Paragraph(tail), 0.0, after);
                } else {
                    self.cur_y += after;
                }
            }
            (None, None) => { /* Empty paragraph — nothing to do. */ }
        }
    }

    fn push_table_split(&mut self, table: TableBox, after: f32) {
        /* If the table is non-empty, try moving the *whole* table to a new
        page first — that handles the common "table just barely overflows
        the page footer" case without an ugly row split. */
        if !self.cur_blocks.is_empty() {
            self.flush_page();
            self.push_block(LayoutBlock::Table(table), 0.0, after);
            return;
        }

        /* The page is empty and the table is still taller than a page —
        emit row-by-row splits. */
        let mut head_rows = Vec::new();
        let mut head_height = 0.0_f32;
        let mut tail_rows = Vec::new();
        let mut tail_height = 0.0_f32;
        let budget = self.geometry.content_height();
        for row in table.rows.iter() {
            let row_h = row.size.height;
            if head_height + row_h <= budget || head_rows.is_empty() {
                let mut r = row.clone();
                r.origin.y = head_height;
                head_rows.push(r);
                head_height += row_h;
            } else {
                let mut r = row.clone();
                r.origin.y = tail_height;
                tail_rows.push(r);
                tail_height += row_h;
            }
        }
        let head = TableBox {
            origin: Point {
                x: table.origin.x,
                y: self.cur_y,
            },
            size: Size {
                width: table.size.width,
                height: head_height,
            },
            columns: table.columns.clone(),
            rows: head_rows,
            outer_borders: table.outer_borders.clone(),
        };
        self.cur_y += head_height;
        self.cur_blocks.push(LayoutBlock::Table(head));

        if !tail_rows.is_empty() {
            let tail = TableBox {
                origin: Point { x: 0.0, y: 0.0 },
                size: Size {
                    width: table.size.width,
                    height: tail_height,
                },
                columns: table.columns,
                rows: tail_rows,
                outer_borders: table.outer_borders,
            };
            self.flush_page();
            self.push_block(LayoutBlock::Table(tail), 0.0, after);
        } else {
            self.cur_y += after;
        }
    }

    /// Phase 2 audit (gap D.1) — stamp every PAGE field in the
    /// paragraph's [`ParagraphBox::fields`] with the 1-based page
    /// number it is about to flush on. NUMPAGES is deferred: its
    /// value is `pages.len()` at end-of-document, which is unknown
    /// here; [`Paginator::finish`] walks every emitted page and
    /// patches them in a second pass.
    fn evaluate_fields_on_paragraph(para: &mut ParagraphBox, current_page: u32) {
        for f in para.fields.iter_mut() {
            /* Keyword extraction lives on `engine::Field` so the
            layout box doesn't need to reimplement the trim + split
            + uppercase walk. Re-build a synthetic Field just to
            call `keyword` — cheap, since instructions are short. */
            let synthetic = engine::Field {
                start: f.byte_range.start,
                end: f.byte_range.end,
                instruction: f.instruction.clone(),
            };
            if synthetic.keyword() == "PAGE" {
                f.evaluated_text = Some(current_page.to_string());
            }
        }
    }

    /// Recursive sweep that visits every paragraph inside a
    /// [`LayoutBlock`] (top-level paragraph, table cell paragraphs,
    /// nested table cell paragraphs, ...) and applies `f`. The
    /// paginator needs this walk for field evaluation; lives on the
    /// paginator side because it mutates `ParagraphBox` in place.
    fn for_each_paragraph_in_block(block: &mut LayoutBlock, f: &mut impl FnMut(&mut ParagraphBox)) {
        match block {
            LayoutBlock::Paragraph(p) => f(p),
            LayoutBlock::Table(t) => {
                for row in t.rows.iter_mut() {
                    for cell in row.cells.iter_mut() {
                        for inner in cell.content.iter_mut() {
                            Self::for_each_paragraph_in_block(inner, f);
                        }
                    }
                }
            }
        }
    }

    fn flush_page(&mut self) {
        let blocks = std::mem::take(&mut self.cur_blocks);
        self.cur_y = 0.0;
        /* Phase 8a — materialize the page's footnote band. The reserved
        height was already subtracted from the body budget during
        `push_block`, so the band fits without overflow. Entries are in
        emission order (the order their refs first appeared in body
        content), each shifted to its own Y inside the band. */
        let mut footnotes: Vec<FootnoteEntry> = Vec::with_capacity(self.cur_footnote_ids.len());
        let mut band_y = 0.0_f32;
        for (idx, id) in self.cur_footnote_ids.iter().enumerate() {
            if let Some(body) = self.footnote_bodies.get(id).cloned() {
                let mut p = body;
                p.origin = Point { x: 0.0, y: band_y };
                band_y += p.size.height;
                footnotes.push(FootnoteEntry {
                    id: *id,
                    marker: (idx + 1).to_string(),
                    paragraph: p,
                });
            }
        }
        self.cur_footnote_ids.clear();
        self.cur_footnote_height = 0.0;

        /* Pick the role *before* pushing the page — `page_role` reads
        `pages.len()` to derive the 1-based page number, and the
        increment happens at `push`. Clone the resolved band slot;
        every other slot stays on the paginator for the next page. */
        let role = self.page_role();
        let mut header = self.headers.resolve(role).cloned();
        let mut footer = self.footers.resolve(role).cloned();

        /* Phase 2 audit (gap D.1) — PAGE field evaluation. The page
        number we're about to emit is `pages.len() + 1` (1-based).
        Stamp every body block + header + footer paragraph the page
        owns; `evaluate_fields_on_paragraph` mutates the
        `evaluated_text` slot the renderer eventually reads. */
        let current_page = (self.pages.len() + 1) as u32;
        let mut blocks = blocks;
        for block in blocks.iter_mut() {
            Self::for_each_paragraph_in_block(block, &mut |p| {
                Self::evaluate_fields_on_paragraph(p, current_page);
            });
        }
        if let Some(hf) = header.as_mut() {
            for p in hf.paragraphs.iter_mut() {
                Self::evaluate_fields_on_paragraph(p, current_page);
            }
        }
        if let Some(hf) = footer.as_mut() {
            for p in hf.paragraphs.iter_mut() {
                Self::evaluate_fields_on_paragraph(p, current_page);
            }
        }

        /* Even an empty page is emitted on an explicit `force_page_break`
        / `start_new_section` — the renderer paints the blank sheet so a
        section break is visible. */
        self.pages.push(PageBox {
            size: Size {
                width: self.geometry.width,
                height: self.geometry.height,
            },
            margins: self.geometry.margins,
            blocks,
            header,
            footer,
            footnotes,
        });

        /* Clear the section-first-page flag once a page has flushed for
        the section. Subsequent pages in the same section pick
        `Default` or `Even`. */
        self.section_first_page_pending = false;
    }

    /// Finalize — drain the in-progress page and return every page emitted.
    /// Always returns at least one page so the renderer has somewhere to
    /// draw the empty document.
    pub fn finish(mut self) -> Vec<PageBox> {
        if !self.cur_blocks.is_empty() || self.pages.is_empty() {
            self.flush_page();
        }
        /* Phase 2 audit (gap D.1) — NUMPAGES second pass. The first
        pass (in `flush_page`) only knows `pages.len() + 1`; the total
        is only fixed once every page has flushed. Walk every emitted
        page and stamp NUMPAGES on any field that hadn't already been
        evaluated as PAGE. */
        let total_pages = self.pages.len() as u32;
        for page in self.pages.iter_mut() {
            let mut stamp = |para: &mut ParagraphBox| {
                for f in para.fields.iter_mut() {
                    let kw = engine::Field {
                        start: f.byte_range.start,
                        end: f.byte_range.end,
                        instruction: f.instruction.clone(),
                    }
                    .keyword();
                    if kw == "NUMPAGES" {
                        f.evaluated_text = Some(total_pages.to_string());
                    }
                }
            };
            for block in page.blocks.iter_mut() {
                Self::for_each_paragraph_in_block(block, &mut stamp);
            }
            if let Some(hf) = page.header.as_mut() {
                for p in hf.paragraphs.iter_mut() {
                    stamp(p);
                }
            }
            if let Some(hf) = page.footer.as_mut() {
                for p in hf.paragraphs.iter_mut() {
                    stamp(p);
                }
            }
        }
        self.pages
    }
}

/// Phase 8a — scan a laid-out block for footnote reference anchors.
/// Returns the display number of every footnote the block touches, in
/// document order, with duplicates preserved (the paginator dedupes).
///
/// The glyph stores the marker text (the 1-based display number); the
/// engine adapter keys its `with_footnote_bodies` map by the *same*
/// numbers — it does the OOXML `w:id` ↔ display_number rebinding
/// before handing the table to the paginator, so the layout layer
/// never sees the raw `w:id`.
pub fn collect_footnote_refs(block: &LayoutBlock) -> Vec<u32> {
    let mut out: Vec<u32> = Vec::new();
    match block {
        LayoutBlock::Paragraph(p) => collect_in_paragraph(p, &mut out),
        LayoutBlock::Table(t) => collect_in_table(t, &mut out),
    }
    out
}

fn collect_in_paragraph(p: &ParagraphBox, out: &mut Vec<u32>) {
    for line in &p.lines {
        for run in &line.runs {
            for g in &run.glyphs {
                if let Some(marker) = g.inline_footnote_marker.as_deref()
                    && let Ok(id) = marker.parse::<u32>()
                {
                    out.push(id);
                }
            }
        }
    }
}

fn collect_in_table(t: &TableBox, out: &mut Vec<u32>) {
    for row in &t.rows {
        for cell in &row.cells {
            for inner in &cell.content {
                match inner {
                    LayoutBlock::Paragraph(p) => collect_in_paragraph(p, out),
                    LayoutBlock::Table(nested) => collect_in_table(nested, out),
                }
            }
        }
    }
}

/// Split `para` so the head fits within `budget` pt of vertical space.
///
/// Returns `(head, tail)`:
/// - `head` — the laid-out paragraph clamped to `budget` (may be `None`
///   when not even the first line fits; the caller flushes the page and
///   retries with a full-height budget).
/// - `tail` — the remaining lines as a fresh `ParagraphBox` with origins
///   reset to its own top (so the paginator can shift it onto the next
///   page).
///
/// Per-line geometry inside the box tree is rewritten so the head's lines
/// keep their absolute Y positions relative to *the head* (origin 0.0), and
/// the tail's lines do likewise (origin 0.0).
pub fn split_paragraph_at_line(
    para: &ParagraphBox,
    budget: f32,
) -> (Option<ParagraphBox>, Option<ParagraphBox>) {
    if para.lines.is_empty() {
        return (Some(para.clone()), None);
    }
    /* Find the split index — the first line whose bottom edge exceeds
    `budget`. Lines stack top-to-bottom; a line's bottom is
    `origin.y + height`. */
    let mut split_idx = para.lines.len();
    for (i, line) in para.lines.iter().enumerate() {
        if line.origin.y + line.height > budget {
            split_idx = i;
            break;
        }
    }

    if split_idx == 0 {
        /* Not even the first line fits. Caller flushes and retries. */
        return (None, Some(para.clone()));
    }
    if split_idx == para.lines.len() {
        /* Everything fits. */
        return (Some(para.clone()), None);
    }

    let head_lines: Vec<LineBox> = para.lines[..split_idx].to_vec();
    let mut tail_lines: Vec<LineBox> = para.lines[split_idx..].to_vec();
    let split_y = tail_lines
        .first()
        .map(|l| l.origin.y)
        .unwrap_or(para.size.height);
    /* Shift tail lines so the first one starts at y=0 in its new box. */
    for l in tail_lines.iter_mut() {
        l.origin.y -= split_y;
    }

    let head_height = head_lines
        .last()
        .map(|l| l.origin.y + l.height)
        .unwrap_or(0.0);
    let tail_height = tail_lines
        .last()
        .map(|l| l.origin.y + l.height)
        .unwrap_or(0.0);

    let head = ParagraphBox {
        origin: para.origin,
        size: Size {
            width: para.size.width,
            height: head_height,
        },
        lines: head_lines,
        direction: para.direction,
        /* The marker is anchored to the paragraph's first line — it stays
        with the head. */
        marker: para.marker.clone(),
        /* Both halves share the same source paragraph — clusters in either
        half's glyphs are byte offsets into the full original text. */
        source_paragraph_id: para.source_paragraph_id,
        /* Field overlays attach to byte ranges in the source paragraph
        text. The split duplicates them onto both halves so a PAGE
        field that ends up on the tail still gets re-evaluated; the
        paginator decides per-page which copy gets stamped. */
        fields: para.fields.clone(),
    };
    let tail = ParagraphBox {
        origin: Point { x: 0.0, y: 0.0 },
        size: Size {
            width: para.size.width,
            height: tail_height,
        },
        lines: tail_lines,
        direction: para.direction,
        marker: None,
        source_paragraph_id: para.source_paragraph_id,
        fields: para.fields.clone(),
    };
    (Some(head), Some(tail))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::boxes::{LayoutField, ParagraphBox};
    use crate::page::{A4Page, Margins};
    use text_pipeline::ShapingDirection;

    fn a4_geometry() -> PageGeometry {
        let page = A4Page::a4();
        PageGeometry {
            width: page.width,
            height: page.height,
            margins: page.margin,
            header_offset: 36.0,
            footer_offset: 36.0,
        }
    }

    /// Build a fake `ParagraphBox` with `n` lines of `line_height` each.
    /// Runs are empty — the splitter only reads `lines[i].origin.y +
    /// lines[i].height`, which is all the test cares about.
    fn fake_paragraph(n: usize, line_height: f32) -> ParagraphBox {
        let mut lines = Vec::with_capacity(n);
        for i in 0..n {
            lines.push(LineBox {
                origin: Point {
                    x: 0.0,
                    y: (i as f32) * line_height,
                },
                baseline: line_height * 0.8,
                height: line_height,
                width: 200.0,
                runs: Vec::new(),
                alignment: text_pipeline::Alignment::Start,
            });
        }
        ParagraphBox {
            origin: Point { x: 0.0, y: 0.0 },
            size: Size {
                width: 200.0,
                height: (n as f32) * line_height,
            },
            lines,
            direction: ShapingDirection::Ltr,
            marker: None,
            source_paragraph_id: ParagraphBox::NO_SOURCE_ID,
            fields: Vec::new(),
        }
    }

    #[test]
    fn paginator_splits_oversize_paragraph_on_empty_page() {
        /* A4 content height ≈ 842 − 144 = 698 pt. 80 lines of 16 pt =
        1280 pt — must split into at least 2 pages even though the
        paragraph is the very first block on the page. */
        let geom = a4_geometry();
        let mut pag = Paginator::with_default_bands(geom, None, None);
        let para = fake_paragraph(80, 16.0);
        pag.push_block(LayoutBlock::Paragraph(para), 0.0, 0.0);
        let pages = pag.finish();
        assert!(
            pages.len() >= 2,
            "expected ≥2 pages from an 80-line paragraph; got {}",
            pages.len()
        );
        for p in &pages {
            for block in &p.blocks {
                let h = block.size().height;
                assert!(
                    h <= geom.content_height() + 0.01,
                    "block height {h} exceeds page budget {}",
                    geom.content_height()
                );
            }
        }
    }

    #[test]
    fn paginator_keeps_short_paragraph_on_one_page() {
        let geom = a4_geometry();
        let mut pag = Paginator::with_default_bands(geom, None, None);
        let para = fake_paragraph(3, 16.0);
        pag.push_block(LayoutBlock::Paragraph(para), 0.0, 0.0);
        let pages = pag.finish();
        assert_eq!(pages.len(), 1);
    }

    #[test]
    fn paginator_pathological_single_line_taller_than_page_does_not_loop() {
        /* Single line of 9999 pt — taller than any A4 budget. Must not
        infinite-loop; the page-emptiness guard accepts the overflow
        atomically. */
        let geom = a4_geometry();
        let mut pag = Paginator::with_default_bands(geom, None, None);
        let para = fake_paragraph(1, 9999.0);
        pag.push_block(LayoutBlock::Paragraph(para), 0.0, 0.0);
        let pages = pag.finish();
        assert_eq!(pages.len(), 1);
    }

    /* `Margins` import isn't otherwise read in this test module. */
    const _M: Margins = Margins::uniform(0.0);

    /// Build a one-line `HeaderFooterBox` carrying `tag` as the only
    /// run's source range — lets a test fingerprint which band landed
    /// on a page by reading back `tag` from the emitted `PageBox`.
    fn fake_band(tag: u32) -> HeaderFooterBox {
        HeaderFooterBox {
            paragraphs: vec![ParagraphBox {
                origin: Point { x: 0.0, y: 0.0 },
                size: Size {
                    width: 200.0,
                    height: 16.0,
                },
                lines: Vec::new(),
                direction: ShapingDirection::Ltr,
                marker: None,
                source_paragraph_id: tag,
                fields: Vec::new(),
            }],
        }
    }

    /// Extract the `source_paragraph_id` of a page's header `ParagraphBox`,
    /// or `u32::MAX` when no header was attached. Lets the per-role
    /// selection tests assert which band the paginator picked.
    fn header_tag(p: &PageBox) -> u32 {
        p.header
            .as_ref()
            .and_then(|h| h.paragraphs.first())
            .map_or(u32::MAX, |para| para.source_paragraph_id)
    }

    /// Build a one-line `ParagraphBox` carrying a single `LayoutField`
    /// with the given instruction. Used by the field-evaluation tests
    /// to assert PAGE/NUMPAGES stamping happens on the right page.
    fn fake_paragraph_with_field(instruction: &str, line_height: f32) -> ParagraphBox {
        ParagraphBox {
            origin: Point { x: 0.0, y: 0.0 },
            size: Size {
                width: 200.0,
                height: line_height,
            },
            lines: vec![LineBox {
                origin: Point { x: 0.0, y: 0.0 },
                baseline: line_height * 0.8,
                height: line_height,
                width: 200.0,
                runs: Vec::new(),
                alignment: text_pipeline::Alignment::Start,
            }],
            direction: ShapingDirection::Ltr,
            marker: None,
            source_paragraph_id: ParagraphBox::NO_SOURCE_ID,
            fields: vec![LayoutField {
                byte_range: 0..1,
                instruction: instruction.to_string(),
                evaluated_text: None,
            }],
        }
    }

    /// Helper — return the `evaluated_text` of the first field on the
    /// first body paragraph of `page`, or `None` if absent.
    fn first_field_eval(page: &PageBox) -> Option<String> {
        let first = page.blocks.first()?;
        if let LayoutBlock::Paragraph(p) = first {
            p.fields.first().and_then(|f| f.evaluated_text.clone())
        } else {
            None
        }
    }

    #[test]
    fn paginator_evaluates_page_field_per_page() {
        /* Three body paragraphs each carrying a PAGE field — landing on
        pages 1, 2, 3 because we force a page break between each.
        After paginate, each paragraph's field must read its own page
        number. */
        let geom = a4_geometry();
        let mut pag = Paginator::with_default_bands(geom, None, None);
        for _ in 0..3 {
            pag.push_block(
                LayoutBlock::Paragraph(fake_paragraph_with_field("PAGE", 16.0)),
                0.0,
                0.0,
            );
            pag.force_page_break();
        }
        let pages = pag.finish();
        assert!(pages.len() >= 3, "expected 3 pages, got {}", pages.len());
        assert_eq!(first_field_eval(&pages[0]).as_deref(), Some("1"));
        assert_eq!(first_field_eval(&pages[1]).as_deref(), Some("2"));
        assert_eq!(first_field_eval(&pages[2]).as_deref(), Some("3"));
    }

    #[test]
    fn paginator_evaluates_numpages_second_pass() {
        /* NUMPAGES needs to know total pages, which is only fixed
        after every page has flushed. `finish` runs a second pass
        and stamps every NUMPAGES field with the total. */
        let geom = a4_geometry();
        let mut pag = Paginator::with_default_bands(geom, None, None);
        for _ in 0..4 {
            pag.push_block(
                LayoutBlock::Paragraph(fake_paragraph_with_field("NUMPAGES \\* MERGEFORMAT", 16.0)),
                0.0,
                0.0,
            );
            pag.force_page_break();
        }
        let pages = pag.finish();
        assert!(pages.len() >= 4);
        /* Every page reads the same total (4). */
        for (i, page) in pages.iter().enumerate().take(4) {
            assert_eq!(
                first_field_eval(page).as_deref(),
                Some("4"),
                "page {i} NUMPAGES must read 4"
            );
        }
    }

    #[test]
    fn header_band_page_field_stamps_per_page() {
        /* Phase 2 audit (gap D.1 follow-up). A single Default header
        carrying a PAGE field is cloned per page by the paginator
        (the `headers.resolve(role)` walk on every flush yields a
        fresh `HeaderFooterBox.clone()`); the per-page field
        evaluator stamps each clone with that page's own number. */
        let geom = a4_geometry();
        let header_bands = HeaderBands {
            default: Some(HeaderFooterBox {
                paragraphs: vec![fake_paragraph_with_field("PAGE", 16.0)],
            }),
            first: None,
            even: None,
        };
        let mut pag = Paginator::new(geom, header_bands, HeaderBands::default(), false, false);
        for _ in 0..3 {
            pag.push_block(LayoutBlock::Paragraph(fake_paragraph(1, 16.0)), 0.0, 0.0);
            pag.force_page_break();
        }
        let pages = pag.finish();
        assert!(pages.len() >= 3);
        let header_eval = |page: &PageBox| -> Option<String> {
            let hf = page.header.as_ref()?;
            let para = hf.paragraphs.first()?;
            para.fields.first().and_then(|f| f.evaluated_text.clone())
        };
        /* Each page's header carries the OWN page number, not a global
        constant — proves the clone-then-stamp ordering keeps every
        page's band independent. */
        assert_eq!(header_eval(&pages[0]).as_deref(), Some("1"));
        assert_eq!(header_eval(&pages[1]).as_deref(), Some("2"));
        assert_eq!(header_eval(&pages[2]).as_deref(), Some("3"));
    }

    #[test]
    fn footer_band_numpages_stamps_after_finish() {
        /* NUMPAGES in a footer band waits on the second pass run by
        `finish`. Before `finish`, the field's `evaluated_text` is
        whatever `flush_page` produced (None — NUMPAGES is not a
        first-pass instruction); after `finish`, every page's footer
        reads the total. */
        let geom = a4_geometry();
        let footer_bands = HeaderBands {
            default: Some(HeaderFooterBox {
                paragraphs: vec![fake_paragraph_with_field("NUMPAGES", 16.0)],
            }),
            first: None,
            even: None,
        };
        let mut pag = Paginator::new(geom, HeaderBands::default(), footer_bands, false, false);
        for _ in 0..4 {
            pag.push_block(LayoutBlock::Paragraph(fake_paragraph(1, 16.0)), 0.0, 0.0);
            pag.force_page_break();
        }
        let pages = pag.finish();
        assert!(pages.len() >= 4);
        for (i, page) in pages.iter().enumerate().take(4) {
            let f = page
                .footer
                .as_ref()
                .and_then(|hf| hf.paragraphs.first())
                .and_then(|p| p.fields.first())
                .and_then(|f| f.evaluated_text.clone());
            assert_eq!(f.as_deref(), Some("4"), "page {i} footer NUMPAGES");
        }
    }

    #[test]
    fn paginator_skips_unknown_field_instruction() {
        /* `DATE` is not evaluated; the field's `evaluated_text` stays
        `None` so the renderer paints the cached glyphs untouched. */
        let geom = a4_geometry();
        let mut pag = Paginator::with_default_bands(geom, None, None);
        pag.push_block(
            LayoutBlock::Paragraph(fake_paragraph_with_field("DATE \\@ \"yyyy\"", 16.0)),
            0.0,
            0.0,
        );
        let pages = pag.finish();
        assert!(first_field_eval(&pages[0]).is_none());
    }

    #[test]
    fn title_pg_routes_first_page_to_first_header() {
        /* Section with all three header slots populated + `title_pg`
        on. The first page must read the `First` band; every page
        after that falls back to `Default` (no even/odd parity). */
        let geom = a4_geometry();
        let headers = HeaderBands {
            default: Some(fake_band(1)),
            first: Some(fake_band(2)),
            even: Some(fake_band(3)),
        };
        let mut pag = Paginator::new(geom, headers, HeaderBands::default(), true, false);
        for _ in 0..3 {
            pag.push_block(LayoutBlock::Paragraph(fake_paragraph(40, 16.0)), 0.0, 0.0);
            pag.force_page_break();
        }
        let pages = pag.finish();
        assert!(pages.len() >= 3, "expected ≥3 pages, got {}", pages.len());
        assert_eq!(header_tag(&pages[0]), 2, "page 1 must use First header");
        assert_eq!(header_tag(&pages[1]), 1, "page 2 must use Default header");
        assert_eq!(header_tag(&pages[2]), 1, "page 3 must use Default header");
    }

    #[test]
    fn even_and_odd_routes_even_pages_to_even_header() {
        /* `even_and_odd_headers` on, `title_pg` off. Page 1 → Default,
        page 2 → Even, page 3 → Default, page 4 → Even. */
        let geom = a4_geometry();
        let headers = HeaderBands {
            default: Some(fake_band(10)),
            first: None,
            even: Some(fake_band(20)),
        };
        let mut pag = Paginator::new(geom, headers, HeaderBands::default(), false, true);
        for _ in 0..4 {
            pag.push_block(LayoutBlock::Paragraph(fake_paragraph(40, 16.0)), 0.0, 0.0);
            pag.force_page_break();
        }
        let pages = pag.finish();
        assert!(pages.len() >= 4);
        assert_eq!(header_tag(&pages[0]), 10);
        assert_eq!(header_tag(&pages[1]), 20);
        assert_eq!(header_tag(&pages[2]), 10);
        assert_eq!(header_tag(&pages[3]), 20);
    }

    #[test]
    fn missing_first_falls_back_to_default() {
        /* `title_pg` requests `First`, but only `Default` is set —
        OOXML §17.10.3 fallback: every page reads Default. */
        let geom = a4_geometry();
        let headers = HeaderBands {
            default: Some(fake_band(7)),
            first: None,
            even: None,
        };
        let mut pag = Paginator::new(geom, headers, HeaderBands::default(), true, false);
        pag.push_block(LayoutBlock::Paragraph(fake_paragraph(40, 16.0)), 0.0, 0.0);
        pag.force_page_break();
        let pages = pag.finish();
        assert_eq!(
            header_tag(&pages[0]),
            7,
            "missing First slot must fall back to Default"
        );
    }

    #[test]
    fn missing_default_leaves_blank_band() {
        /* No header slots populated. Every page has `header: None`. */
        let geom = a4_geometry();
        let mut pag = Paginator::new(
            geom,
            HeaderBands::default(),
            HeaderBands::default(),
            true,
            true,
        );
        pag.push_block(LayoutBlock::Paragraph(fake_paragraph(40, 16.0)), 0.0, 0.0);
        pag.force_page_break();
        let pages = pag.finish();
        assert_eq!(
            header_tag(&pages[0]),
            u32::MAX,
            "absent default + absent variant must produce no band"
        );
    }

    #[test]
    fn even_only_archive_falls_back_when_default_missing() {
        /* Document supplies only an `Even` header but no `Default`.
        OOXML resolves any unset variant via Default → None; for an
        Even-only doc that means every page renders with no band
        because the Default fallback walks to nothing. */
        let geom = a4_geometry();
        let headers = HeaderBands {
            default: None,
            first: None,
            even: Some(fake_band(99)),
        };
        let mut pag = Paginator::new(geom, headers, HeaderBands::default(), false, true);
        for _ in 0..2 {
            pag.push_block(LayoutBlock::Paragraph(fake_paragraph(40, 16.0)), 0.0, 0.0);
            pag.force_page_break();
        }
        let pages = pag.finish();
        /* Page 2 picks `Even` directly (variant populated). Page 1 asks
        for `Default` → falls back to Even? No — fallback is variant
        → Default, NOT the other way around. Default → None means
        page 1 carries no band. */
        assert_eq!(header_tag(&pages[0]), u32::MAX);
        assert_eq!(header_tag(&pages[1]), 99);
    }
}
