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
use crate::watchdog::{BlockFingerprint, DegradeReason, DegradeStage, LayoutDegradation, Watchdog};
use std::collections::HashMap;

/// Per-role header / footer bands the paginator picks from for each
/// page (Phase 2 audit — C.1 / C.2 / C.3). `default` covers pages
/// where no more-specific variant applies. `first` is used for the
/// first page of the section when [`Paginator::title_pg`] is `true`;
/// `even` is used for even-NUMBERED pages when
/// [`Paginator::even_and_odd_headers`] is `true`. Issue #70 — a
/// missing variant is a BLANK band, never a fallback to `default`:
/// §17.10.3 inherits each role independently ACROSS sections (the
/// engine resolves that before bands are built), and Word observably
/// shows an empty first-page header when titlePg is on with no first
/// part.
#[derive(Debug, Clone, Default)]
pub struct HeaderBands {
    pub default: Option<HeaderFooterBox>,
    pub first: Option<HeaderFooterBox>,
    pub even: Option<HeaderFooterBox>,
}

pub use crate::boxes::HeaderRole;

impl HeaderBands {
    /// The band for `role`, exactly — `None` means a blank band
    /// (issue #70 removed the role→`Default` fallback; see the type
    /// docs).
    pub fn resolve(&self, role: HeaderRole) -> Option<&HeaderFooterBox> {
        match role {
            HeaderRole::Default => self.default.as_ref(),
            HeaderRole::First => self.first.as_ref(),
            HeaderRole::Even => self.even.as_ref(),
        }
    }
}

/// Phase 8a — vertical gap above the footnote separator rule, in layout
/// pt at scale=1. The renderer multiplies by `scale` if it needs device
/// pixels.
pub const FOOTNOTE_SEPARATOR_HEIGHT_PT: f32 = 12.0;

/// Issue #87 — stage (c) of the watchdog ladder: the hard cap on pages
/// one paginator emits. Past it the remaining flow is appended to the
/// current page without further page breaks and a `PageCap` note is
/// recorded. A last-resort net, not a product limit: the 500-page perf
/// fixture sits two orders of magnitude below it, and at this height
/// `f32` page-top arithmetic has already lost sub-point precision.
pub const DEFAULT_PAGE_CAP: usize = 65_536;

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
    /// Issue #43 — the render-time date `(year, month, day)` DATE
    /// fields resolve against. `None` (tests, headless) keeps the
    /// cached text. Injected by the shell at boot (`SetRenderDate`) —
    /// the engine core never reads a wall clock.
    render_date: Option<(i32, u32, u32)>,
    /// Issue #87 — churn watchdog + degradation notes for this flow.
    watchdog: Watchdog,
    /// Issue #87 — stage (c): hard cap on emitted pages.
    page_cap: usize,
    /// Set once the cap is hit; every later block is appended to the
    /// current page atomically, without a page break.
    capped: bool,
}

impl Paginator {
    pub fn new(
        geometry: PageGeometry,
        headers: HeaderBands,
        footers: HeaderBands,
        title_pg: bool,
        even_and_odd_headers: bool,
    ) -> Self {
        let mut p = Self {
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
            render_date: None,
            watchdog: Watchdog::default(),
            page_cap: DEFAULT_PAGE_CAP,
            capped: false,
        };
        /* Issue #71 (design review B3) — page 1 opens BELOW its
        header band when the band is taller than the top margin. */
        p.cur_y = p.opening_cur_y();
        p
    }

    /// Issue #87 — override the stage (c) page cap (tests, embedders
    /// with a known page budget).
    pub fn with_page_cap(mut self, cap: usize) -> Self {
        self.page_cap = cap.max(1);
        self
    }

    /// Issue #87 — test-only switch: every degradation becomes a hard
    /// failure so CI catches a new loop instead of a silent recovery.
    pub fn with_strict_watchdog(mut self, strict: bool) -> Self {
        self.watchdog = std::mem::take(&mut self.watchdog).strict(strict);
        self
    }

    /// Issue #87 — degradation notes recorded so far (drained by
    /// [`Self::finish_with_notes`]).
    pub fn degradations(&self) -> &[LayoutDegradation] {
        self.watchdog.notes()
    }

    /// Issue #87 — the watchdog stage in force for the block currently
    /// being placed (`Nominal` between blocks).
    pub fn watchdog_stage(&self) -> DegradeStage {
        self.watchdog.stage()
    }

    /// Issue #87 — direct access for tests that exercise a ladder stage
    /// the local termination rules would otherwise pre-empt.
    #[cfg(test)]
    fn watchdog_mut(&mut self) -> &mut Watchdog {
        &mut self.watchdog
    }

    /// 0-based index of the page currently being filled — what the
    /// degradation notes stamp.
    fn cur_page_index(&self) -> u32 {
        self.pages.len() as u32
    }

    /// Issue #43 — install the render-time date for DATE fields.
    pub fn with_render_date(mut self, date: Option<(i32, u32, u32)>) -> Self {
        self.render_date = date;
        self
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
        /* Issue #87 — the nominal column walk re-pushes identical
        content once per remaining column before a page flush; the
        non-fresh churn threshold must sit above that. */
        self.watchdog
            .set_walk_threshold(u32::from(self.column_count) + 1);
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

    /// Pick which header/footer role applies to the page numbered
    /// `pages.len() + 1` within this paginator. Precedence:
    /// 1. Section first page AND `title_pg` → `First`.
    /// 2. `even_and_odd_headers` AND the FORMATTED page number is even
    ///    → `Even`.
    /// 3. Otherwise → `Default`.
    ///
    /// Issue #74 — parity comes from [`Self::current_formatted_page_no`]:
    /// doc-wide (`doc_page_offset` carries prior sections' pages — the
    /// old `pages.len()+1` reset every non-continuous section) AND
    /// restart-aware. Word keys even/odd bands to the page NUMBER, not
    /// the physical sheet: a section restarting at 2 opens with the
    /// even-page header. (Recorded design decision R-M4 — supersedes
    /// the earlier "absolute parity" comment, which predates restart
    /// awareness.)
    fn page_role(&self) -> HeaderRole {
        if self.section_first_page_pending && self.title_pg {
            HeaderRole::First
        } else if self.even_and_odd_headers && self.current_formatted_page_no() % 2 == 0 {
            HeaderRole::Even
        } else {
            HeaderRole::Default
        }
    }

    /// The formatted (displayed) number of the page currently being
    /// filled — the same value a PAGE field on it renders. Shared by
    /// role parity and `flush_page`'s `PageBox::page_number` stamp so
    /// they can never diverge.
    fn current_formatted_page_no(&self) -> u32 {
        let doc_page = self.doc_page_offset + self.pages.len() as u32 + 1;
        let section_start_doc_page = self.doc_page_offset + 1;
        match self.page_num.start {
            Some(n) => n + doc_page.saturating_sub(section_start_doc_page),
            None => doc_page,
        }
    }

    /// Issue #71 — how far the given role's HEADER band spills past
    /// the top margin. Body content on such a page starts below the
    /// band instead of overlapping it (Word treats the header offset
    /// as a soft minimum).
    fn header_intrusion(&self, role: HeaderRole) -> f32 {
        let h = self
            .headers
            .resolve(role)
            .map_or(0.0, HeaderFooterBox::content_height);
        (self.geometry.header_offset + h - self.geometry.margins.top).max(0.0)
    }

    /// Footer twin of [`Self::header_intrusion`] — the band grows
    /// UPWARD past the bottom margin, shrinking the body budget.
    fn footer_intrusion(&self, role: HeaderRole) -> f32 {
        let h = self
            .footers
            .resolve(role)
            .map_or(0.0, HeaderFooterBox::content_height);
        (self.geometry.footer_offset + h - self.geometry.margins.bottom).max(0.0)
    }

    /// Where `cur_y` opens on the page/column currently being filled:
    /// 0 for a band that fits its margin, the intrusion depth when it
    /// spills. Uses the OPENING page's role — callers must invoke this
    /// AFTER `pages.push` + the pending-flag update (design review B3:
    /// reusing the closing page's role feeds the wrong band height).
    fn opening_cur_y(&self) -> f32 {
        self.header_intrusion(self.page_role())
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

    /// Total height of the pages already finalised inside this paginator
    /// (excluding the in-progress page). The viewport cull sums this with
    /// [`Self::cursor_y`] to know how much document height the current
    /// section has really committed — `cursor_y` alone resets at every
    /// page break, so without this term a single-section document never
    /// trips the cull budget no matter how many pages it has flushed.
    pub fn emitted_pages_height(&self) -> f32 {
        self.pages.iter().map(|p| p.size.height).sum()
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
            /* Same page → same role → same header intrusion: every
            column starts below the band (issue #71). */
            self.cur_y = self.opening_cur_y();
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
        self.watchdog
            .set_walk_threshold(Watchdog::DEFAULT_WALK_THRESHOLD);
        /* The flush above seeded `cur_y` from the OLD section's bands;
        the new section's first page opens under its own (issue #71). */
        self.cur_y = self.opening_cur_y();
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
    pub fn push_block(&mut self, block: LayoutBlock, before: f32, after: f32) {
        /* Issue #87 — one top-level block is the watchdog's window:
        churn counters and the escalation stage restart here. */
        self.watchdog.begin_block();
        self.push_block_inner(block, before, after, true);
    }

    /// The flow step proper. Every internal re-push (a paragraph tail, a
    /// table continuation, a relocated keep-chain) re-enters HERE, not
    /// through [`Self::push_block`], so the watchdog sees the whole
    /// attempt sequence of one top-level block. `observe` is `true` for
    /// the top-level block and its own tails; a relocated keep-chain
    /// block re-enters with `false` — it is a different block whose
    /// attempts must not be counted against the follower's fingerprint
    /// (both may well be "one line"). Its sub-flow is bounded by
    /// construction (tails shrink, the oversize guard clips) and still
    /// honours the stage the follower's churn has reached.
    fn push_block_inner(&mut self, block: LayoutBlock, before: f32, after: f32, observe: bool) {
        /* Apply the paragraph's `<w:spacing w:before>` first — the engine
        layer already had this concept; we keep it here so the paginator
        owns every Y-coordinate. */
        self.cur_y += before;

        /* Issue #87 stage (c) — past the page cap nothing breaks pages
        any more: append atomically and accept the state. */
        if self.capped {
            self.place_atomic(block, after);
            return;
        }
        /* Issue #87 — churn accounting. A fingerprint the watchdog has
        seen before on a fresh page is a proof of non-progress; the
        ladder answers with stage (a) (constraints released, retried
        below) and then stage (b): pin the block right here. */
        let stage = if observe {
            let fp = BlockFingerprint::of(&block, self.cur_blocks.is_empty());
            self.watchdog.observe(fp)
        } else {
            self.watchdog.stage()
        };
        if stage >= DegradeStage::Freeze {
            let page = self.cur_page_index();
            self.watchdog.note(DegradeReason::FrozenPlacement, page);
            self.place_atomic(block, after);
            return;
        }

        /* Phase 8a — gather every NEW footnote referenced by this block
        (already-on-page refs don't grow the band) and provisionally
        commit their heights to the budget. We undo the commit if the
        block ends up forced onto a new page. */
        let new_refs: Vec<u32> = collect_footnote_refs(&block)
            .into_iter()
            .filter(|id| !self.cur_footnote_ids.contains(id))
            .collect();
        let (added_height, added_separator) = self.try_consume_footnotes(&new_refs);

        let remaining = self.geometry.content_height()
            - self.cur_y
            - self.cur_footnote_height
            - self.footer_intrusion(self.page_role());
        let block_height = block.size().height;

        /* Issue #87 — footnote negotiation is one-shot (commit, check,
        roll back), so it cannot loop; but a band that leaves NO body
        budget on a fresh page can never be satisfied by any later page
        either. Bouncing the block forward would drop the footnote on
        the floor (the rollback below discards the refs while the head
        that carries them stays here). Keep the band, place the block
        atomically over it and say so. */
        if remaining < 0.0 && !new_refs.is_empty() && self.cur_blocks.is_empty() {
            let page = self.cur_page_index();
            self.watchdog.note(DegradeReason::FootnoteOverflow, page);
            self.place_atomic(block, after);
            return;
        }

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
            /* Issue #87 — an atomic table taller than the budget clips
            past the bottom margin. Same placement as before; now the
            paint says so. */
            if block_height > remaining {
                let page = self.cur_page_index();
                self.watchdog.note(DegradeReason::OversizeLine, page);
            }
            /* Audit gap A.H2 — origin.x carries the column offset for
            multi-column sections (zero for single-column, preserving
            the legacy "page-wide" behaviour). */
            self.place_atomic(block, after);
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
            LayoutBlock::Paragraph(p) => self.push_paragraph_split(p, after, observe),
            LayoutBlock::Table(t) => self.push_table_split(t, after, observe),
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

    /// Issue #87 — un-commit footnotes a relocated keep-chain carried:
    /// the ids leave this page's band (they re-commit on the page the
    /// chain lands on) and the band height shrinks accordingly.
    fn uncommit_footnotes(&mut self, ids: &[u32]) {
        for id in ids {
            if let Some(pos) = self.cur_footnote_ids.iter().rposition(|x| x == id) {
                self.cur_footnote_ids.remove(pos);
                let h = self.footnote_bodies.get(id).map_or(0.0, |b| b.size.height);
                self.cur_footnote_height -= h;
            }
        }
        if self.cur_footnote_ids.is_empty() || self.cur_footnote_height < 0.0 {
            self.cur_footnote_height = 0.0;
        }
    }

    /// Place `block` at the cursor in the current column, whatever its
    /// height — the "fits" step and every terminal degradation share it.
    fn place_atomic(&mut self, mut block: LayoutBlock, after: f32) {
        let h = block.size().height;
        let mut origin = block.origin();
        origin.x = self.current_column_origin_x();
        origin.y = self.cur_y;
        block.set_origin(origin);
        self.cur_y += h + after;
        self.cur_blocks.push(block);
    }

    /// Issue #87 — keep-with-next. The trailing run of `keep_next`
    /// paragraphs in the current column is the chain that must travel
    /// with the block about to move to the next column / page. Returns
    /// the detached chain (in flow order), or an empty vector when the
    /// constraint is released: no chain, the chain already opens the
    /// column (there is no earlier page it could move back from — the
    /// constraint is unsatisfiable, Word drops it too), or the watchdog
    /// is at stage (a) or beyond. Releasing records `KeepChainDropped`.
    fn detach_keep_chain(&mut self) -> Vec<LayoutBlock> {
        let col_x = self.current_column_origin_x();
        let in_column = |b: &LayoutBlock| (b.origin().x - col_x).abs() < 0.001;
        let mut start = self.cur_blocks.len();
        while start > 0 {
            match &self.cur_blocks[start - 1] {
                LayoutBlock::Paragraph(p)
                    if p.keep_next && in_column(&self.cur_blocks[start - 1]) =>
                {
                    start -= 1;
                }
                _ => break,
            }
        }
        if start == self.cur_blocks.len() {
            return Vec::new();
        }
        let has_anchor_before = start > 0 && in_column(&self.cur_blocks[start - 1]);
        if !has_anchor_before || self.watchdog.stage() >= DegradeStage::DropOptional {
            let page = self.cur_page_index();
            self.watchdog.note(DegradeReason::KeepChainDropped, page);
            /* Release the flag on the chain so a later block cannot
            re-trigger the same unsatisfiable move. */
            for b in self.cur_blocks[start..].iter_mut() {
                if let LayoutBlock::Paragraph(p) = b {
                    p.keep_next = false;
                }
            }
            return Vec::new();
        }
        let chain: Vec<LayoutBlock> = self.cur_blocks.drain(start..).collect();
        let ids: Vec<u32> = chain.iter().flat_map(collect_footnote_refs).collect();
        self.uncommit_footnotes(&ids);
        chain
    }

    /// Issue #87 — move the block that does not fit to the next column /
    /// page, carrying its keep-with-next chain along. Chain blocks re-enter
    /// the flow ahead of `block`; their original `before` / `after`
    /// spacing is not stored on the box and is not re-applied.
    fn advance_with_keep_chain(&mut self, block: LayoutBlock, after: f32, observe: bool) {
        let chain = self.detach_keep_chain();
        self.advance_column_or_flush_page();
        for b in chain {
            self.push_block_inner(b, 0.0, 0.0, false);
        }
        self.push_block_inner(block, 0.0, after, observe);
    }

    fn push_paragraph_split(&mut self, para: ParagraphBox, after: f32, observe: bool) {
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
                self.push_block_inner(LayoutBlock::Paragraph(head), 0.0, 0.0, observe);
            }
            /* Force the flush even when head was empty (page break at
            the very first line — produces a blank "current page"
            then the tail starts fresh; matches Word's behaviour). */
            self.flush_page();
            if let Some(tail) = tail_opt {
                self.push_block_inner(LayoutBlock::Paragraph(tail), 0.0, after, observe);
            }
            return;
        }

        let remaining = self.geometry.content_height()
            - self.cur_y
            - self.cur_footnote_height
            - self.footer_intrusion(self.page_role());
        let (head, tail) = split_paragraph_at_line(&para, remaining);

        match (head, tail) {
            (None, Some(tail)) if self.cur_blocks.is_empty() => {
                /* Pathological case — even the first line of the
                paragraph is taller than a fresh content area. Stuff
                atomically (single oversize line clips the bottom; a
                proper line-internal splitter is deferred). Without
                this guard `push_block` would recurse on the same
                tail on every fresh page → infinite loop. Issue #87 —
                the clip is now a reported degradation. */
                let page = self.cur_page_index();
                self.watchdog.note(DegradeReason::OversizeLine, page);
                self.place_atomic(LayoutBlock::Paragraph(tail), after);
            }
            (None, Some(tail)) => {
                /* Not even the first line fits in the *current* column.
                Audit gap A.H2 — snake into the next column if available,
                otherwise flush the page. The tail re-enters the flow
                with a full column-of-content budget so the next attempt
                always succeeds (or hits the atomic-single-line clip
                path above). Issue #87 — a keep-with-next chain ending
                right before this paragraph travels with it. */
                self.advance_with_keep_chain(LayoutBlock::Paragraph(tail), after, observe);
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
                    self.push_block_inner(LayoutBlock::Paragraph(tail), 0.0, after, observe);
                } else {
                    self.cur_y += after;
                }
            }
            (None, None) => { /* Empty paragraph — nothing to do. */ }
        }
    }

    fn push_table_split(&mut self, table: TableBox, after: f32, observe: bool) {
        /* If the table is non-empty, try moving the *whole* table to a new
        column (or, when no further columns exist, a new page) first —
        that handles the common "table just barely overflows the column
        footer" case without an ugly row split. Audit gap A.H2 — the
        snake advance keeps the table inside the current page when a
        sibling column has room. */
        if !self.cur_blocks.is_empty() {
            /* Issue #87 — a keep-with-next chain ending right before
            this table travels with it. */
            self.advance_with_keep_chain(LayoutBlock::Table(table), after, observe);
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
        /* Design review M3 — an "empty" page no longer implies
        `cur_y == 0`: the header-intrusion opening offset (and any
        footer intrusion) already consumed budget. The old bare
        `content_height()` here silently seated rows into the footer
        band on intruded pages. */
        let budget = self.geometry.content_height()
            - self.cur_y
            - self.cur_footnote_height
            - self.footer_intrusion(self.page_role());
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
        let mut header_rows: Vec<TableRowBox> =
            head.rows.iter().filter(|r| r.header).cloned().collect();
        /* Issue #87 stage (a) — repeated headers are an OPTIONAL
        constraint. When every row that fit on this page was a header
        row, the continuation would be `headers + the same body rows`
        — the exact table we started from — and the next page would
        replay this split forever (the #7 class: an autofit column
        narrower than its longest token wraps a header row past the
        page height). Suppress the repeat for this continuation so the
        tail is strictly smaller; likewise once the watchdog has
        reached stage (a) on its own. */
        let head_all_headers = head.rows.iter().all(|r| r.header);
        let drop_repeat = !tail_rows.is_empty()
            && !header_rows.is_empty()
            && (head_all_headers || self.watchdog.stage() >= DegradeStage::DropOptional);
        if drop_repeat {
            let page = self.cur_page_index();
            self.watchdog.note(DegradeReason::HeaderRepeatDropped, page);
            header_rows.clear();
        }
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
            self.push_block_inner(LayoutBlock::Table(tail), 0.0, after, observe);
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
        render_date: Option<(i32, u32, u32)>,
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
            match synthetic.keyword().as_str() {
                "PAGE" => {
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
                /* Issue #43 — DATE resolves against the shell-injected
                render date (Word updates DATE on open/print). No date
                installed → cached text stands. */
                "DATE" => {
                    if let Some((y, m, d)) = render_date {
                        let pic = synthetic
                            .date_picture()
                            .unwrap_or_else(|| "M/d/yyyy".to_string());
                        f.evaluated_text = Some(engine::render_date_picture(&pic, y, m, d));
                    }
                }
                _ => {}
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
        /* `cur_y` is re-seeded at the END of this fn — the opening
        offset depends on the NEXT page's role, which is only known
        after `pages.push` (design review B3). */
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
        let render_date = self.render_date;
        let mut blocks = blocks;
        for block in blocks.iter_mut() {
            Self::for_each_paragraph_in_block(block, &mut |p| {
                Self::evaluate_fields_on_paragraph(
                    p,
                    doc_page,
                    page_num,
                    section_start_doc_page,
                    render_date,
                );
            });
        }
        if let Some(hf) = header.as_mut() {
            hf.for_each_paragraph_mut(&mut |p| {
                Self::evaluate_fields_on_paragraph(
                    p,
                    doc_page,
                    page_num,
                    section_start_doc_page,
                    render_date,
                );
            });
        }
        if let Some(hf) = footer.as_mut() {
            hf.for_each_paragraph_mut(&mut |p| {
                Self::evaluate_fields_on_paragraph(
                    p,
                    doc_page,
                    page_num,
                    section_start_doc_page,
                    render_date,
                );
            });
        }

        /* Even an empty page is emitted on an explicit `force_page_break`
        / `start_new_section` — the renderer paints the blank sheet so a
        section break is visible. */
        let page_number = self.current_formatted_page_no();
        let mut page = PageBox {
            size: Size {
                width: self.geometry.width,
                height: self.geometry.height,
            },
            margins: self.geometry.margins,
            blocks,
            header,
            footer,
            /* Phase 3 (#39) — carried forward so the renderer + story
            hit-testing place bands from the document's real
            `<w:pgMar w:header/w:footer>` instead of a margin fraction. */
            header_offset: self.geometry.header_offset,
            footer_offset: self.geometry.footer_offset,
            footnotes,
            /* Issues #70/#74/#43 — which band slot this page resolved
            and the formatted number it displays. Enter-header/footer
            derives the double-clicked page's role from `hf_role`; the
            field-resolution pass reads `page_number`. */
            hf_role: role,
            page_number,
            floats: Vec::new(),
        };
        /* Issue #69 — floating objects are a pure function of the placed
        blocks (no wrap yet, so no feedback into the flow): resolve them
        once the page's geometry is final. One pass, no iteration. */
        page.floats = crate::floats::resolve_page_floats(
            &page,
            crate::floats::ColumnLayout {
                count: self.column_count.max(1),
                gutter: self.column_gutter,
            },
        );
        self.pages.push(page);

        /* Clear the section-first-page flag once a page has flushed for
        the section. Subsequent pages in the same section pick
        `Default` or `Even`. */
        self.section_first_page_pending = false;
        /* Design review B3 — NOW the next page's role is computable
        (pages.len() bumped, pending flag settled): open below its own
        header band. */
        self.cur_y = self.opening_cur_y();
        /* Issue #87 stage (c) — the page cap. From here on every block
        is appended to the current page without a break; the layout is
        accepted as final rather than risking an unbounded flow. */
        if !self.capped && self.pages.len() >= self.page_cap {
            self.capped = true;
            let page = self.cur_page_index();
            self.watchdog.note(DegradeReason::PageCap, page);
            self.watchdog.escalate_to(DegradeStage::ForceValidate);
        }
    }

    /// [`Self::finish`] plus every degradation note the watchdog
    /// recorded for this flow (issue #87). The engine forwards the notes
    /// on `Event::Painted`.
    pub fn finish_with_notes(mut self) -> (Vec<PageBox>, Vec<LayoutDegradation>) {
        let notes = self.watchdog.take_notes();
        (self.finish(), notes)
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
                hf.for_each_paragraph_mut(&mut stamp);
            }
            if let Some(hf) = page.footer.as_mut() {
                hf.for_each_paragraph_mut(&mut stamp);
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
        keep_next: false,
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
        keep_next: para.keep_next,
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
        keep_next: false,
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
        keep_next: para.keep_next,
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
    use crate::watchdog::geometry_fingerprint;
    use std::time::{Duration, Instant};
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
                source_start: 0,
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
            keep_next: false,
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
            blocks: vec![LayoutBlock::Paragraph(ParagraphBox {
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
                keep_next: false,
            })],
            source_rid: None,
        }
    }

    /// Extract the `source_paragraph_id` of a page's header `ParagraphBox`,
    /// or `u32::MAX` when no header was attached. Lets the per-role
    /// selection tests assert which band the paginator picked.
    fn header_tag(p: &PageBox) -> u32 {
        p.header
            .as_ref()
            .and_then(|h| h.blocks.first())
            .and_then(|b| match b {
                LayoutBlock::Paragraph(p) => Some(p),
                LayoutBlock::Table(_) => None,
            })
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
                source_start: 0,
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
            keep_next: false,
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
                blocks: vec![LayoutBlock::Paragraph(fake_paragraph_with_field(
                    "PAGE", 16.0,
                ))],
                source_rid: None,
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
            let LayoutBlock::Paragraph(para) = hf.blocks.first()? else {
                return None;
            };
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
                blocks: vec![LayoutBlock::Paragraph(fake_paragraph_with_field(
                    "NUMPAGES", 16.0,
                ))],
                source_rid: None,
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
                .and_then(|hf| hf.blocks.first())
                .and_then(|b| match b {
                    LayoutBlock::Paragraph(p) => Some(p),
                    LayoutBlock::Table(_) => None,
                })
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
    fn missing_first_is_a_blank_band_not_default() {
        /* Issue #70 flipped this pin: `title_pg` requests `First` and
        only `Default` is set — §17.10.3 inherits each role
        independently across sections and an exhausted chain is a
        BLANK band. Word observably shows an empty first-page header
        when titlePg is on with no first part; the Default content
        must NOT bleed through. Pages after the first read Default as
        before. */
        let geom = a4_geometry();
        let headers = HeaderBands {
            default: Some(fake_band(7)),
            first: None,
            even: None,
        };
        let mut pag = Paginator::new(geom, headers, HeaderBands::default(), true, false);
        pag.push_block(LayoutBlock::Paragraph(fake_paragraph(40, 16.0)), 0.0, 0.0);
        pag.force_page_break();
        pag.push_block(LayoutBlock::Paragraph(fake_paragraph(1, 16.0)), 0.0, 0.0);
        let pages = pag.finish();
        assert!(
            pages[0].header.is_none(),
            "titlePg with no First part → blank first-page header"
        );
        assert_eq!(
            header_tag(&pages[1]),
            7,
            "subsequent pages read Default as before"
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

    /// Taller `fake_band` variant for the intrusion tests — one
    /// paragraph of the given height.
    fn fake_band_tall(tag: u32, height: f32) -> HeaderFooterBox {
        let mut band = fake_band(tag);
        if let Some(LayoutBlock::Paragraph(p)) = band.blocks.first_mut() {
            p.size.height = height;
        }
        band
    }

    #[test]
    fn even_odd_parity_survives_a_section_boundary() {
        /* Issue #74 regression — the old `pages.len() + 1` parity
        reset on every fresh (per-section) Paginator, so a section
        starting on doc page 2 wrongly opened with the ODD header.
        `doc_page_offset` carries the true document-wide count. */
        let geom = a4_geometry();
        let headers = HeaderBands {
            default: Some(fake_band(10)),
            first: None,
            even: Some(fake_band(20)),
        };
        let mut pag = Paginator::new(geom, headers, HeaderBands::default(), false, true);
        /* Simulate "one page already emitted by the prior section". */
        pag.set_page_numbering(engine::PageNumType::default(), 1);
        pag.push_block(LayoutBlock::Paragraph(fake_paragraph(1, 16.0)), 0.0, 0.0);
        let pages = pag.finish();
        assert_eq!(
            header_tag(&pages[0]),
            20,
            "section opening on doc page 2 shows the EVEN header"
        );
    }

    #[test]
    fn even_odd_parity_is_restart_aware() {
        /* Design decision R-M4 — Word keys even/odd bands to the page
        NUMBER, not the physical sheet: a section restarting at 2
        opens with the even-page header even on the document's first
        physical page. */
        let geom = a4_geometry();
        let headers = HeaderBands {
            default: Some(fake_band(10)),
            first: None,
            even: Some(fake_band(20)),
        };
        let mut pag = Paginator::new(geom, headers, HeaderBands::default(), false, true);
        pag.set_page_numbering(
            engine::PageNumType {
                start: Some(2),
                ..Default::default()
            },
            0,
        );
        pag.push_block(LayoutBlock::Paragraph(fake_paragraph(1, 16.0)), 0.0, 0.0);
        pag.push_block(LayoutBlock::Paragraph(fake_paragraph(1, 16.0)), 0.0, 0.0);
        pag.force_page_break();
        pag.push_block(LayoutBlock::Paragraph(fake_paragraph(1, 16.0)), 0.0, 0.0);
        let pages = pag.finish();
        assert_eq!(header_tag(&pages[0]), 20, "displays 2 → even band");
        assert_eq!(header_tag(&pages[1]), 10, "displays 3 → default band");
        assert_eq!(pages[0].page_number, 2, "formatted number stamped");
        assert_eq!(pages[1].page_number, 3);
    }

    #[test]
    fn tall_header_band_pushes_body_content_down() {
        /* Issue #71 — header intrusion. Band 100 pt, header_offset 36,
        margins.top 72 → the band's bottom sits at 136, i.e. 64 pt past
        the margin. Body line 1 must open at cur_y = 64, not 0. */
        let geom = a4_geometry();
        let headers = HeaderBands {
            default: Some(fake_band_tall(9, 100.0)),
            first: None,
            even: None,
        };
        let mut pag = Paginator::new(geom, headers, HeaderBands::default(), false, false);
        pag.push_block(LayoutBlock::Paragraph(fake_paragraph(1, 16.0)), 0.0, 0.0);
        let pages = pag.finish();
        let first_block_y = pages[0].blocks[0].origin().y;
        assert!(
            (first_block_y - 64.0).abs() < 0.5,
            "body opens below the intruding band; got {first_block_y}"
        );
    }

    #[test]
    fn tall_footer_band_shrinks_the_body_budget() {
        /* Footer 100 pt, footer_offset 36, margins.bottom 72 → 64 pt
        of body budget consumed from the bottom. content_height 697.9;
        with 16 pt lines: floor((697.9 - 64) / 16) = 39 lines fit. */
        let geom = a4_geometry();
        let footers = HeaderBands {
            default: Some(fake_band_tall(9, 100.0)),
            first: None,
            even: None,
        };
        let mut pag = Paginator::new(geom, HeaderBands::default(), footers, false, false);
        pag.push_block(LayoutBlock::Paragraph(fake_paragraph(60, 16.0)), 0.0, 0.0);
        let pages = pag.finish();
        let page0_lines = match &pages[0].blocks[0] {
            LayoutBlock::Paragraph(p) => p.lines.len(),
            LayoutBlock::Table(_) => 0,
        };
        let budget = geom.content_height() - 64.0;
        let expect = (budget / 16.0).floor() as usize;
        assert_eq!(
            page0_lines, expect,
            "footer intrusion trims the split budget"
        );
        assert!(pages.len() >= 2, "overflow flowed to page 2");
    }

    #[test]
    fn opening_intrusion_uses_the_next_pages_role() {
        /* Design review B3 — title_pg with a TALL First band (100 pt →
        64 pt intrusion) and a short Default band. Page 1 opens at 64;
        page 2 must open at 0 (Default role), NOT reuse page 1's First
        intrusion. */
        let geom = a4_geometry();
        let headers = HeaderBands {
            default: Some(fake_band(10)),
            first: Some(fake_band_tall(11, 100.0)),
            even: None,
        };
        let mut pag = Paginator::new(geom, headers, HeaderBands::default(), true, false);
        pag.push_block(LayoutBlock::Paragraph(fake_paragraph(1, 16.0)), 0.0, 0.0);
        pag.force_page_break();
        pag.push_block(LayoutBlock::Paragraph(fake_paragraph(1, 16.0)), 0.0, 0.0);
        let pages = pag.finish();
        assert!(
            (pages[0].blocks[0].origin().y - 64.0).abs() < 0.5,
            "page 1 opens below the First band"
        );
        assert!(
            pages[1].blocks[0].origin().y.abs() < 0.5,
            "page 2 opens at the margin (Default role, no intrusion); got {}",
            pages[1].blocks[0].origin().y
        );
        assert_eq!(pages[0].hf_role, HeaderRole::First);
        assert_eq!(pages[1].hf_role, HeaderRole::Default);
    }

    #[test]
    fn date_field_resolves_against_the_render_date() {
        let geom = a4_geometry();
        let mut pag = Paginator::new(
            geom,
            HeaderBands::default(),
            HeaderBands::default(),
            false,
            false,
        )
        .with_render_date(Some((2026, 7, 5)));
        pag.push_block(
            LayoutBlock::Paragraph(fake_paragraph_with_field("DATE \\@ \"yyyy-MM-dd\"", 16.0)),
            0.0,
            0.0,
        );
        pag.push_block(
            LayoutBlock::Paragraph(fake_paragraph_with_field("DATE", 16.0)),
            0.0,
            0.0,
        );
        let pages = pag.finish();
        let evals: Vec<Option<String>> = pages[0]
            .blocks
            .iter()
            .filter_map(|b| match b {
                LayoutBlock::Paragraph(p) => {
                    Some(p.fields.first().and_then(|f| f.evaluated_text.clone()))
                }
                LayoutBlock::Table(_) => None,
            })
            .collect();
        assert_eq!(evals[0].as_deref(), Some("2026-07-05"), "picture honoured");
        assert_eq!(evals[1].as_deref(), Some("7/5/2026"), "Word en-default");

        /* No render date installed → cached text stands (None). */
        let mut bare = Paginator::new(
            geom,
            HeaderBands::default(),
            HeaderBands::default(),
            false,
            false,
        );
        bare.push_block(
            LayoutBlock::Paragraph(fake_paragraph_with_field("DATE", 16.0)),
            0.0,
            0.0,
        );
        let pages = bare.finish();
        let ev = match &pages[0].blocks[0] {
            LayoutBlock::Paragraph(p) => p.fields[0].evaluated_text.clone(),
            LayoutBlock::Table(_) => None,
        };
        assert_eq!(ev, None, "DATE is inert without an injected date");
    }

    /* ================================================================
    Issue #87 — layout self-defense: geometry regression anchor.
    ================================================================ */

    /// A fake one-cell-per-row table. `rows` = `(height, is_header)`.
    fn fake_table(rows: &[(f32, bool)]) -> TableBox {
        let mut out_rows = Vec::with_capacity(rows.len());
        let mut y = 0.0_f32;
        for &(h, header) in rows {
            out_rows.push(TableRowBox {
                origin: Point { x: 0.0, y },
                size: Size {
                    width: 200.0,
                    height: h,
                },
                cells: vec![crate::boxes::TableCellBox {
                    origin: Point { x: 0.0, y: 0.0 },
                    size: Size {
                        width: 200.0,
                        height: h,
                    },
                    grid_span: 1,
                    v_merge: engine::VMergeRole::None,
                    borders: engine::CellBorders::default(),
                    shading: None,
                    content: vec![LayoutBlock::Paragraph(fake_paragraph(1, h))],
                    padding_left: 0.0,
                    padding_top: 0.0,
                    padding_right: 0.0,
                    padding_bottom: 0.0,
                }],
                header,
                cant_split: false,
            });
            y += h;
        }
        TableBox {
            origin: Point::default(),
            size: Size {
                width: 200.0,
                height: y,
            },
            columns: vec![200.0],
            rows: out_rows,
            outer_borders: engine::CellBorders::default(),
        }
    }

    /// A one-line paragraph whose only glyph anchors footnote `id`.
    fn fake_paragraph_with_footnote_ref(id: u32, n_lines: usize, line_height: f32) -> ParagraphBox {
        let mut p = fake_paragraph(n_lines, line_height);
        let glyph = crate::boxes::PositionedGlyph {
            id: 0,
            cluster: 0,
            x_advance: 8.0,
            y_advance: 0.0,
            x_offset: 0.0,
            y_offset: 0.0,
            synthetic: false,
            inline_image_rel_id: None,
            inline_footnote_marker: Some(id.to_string()),
            inline_object_height: 0.0,
            float: None,
        };
        p.lines[0].runs.push(crate::boxes::VisualRun {
            glyphs: vec![glyph],
            font: "f".to_string(),
            direction: ShapingDirection::Ltr,
            source_range: 0..1,
            attrs: crate::boxes::TextAttrs {
                px_size: 12.0,
                color: [0, 0, 0, 255],
                faux_bold: false,
                faux_italic: false,
                underline: engine::UnderlineStyle::None,
                strike: false,
                bg_color: None,
                baseline_shift_px: 0.0,
            },
        });
        p
    }

    /// Every nominal paginator shape the crate exercises, laid out and
    /// finished. The fingerprints pinned in
    /// `nominal_fixtures_are_geometrically_identical_to_the_pre_watchdog_paginator`
    /// were recorded on the paginator BEFORE the #87 watchdog landed, so
    /// the test is a native stand-in for the browser goldens: the
    /// non-degraded path must be output-identical.
    fn nominal_fixtures() -> Vec<(&'static str, Vec<PageBox>, Vec<LayoutDegradation>)> {
        let geom = a4_geometry();
        let mut out: Vec<(&'static str, Vec<PageBox>, Vec<LayoutDegradation>)> = Vec::new();
        let mut finish = |name: &'static str, pag: Paginator| {
            let (pages, notes) = pag.finish_with_notes();
            out.push((name, pages, notes));
        };

        let mut pag = Paginator::with_default_bands(geom, None, None);
        pag.push_block(LayoutBlock::Paragraph(fake_paragraph(80, 16.0)), 0.0, 0.0);
        finish("split80", pag);

        let mut pag = Paginator::with_default_bands(geom, None, None);
        pag.push_block(LayoutBlock::Paragraph(fake_paragraph(3, 16.0)), 12.0, 6.0);
        pag.push_block(LayoutBlock::Paragraph(fake_paragraph(5, 14.0)), 12.0, 6.0);
        finish("short_with_spacing", pag);

        let mut pag = Paginator::with_default_bands(geom, None, None);
        pag.push_block(LayoutBlock::Paragraph(fake_paragraph(1, 9999.0)), 0.0, 0.0);
        pag.push_block(LayoutBlock::Paragraph(fake_paragraph(2, 16.0)), 0.0, 0.0);
        finish("oversize_line", pag);

        let mut pag = Paginator::with_default_bands(geom, None, None);
        pag.set_columns(2, 12.0);
        pag.push_block(LayoutBlock::Paragraph(fake_paragraph(120, 16.0)), 0.0, 0.0);
        pag.push_block(LayoutBlock::Paragraph(fake_paragraph(4, 16.0)), 0.0, 0.0);
        finish("snake_two_columns", pag);

        let mut pag = Paginator::with_default_bands(geom, None, None);
        pag.set_columns(3, 12.0);
        pag.push_block(LayoutBlock::Paragraph(fake_paragraph(1, 9999.0)), 0.0, 0.0);
        pag.push_block(LayoutBlock::Paragraph(fake_paragraph(2, 16.0)), 0.0, 0.0);
        finish("oversize_line_three_columns", pag);

        let mut pag = Paginator::with_default_bands(geom, None, None);
        pag.set_columns(2, 12.0);
        for h in [50.0, 30.0, 70.0, 50.0] {
            pag.push_block(LayoutBlock::Paragraph(fake_paragraph(1, h)), 0.0, 0.0);
        }
        pag.balance_current_section_columns();
        pag.set_columns(1, 0.0);
        pag.push_block(LayoutBlock::Paragraph(fake_paragraph(2, 16.0)), 0.0, 0.0);
        finish("continuous_balance", pag);

        let mut pag = Paginator::with_default_bands(geom, None, None);
        pag.push_block(
            LayoutBlock::Paragraph(fake_paragraph_with_page_break(6, 16.0, 2)),
            0.0,
            0.0,
        );
        pag.push_block(LayoutBlock::Paragraph(fake_paragraph(5, 16.0)), 0.0, 0.0);
        finish("form_feed", pag);

        let headers = HeaderBands {
            default: Some(fake_band_tall(9, 100.0)),
            first: Some(fake_band_tall(3, 20.0)),
            even: None,
        };
        let footers = HeaderBands {
            default: Some(fake_band_tall(8, 120.0)),
            first: None,
            even: None,
        };
        let mut pag = Paginator::new(geom, headers, footers, true, false);
        pag.push_block(LayoutBlock::Paragraph(fake_paragraph(100, 16.0)), 0.0, 0.0);
        finish("intruding_bands_title_pg", pag);

        let mut bodies = HashMap::new();
        bodies.insert(1, fake_paragraph(3, 14.0));
        bodies.insert(2, fake_paragraph(2, 14.0));
        let mut pag = Paginator::with_default_bands(geom, None, None).with_footnote_bodies(bodies);
        pag.push_block(
            LayoutBlock::Paragraph(fake_paragraph_with_footnote_ref(1, 3, 16.0)),
            0.0,
            0.0,
        );
        pag.push_block(LayoutBlock::Paragraph(fake_paragraph(40, 16.0)), 0.0, 0.0);
        pag.push_block(
            LayoutBlock::Paragraph(fake_paragraph_with_footnote_ref(2, 2, 16.0)),
            0.0,
            0.0,
        );
        pag.push_block(LayoutBlock::Paragraph(fake_paragraph(30, 16.0)), 0.0, 0.0);
        finish("footnotes", pag);

        let mut pag = Paginator::with_default_bands(geom, None, None);
        pag.push_block(LayoutBlock::Paragraph(fake_paragraph(2, 16.0)), 0.0, 0.0);
        pag.start_new_section(geom, HeaderBands::default(), HeaderBands::default(), false);
        pag.push_block(LayoutBlock::Paragraph(fake_paragraph(2, 16.0)), 0.0, 0.0);
        pag.force_page_break();
        pag.push_block(LayoutBlock::Paragraph(fake_paragraph(2, 16.0)), 0.0, 0.0);
        finish("sections_and_forced_breaks", pag);

        let mut pag = Paginator::with_default_bands(geom, None, None);
        pag.push_block(LayoutBlock::Paragraph(fake_paragraph(40, 16.0)), 0.0, 0.0);
        pag.push_block(
            LayoutBlock::Table(fake_table(&[(20.0, true), (20.0, false), (20.0, false)])),
            0.0,
            0.0,
        );
        pag.push_block(
            LayoutBlock::Table(fake_table(&[(100.0, true), (100.0, false), (900.0, false)])),
            0.0,
            0.0,
        );
        pag.push_block(LayoutBlock::Paragraph(fake_paragraph(2, 16.0)), 0.0, 0.0);
        finish("tables_move_whole_and_atomic", pag);

        out
    }

    /// Pinned on the pre-#87 paginator (recorded via `--nocapture` before
    /// the watchdog landed). A changed value means the nominal path is
    /// no longer output-identical — the browser goldens would move. The
    /// third column is the degradation the fixture is EXPECTED to report
    /// (the pre-existing clip paths, now visible), page-stamped.
    type PinnedFixture = (&'static str, u64, &'static [(DegradeReason, u32)]);
    const PINNED_FINGERPRINTS: &[PinnedFixture] = &[
        ("split80", 0xfdd9dbb6fafb7a42, &[]),
        ("short_with_spacing", 0xa4a5aeb949abca0c, &[]),
        (
            "oversize_line",
            0x5fcdbcf7ef8bf095,
            &[(DegradeReason::OversizeLine, 0)],
        ),
        ("snake_two_columns", 0x3ae2e4d684a08cb1, &[]),
        (
            "oversize_line_three_columns",
            0x4f9b91bec5c91598,
            &[(DegradeReason::OversizeLine, 0)],
        ),
        ("continuous_balance", 0x4223f896a4f3452c, &[]),
        ("form_feed", 0x9cc8e34c49eb9a4e, &[]),
        ("intruding_bands_title_pg", 0xd740800cab2c8c6d, &[]),
        ("footnotes", 0x7f99643a17eaa915, &[]),
        ("sections_and_forced_breaks", 0xc92c5638ce1f2440, &[]),
        (
            "tables_move_whole_and_atomic",
            0x7e5c0eabb84250c6,
            &[(DegradeReason::OversizeLine, 2)],
        ),
    ];

    #[test]
    fn nominal_fixtures_are_geometrically_identical_to_the_pre_watchdog_paginator() {
        for (name, pages, notes) in nominal_fixtures() {
            let fp = geometry_fingerprint(&pages);
            let got: Vec<(DegradeReason, u32)> = notes.iter().map(|n| (n.reason, n.page)).collect();
            match PINNED_FINGERPRINTS.iter().find(|(n, _, _)| *n == name) {
                Some((_, want, want_notes)) => {
                    assert_eq!(
                        fp, *want,
                        "fixture `{name}` changed geometry (got {fp:#x}, pinned {want:#x})"
                    );
                    assert_eq!(
                        got.as_slice(),
                        *want_notes,
                        "fixture `{name}` reported unexpected degradations"
                    );
                }
                None => eprintln!("FINGERPRINT {name} = {fp:#x} notes={got:?}"),
            }
        }
    }

    /// The strict switch: nominal shapes must not raise a single note.
    #[test]
    fn strict_watchdog_is_silent_on_nominal_flow() {
        let geom = a4_geometry();
        let mut pag = Paginator::with_default_bands(geom, None, None).with_strict_watchdog(true);
        pag.set_columns(2, 12.0);
        pag.push_block(LayoutBlock::Paragraph(fake_paragraph(120, 16.0)), 0.0, 0.0);
        pag.push_block(
            LayoutBlock::Table(fake_table(&[(20.0, true), (20.0, false), (20.0, false)])),
            6.0,
            6.0,
        );
        pag.push_block(LayoutBlock::Paragraph(fake_paragraph(200, 16.0)), 0.0, 0.0);
        let (pages, notes) = pag.finish_with_notes();
        assert!(pages.len() >= 2);
        assert!(notes.is_empty());
    }

    /* ================================================================
    Issue #87 — adversarial fixtures. Each must terminate under budget
    with a degraded-but-painted result, and say so in the notes.
    ================================================================ */

    /// The 250 ms acceptance budget is a release / perf-tier figure;
    /// debug builds get an order of magnitude of slack and still trip on
    /// anything that loops.
    fn adversarial_budget() -> Duration {
        if cfg!(debug_assertions) {
            Duration::from_millis(2500)
        } else {
            Duration::from_millis(250)
        }
    }

    fn reasons(notes: &[LayoutDegradation]) -> Vec<DegradeReason> {
        notes.iter().map(|n| n.reason).collect()
    }

    fn keep_para(n: usize, line_height: f32) -> ParagraphBox {
        let mut p = fake_paragraph(n, line_height);
        p.keep_next = true;
        p
    }

    fn total_blocks(pages: &[PageBox]) -> usize {
        pages.iter().map(|p| p.blocks.len()).sum()
    }

    /// Keep-with-next chain longer than a page: 60 one-line keep-next
    /// paragraphs (960 pt on a 698 pt budget) followed by a free
    /// paragraph. The chain can never satisfy its constraint; the
    /// paginator must release it (stage a) instead of bouncing the
    /// chain page after page, emit no blank page, and keep every block.
    #[test]
    fn keep_chain_longer_than_a_page_drops_the_constraint_and_terminates() {
        let t0 = Instant::now();
        let geom = a4_geometry();
        let mut pag = Paginator::with_default_bands(geom, None, None);
        for _ in 0..60 {
            pag.push_block(LayoutBlock::Paragraph(keep_para(1, 16.0)), 0.0, 0.0);
        }
        pag.push_block(LayoutBlock::Paragraph(fake_paragraph(2, 16.0)), 0.0, 0.0);
        let (pages, notes) = pag.finish_with_notes();
        assert!(
            t0.elapsed() < adversarial_budget(),
            "took {:?}",
            t0.elapsed()
        );
        assert_eq!(
            pages.len(),
            2,
            "43 lines fit a page; the rest flow to page 2"
        );
        assert!(pages.iter().all(|p| !p.blocks.is_empty()), "no blank page");
        assert_eq!(total_blocks(&pages), 61, "every block painted");
        assert_eq!(reasons(&notes), vec![DegradeReason::KeepChainDropped]);
        assert_eq!(notes[0].page, 0);
    }

    /// The satisfiable case: a chain that CAN move does, together with
    /// its follower, and nothing is reported.
    #[test]
    fn keep_chain_moves_with_its_follower_when_it_can() {
        let geom = a4_geometry();
        let mut pag = Paginator::with_default_bands(geom, None, None);
        /* 640 pt anchor, 3 × 16 pt keep-next chain (688 total), then a
        follower whose first line does not fit the remaining 10 pt. */
        pag.push_block(LayoutBlock::Paragraph(fake_paragraph(40, 16.0)), 0.0, 0.0);
        for _ in 0..3 {
            pag.push_block(LayoutBlock::Paragraph(keep_para(1, 16.0)), 0.0, 0.0);
        }
        pag.push_block(LayoutBlock::Paragraph(fake_paragraph(5, 16.0)), 0.0, 0.0);
        let (pages, notes) = pag.finish_with_notes();
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0].blocks.len(), 1, "the anchor stays");
        assert_eq!(pages[1].blocks.len(), 4, "chain + follower moved together");
        let ys: Vec<f32> = pages[1].blocks.iter().map(|b| b.origin().y).collect();
        assert_eq!(
            ys,
            vec![0.0, 16.0, 32.0, 48.0],
            "re-flowed from the page top"
        );
    }

    /// Oscillation guard: the chain moves once, fills the fresh page, and
    /// its follower still does not fit — a naive "move the chain again"
    /// would bounce forever. A chain already at a page top is released.
    #[test]
    fn keep_chain_that_fills_a_fresh_page_is_released_not_bounced() {
        let t0 = Instant::now();
        let geom = a4_geometry();
        let mut pag = Paginator::with_default_bands(geom, None, None);
        pag.push_block(LayoutBlock::Paragraph(fake_paragraph(1, 16.0)), 0.0, 0.0);
        for _ in 0..43 {
            pag.push_block(LayoutBlock::Paragraph(keep_para(1, 16.0)), 0.0, 0.0);
        }
        pag.push_block(LayoutBlock::Paragraph(fake_paragraph(5, 16.0)), 0.0, 0.0);
        let (pages, notes) = pag.finish_with_notes();
        assert!(t0.elapsed() < adversarial_budget());
        assert_eq!(pages.len(), 3, "anchor | 43-line chain | follower");
        assert_eq!(pages[0].blocks.len(), 1);
        assert_eq!(pages[1].blocks.len(), 43);
        assert_eq!(pages[2].blocks.len(), 1);
        assert_eq!(reasons(&notes), vec![DegradeReason::KeepChainDropped]);
        assert_eq!(notes[0].page, 1);
    }

    /// Keep-with-next in front of a table travels the same way.
    #[test]
    fn keep_chain_moves_with_a_following_table() {
        let geom = a4_geometry();
        let mut pag = Paginator::with_default_bands(geom, None, None);
        pag.push_block(LayoutBlock::Paragraph(fake_paragraph(40, 16.0)), 0.0, 0.0);
        pag.push_block(LayoutBlock::Paragraph(keep_para(2, 16.0)), 0.0, 0.0);
        pag.push_block(
            LayoutBlock::Table(fake_table(&[(40.0, false), (40.0, false)])),
            0.0,
            0.0,
        );
        let (pages, notes) = pag.finish_with_notes();
        assert!(notes.is_empty());
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0].blocks.len(), 1);
        assert_eq!(pages[1].blocks.len(), 2, "heading + table together");
    }

    /// Repeated header rows taller than the page (the #7 class: an
    /// autofit column narrower than its longest unbreakable token wraps
    /// the header row char-by-char past the page height). The row split
    /// would otherwise produce `headers + the same body rows` — the exact
    /// table it started from — on every page forever. Stage (a) suppresses
    /// the repeat for that continuation so the tail strictly shrinks.
    ///
    /// Exercises `push_table_split`'s row path directly: through
    /// `push_block` a table on a fresh page is placed atomically (its
    /// row-split branch is unreachable today — see the #87 report).
    #[test]
    fn header_row_taller_than_the_page_terminates_with_the_repeat_dropped() {
        let t0 = Instant::now();
        let geom = a4_geometry();
        let mut pag = Paginator::with_default_bands(geom, None, None);
        let table = fake_table(&[(800.0, true), (20.0, false), (20.0, false), (20.0, false)]);
        pag.push_table_split(table, 0.0, true);
        let (pages, notes) = pag.finish_with_notes();
        assert!(t0.elapsed() < adversarial_budget());
        assert_eq!(pages.len(), 2, "header page, then the body rows");
        let rows_per_page: Vec<usize> = pages
            .iter()
            .map(|p| {
                p.blocks
                    .iter()
                    .filter_map(LayoutBlock::as_table)
                    .map(|t| t.rows.len())
                    .sum()
            })
            .collect();
        assert_eq!(rows_per_page, vec![1, 3]);
        assert_eq!(reasons(&notes), vec![DegradeReason::HeaderRepeatDropped]);
    }

    /// Header + body rows that DO fit keep repeating the header — the
    /// constraint is only released when honouring it cannot progress.
    #[test]
    fn header_repeat_is_kept_when_a_body_row_fits_under_it() {
        let geom = a4_geometry();
        let mut pag = Paginator::with_default_bands(geom, None, None);
        let table = fake_table(&[
            (20.0, true),
            (300.0, false),
            (300.0, false),
            (300.0, false),
            (300.0, false),
        ]);
        pag.push_table_split(table, 0.0, true);
        let (pages, notes) = pag.finish_with_notes();
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(pages.len(), 2);
        for p in &pages {
            let t = p.blocks[0].as_table().expect("table");
            assert!(t.rows[0].header, "every continuation opens with the header");
            assert_eq!(t.rows.len(), 3);
        }
    }

    /// Footnote taller than a page: its band leaves no body budget on any
    /// page. The old flow bounced the paragraph forward and lost the
    /// footnote; now the block is placed over the band on the fresh page,
    /// the footnote paints, and the flow continues.
    #[test]
    fn footnote_taller_than_a_page_terminates_and_paints() {
        let t0 = Instant::now();
        let geom = a4_geometry();
        let mut bodies = HashMap::new();
        bodies.insert(1, fake_paragraph(1, 2000.0));
        let mut pag = Paginator::with_default_bands(geom, None, None).with_footnote_bodies(bodies);
        pag.push_block(
            LayoutBlock::Paragraph(fake_paragraph_with_footnote_ref(1, 3, 16.0)),
            0.0,
            0.0,
        );
        pag.push_block(LayoutBlock::Paragraph(fake_paragraph(40, 16.0)), 0.0, 0.0);
        let (pages, notes) = pag.finish_with_notes();
        assert!(t0.elapsed() < adversarial_budget());
        assert_eq!(pages.len(), 2);
        assert_eq!(
            pages[0].blocks.len(),
            1,
            "the referencing paragraph painted"
        );
        assert_eq!(
            pages[0].footnotes.len(),
            1,
            "its footnote painted (clipped)"
        );
        assert_eq!(pages[0].footnotes[0].id, 1);
        assert_eq!(pages[1].blocks.len(), 1, "the flow continued");
        assert_eq!(reasons(&notes), vec![DegradeReason::FootnoteOverflow]);
    }

    /// Stage (c): the page cap. Past it the flow is appended to the
    /// current page without breaks and the layout is accepted as final.
    #[test]
    fn page_cap_force_validates_the_remaining_flow() {
        let t0 = Instant::now();
        let geom = a4_geometry();
        let mut pag = Paginator::with_default_bands(geom, None, None).with_page_cap(3);
        for _ in 0..10 {
            pag.push_block(LayoutBlock::Paragraph(fake_paragraph(43, 16.0)), 0.0, 0.0);
        }
        let (pages, notes) = pag.finish_with_notes();
        assert!(t0.elapsed() < adversarial_budget());
        assert_eq!(pages.len(), 4, "3 capped pages + the in-progress page");
        assert_eq!(total_blocks(&pages), 10, "nothing dropped");
        assert_eq!(
            pages[3].blocks.len(),
            7,
            "the remainder piled onto the last page"
        );
        assert_eq!(reasons(&notes), vec![DegradeReason::PageCap]);
        assert_eq!(pag_stage_after_cap(), DegradeStage::ForceValidate);
    }

    fn pag_stage_after_cap() -> DegradeStage {
        let geom = a4_geometry();
        let mut pag = Paginator::with_default_bands(geom, None, None).with_page_cap(1);
        pag.push_block(LayoutBlock::Paragraph(fake_paragraph(43, 16.0)), 0.0, 0.0);
        pag.push_block(LayoutBlock::Paragraph(fake_paragraph(43, 16.0)), 0.0, 0.0);
        pag.watchdog_stage()
    }

    /// Stage (b): a block the ladder has frozen is pinned atomically at
    /// the cursor, clipping, instead of being split or moved.
    #[test]
    fn frozen_block_is_pinned_at_the_cursor() {
        let geom = a4_geometry();
        let mut pag = Paginator::with_default_bands(geom, None, None);
        pag.push_block(LayoutBlock::Paragraph(fake_paragraph(2, 16.0)), 0.0, 0.0);
        pag.watchdog_mut().escalate_to(DegradeStage::Freeze);
        pag.push_block_inner(
            LayoutBlock::Paragraph(fake_paragraph(100, 16.0)),
            0.0,
            0.0,
            true,
        );
        let (pages, notes) = pag.finish_with_notes();
        assert_eq!(pages.len(), 1, "no split, no page move");
        assert_eq!(pages[0].blocks.len(), 2);
        assert_eq!(pages[0].blocks[1].origin().y, 32.0);
        assert_eq!(reasons(&notes), vec![DegradeReason::FrozenPlacement]);
    }

    /// The generic ladder end to end on the paginator: identical content
    /// re-entering on fresh pages escalates a → b; a strict watchdog turns
    /// the recovery into a hard failure so CI catches a new loop.
    #[test]
    #[should_panic(expected = "layout watchdog (strict): FROZEN_PLACEMENT")]
    fn strict_watchdog_fails_hard_on_churn() {
        let geom = a4_geometry();
        let mut pag = Paginator::with_default_bands(geom, None, None).with_strict_watchdog(true);
        let block = || LayoutBlock::Paragraph(fake_paragraph(100, 16.0));
        pag.watchdog_mut().begin_block();
        /* Three fresh-page attempts of the same content: nominal, stage
        (a), then stage (b) → the strict note panics. */
        for _ in 0..3 {
            pag.force_page_break();
            let fp = BlockFingerprint::of(&block(), true);
            let stage = pag.watchdog_mut().observe(fp);
            if stage >= DegradeStage::Freeze {
                pag.watchdog_mut().note(DegradeReason::FrozenPlacement, 0);
            }
        }
    }
}
