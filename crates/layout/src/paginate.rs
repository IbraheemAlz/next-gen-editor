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
    FootnoteEntry, HeaderFooterBox, LayoutBlock, LineBox, NoteBand, PageBox, ParagraphBox, Point,
    Size, TableBox, TableRowBox,
};
use crate::page::Margins;
use crate::watchdog::{BlockFingerprint, DegradeReason, DegradeStage, LayoutDegradation, Watchdog};
use engine::{NoteAnchor, NotePosition};
use std::collections::{HashMap, VecDeque};

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

/* ============================================================
Issue #80 — footnote / endnote space negotiation.

The protocol (clean-room design notes: PRD_LIBREOFFICE §4C, the
"deadline" rule; PRD_ONLYOFFICE §4.7, per-line reservation):

- Reservation is PER LINE (per row for tables), not per block. When a
  line carrying a note reference is placed, its note reserves band
  space at the page bottom, shrinking the body budget for every later
  line. The reference line must itself fit above its own note — the
  deadline — so a note can never push its reference off the page.
- A note that does not fit whole under its reference line SPLITS at a
  line / row boundary: at least one line of it stays with the
  reference, the remainder carries over and opens the next page's band
  as a continuation (the continuation-notice story, when the document
  ships one, closes the cut). A continuation may take at most
  `1 - NOTE_BODY_RESERVE_FRACTION` of a fresh page so every page keeps
  some body text (Word never emits a notes-only page) and a body block
  always finds room to make measurable progress.
- References travel with the fragment that holds their line (issue
  #92): the fitter decides the split index WITH the notes counted, so
  there is no commit-then-roll-back — a head keeps the notes of its own
  lines, a tail re-collects its own when it is re-pushed.
- Endnotes are a trailing story: [`Paginator::push_trailing_notes`]
  stacks them beneath the body at section / document end, splitting
  across pages the same way.

Termination (issue #87): no negotiation loop re-pushes a block. The only
loop is the carry-over drain — each page consumes at least one line of
the carried note (the waiver clips a line that is taller than the
allowance and reports `FootnoteOverflow`) — plus the page cap, which
dumps the carry whole. Every escape hatch paints the note, clipped.
============================================================ */

/// Issue #80 — the laid-out blocks of one note story, stacked from
/// `y = 0` at the band's content width. The engine lays every referenced
/// story out once per paint; the paginator only splits and places.
pub type NoteBody = Vec<LayoutBlock>;

/// Issue #80 — share of a fresh page's body budget a note CONTINUATION
/// leaves to the body (see the module protocol note).
pub const NOTE_BODY_RESERVE_FRACTION: f32 = 0.2;

/// Issue #80 — one note committed to the page being filled.
#[derive(Debug, Clone)]
struct PendingNote {
    anchor: NoteAnchor,
    marker: String,
    /// Blocks placed on this page, stacked from `y = 0`; ends with the
    /// continuation-notice story when the note continues.
    blocks: Vec<LayoutBlock>,
    height: f32,
    /// Story-block index of `blocks[0]` (see
    /// [`FootnoteEntry::first_block_index`]).
    first_block_index: u32,
    continued_from_previous: bool,
    continues_on_next: bool,
}

/// Issue #80 — the remainder of a split note, waiting for the next
/// page's band.
#[derive(Debug, Clone)]
struct NoteCarry {
    anchor: NoteAnchor,
    marker: String,
    blocks: Vec<LayoutBlock>,
    /// Story-block index of `blocks[0]`.
    first_block_index: u32,
    /// The blocks continue a note whose head sat on an earlier page.
    continued: bool,
}

/// Issue #80 — one flow item the fitter walks (a body line or a table
/// row) with the note references anchored on it.
struct FlowItem {
    /// Bottom edge of the item, block-relative.
    bottom: f32,
    anchors: Vec<(NoteAnchor, String)>,
}

/// Issue #80 — the fitter's verdict for one block's items.
struct FitPlan {
    /// Items `[0, count)` fit, with their notes reserved.
    count: usize,
    notes: Vec<PendingNote>,
    carry: Vec<NoteCarry>,
    /// The first-item waiver clipped a note (reported as
    /// `FootnoteOverflow` when the plan commits).
    clipped: bool,
}

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
    /// Issue #80 — pre-laid-out note bodies keyed by `(kind, w:id)`.
    /// The engine builds these once per paint with the same block
    /// layout pipeline the body uses; the paginator only splits and
    /// places.
    note_bodies: HashMap<NoteAnchor, NoteBody>,
    /// Issue #80 — the laid-out `continuationNotice` story, appended
    /// to a note's head when it is cut. `None` ⇒ no notice.
    continuation_notice: Option<NoteBody>,
    /// Issue #80 — `<w:footnotePr><w:pos>`: page bottom (default) or
    /// beneath the body's last line.
    footnote_position: NotePosition,
    /// Issue #80 — footnotes committed to the current page, in band
    /// order (a carried continuation first, then reference order).
    cur_notes: Vec<PendingNote>,
    /// Issue #80 — total height the current page's footnote band
    /// consumes, separator gap included. Subtracted from the body
    /// budget so the body never overruns the band.
    cur_footnote_height: f32,
    /// Issue #80 — note remainders waiting to open the next page's
    /// footnote band. Applied in `flush_page`, drained by `finish`.
    footnote_carry: Vec<NoteCarry>,
    /// Issue #80 — endnotes committed to the current page (trailing
    /// band beneath the body) and the content-relative Y its first
    /// entry opens at.
    cur_endnotes: Vec<PendingNote>,
    cur_endnote_band_y: f32,
    cur_endnote_band_continuation: bool,
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
            note_bodies: HashMap::new(),
            continuation_notice: None,
            footnote_position: NotePosition::PageBottom,
            cur_notes: Vec::new(),
            cur_footnote_height: 0.0,
            footnote_carry: Vec::new(),
            cur_endnotes: Vec::new(),
            cur_endnote_band_y: 0.0,
            cur_endnote_band_continuation: false,
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

    /// Issue #80 — install the per-document note body table. The
    /// paginator looks each reference anchor up here when it places
    /// the line carrying it and grows the footnote band before
    /// deciding whether the next line still fits.
    pub fn with_note_bodies(mut self, bodies: HashMap<NoteAnchor, NoteBody>) -> Self {
        self.note_bodies = bodies;
        self
    }

    /// Issue #80 — install the laid-out `continuationNotice` story
    /// (appended to a note's head on the page where it is cut).
    pub fn with_continuation_notice(mut self, notice: Option<NoteBody>) -> Self {
        self.continuation_notice = notice.filter(|n| !n.is_empty());
        self
    }

    /// Issue #80 — `<w:footnotePr><w:pos>` for the active section.
    /// `SectEnd` / `DocEnd` are meaningless for footnotes and behave as
    /// `BeneathText` (Word's observed reading).
    pub fn set_footnote_position(&mut self, position: NotePosition) {
        self.footnote_position = match position {
            NotePosition::PageBottom => NotePosition::PageBottom,
            _ => NotePosition::BeneathText,
        };
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

        let remaining = self.body_budget();
        let block_height = block.size().height;

        /* Paragraphs always run through the line-fitter: issue #80
        reserves note-band space PER LINE (the reference line must fit
        above its own note), so even a paragraph that fits by height
        alone may split once its notes are counted. A paragraph that
        does not fit turns into N pages, not one overflowing bag of
        content. Tables stay atomic on an empty page: the line-splitter
        doesn't apply, and a table taller than a full page is a rare
        authoring decision the user took deliberately. */
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
        match block {
            LayoutBlock::Paragraph(p) => {
                /* The nominal note-free fast path is byte-identical to
                the pre-#80 "fits whole" step; the fitter takes over as
                soon as a line carries a reference. */
                if !has_forced_break && block_height <= remaining && !paragraph_has_note_anchors(&p)
                {
                    self.place_atomic(LayoutBlock::Paragraph(p), after);
                    return;
                }
                self.push_paragraph_split(p, remaining, after, observe)
            }
            LayoutBlock::Table(t) => {
                /* Issue #80 — the table's notes are reserved as a block:
                a table that fits by height but not with its notes moves
                like one that does not fit. */
                let anchors = collect_note_anchors(&LayoutBlock::Table(t.clone()));
                let plan = self.fit_items(
                    &[FlowItem {
                        bottom: block_height,
                        anchors,
                    }],
                    remaining,
                    atomic_overflow_ok,
                );
                if plan.count == 1 || atomic_overflow_ok {
                    /* Issue #87 — an atomic table taller than the budget
                    clips past the bottom margin. Same placement as
                    before; now the paint says so. */
                    if block_height > remaining {
                        let page = self.cur_page_index();
                        self.watchdog.note(DegradeReason::OversizeLine, page);
                    }
                    self.commit_plan(plan);
                    /* Audit gap A.H2 — origin.x carries the column offset
                    for multi-column sections (zero for single-column,
                    preserving the legacy "page-wide" behaviour). */
                    self.place_atomic(LayoutBlock::Table(t), after);
                    return;
                }
                self.push_table_split(t, after, observe);
            }
        }
    }

    /// Issue #80 — the vertical room left for body content in the
    /// current column: the content height minus the cursor, the footnote
    /// band and the footer's intrusion.
    fn body_budget(&self) -> f32 {
        self.geometry.content_height()
            - self.cur_y
            - self.cur_footnote_height
            - self.footer_intrusion(self.page_role())
    }

    /// Issue #80 — the deadline fitter. Walks `items` (lines or rows) in
    /// order and decides how many fit in `remaining` once the notes
    /// anchored on them are reserved at the page bottom. Notes already on
    /// the page (or earlier in the plan) reserve nothing. The first item
    /// whose notes do not fit whole splits the offending note — at least
    /// one line stays under the reference — and closes the band; if not
    /// even one line fits, the item moves forward, unless it is the first
    /// item of a fresh page (`fresh`), where the rule is waived and the
    /// first line is clipped (`FootnoteOverflow`) so the flow terminates.
    fn fit_items(&self, items: &[FlowItem], remaining: f32, fresh: bool) -> FitPlan {
        let mut plan = FitPlan {
            count: 0,
            notes: Vec::new(),
            carry: Vec::new(),
            clipped: false,
        };
        let mut reserve = 0.0_f32;
        let mut band_open = !self.cur_notes.is_empty();
        let notice_h = self
            .continuation_notice
            .as_deref()
            .map_or(0.0, blocks_height);
        for (i, item) in items.iter().enumerate() {
            let mut bodies: Vec<(NoteAnchor, String, Vec<LayoutBlock>)> = Vec::new();
            for (anchor, marker) in &item.anchors {
                let seen = self.cur_notes.iter().any(|n| n.anchor == *anchor)
                    || plan.notes.iter().any(|n| n.anchor == *anchor)
                    || bodies.iter().any(|(a, _, _)| a == anchor);
                if seen {
                    continue;
                }
                /* A dangling id (no story) reserves nothing — Word
                tolerates those and simply paints no note. */
                let Some(body) = self.note_bodies.get(anchor) else {
                    continue;
                };
                if body.is_empty() {
                    continue;
                }
                bodies.push((*anchor, marker.clone(), body.clone()));
            }
            let sep = if !band_open && !bodies.is_empty() {
                FOOTNOTE_SEPARATOR_HEIGHT_PT
            } else {
                0.0
            };
            let need: f32 = sep + bodies.iter().map(|(_, _, b)| blocks_height(b)).sum::<f32>();
            if item.bottom + reserve + need <= remaining {
                reserve += need;
                if !bodies.is_empty() {
                    band_open = true;
                }
                for (anchor, marker, blocks) in bodies {
                    let height = blocks_height(&blocks);
                    plan.notes.push(PendingNote {
                        anchor,
                        marker,
                        blocks,
                        height,
                        first_block_index: 0,
                        continued_from_previous: false,
                        continues_on_next: false,
                    });
                }
                plan.count = i + 1;
                continue;
            }
            /* The item does not fit with its notes whole. A plain item
            splits the block here; an item with notes may still land if
            the offending note splits. */
            if bodies.is_empty() {
                break;
            }
            let waiver = fresh && i == 0 && plan.notes.is_empty();
            let item_fits = item.bottom + reserve <= remaining;
            if !item_fits && !waiver {
                break;
            }
            let avail = ((remaining - reserve - item.bottom).max(0.0) - sep).max(0.0);
            let mut used = 0.0_f32;
            let mut placed: Vec<PendingNote> = Vec::new();
            let mut carry: Vec<NoteCarry> = Vec::new();
            let mut split_done = false;
            let mut stalled = false;
            for (anchor, marker, blocks) in bodies {
                if split_done {
                    /* A whole note carried behind a split one is not a
                    continuation: it opens fresh on the next page. */
                    carry.push(NoteCarry {
                        anchor,
                        marker,
                        blocks,
                        first_block_index: 0,
                        continued: false,
                    });
                    continue;
                }
                let h = blocks_height(&blocks);
                if used + h <= avail {
                    placed.push(PendingNote {
                        anchor,
                        marker,
                        blocks,
                        height: h,
                        first_block_index: 0,
                        continued_from_previous: false,
                        continues_on_next: false,
                    });
                    used += h;
                    continue;
                }
                let budget = avail - used - notice_h;
                let (mut head, mut tail, mut tail_first) = if budget > 0.0 {
                    split_note_blocks(&blocks, budget)
                } else {
                    (Vec::new(), blocks.clone(), 0)
                };
                if head.is_empty() {
                    if waiver && placed.is_empty() {
                        /* First line of a fresh page: nothing later could
                        ever host it either. Clip the note's first slice
                        under the line and say so. */
                        let (h0, t0, f0) = first_note_slice(&blocks);
                        head = h0;
                        tail = t0;
                        tail_first = f0;
                        plan.clipped = true;
                    } else {
                        stalled = true;
                        break;
                    }
                }
                let continues = !tail.is_empty();
                if continues {
                    if let Some(notice) = self.continuation_notice.as_deref() {
                        append_stacked(&mut head, notice);
                    }
                    carry.push(NoteCarry {
                        anchor,
                        marker: marker.clone(),
                        blocks: tail,
                        first_block_index: tail_first,
                        continued: true,
                    });
                }
                let hh = blocks_height(&head);
                placed.push(PendingNote {
                    anchor,
                    marker,
                    blocks: head,
                    height: hh,
                    first_block_index: 0,
                    continued_from_previous: false,
                    continues_on_next: continues,
                });
                used += hh;
                split_done = true;
            }
            if stalled {
                /* The item moves forward with its notes. */
                break;
            }
            plan.notes.extend(placed);
            plan.carry = carry;
            plan.count = i + 1;
            /* The band is full (or the waiver fired): nothing below this
            item can fit on the page. */
            break;
        }
        plan
    }

    /// Issue #80 — commit a fitter plan to the page: the notes join the
    /// band (separator gap when it opens), the carry waits for the next
    /// page, and a clip is reported.
    fn commit_plan(&mut self, plan: FitPlan) {
        if plan.notes.is_empty() && plan.carry.is_empty() && !plan.clipped {
            return;
        }
        if self.cur_notes.is_empty() && !plan.notes.is_empty() {
            self.cur_footnote_height += FOOTNOTE_SEPARATOR_HEIGHT_PT;
        }
        for n in plan.notes {
            self.cur_footnote_height += n.height;
            self.cur_notes.push(n);
        }
        self.footnote_carry.extend(plan.carry);
        if plan.clipped {
            let page = self.cur_page_index();
            self.watchdog.note(DegradeReason::FootnoteOverflow, page);
        }
    }

    /// Issue #80 / #87 — un-commit the notes a relocated keep-chain
    /// carried: the anchors leave this page's band (they re-commit on
    /// the page the chain lands on, from the chain's own glyphs) and
    /// any carry they produced is dropped — the full body re-collects.
    fn uncommit_notes(&mut self, anchors: &[NoteAnchor]) {
        if anchors.is_empty() {
            return;
        }
        self.cur_notes.retain(|n| !anchors.contains(&n.anchor));
        self.footnote_carry.retain(|c| !anchors.contains(&c.anchor));
        self.recompute_footnote_height();
    }

    fn recompute_footnote_height(&mut self) {
        self.cur_footnote_height = if self.cur_notes.is_empty() {
            0.0
        } else {
            FOOTNOTE_SEPARATOR_HEIGHT_PT + self.cur_notes.iter().map(|n| n.height).sum::<f32>()
        };
    }

    /// Issue #80 — open a fresh page's footnote band with the carried
    /// continuation(s). The continuation may take at most
    /// `1 - NOTE_BODY_RESERVE_FRACTION` of the body budget; what does not
    /// fit is split again and carried on. Progress is guaranteed: a
    /// slice that cannot be split further is placed whole and clipped
    /// (`FootnoteOverflow`). Past the page cap the carry lands whole.
    fn apply_footnote_carry(&mut self) {
        if self.footnote_carry.is_empty() {
            return;
        }
        let carry = std::mem::take(&mut self.footnote_carry);
        let budget =
            (self.geometry.content_height() - self.cur_y - self.footer_intrusion(self.page_role()))
                .max(0.0);
        let allowance = if self.capped {
            f32::INFINITY
        } else {
            (budget * (1.0 - NOTE_BODY_RESERVE_FRACTION) - FOOTNOTE_SEPARATOR_HEIGHT_PT).max(0.0)
        };
        let notice_h = self
            .continuation_notice
            .as_deref()
            .map_or(0.0, blocks_height);
        let mut used = 0.0_f32;
        let mut rest: Vec<NoteCarry> = Vec::new();
        let mut split_done = false;
        for c in carry {
            if split_done {
                rest.push(c);
                continue;
            }
            let h = blocks_height(&c.blocks);
            if used + h <= allowance {
                self.cur_notes.push(PendingNote {
                    anchor: c.anchor,
                    marker: c.marker,
                    blocks: c.blocks,
                    height: h,
                    first_block_index: c.first_block_index,
                    continued_from_previous: c.continued,
                    continues_on_next: false,
                });
                used += h;
                continue;
            }
            let split_budget = allowance - used - notice_h;
            let (mut head, mut tail, mut tail_first) = if split_budget > 0.0 {
                split_note_blocks(&c.blocks, split_budget)
            } else {
                (Vec::new(), c.blocks.clone(), 0)
            };
            if head.is_empty() {
                let (h0, t0, f0) = first_note_slice(&c.blocks);
                head = h0;
                tail = t0;
                tail_first = f0;
                let page = self.cur_page_index();
                self.watchdog.note(DegradeReason::FootnoteOverflow, page);
            }
            let continues = !tail.is_empty();
            if continues {
                if let Some(notice) = self.continuation_notice.as_deref() {
                    append_stacked(&mut head, notice);
                }
                rest.push(NoteCarry {
                    anchor: c.anchor,
                    marker: c.marker.clone(),
                    blocks: tail,
                    first_block_index: c.first_block_index + tail_first,
                    continued: true,
                });
            }
            let hh = blocks_height(&head);
            self.cur_notes.push(PendingNote {
                anchor: c.anchor,
                marker: c.marker,
                blocks: head,
                height: hh,
                first_block_index: c.first_block_index,
                continued_from_previous: c.continued,
                continues_on_next: continues,
            });
            used += hh;
            split_done = true;
        }
        self.footnote_carry = rest;
        self.recompute_footnote_height();
    }

    /// Issue #80 — append note stories as a TRAILING band beneath the
    /// body (endnotes at section / document end). Entries stack in the
    /// given order below the current cursor with the separator gap; a
    /// note that does not fit splits at a line boundary and continues
    /// at the top of the next page (the continuation separator paints
    /// full-width). Multi-column sections drop below their deepest
    /// column first — the band spans the content width.
    pub fn push_trailing_notes(&mut self, notes: Vec<(NoteAnchor, String, NoteBody)>) {
        if notes.is_empty() {
            return;
        }
        if self.column_count > 1 {
            let deepest = self.cur_blocks[self.cur_section_start_idx.min(self.cur_blocks.len())..]
                .iter()
                .map(|b| b.origin().y + b.size().height)
                .fold(0.0_f32, f32::max);
            self.cur_y = self.cur_y.max(deepest);
            self.cur_column_index = 0;
        }
        let notice_h = self
            .continuation_notice
            .as_deref()
            .map_or(0.0, blocks_height);
        let mut queue: VecDeque<NoteCarry> = notes
            .into_iter()
            .filter(|(_, _, b)| !b.is_empty())
            .map(|(anchor, marker, blocks)| NoteCarry {
                anchor,
                marker,
                blocks,
                first_block_index: 0,
                continued: false,
            })
            .collect();
        while let Some(c) = queue.pop_front() {
            let band_open = !self.cur_endnotes.is_empty();
            let sep = if band_open {
                0.0
            } else {
                FOOTNOTE_SEPARATOR_HEIGHT_PT
            };
            let remaining = self.body_budget() - sep;
            let h = blocks_height(&c.blocks);
            if h <= remaining || self.capped {
                self.place_endnote(c, h, sep, false);
                continue;
            }
            let split_budget = remaining - notice_h;
            let (mut head, mut tail, mut tail_first) = if split_budget > 0.0 {
                split_note_blocks(&c.blocks, split_budget)
            } else {
                (Vec::new(), c.blocks.clone(), 0)
            };
            if head.is_empty() {
                let fresh = self.cur_blocks.is_empty() && !band_open && self.cur_notes.is_empty();
                if !fresh {
                    /* Nothing of it fits here: close the page and retry on
                    a fresh one (the fresh page always makes progress). */
                    queue.push_front(c);
                    self.flush_page();
                    continue;
                }
                let (h0, t0, f0) = first_note_slice(&c.blocks);
                head = h0;
                tail = t0;
                tail_first = f0;
                let page = self.cur_page_index();
                self.watchdog.note(DegradeReason::FootnoteOverflow, page);
            }
            let continues = !tail.is_empty();
            if continues {
                if let Some(notice) = self.continuation_notice.as_deref() {
                    append_stacked(&mut head, notice);
                }
                queue.push_front(NoteCarry {
                    anchor: c.anchor,
                    marker: c.marker.clone(),
                    blocks: tail,
                    first_block_index: c.first_block_index + tail_first,
                    continued: true,
                });
            }
            let hh = blocks_height(&head);
            let cut = NoteCarry {
                anchor: c.anchor,
                marker: c.marker,
                blocks: head,
                first_block_index: c.first_block_index,
                continued: c.continued,
            };
            self.place_endnote(cut, hh, sep, continues);
            if continues {
                self.flush_page();
            }
        }
    }

    /// Issue #80 — stack one endnote entry (or slice) onto the current
    /// page's trailing band and advance the cursor past it.
    fn place_endnote(&mut self, c: NoteCarry, height: f32, sep: f32, continues: bool) {
        if self.cur_endnotes.is_empty() {
            self.cur_endnote_band_y = self.cur_y + sep;
            self.cur_endnote_band_continuation = c.continued;
            self.cur_y = self.cur_endnote_band_y;
        }
        self.cur_endnotes.push(PendingNote {
            anchor: c.anchor,
            marker: c.marker,
            blocks: c.blocks,
            height,
            first_block_index: c.first_block_index,
            continued_from_previous: c.continued,
            continues_on_next: continues,
        });
        self.cur_y += height;
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
        let anchors: Vec<NoteAnchor> = chain
            .iter()
            .flat_map(collect_note_anchors)
            .map(|(a, _)| a)
            .collect();
        self.uncommit_notes(&anchors);
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

    fn push_paragraph_split(
        &mut self,
        para: ParagraphBox,
        remaining: f32,
        after: f32,
        observe: bool,
    ) {
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

        /* Issue #80 — the deadline fitter decides the split index WITH
        every line's notes reserved (a plain paragraph reproduces
        `split_paragraph_at_line`'s cut exactly). Notes of the head's
        lines commit here; the tail's re-collect when it lands. */
        let items: Vec<FlowItem> = para
            .lines
            .iter()
            .map(|l| FlowItem {
                bottom: l.origin.y + l.height,
                anchors: anchors_on_line(l),
            })
            .collect();
        let plan = self.fit_items(&items, remaining, self.cur_blocks.is_empty());
        let (head, tail) = if para.lines.is_empty() {
            (Some(para.clone()), None)
        } else {
            split_paragraph_at_line_index(&para, plan.count)
        };
        if head.is_some() {
            self.commit_plan(plan);
        }

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
        let budget = self.body_budget();
        /* Issue #80 — rows are the fitter's items here: a row's notes
        reserve band space when the row lands, and the first row is
        forced (clipping) so an oversize row cannot loop. */
        let mut bottom = 0.0_f32;
        let items: Vec<FlowItem> = table
            .rows
            .iter()
            .map(|r| {
                bottom += r.size.height;
                FlowItem {
                    bottom,
                    anchors: anchors_in_row(r),
                }
            })
            .collect();
        let plan = self.fit_items(&items, budget, true);
        let head_count = plan.count.max(1).min(table.rows.len());
        self.commit_plan(plan);
        for (idx, row) in table.rows.iter().enumerate() {
            let row_h = row.size.height;
            if idx < head_count {
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
        /* Pick the role *before* pushing the page — `page_role` reads
        `pages.len()` to derive the 1-based page number, and the
        increment happens at `push`. Clone the resolved band slot;
        every other slot stays on the paginator for the next page. */
        let role = self.page_role();
        let mut header = self.headers.resolve(role).cloned();
        let mut footer = self.footers.resolve(role).cloned();

        /* Issue #80 — materialize the page's note bands. The reserved
        heights were already subtracted from the body budget as lines
        were placed, so the bands fit without overflow. Footnote entries
        are in band order (a carried continuation first, then the order
        their references were placed), each at its own Y inside the
        band; the band's page-relative Y is THE placement paint, PDF and
        hit-testing share. */
        let band_content_h: f32 = self.cur_notes.iter().map(|n| n.height).sum();
        let body_bottom = blocks
            .iter()
            .map(|b| b.origin().y + b.size().height)
            .fold(0.0_f32, f32::max);
        let footnote_band_y = if self.cur_notes.is_empty() {
            0.0
        } else {
            match self.footnote_position {
                NotePosition::PageBottom => {
                    self.geometry.height
                        - self.geometry.margins.bottom
                        - self.footer_intrusion(role)
                        - band_content_h
                }
                _ => self.geometry.margins.top + body_bottom + FOOTNOTE_SEPARATOR_HEIGHT_PT,
            }
        };
        let footnote_band_continuation = self
            .cur_notes
            .first()
            .is_some_and(|n| n.continued_from_previous);
        let mut footnotes = NoteBand {
            entries: materialize_band(std::mem::take(&mut self.cur_notes)),
            y: footnote_band_y,
            continuation: footnote_band_continuation,
        };
        self.cur_footnote_height = 0.0;
        let mut endnotes = NoteBand {
            y: if self.cur_endnotes.is_empty() {
                0.0
            } else {
                self.geometry.margins.top + self.cur_endnote_band_y
            },
            continuation: self.cur_endnote_band_continuation,
            entries: materialize_band(std::mem::take(&mut self.cur_endnotes)),
        };
        self.cur_endnote_band_y = 0.0;
        self.cur_endnote_band_continuation = false;

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
        for band in [&mut footnotes, &mut endnotes] {
            band.for_each_paragraph_mut(&mut |p| {
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
        self.pages.push(PageBox {
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
            endnotes,
            /* Issues #70/#74/#43 — which band slot this page resolved
            and the formatted number it displays. Enter-header/footer
            derives the double-clicked page's role from `hf_role`; the
            field-resolution pass reads `page_number`. */
            hf_role: role,
            page_number,
        });

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
        /* Issue #80 — a note cut on the page just closed opens this
        page's band (after the cap check, so a capped flow dumps the
        carry whole). */
        self.apply_footnote_carry();
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
        if !self.cur_blocks.is_empty()
            || self.pages.is_empty()
            || !self.cur_notes.is_empty()
            || !self.cur_endnotes.is_empty()
        {
            self.flush_page();
        }
        /* Issue #80 — drain a continuation that outlived the body: each
        flush applies the carry to a fresh band and consumes at least
        one line of it (or dumps it whole past the page cap), so the
        loop is bounded by the note's own height. */
        while !self.cur_notes.is_empty() || !self.footnote_carry.is_empty() {
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
            page.footnotes.for_each_paragraph_mut(&mut stamp);
            page.endnotes.for_each_paragraph_mut(&mut stamp);
        }
        self.pages
    }
}

/// Issue #80 — turn the page's committed notes into band entries stacked
/// from the band's top.
fn materialize_band(notes: Vec<PendingNote>) -> Vec<FootnoteEntry> {
    let mut out = Vec::with_capacity(notes.len());
    let mut y = 0.0_f32;
    for n in notes {
        out.push(FootnoteEntry {
            id: n.anchor.id,
            kind: n.anchor.kind,
            marker: n.marker,
            origin: Point { x: 0.0, y },
            blocks: n.blocks,
            first_block_index: n.first_block_index,
            continued_from_previous: n.continued_from_previous,
            continues_on_next: n.continues_on_next,
        });
        y += n.height;
    }
    out
}

/// Deepest bottom edge of a stacked block list.
fn blocks_height(blocks: &[LayoutBlock]) -> f32 {
    blocks
        .iter()
        .map(|b| b.origin().y + b.size().height)
        .fold(0.0_f32, f32::max)
}

/// Re-stack `blocks` from `y = 0` at `x = 0`; returns the total height.
fn restack_blocks(blocks: &mut [LayoutBlock]) -> f32 {
    let mut y = 0.0_f32;
    for b in blocks.iter_mut() {
        b.set_origin(Point { x: 0.0, y });
        y += b.size().height;
    }
    y
}

/// Append `extra` (cloned) below `head`, stacked.
fn append_stacked(head: &mut Vec<LayoutBlock>, extra: &[LayoutBlock]) {
    let mut y = blocks_height(head);
    for b in extra {
        let mut c = b.clone();
        c.set_origin(Point { x: 0.0, y });
        y += c.size().height;
        head.push(c);
    }
}

/// Issue #80 — cut a stacked note body at the deepest line / row
/// boundary within `budget`. Whole blocks that fit go to the head; the
/// first block that does not is split (paragraphs at a line, tables at a
/// row); everything after it goes to the tail, re-stacked from `y = 0`.
/// The head may come back empty (nothing fits) and the tail empty
/// (everything fits). The third value is the input index of the block
/// the tail opens with (a cut block repeats its own index).
fn split_note_blocks(
    blocks: &[LayoutBlock],
    budget: f32,
) -> (Vec<LayoutBlock>, Vec<LayoutBlock>, u32) {
    let mut head: Vec<LayoutBlock> = Vec::new();
    let mut tail: Vec<LayoutBlock> = Vec::new();
    let mut y = 0.0_f32;
    let mut tail_first = 0u32;
    let mut iter = blocks.iter().enumerate();
    for (idx, b) in iter.by_ref() {
        let h = b.size().height;
        tail_first = idx as u32;
        if y + h <= budget {
            let mut c = b.clone();
            c.set_origin(Point { x: 0.0, y });
            head.push(c);
            y += h;
            tail_first = idx as u32 + 1;
            continue;
        }
        match b {
            LayoutBlock::Paragraph(p) => {
                let (hd, tl) = split_paragraph_at_line(p, budget - y);
                if let Some(mut hd) = hd {
                    hd.origin = Point { x: 0.0, y };
                    head.push(LayoutBlock::Paragraph(hd));
                }
                if let Some(tl) = tl {
                    tail.push(LayoutBlock::Paragraph(tl));
                }
            }
            LayoutBlock::Table(t) => {
                let (hd, tl) = split_table_rows(t, budget - y);
                if let Some(mut hd) = hd {
                    hd.origin = Point { x: 0.0, y };
                    head.push(LayoutBlock::Table(hd));
                }
                if let Some(tl) = tl {
                    tail.push(LayoutBlock::Table(tl));
                }
            }
        }
        break;
    }
    tail.extend(iter.map(|(_, b)| b.clone()));
    restack_blocks(&mut tail);
    (head, tail, tail_first)
}

/// Issue #80 — the smallest slice a note body can yield: the first line
/// of its first paragraph (or the first row of its first table, or the
/// whole first block when it cannot be split). The progress guarantee
/// of every carry-over loop. The third value is the tail's first block
/// index (0 when the first block was cut, 1 when it went whole).
fn first_note_slice(blocks: &[LayoutBlock]) -> (Vec<LayoutBlock>, Vec<LayoutBlock>, u32) {
    let Some(first) = blocks.first() else {
        return (Vec::new(), Vec::new(), 0);
    };
    let (mut head, mut tail): (Vec<LayoutBlock>, Vec<LayoutBlock>) = match first {
        LayoutBlock::Paragraph(p) => {
            let (hd, tl) = split_paragraph_at_line_index(p, 1);
            (
                hd.into_iter().map(LayoutBlock::Paragraph).collect(),
                tl.into_iter().map(LayoutBlock::Paragraph).collect(),
            )
        }
        LayoutBlock::Table(t) => {
            let (hd, tl) = split_table_rows_at(t, 1);
            (
                hd.into_iter().map(LayoutBlock::Table).collect(),
                tl.into_iter().map(LayoutBlock::Table).collect(),
            )
        }
    };
    if head.is_empty() {
        head.push(first.clone());
    }
    let tail_first = if tail.is_empty() { 1 } else { 0 };
    tail.extend(blocks[1..].iter().cloned());
    restack_blocks(&mut head);
    restack_blocks(&mut tail);
    (head, tail, tail_first)
}

/// Split a table at the deepest row boundary within `budget`.
fn split_table_rows(t: &TableBox, budget: f32) -> (Option<TableBox>, Option<TableBox>) {
    let mut n = 0usize;
    let mut y = 0.0_f32;
    for r in &t.rows {
        if y + r.size.height > budget {
            break;
        }
        y += r.size.height;
        n += 1;
    }
    split_table_rows_at(t, n)
}

/// Split a table so rows `[0, n)` form the head and `[n, ..)` the tail
/// (each re-stacked from `y = 0`; no header-row repeat — notes are not
/// the body).
fn split_table_rows_at(t: &TableBox, n: usize) -> (Option<TableBox>, Option<TableBox>) {
    if n == 0 {
        return (None, Some(t.clone()));
    }
    if n >= t.rows.len() {
        return (Some(t.clone()), None);
    }
    let build = |rows: &[TableRowBox]| {
        let mut y = 0.0_f32;
        let rows: Vec<TableRowBox> = rows
            .iter()
            .map(|r| {
                let mut rr = r.clone();
                rr.origin.y = y;
                y += rr.size.height;
                rr
            })
            .collect();
        TableBox {
            origin: Point { x: 0.0, y: 0.0 },
            size: Size {
                width: t.size.width,
                height: y,
            },
            columns: t.columns.clone(),
            rows,
            outer_borders: t.outer_borders.clone(),
        }
    };
    (Some(build(&t.rows[..n])), Some(build(&t.rows[n..])))
}

/// Issue #80 — scan a laid-out block for note reference anchors. Returns
/// `(anchor, marker text)` for every reference the block carries, in
/// document order, duplicates preserved (the fitter dedupes).
pub fn collect_note_anchors(block: &LayoutBlock) -> Vec<(NoteAnchor, String)> {
    let mut out: Vec<(NoteAnchor, String)> = Vec::new();
    match block {
        LayoutBlock::Paragraph(p) => {
            for line in &p.lines {
                out.extend(anchors_on_line(line));
            }
        }
        LayoutBlock::Table(t) => {
            for row in &t.rows {
                out.extend(anchors_in_row(row));
            }
        }
    }
    out
}

/// The note references anchored on one line, in run order.
fn anchors_on_line(line: &LineBox) -> Vec<(NoteAnchor, String)> {
    let mut out = Vec::new();
    for run in &line.runs {
        for g in &run.glyphs {
            if let Some(anchor) = g.inline_note_anchor {
                out.push((anchor, g.inline_footnote_marker.clone().unwrap_or_default()));
            }
        }
    }
    out
}

/// The note references anchored anywhere in one table row (cells
/// depth-first; a vertically merged continuation cell repeats its
/// origin's content and is skipped).
fn anchors_in_row(row: &TableRowBox) -> Vec<(NoteAnchor, String)> {
    let mut out = Vec::new();
    for cell in &row.cells {
        if matches!(cell.v_merge, engine::VMergeRole::Continue) {
            continue;
        }
        for inner in &cell.content {
            out.extend(collect_note_anchors(inner));
        }
    }
    out
}

/// `true` when any line of `p` carries a note reference.
fn paragraph_has_note_anchors(p: &ParagraphBox) -> bool {
    p.lines.iter().any(|l| {
        l.runs
            .iter()
            .any(|r| r.glyphs.iter().any(|g| g.inline_note_anchor.is_some()))
    })
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

    /// An `n_lines` paragraph whose FIRST line anchors footnote `id`.
    fn fake_paragraph_with_footnote_ref(id: u32, n_lines: usize, line_height: f32) -> ParagraphBox {
        fake_paragraph_with_footnote_ref_on_line(id, 0, n_lines, line_height)
    }

    fn fn_anchor(id: u32) -> NoteAnchor {
        NoteAnchor {
            kind: engine::NoteKind::Footnote,
            id,
        }
    }

    /// Issue #80 — note bodies keyed by footnote id: each a single
    /// `n_lines × line_height` paragraph.
    fn fake_note_bodies(specs: &[(u32, usize, f32)]) -> HashMap<NoteAnchor, NoteBody> {
        let mut out = HashMap::new();
        for (id, n, h) in specs {
            out.insert(
                fn_anchor(*id),
                vec![LayoutBlock::Paragraph(fake_paragraph(*n, *h))],
            );
        }
        out
    }

    /// An `n_lines` paragraph whose line `line_idx` anchors footnote `id`
    /// (issue #92 fixtures put the reference deep inside the paragraph).
    fn fake_paragraph_with_footnote_ref_on_line(
        id: u32,
        line_idx: usize,
        n_lines: usize,
        line_height: f32,
    ) -> ParagraphBox {
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
            inline_note_anchor: Some(fn_anchor(id)),
            inline_object_height: 0.0,
        };
        p.lines[line_idx].runs.push(crate::boxes::VisualRun {
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

        let bodies = fake_note_bodies(&[(1, 3, 14.0), (2, 2, 14.0)]);
        let mut pag = Paginator::with_default_bands(geom, None, None).with_note_bodies(bodies);
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
        ("footnotes", 0xd598c54612629596, &[]),
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

    /// Issue #80 — a footnote taller than a page splits: the reference
    /// line keeps the head of the note (as much as fits under it), every
    /// following page opens its band with a continuation capped at
    /// `1 - NOTE_BODY_RESERVE_FRACTION` of the body budget so body text
    /// keeps flowing, and the drain terminates without a degradation.
    #[test]
    fn footnote_taller_than_a_page_splits_with_continuations_and_paints() {
        let t0 = Instant::now();
        let geom = a4_geometry();
        let bodies = fake_note_bodies(&[(1, 125, 16.0)]); // 2000 pt of note
        let mut pag = Paginator::with_default_bands(geom, None, None).with_note_bodies(bodies);
        pag.push_block(
            LayoutBlock::Paragraph(fake_paragraph_with_footnote_ref(1, 3, 16.0)),
            0.0,
            0.0,
        );
        pag.push_block(LayoutBlock::Paragraph(fake_paragraph(40, 16.0)), 0.0, 0.0);
        let (pages, notes) = pag.finish_with_notes();
        assert!(t0.elapsed() < adversarial_budget());
        assert!(notes.is_empty(), "no degradation: {notes:?}");
        assert!(pages.len() >= 3, "the note spans pages: {}", pages.len());
        assert_eq!(
            pages[0].blocks.len(),
            1,
            "the referencing paragraph painted"
        );
        assert_eq!(pages[0].footnotes.entries.len(), 1);
        assert_eq!(pages[0].footnotes.entries[0].id, 1);
        assert!(pages[0].footnotes.entries[0].continues_on_next);
        assert!(!pages[0].footnotes.continuation);
        assert!(
            pages[1].footnotes.continuation,
            "page 2 opens with the continuation"
        );
        assert!(pages[1].footnotes.entries[0].continued_from_previous);
        assert!(
            !pages[1].blocks.is_empty(),
            "body text keeps flowing on a continuation page"
        );
        let total_note_lines: usize = pages
            .iter()
            .flat_map(|p| p.footnotes.entries.iter())
            .flat_map(|e| e.blocks.iter())
            .map(|b| b.as_paragraph().map_or(0, |p| p.lines.len()))
            .sum();
        assert_eq!(
            total_note_lines, 125,
            "every note line painted exactly once"
        );
        let total_body_lines: usize = pages
            .iter()
            .flat_map(|p| p.blocks.iter())
            .map(|b| b.as_paragraph().map_or(0, |p| p.lines.len()))
            .sum();
        assert_eq!(total_body_lines, 43, "no body line dropped or duplicated");
        for p in &pages {
            let band_h: f32 = p.footnotes.content_height();
            if !p.footnotes.is_empty() {
                assert!(
                    (p.footnotes.y + band_h - (p.size.height - p.margins.bottom)).abs() < 0.01,
                    "band bottom-anchored at the margin"
                );
                let body_bottom = p
                    .blocks
                    .iter()
                    .map(|b| b.origin().y + b.size().height)
                    .fold(0.0_f32, f32::max);
                assert!(
                    p.margins.top + body_bottom
                        <= p.footnotes.y - FOOTNOTE_SEPARATOR_HEIGHT_PT + 0.01,
                    "body never overruns the band"
                );
            }
        }
    }

    /// Issue #80 — the nominal `footnotes` fixture's shape (its pinned
    /// fingerprint moved with the entry schema; this pins the geometry
    /// in words): note 1 rides page 1 under paragraph A, the 40-line
    /// paragraph splits 37/3 around the 54 pt band, note 2 rides page 2.
    #[test]
    fn nominal_footnotes_fixture_places_each_note_on_its_reference_page() {
        let (_, pages, notes) = nominal_fixtures()
            .into_iter()
            .find(|(n, _, _)| *n == "footnotes")
            .expect("fixture");
        assert!(notes.is_empty());
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0].blocks.len(), 2);
        assert_eq!(pages[0].blocks[1].as_paragraph().unwrap().lines.len(), 37);
        assert_eq!(pages[0].footnotes.entries.len(), 1);
        assert_eq!(pages[0].footnotes.entries[0].id, 1);
        assert_eq!(pages[0].footnotes.entries[0].marker, "1");
        let geom = a4_geometry();
        assert!((pages[0].footnotes.y - (geom.height - geom.margins.bottom - 42.0)).abs() < 0.01);
        assert_eq!(pages[1].blocks.len(), 3);
        assert_eq!(pages[1].footnotes.entries.len(), 1);
        assert_eq!(pages[1].footnotes.entries[0].id, 2);
        assert!(pages[1].endnotes.is_empty());
    }

    /// Issue #80 — the waiver: the first line of a fresh page carries a
    /// note whose single line is taller than the page. Nothing later
    /// could host it either, so it is clipped under the line and
    /// reported (`FootnoteOverflow`); the flow continues.
    #[test]
    fn unsplittable_oversize_footnote_clips_and_reports() {
        let t0 = Instant::now();
        let geom = a4_geometry();
        let bodies = fake_note_bodies(&[(1, 1, 2000.0)]);
        let mut pag = Paginator::with_default_bands(geom, None, None).with_note_bodies(bodies);
        pag.push_block(
            LayoutBlock::Paragraph(fake_paragraph_with_footnote_ref(1, 3, 16.0)),
            0.0,
            0.0,
        );
        pag.push_block(LayoutBlock::Paragraph(fake_paragraph(40, 16.0)), 0.0, 0.0);
        let (pages, notes) = pag.finish_with_notes();
        assert!(t0.elapsed() < adversarial_budget());
        assert_eq!(pages.len(), 2);
        /* The deadline rule still holds under the waiver: only the
        reference line stays above the (clipped) band; the paragraph's
        other two lines flow on. */
        assert_eq!(pages[0].blocks.len(), 1, "the referencing line painted");
        assert_eq!(pages[0].blocks[0].as_paragraph().unwrap().lines.len(), 1);
        assert_eq!(
            pages[0].footnotes.entries.len(),
            1,
            "its footnote painted (clipped)"
        );
        assert_eq!(pages[0].footnotes.entries[0].id, 1);
        assert!(
            !pages[0].footnotes.entries[0].continues_on_next,
            "nothing left to carry"
        );
        assert_eq!(
            pages[1].blocks.len(),
            2,
            "the flow continued: tail + next paragraph"
        );
        assert!(pages[1].footnotes.is_empty());
        assert_eq!(reasons(&notes), vec![DegradeReason::FootnoteOverflow]);
    }

    /// Issue #92 — a reference on the 5th line of a paragraph that
    /// splits after line 3: the note lands on the page holding the
    /// TAIL, never dropped.
    #[test]
    fn reference_in_tail_lands_its_note_on_the_tail_page() {
        let geom = a4_geometry();
        let bodies = fake_note_bodies(&[(7, 2, 14.0)]);
        let mut pag = Paginator::with_default_bands(geom, None, None).with_note_bodies(bodies);
        /* Fill the page so the next paragraph has room for exactly three
        16 pt lines: content height 842 - 72 - 72 = 698 → 43 lines of 16
        leaves 10 pt; 40 lines leave 58 pt → 3 lines fit. */
        pag.push_block(LayoutBlock::Paragraph(fake_paragraph(40, 16.0)), 0.0, 0.0);
        pag.push_block(
            LayoutBlock::Paragraph(fake_paragraph_with_footnote_ref_on_line(7, 4, 8, 16.0)),
            0.0,
            0.0,
        );
        let (pages, notes) = pag.finish_with_notes();
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0].blocks.len(), 2);
        assert_eq!(
            pages[0].blocks[1].as_paragraph().unwrap().lines.len(),
            3,
            "head"
        );
        assert!(
            pages[0].footnotes.is_empty(),
            "the head does not own the note"
        );
        assert_eq!(
            pages[1].blocks[0].as_paragraph().unwrap().lines.len(),
            5,
            "tail"
        );
        assert_eq!(
            pages[1].footnotes.entries.len(),
            1,
            "the tail's page hosts the note"
        );
        assert_eq!(pages[1].footnotes.entries[0].id, 7);
    }

    /// Issue #92 — a reference on line 2 of a paragraph whose lines 4+
    /// overflow: the note stays with the HEAD and shrinks the head's
    /// budget (the deadline), so the head loses a line to make room.
    #[test]
    fn reference_in_head_keeps_its_note_and_shrinks_the_head() {
        let geom = a4_geometry();
        /* 38 body lines leave 90 pt. Without the note five 16 pt lines
        fit (80); with the reference on line 2 the 28 pt note + 12 pt
        separator are reserved from that line on, so only three do
        (48 + 40 = 88 ≤ 90 < 64 + 40). */
        let bodies = fake_note_bodies(&[(3, 2, 14.0)]);
        let mut pag = Paginator::with_default_bands(geom, None, None).with_note_bodies(bodies);
        pag.push_block(LayoutBlock::Paragraph(fake_paragraph(38, 16.0)), 0.0, 0.0);
        pag.push_block(
            LayoutBlock::Paragraph(fake_paragraph_with_footnote_ref_on_line(3, 1, 8, 16.0)),
            0.0,
            0.0,
        );
        let (pages, notes) = pag.finish_with_notes();
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(pages.len(), 2);
        let head = pages[0].blocks[1].as_paragraph().unwrap();
        assert_eq!(
            head.lines.len(),
            3,
            "deadline: the band ate two lines of room"
        );
        assert_eq!(
            pages[0].footnotes.entries.len(),
            1,
            "the head's page hosts the note"
        );
        assert_eq!(pages[0].footnotes.entries[0].id, 3);
        assert!(pages[1].footnotes.is_empty());
        assert_eq!(pages[1].blocks[0].as_paragraph().unwrap().lines.len(), 5);
        /* Band geometry: bottom-anchored, 40 pt tall, body above it. */
        let p = &pages[0];
        assert!((p.footnotes.y - (geom.height - geom.margins.bottom - 28.0)).abs() < 0.01);
        assert!(
            p.margins.top + head.origin.y + head.size.height
                <= p.footnotes.y - FOOTNOTE_SEPARATOR_HEIGHT_PT + 0.01
        );
    }

    /// Issue #80 — a note that does not fit whole under its reference
    /// line splits: at least one note line stays, the rest opens the next
    /// page's band with a continuation; the continuation-notice story
    /// closes the cut.
    #[test]
    fn note_splits_under_its_reference_with_a_notice() {
        let geom = a4_geometry();
        let bodies = fake_note_bodies(&[(1, 6, 14.0)]); // 84 pt note
        let notice = vec![LayoutBlock::Paragraph(fake_paragraph(1, 10.0))];
        let mut pag = Paginator::with_default_bands(geom, None, None)
            .with_note_bodies(bodies)
            .with_continuation_notice(Some(notice));
        /* 40 lines leave 58 pt: the 16 pt reference line + 12 pt gap +
        10 pt notice leave 20 pt → one 14 pt note line stays. */
        pag.push_block(LayoutBlock::Paragraph(fake_paragraph(40, 16.0)), 0.0, 0.0);
        pag.push_block(
            LayoutBlock::Paragraph(fake_paragraph_with_footnote_ref(1, 1, 16.0)),
            0.0,
            0.0,
        );
        pag.push_block(LayoutBlock::Paragraph(fake_paragraph(2, 16.0)), 0.0, 0.0);
        let (pages, notes) = pag.finish_with_notes();
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0].blocks.len(), 2, "the reference line stayed");
        let head = &pages[0].footnotes.entries[0];
        assert!(head.continues_on_next);
        assert_eq!(head.blocks.len(), 2, "one note slice + the notice");
        assert_eq!(head.blocks[0].as_paragraph().unwrap().lines.len(), 1);
        assert!((head.content_height() - 24.0).abs() < 0.01);
        assert!(pages[1].footnotes.continuation);
        let cont = &pages[1].footnotes.entries[0];
        assert!(cont.continued_from_previous && !cont.continues_on_next);
        assert_eq!(cont.blocks[0].as_paragraph().unwrap().lines.len(), 5);
        assert_eq!(
            pages[1].blocks.len(),
            1,
            "the following paragraph flowed to page 2"
        );
    }

    /// Issue #80 — endnotes are a trailing band beneath the body; a
    /// page may carry both a footnote band and an endnote band, and a
    /// long endnote continues at the top of the next page.
    #[test]
    fn endnotes_trail_the_body_and_continue_across_pages() {
        let geom = a4_geometry();
        let bodies = fake_note_bodies(&[(1, 2, 14.0)]);
        let mut pag = Paginator::with_default_bands(geom, None, None).with_note_bodies(bodies);
        pag.push_block(
            LayoutBlock::Paragraph(fake_paragraph_with_footnote_ref(1, 30, 16.0)),
            0.0,
            0.0,
        );
        let en = |id: u32| NoteAnchor {
            kind: engine::NoteKind::Endnote,
            id,
        };
        pag.push_trailing_notes(vec![
            (
                en(1),
                "i".to_string(),
                vec![LayoutBlock::Paragraph(fake_paragraph(2, 14.0))],
            ),
            (
                en(2),
                "ii".to_string(),
                vec![LayoutBlock::Paragraph(fake_paragraph(30, 14.0))],
            ),
        ]);
        let (pages, notes) = pag.finish_with_notes();
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(pages.len(), 2);
        let p0 = &pages[0];
        assert_eq!(p0.footnotes.entries.len(), 1, "footnote band on page 1");
        assert_eq!(p0.endnotes.entries.len(), 2, "both endnotes open on page 1");
        assert_eq!(p0.endnotes.entries[0].kind, engine::NoteKind::Endnote);
        assert!(!p0.endnotes.entries[0].continues_on_next);
        assert!(
            p0.endnotes.entries[1].continues_on_next,
            "the long one is cut"
        );
        /* 30 body lines = 480 pt; the endnote band opens 12 pt below. */
        assert!((p0.endnotes.y - (geom.margins.top + 480.0 + 12.0)).abs() < 0.01);
        /* Endnote band + footnote band never overlap: endnotes end above
        the footnote band's separator. */
        let en_bottom = p0.endnotes.y + p0.endnotes.content_height();
        assert!(en_bottom <= p0.footnotes.y - FOOTNOTE_SEPARATOR_HEIGHT_PT + 0.01);
        let p1 = &pages[1];
        assert!(p1.endnotes.continuation);
        assert_eq!(p1.endnotes.entries.len(), 1);
        assert!(p1.endnotes.entries[0].continued_from_previous);
        assert_eq!(p1.endnotes.entries[0].id, 2);
        assert_eq!(
            p1.endnotes.entries[0].first_block_index, 0,
            "cut inside block 0"
        );
        let total: usize = pages
            .iter()
            .flat_map(|p| p.endnotes.entries.iter())
            .filter(|e| e.id == 2)
            .flat_map(|e| e.blocks.iter())
            .map(|b| b.as_paragraph().map_or(0, |p| p.lines.len()))
            .sum();
        assert_eq!(total, 30, "every endnote line painted once");
    }

    /// Issue #80 — `beneathText` pins the band right under the body's
    /// last line instead of the bottom margin.
    #[test]
    fn beneath_text_position_places_the_band_under_the_body() {
        let geom = a4_geometry();
        let bodies = fake_note_bodies(&[(1, 2, 14.0)]);
        let mut pag = Paginator::with_default_bands(geom, None, None).with_note_bodies(bodies);
        pag.set_footnote_position(NotePosition::BeneathText);
        pag.push_block(
            LayoutBlock::Paragraph(fake_paragraph_with_footnote_ref(1, 5, 16.0)),
            0.0,
            0.0,
        );
        let pages = pag.finish();
        assert_eq!(pages.len(), 1);
        assert!((pages[0].footnotes.y - (geom.margins.top + 80.0 + 12.0)).abs() < 0.01);
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
