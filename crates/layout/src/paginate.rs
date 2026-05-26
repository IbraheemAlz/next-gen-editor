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
    TableBox, TableRowBox,
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
    /// Audit gap A.H2 — `<w:cols w:num>` for the active section. `1` is
    /// the single-column historical path; `> 1` enables snake-flow
    /// where overflow advances [`Self::cur_column_index`] and only
    /// flushes the page once every column is full.
    column_count: u8,
    /// `<w:cols w:space>` — gutter between adjacent columns, in layout
    /// pt at the active scale. Zero when single-column.
    column_gutter: f32,
    /// Active column the cursor is currently flowing into (0-based).
    /// Reset to 0 on every page flush + section break.
    cur_column_index: u8,
    /// Accumulating page state.
    cur_blocks: Vec<LayoutBlock>,
    /// L2.3 (#8) — index in [`Self::cur_blocks`] where the current
    /// section's first block sits. Stamped by
    /// [`Self::balance_current_section_columns`] when a continuous
    /// section break terminates a multi-column section; reset to 0
    /// on every page flush (when [`Self::cur_blocks`] is taken).
    cur_section_start_idx: usize,
    /// Cursor inside the current column's content area, in content-relative
    /// pt (0.0 at the top of the content rect). Reset on every column
    /// advance + page flush.
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
    /// Audit gap A.M11 — `<w:pgNumType>` for the active section.
    /// PAGE-field evaluator translates the doc-wide page number into
    /// (start + (doc_page - section_start_page)) when `start` is
    /// `Some`, then formats per `PageNumFormat`.
    page_num: engine::PageNumType,
    /// Audit gap A.M11 — doc-wide page index (1-based) at the start of
    /// the section this paginator owns. Engine-wasm sets this when it
    /// creates the per-section paginator. PAGE-field eval uses it to
    /// compute section-relative page numbers without needing to know
    /// about the global page accumulator.
    doc_page_offset: u32,
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
            column_count: 1,
            column_gutter: 0.0,
            cur_column_index: 0,
            cur_blocks: Vec::new(),
            cur_section_start_idx: 0,
            cur_y: 0.0,
            pages: Vec::new(),
            footnote_bodies: HashMap::new(),
            cur_footnote_ids: Vec::new(),
            cur_footnote_height: 0.0,
            page_num: engine::PageNumType::default(),
            doc_page_offset: 0,
        }
    }

    /// Audit gap A.M11 — install the section's `<w:pgNumType>` plus the
    /// doc-wide page index this section starts at. Engine-wasm calls
    /// this right after constructing a per-section paginator.
    pub fn set_page_numbering(&mut self, page_num: engine::PageNumType, doc_page_offset: u32) {
        self.page_num = page_num;
        self.doc_page_offset = doc_page_offset;
    }

    /// Audit gap A.M12 — install the section's column descriptor + the
    /// paginator's existing cursor state. `continue_in_place=true`
    /// (continuous section break) preserves `cur_y` / `cur_blocks` so
    /// the new section's first block flows directly below the previous
    /// section on the SAME page. `false` (NextPage default) is handled
    /// by the engine-wasm flush path instead.
    pub fn set_section_cursor(&mut self, geometry: PageGeometry) {
        self.geometry = geometry;
    }

    /// Audit gap A.H2 — install the active section's `<w:cols>` descriptor.
    /// Single-column sections leave the defaults (count = 1) untouched.
    /// `count == 0` clamps to 1 (defensive against malformed input).
    /// Always reset `cur_column_index` so a column-spec swap mid-flow
    /// doesn't strand the cursor in a column that no longer exists.
    pub fn set_columns(&mut self, count: u8, gutter_pt: f32) {
        self.column_count = count.max(1);
        self.column_gutter = gutter_pt.max(0.0);
        self.cur_column_index = 0;
    }

    /// L2.3 (#8) — column balance pass for the just-finished section.
    /// Called by the continuous-section-break handoff in engine-wasm
    /// BEFORE [`Self::set_section_cursor`] / [`Self::set_columns`]
    /// swap the column descriptor.
    ///
    /// Walks the blocks in `cur_blocks[cur_section_start_idx..]`
    /// (i.e. every block pushed since the prior section started on
    /// this page) and redistributes them across the section's
    /// columns using a greedy snake-fill targeting
    /// `total_section_height / column_count`. Each block's
    /// `origin.x` is rewritten to the column's x offset; `origin.y`
    /// is rewritten to the block's running position within the
    /// column, anchored at the section's top-of-page y baseline.
    ///
    /// `cur_y` is updated to the bottom edge of the deepest column
    /// so the next section's first block lands strictly below every
    /// column's tail with no overlap.
    ///
    /// Single-column sections (or sections with no pushed blocks)
    /// short-circuit; the existing snake-flow output is preserved
    /// byte-identical and the regression goldens stay at 0.000 %.
    ///
    /// The implementation is greedy O(n_blocks). Pathological block-
    /// size mixes can leave one column up to `max(block_height)`
    /// taller than the target; Knuth-LP balance is a follow-up if
    /// the v1 result surfaces unacceptable cases on real corpora.
    pub fn balance_current_section_columns(&mut self) {
        let end = self.cur_blocks.len();
        /* Single-column sections need no balancing — stamp the
        boundary so any subsequent push lands in the "new" section
        and short-circuit. */
        if self.column_count <= 1 {
            self.cur_section_start_idx = end;
            return;
        }
        let n = self.column_count as usize;
        let start = self.cur_section_start_idx;
        if start >= end {
            return;
        }

        /* Section's y-baseline = origin.y of the first block placed
        by this section. Captures the bottom of any prior same-page
        section (e.g. a 1-col title above the 2-col body). */
        let section_top_y = self.cur_blocks[start].origin().y;

        let total_h: f32 = self.cur_blocks[start..end]
            .iter()
            .map(|b| b.size().height)
            .sum();
        if total_h <= 0.0 {
            self.cur_section_start_idx = end;
            return;
        }
        let target_h = total_h / n as f32;

        /* Precompute per-column x offsets so the inner loop can take
        a mutable borrow on `cur_blocks` without aliasing `self`. */
        let col_x: Vec<f32> = (0..n).map(|i| self.column_x_offset(i as u8)).collect();

        /* Greedy snake-fill. Place blocks in logical order; advance
        the column when the next block would push past `target_h`
        AND another column is available. The last column accepts
        whatever remains (cannot snake past N - 1). */
        let mut col: usize = 0;
        let mut y_in_col = 0.0_f32;
        let mut col_tails = vec![0.0_f32; n];
        for block in &mut self.cur_blocks[start..end] {
            let h = block.size().height;
            if y_in_col > 0.0 && y_in_col + h > target_h && col < n - 1 {
                col += 1;
                y_in_col = 0.0;
            }
            block.set_origin(Point {
                x: col_x[col],
                y: section_top_y + y_in_col,
            });
            y_in_col += h;
            col_tails[col] = y_in_col;
        }

        /* Cursor handoff: next section begins below the LONGEST
        column's bottom edge so no block overlaps the new flow. */
        let max_tail = col_tails.iter().copied().fold(0.0_f32, f32::max);
        self.cur_y = section_top_y + max_tail;

        /* Section boundary stamp — subsequent push_blocks belong to
        the NEW section. `set_section_cursor` + `set_columns` run
        after this call. */
        self.cur_section_start_idx = end;
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

    /// Audit gap A.H2 — width (in layout pt) of one column for the active
    /// section's `<w:cols>` descriptor. Single-column ⇒ same as
    /// [`Self::content_width`]; multi-column subtracts `(N - 1) *
    /// gutter` from the content width and equal-shares the remainder.
    pub fn column_width(&self) -> f32 {
        let cw = self.content_width();
        let n = self.column_count.max(1) as f32;
        if n <= 1.0 {
            return cw;
        }
        let gutters = (n - 1.0) * self.column_gutter;
        ((cw - gutters) / n).max(0.0)
    }

    /// Distance (in layout pt) from the section's content-area leading
    /// edge to the leading edge of column `idx`. `idx` clamps to the
    /// last column when out of range.
    pub fn column_x_offset(&self, idx: u8) -> f32 {
        let cw = self.column_width();
        (idx.min(self.column_count.saturating_sub(1)) as f32) * (cw + self.column_gutter)
    }

    /// The x origin (in content-area-relative pt) for blocks the
    /// paginator is currently flowing into.
    fn current_column_origin_x(&self) -> f32 {
        self.column_x_offset(self.cur_column_index)
    }

    /// Audit gap A.H2 — overflow advance. If the active section has
    /// another column available, snake into it (reset `cur_y`, leave
    /// `cur_blocks` alone — they belong to the in-progress multi-column
    /// page). Otherwise flush the page (which also resets the column
    /// cursor back to 0 for the next page). The boolean return makes
    /// the split paths' "did we cross a page boundary?" check explicit.
    fn advance_column_or_flush_page(&mut self) -> bool {
        if (self.cur_column_index as u16 + 1) < self.column_count as u16 {
            self.cur_column_index += 1;
            self.cur_y = 0.0;
            false
        } else {
            self.flush_page();
            true
        }
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
        /* Audit gap A.H2 — column descriptor lives on the new section;
        the caller installs it via `set_columns` after this call. Clear
        the cursor so the first block of the new section lands in
        column 0 of column-0 X offset. */
        self.column_count = 1;
        self.column_gutter = 0.0;
        self.cur_column_index = 0;
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
        /* Phase 2 audit (gap A.12) — paragraphs carrying a forced
        page break (`\u{000C}` FORM FEED → `page_break_after_line`
        populated by the layout pass) must always route through the
        split path, regardless of remaining budget. Otherwise the
        fast "fits whole" branch swallows the break and Word's
        page-break semantics are silently dropped. */
        let has_forced_break = if let LayoutBlock::Paragraph(p) = &block {
            !p.page_break_after_line.is_empty()
        } else {
            false
        };
        if (block_height <= remaining || atomic_overflow_ok) && !has_forced_break {
            let mut origin = block.origin();
            /* Audit gap A.H2 — origin.x carries the column offset for
            multi-column sections (zero for single-column, preserving
            the legacy "page-wide" behaviour). */
            origin.x = self.current_column_origin_x();
            origin.y = self.cur_y;
            block.set_origin(origin);
            self.cur_y += block_height + after;
            self.cur_blocks.push(block);
            return;
        }
        /* Rollback footnote provisional commit only for the
        budget-overflow branch — a forced break still keeps its
        footnote refs on the current page (the head lands here, the
        tail starts a new page where its own footnotes accumulate
        anew). */
        if !has_forced_break {
            /* Overflow. Roll back the provisional footnote commit
            before retrying: the refs belong to the block, the block
            is going to the next page, and they should land in that
            page's band. */
            self.rollback_footnotes(&new_refs, added_height, added_separator);
        }

        /* Pure block-level (a table on a non-empty page that doesn't
        fit) is the easy case: flush, retry. Paragraphs split at line
        boundaries (`push_paragraph_split` also handles the forced
        page-break path). */
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
        /* Phase 2 audit (gap A.12) — forced page break path. If the
        paragraph carries a `\u{000C}` FORM FEED (the reader's
        mapping of `<w:br w:type="page"/>`), the earliest line index
        in `page_break_after_line` wins over the budget-based split:
        head covers `0..=first_break` and flushes the page
        unconditionally, then the tail re-enters `push_block` on a
        fresh page where any remaining breaks fire on their own
        iteration.

        Head's page_break_after_line slot is cleared before the
        inner push so the budget path doesn't re-trigger this same
        forced flush. Tail keeps its remaining breaks (already
        index-shifted by `split_page_breaks`). */
        if let Some(&first_break) = para.page_break_after_line.first() {
            let split_after = first_break + 1;
            let (head_opt, tail_opt) = split_paragraph_at_line_index(&para, split_after);
            if let Some(mut head) = head_opt {
                /* Strip the consumed break from head's list so the
                inner budget push doesn't recurse on it. */
                head.page_break_after_line.clear();
                /* Re-route through `push_block` — the budget split
                still applies inside head (a paragraph longer than
                a page that also contains a page break needs to
                page-overflow the head AND then force-flush at the
                break boundary). `after = 0.0` because the
                outer-call's `after` belongs to the tail's last
                line, not the forced break. */
                self.push_block(LayoutBlock::Paragraph(head), 0.0, 0.0);
            }
            /* Force the flush even when head was empty (page break at
            the very first line — produces a blank "current page"
            then the tail starts fresh; matches Word's behaviour). */
            self.flush_page();
            if let Some(tail) = tail_opt {
                self.push_block(LayoutBlock::Paragraph(tail), 0.0, after);
            }
            return;
        }

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
                    x: self.current_column_origin_x(),
                    y: self.cur_y,
                };
                self.cur_y += h + after;
                self.cur_blocks.push(LayoutBlock::Paragraph(t));
            }
            (None, Some(tail)) => {
                /* Not even the first line fits in the *current* column.
                Audit gap A.H2 — snake into the next column if available,
                otherwise flush the page. The tail re-enters `push_block`
                with a full column-of-content budget so the next attempt
                always succeeds (or hits the atomic-single-line clip
                path above). */
                self.advance_column_or_flush_page();
                self.push_block(LayoutBlock::Paragraph(tail), 0.0, after);
            }
            (Some(head), tail) => {
                let h = head.size.height;
                let mut head = head;
                head.origin = Point {
                    x: self.current_column_origin_x(),
                    y: self.cur_y,
                };
                self.cur_y += h;
                self.cur_blocks.push(LayoutBlock::Paragraph(head));
                if let Some(tail) = tail {
                    /* Audit gap A.H2 — column-aware tail routing. The
                    head sits where it landed; the tail goes into the
                    next column (snake) or, when this is the last
                    column, onto a fresh page. */
                    self.advance_column_or_flush_page();
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
        column (or, when no further columns exist, a new page) first —
        that handles the common "table just barely overflows the column
        footer" case without an ugly row split. Audit gap A.H2 — the
        snake advance keeps the table inside the current page when a
        sibling column has room. */
        if !self.cur_blocks.is_empty() {
            self.advance_column_or_flush_page();
            self.push_block(LayoutBlock::Table(table), 0.0, after);
            return;
        }

        /* The page is empty and the table is still taller than a page —
        emit row-by-row splits.

        Audit gap C.M2 — `<w:trPr><w:cantSplit/>` honour. The current
        implementation already keeps every row atomic (no mid-row
        paragraph split), so `cant_split=true` is the default
        behaviour. The flag is threaded through `TableRowBox` for
        when the mid-cell split lands in a follow-up sprint; the
        check below mirrors what the future split path will do. */
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
                x: self.current_column_origin_x(),
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
        /* Audit gap A.M9 — collect header rows from the head BEFORE we
        move it into `cur_blocks`. Cloning is cheap (handful of cells
        each carrying paragraph-content `Vec`s); the originals stay in
        place at the top of the head. The tail prepends fresh clones
        so the headers repeat. */
        let header_rows: Vec<TableRowBox> =
            head.rows.iter().filter(|r| r.header).cloned().collect();
        self.cur_blocks.push(LayoutBlock::Table(head));

        if !tail_rows.is_empty() {
            /* Audit gap A.M9 — prepend cloned headers to every tail
            page. Re-stamp `origin.y` so the headers sit at the top of
            the new TableBox and the original tail rows shift down by
            the headers' total height. */
            let header_total: f32 = header_rows.iter().map(|r| r.size.height).sum();
            let mut combined: Vec<TableRowBox> =
                Vec::with_capacity(header_rows.len() + tail_rows.len());
            let mut cursor_y = 0.0_f32;
            for h in &header_rows {
                let mut hh = h.clone();
                hh.origin.y = cursor_y;
                cursor_y += hh.size.height;
                combined.push(hh);
            }
            for r in &tail_rows {
                let mut rr = r.clone();
                rr.origin.y = cursor_y;
                cursor_y += rr.size.height;
                combined.push(rr);
            }
            let tail = TableBox {
                origin: Point { x: 0.0, y: 0.0 },
                size: Size {
                    width: table.size.width,
                    height: tail_height + header_total,
                },
                columns: table.columns,
                rows: combined,
                outer_borders: table.outer_borders,
            };
            /* Audit gap A.H2 — snake into the next column before
            forcing a page; matches the paragraph split policy. */
            self.advance_column_or_flush_page();
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
    fn evaluate_fields_on_paragraph(
        para: &mut ParagraphBox,
        doc_page: u32,
        page_num: engine::PageNumType,
        section_start_doc_page: u32,
    ) {
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
                /* Audit gap A.M11 — section-relative page numbering.
                `start: Some(n)` rebases: the section's first page is
                `n`, every subsequent page is `n + (doc_page -
                section_start_doc_page)`. `start: None` keeps the
                doc-wide count. Format renders the integer. */
                let section_page = match page_num.start {
                    Some(n) => n + doc_page.saturating_sub(section_start_doc_page),
                    None => doc_page,
                };
                f.evaluated_text = Some(page_num.format.render(section_page));
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
        /* Audit gap A.H2 — every page starts in column 0 of its
        section's column descriptor. The descriptor itself
        (`column_count` / `column_gutter`) stays — it is a section
        property, not a per-page one. */
        self.cur_column_index = 0;
        /* L2.3 (#8) — `cur_blocks` was just emptied; the next push
        starts the section's first-on-this-page block at index 0. */
        self.cur_section_start_idx = 0;
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
        /* Audit gap A.M11 — doc-wide page index is the section's
        `doc_page_offset` plus this paginator's local page count. The
        +1 is the 1-based convention (Word, OOXML, and Word's PAGE
        field are all 1-based). */
        let doc_page = self.doc_page_offset + (self.pages.len() as u32) + 1;
        let section_start_doc_page = self.doc_page_offset + 1;
        let page_num = self.page_num;
        let mut blocks = blocks;
        for block in blocks.iter_mut() {
            Self::for_each_paragraph_in_block(block, &mut |p| {
                Self::evaluate_fields_on_paragraph(p, doc_page, page_num, section_start_doc_page);
            });
        }
        if let Some(hf) = header.as_mut() {
            for p in hf.paragraphs.iter_mut() {
                Self::evaluate_fields_on_paragraph(p, doc_page, page_num, section_start_doc_page);
            }
        }
        if let Some(hf) = footer.as_mut() {
            for p in hf.paragraphs.iter_mut() {
                Self::evaluate_fields_on_paragraph(p, doc_page, page_num, section_start_doc_page);
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

    let (head_pb, tail_pb) = split_page_breaks(&para.page_break_after_line, split_idx);
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
        page_break_after_line: head_pb,
        borders: para.borders.clone(),
        shading: para.shading,
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
        page_break_after_line: tail_pb,
        borders: para.borders.clone(),
        shading: para.shading,
    };
    (Some(head), Some(tail))
}

/// Phase 2 audit (gap A.12) — paragraph splitter that cuts at an
/// exact line index instead of a height budget. Mirrors
/// [`split_paragraph_at_line`] but takes a deterministic `split_idx`
/// (number of lines that go to the head; lines `[split_idx..]` go to
/// the tail). The forced-flush path the paginator uses for
/// `\u{000C}` FORM FEED breaks needs this — budget-based split
/// won't fire when the paragraph still has remaining content height.
pub fn split_paragraph_at_line_index(
    para: &ParagraphBox,
    split_idx: usize,
) -> (Option<ParagraphBox>, Option<ParagraphBox>) {
    if split_idx == 0 {
        return (None, Some(para.clone()));
    }
    if split_idx >= para.lines.len() {
        return (Some(para.clone()), None);
    }
    let head_lines: Vec<LineBox> = para.lines[..split_idx].to_vec();
    let mut tail_lines: Vec<LineBox> = para.lines[split_idx..].to_vec();
    let split_y = tail_lines
        .first()
        .map(|l| l.origin.y)
        .unwrap_or(para.size.height);
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
    let (head_pb, tail_pb) = split_page_breaks(&para.page_break_after_line, split_idx);
    let head = ParagraphBox {
        origin: para.origin,
        size: Size {
            width: para.size.width,
            height: head_height,
        },
        lines: head_lines,
        direction: para.direction,
        marker: para.marker.clone(),
        source_paragraph_id: para.source_paragraph_id,
        fields: para.fields.clone(),
        page_break_after_line: head_pb,
        borders: para.borders.clone(),
        shading: para.shading,
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
        page_break_after_line: tail_pb,
        borders: para.borders.clone(),
        shading: para.shading,
    };
    (Some(head), Some(tail))
}

/// Phase 2 audit (gap A.12) — split a `page_break_after_line` index
/// list across a paragraph break at line `split_idx`. The split
/// boundary itself lives in `head` (head retains lines `0..split_idx`,
/// tail starts at the original `split_idx`).
///
/// Indices `< split_idx` stay on `head` verbatim. Indices `>= split_idx`
/// move to `tail` and shift down by `split_idx` so they index into the
/// tail's own line array. Index `split_idx - 1` (a page break exactly
/// at the line that ends the head) stays with head; the paginator
/// flushes after head emits and tail starts on a fresh page.
fn split_page_breaks(orig: &[usize], split_idx: usize) -> (Vec<usize>, Vec<usize>) {
    let head: Vec<usize> = orig.iter().filter(|&&i| i < split_idx).copied().collect();
    let tail: Vec<usize> = orig
        .iter()
        .filter(|&&i| i >= split_idx)
        .map(|&i| i - split_idx)
        .collect();
    (head, tail)
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
            page_break_after_line: Vec::new(),
            borders: None,
            shading: None,
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
                page_break_after_line: Vec::new(),
                borders: None,
                shading: None,
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
            page_break_after_line: Vec::new(),
            borders: None,
            shading: None,
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

    /// Helper — build a paragraph with N lines and a forced page
    /// break recorded after line index `break_after`.
    fn fake_paragraph_with_page_break(
        n: usize,
        line_height: f32,
        break_after: usize,
    ) -> ParagraphBox {
        let mut p = fake_paragraph(n, line_height);
        p.page_break_after_line = vec![break_after];
        p
    }

    #[test]
    fn form_feed_forces_page_flush_mid_paragraph() {
        /* Paragraph: 6 lines, page break after line 2. Even though
        the whole paragraph fits on a single A4 page (6 × 16 = 96 pt
        << 698 pt budget), the break forces a flush after line 2 →
        page 1 carries lines 0..=2 (3 lines), page 2 carries lines
        3..=5 (3 lines). */
        let geom = a4_geometry();
        let mut pag = Paginator::with_default_bands(geom, None, None);
        pag.push_block(
            LayoutBlock::Paragraph(fake_paragraph_with_page_break(6, 16.0, 2)),
            0.0,
            0.0,
        );
        let pages = pag.finish();
        assert_eq!(pages.len(), 2, "expected 2 pages from forced break");
        let head_lines = pages[0]
            .blocks
            .iter()
            .filter_map(|b| match b {
                LayoutBlock::Paragraph(p) => Some(p.lines.len()),
                _ => None,
            })
            .next()
            .unwrap_or(0);
        let tail_lines = pages[1]
            .blocks
            .iter()
            .filter_map(|b| match b {
                LayoutBlock::Paragraph(p) => Some(p.lines.len()),
                _ => None,
            })
            .next()
            .unwrap_or(0);
        assert_eq!(head_lines, 3, "page 1 head must hold lines 0..=2");
        assert_eq!(tail_lines, 3, "page 2 tail must hold lines 3..=5");
    }

    #[test]
    fn multiple_form_feeds_split_into_three_pages() {
        /* Two breaks in one paragraph: after line 1 and line 3 →
        page 1 = lines 0..=1, page 2 = lines 2..=3, page 3 = lines
        4..=5. Verifies index remap on the recursive split (the
        second break's index shifts down by `split_idx` for the
        tail's local indexing). */
        let geom = a4_geometry();
        let mut pag = Paginator::with_default_bands(geom, None, None);
        let mut p = fake_paragraph(6, 16.0);
        p.page_break_after_line = vec![1, 3];
        pag.push_block(LayoutBlock::Paragraph(p), 0.0, 0.0);
        let pages = pag.finish();
        assert_eq!(pages.len(), 3, "expected 3 pages from two breaks");
        let count = |i: usize| -> usize {
            pages[i]
                .blocks
                .iter()
                .filter_map(|b| match b {
                    LayoutBlock::Paragraph(p) => Some(p.lines.len()),
                    _ => None,
                })
                .next()
                .unwrap_or(0)
        };
        assert_eq!(count(0), 2);
        assert_eq!(count(1), 2);
        assert_eq!(count(2), 2);
    }

    #[test]
    fn form_feed_on_last_line_flushes_but_no_dangling_empty_page() {
        /* Break after the final line: head = whole paragraph, tail
        is empty so no second page emits. `finish` still produces a
        page count of 1 (the head page), not 2 (avoiding a dangling
        empty page after the break). */
        let geom = a4_geometry();
        let mut pag = Paginator::with_default_bands(geom, None, None);
        pag.push_block(
            LayoutBlock::Paragraph(fake_paragraph_with_page_break(4, 16.0, 3)),
            0.0,
            0.0,
        );
        let pages = pag.finish();
        /* Head fills page 1; tail is empty so the forced flush emits
        page 1 but `finish` does not emit a phantom page 2 because
        `cur_blocks` stays empty after the flush. */
        assert_eq!(
            pages.len(),
            1,
            "no dangling empty page after terminal break"
        );
        let line_count = pages[0]
            .blocks
            .iter()
            .filter_map(|b| match b {
                LayoutBlock::Paragraph(p) => Some(p.lines.len()),
                _ => None,
            })
            .next()
            .unwrap_or(0);
        assert_eq!(line_count, 4, "all 4 lines on the single page");
    }

    #[test]
    fn form_feed_tail_starts_at_top_no_y_offset_drift() {
        /* After the forced split, the tail's first line origin must
        sit at y=0 within its new ParagraphBox. The renderer adds
        the page-level origin (content_y), so a non-zero residual
        from the source paragraph would push the tail down the page
        — exactly the "dangling empty line" bug. Verify by
        inspecting the first line of the tail page. */
        let geom = a4_geometry();
        let mut pag = Paginator::with_default_bands(geom, None, None);
        pag.push_block(
            LayoutBlock::Paragraph(fake_paragraph_with_page_break(4, 16.0, 1)),
            0.0,
            0.0,
        );
        let pages = pag.finish();
        assert_eq!(pages.len(), 2);
        let tail_para = pages[1]
            .blocks
            .iter()
            .find_map(|b| match b {
                LayoutBlock::Paragraph(p) => Some(p),
                _ => None,
            })
            .expect("tail paragraph");
        assert_eq!(
            tail_para.lines[0].origin.y, 0.0,
            "tail's first line must start at y=0 (no drift)"
        );
        assert_eq!(
            tail_para.origin.y, 0.0,
            "tail paragraph origin at content top"
        );
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

    /// Audit gap A.H2 — a 2-column section keeps an oversized paragraph
    /// on a single page: the first column overflows, the paginator
    /// snakes into column 2, and only when both columns fill does it
    /// flush a new page. Column-1 blocks land at `x = 0`; column-2
    /// blocks land at `x = column_width + gutter`.
    #[test]
    fn paginator_snake_flows_two_columns_before_flushing_page() {
        let geom = a4_geometry();
        let mut pag = Paginator::with_default_bands(geom, None, None);
        let column_width = geom.width - geom.margins.left - geom.margins.right; // pre-set
        pag.set_columns(2, 12.0);
        let new_column_width = pag.column_width();
        assert!(new_column_width < column_width);
        /* 120 lines × 16 pt = 1920 pt content. A4 column budget is
        ~698 pt per column; two columns fit ~1396 pt → one overflow
        page expected. */
        let para = fake_paragraph(120, 16.0);
        pag.push_block(LayoutBlock::Paragraph(para), 0.0, 0.0);
        let pages = pag.finish();
        assert!(
            pages.len() >= 2,
            "120-line para in 2-col layout should span ≥2 pages; got {}",
            pages.len()
        );
        /* Page 1 must carry blocks in both columns — at least one
        with x=0 and at least one with x > 0. */
        let page1 = &pages[0];
        let xs: Vec<f32> = page1.blocks.iter().map(|b| b.origin().x).collect();
        assert!(
            xs.iter().any(|&x| x.abs() < 0.5),
            "expected a column-0 block on page 1, got xs={xs:?}"
        );
        assert!(
            xs.iter().any(|&x| x > 0.5),
            "expected a column-1 block on page 1, got xs={xs:?}"
        );
        /* Page 2 starts a fresh page in column 0. */
        let page2 = &pages[1];
        assert!(
            page2
                .blocks
                .first()
                .map(|b| b.origin().x.abs() < 0.5)
                .unwrap_or(false),
            "first block of a fresh page must reset to column 0",
        );
    }

    /// Audit gap A.H2 — single-column sections behave exactly as before.
    /// Locks the regression-proofing invariant: every committed block
    /// stays at `x = 0` and overflow flushes the page (no surprise
    /// column-advance step). Mirror of the legacy single-page test.
    #[test]
    fn paginator_single_column_unchanged_origin_x() {
        let geom = a4_geometry();
        let mut pag = Paginator::with_default_bands(geom, None, None);
        let para = fake_paragraph(3, 16.0);
        pag.push_block(LayoutBlock::Paragraph(para), 0.0, 0.0);
        let pages = pag.finish();
        assert_eq!(pages.len(), 1);
        for b in &pages[0].blocks {
            assert!(
                b.origin().x.abs() < 0.001,
                "single-column block must have origin.x == 0; got {}",
                b.origin().x
            );
        }
    }

    /* ---------- L2.3 (#8) continuous-section column balancing ---------- */

    /// L2.3 (#8) — even-sized blocks in a 2-column section must
    /// balance to columns ending within ±1 pt of each other after
    /// the balance pass.
    #[test]
    fn continuous_section_balances_two_col_within_one_pt() {
        let geom = a4_geometry();
        let mut pag = Paginator::with_default_bands(geom, None, None);
        pag.set_columns(2, 12.0);
        /* 4 paragraphs × 50 pt each = 200 pt total → target 100 pt
        per column. */
        for _ in 0..4 {
            let para = fake_paragraph(1, 50.0);
            pag.push_block(LayoutBlock::Paragraph(para), 0.0, 0.0);
        }
        pag.balance_current_section_columns();
        /* Inspect cur_blocks via finishing the paginator. */
        let pages = pag.finish();
        assert_eq!(pages.len(), 1, "all content fits one A4 page");
        let blocks = &pages[0].blocks;
        assert_eq!(blocks.len(), 4);

        let col_x_0 = blocks[0].origin().x;
        let col_x_1 = blocks[2].origin().x;
        assert!(
            (col_x_0 - 0.0).abs() < 0.5,
            "col 0 first block x ≈ 0; got {col_x_0}"
        );
        assert!(
            col_x_1 > col_x_0 + 0.5,
            "col 1 first block x must be greater than col 0; got col0={col_x_0} col1={col_x_1}"
        );

        /* Column tails: bottom of last block in each column. */
        let col0_tail = blocks[1].origin().y + blocks[1].size().height;
        let col1_tail = blocks[3].origin().y + blocks[3].size().height;
        assert!(
            (col0_tail - col1_tail).abs() < 1.0,
            "balanced columns must end within ±1 pt; col0_tail={col0_tail}, col1_tail={col1_tail}"
        );
    }

    /// L2.3 (#8) — single-column sections short-circuit the balance
    /// pass; block origins stay byte-identical to today's snake-flow
    /// output. Regression guard for the visual-diff farm.
    #[test]
    fn continuous_section_single_column_skips_balance() {
        let geom = a4_geometry();
        let mut pag = Paginator::with_default_bands(geom, None, None);
        /* No set_columns call → default single-column. */
        for _ in 0..3 {
            let para = fake_paragraph(1, 50.0);
            pag.push_block(LayoutBlock::Paragraph(para), 0.0, 0.0);
        }
        /* Snapshot block origins BEFORE the balance pass via a
        finish() round-trip. Then re-run on a fresh paginator with
        balance interleaved, and confirm identical y-stack. */
        let mut pag_balanced = Paginator::with_default_bands(geom, None, None);
        for _ in 0..3 {
            let para = fake_paragraph(1, 50.0);
            pag_balanced.push_block(LayoutBlock::Paragraph(para), 0.0, 0.0);
        }
        pag_balanced.balance_current_section_columns();

        let pages_a = pag.finish();
        let pages_b = pag_balanced.finish();
        assert_eq!(pages_a.len(), pages_b.len(), "page count unchanged");
        for (a, b) in pages_a[0].blocks.iter().zip(pages_b[0].blocks.iter()) {
            assert!(
                (a.origin().x - b.origin().x).abs() < 0.001,
                "single-col x unchanged; before={} after={}",
                a.origin().x,
                b.origin().x
            );
            assert!(
                (a.origin().y - b.origin().y).abs() < 0.001,
                "single-col y unchanged; before={} after={}",
                a.origin().y,
                b.origin().y
            );
        }
    }

    /// L2.3 (#8) — uneven block sizes in a 2-col section: the last
    /// column accepts overflow because it cannot snake further.
    /// Greedy v1 pins the documented imbalance behaviour; LP-style
    /// balance is a follow-up.
    #[test]
    fn continuous_section_uneven_blocks_respect_last_column_overflow() {
        let geom = a4_geometry();
        let mut pag = Paginator::with_default_bands(geom, None, None);
        pag.set_columns(2, 12.0);
        /* One 80 pt block + three 40 pt blocks = 200 pt total →
        target 100 pt. Block 0 (80) lands in col 0 (y_in_col=80).
        Block 1 (40): 80+40=120 > 100 → advance to col 1.
        Blocks 1, 2, 3 chain in col 1 → col 1 tail = 120. */
        let heights = [80.0_f32, 40.0, 40.0, 40.0];
        for h in heights {
            let para = fake_paragraph(1, h);
            pag.push_block(LayoutBlock::Paragraph(para), 0.0, 0.0);
        }
        pag.balance_current_section_columns();
        let pages = pag.finish();
        let blocks = &pages[0].blocks;
        assert_eq!(blocks.len(), 4);

        /* Block 0 (the 80 pt block) is alone in col 0. */
        let col0_first_x = blocks[0].origin().x;
        let col1_first_x = blocks[1].origin().x;
        assert!(
            col1_first_x > col0_first_x + 0.5,
            "block 1 must snake into col 1; col0_x={col0_first_x}, col1_x={col1_first_x}"
        );
        let col0_tail = blocks[0].origin().y + blocks[0].size().height;
        let col1_tail = blocks[3].origin().y + blocks[3].size().height;
        assert!(
            (col0_tail - 80.0).abs() < 0.5,
            "col 0 ends at the 80 pt block's bottom; got {col0_tail}"
        );
        assert!(
            (col1_tail - 120.0).abs() < 0.5,
            "col 1 absorbs the three 40 pt blocks; got {col1_tail}"
        );
    }
}
