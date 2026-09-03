//! Layout self-defense (issue #87) — pagination watchdog, staged
//! degradation, verified fast paths.
//!
//! Doctrine: **an imperfect layout that terminates beats a perfect one
//! that hangs.** Every convergence loop in the layout pipeline is bounded
//! by construction (monotone progress) *and* backstopped by a watchdog
//! that counts churn rounds — placement attempts that re-place the same
//! content on a fresh page without consuming any of it — and escalates
//! through fixed stages once churn crosses a threshold:
//!
//! 1. [`DegradeStage::DropOptional`] — optional constraints (keep-with-next
//!    chains, repeated table header rows) are released and the block is
//!    retried.
//! 2. [`DegradeStage::Freeze`] — the churning block is pinned atomically
//!    at the cursor: no more column advances or page moves, the overflow
//!    clips.
//! 3. [`DegradeStage::ForceValidate`] — document level: the page cap was
//!    hit, the remaining flow is appended without further page breaks and
//!    the current state is accepted as the final layout.
//!
//! The counter is keyed by a content fingerprint, so a strictly smaller
//! re-push (a paragraph tail, a table continuation) starts a fresh
//! counter — only genuine churn escalates. The window is one top-level
//! block: [`Watchdog::begin_block`] resets both counter and stage.
//! Deviation from the reference doctrine, deliberately: the stage never
//! backs off *inside* a block. A churn round here is a proof of
//! non-progress (same content, fresh page), not a heuristic window, so
//! there is nothing to be gained from un-escalating mid-block.
//!
//! Every degradation is recorded as a [`LayoutDegradation`] note. The
//! engine forwards the notes on `Event::Painted` (`layout_degraded`) so
//! telemetry and QA can see that a paint was laid out degraded instead of
//! silently trusting the pixels.
//!
//! The second half of the module is the **verified fast path**:
//! [`verify_prefix`] checks that an incremental (viewport-culled) band is
//! a geometric prefix of a deeper band of the same document. The engine
//! runs it whenever a lazy `ExpandLayout` extends a band and demotes to a
//! full reflow on mismatch — a stale-layout bug becomes a perf blip, not
//! corruption. [`geometry_fingerprint`] is the companion: a stable hash
//! of a page list's geometry that the regression tests pin so a pure
//! self-defense refactor is provably output-identical on the nominal
//! path.

use crate::boxes::{FootnoteEntry, LayoutBlock, NoteBand, PageBox, ParagraphBox, TableBox};
use std::hash::{Hash, Hasher};

/// Escalation ladder. Ordered: a later stage is a stronger response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum DegradeStage {
    /// No churn observed — every constraint is honoured.
    #[default]
    Nominal,
    /// Stage (a): optional constraints released (keep-with-next, header
    /// repeat), the block is retried.
    DropOptional,
    /// Stage (b): the block is pinned atomically where the cursor is.
    Freeze,
    /// Stage (c): the document-level cap — accept the current state.
    ForceValidate,
}

/// Why a layout was degraded. Mirrors `bridge::LayoutDegradeReason`
/// one-to-one; the engine maps between them at the bridge boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DegradeReason {
    /// A single line (or an atomic table) taller than the page budget was
    /// placed on a fresh page and clips past the bottom margin.
    OversizeLine,
    /// A keep-with-next chain could not move to the next page (already at
    /// a page top, or stage (a) in force); the constraint was released.
    KeepChainDropped,
    /// Repeated table header rows left no room for a body row on a
    /// continuation page; the repeat was suppressed for that page.
    HeaderRepeatDropped,
    /// A footnote band left no body budget on a fresh page; the block was
    /// placed atomically over the band instead of being bounced forever.
    FootnoteOverflow,
    /// Stage (b): the block churned and was pinned at the cursor.
    FrozenPlacement,
    /// Stage (c): the page cap was hit; the remaining flow was appended
    /// without further page breaks.
    PageCap,
    /// An incremental band failed its prefix invariant against the
    /// previous band; the engine demoted to a full reflow.
    FastPathMismatch,
    /// A paragraph layout-cache entry failed its post-conditions and was
    /// re-laid from scratch.
    CacheMismatch,
    /// The autofit shrink solver hit its iteration cap; column floors were
    /// used as-is.
    AutofitCap,
}

impl DegradeReason {
    /// Stable wire-friendly name (SCREAMING_SNAKE_CASE, matching the
    /// bridge enum's serde rename).
    pub fn as_str(self) -> &'static str {
        match self {
            DegradeReason::OversizeLine => "OVERSIZE_LINE",
            DegradeReason::KeepChainDropped => "KEEP_CHAIN_DROPPED",
            DegradeReason::HeaderRepeatDropped => "HEADER_REPEAT_DROPPED",
            DegradeReason::FootnoteOverflow => "FOOTNOTE_OVERFLOW",
            DegradeReason::FrozenPlacement => "FROZEN_PLACEMENT",
            DegradeReason::PageCap => "PAGE_CAP",
            DegradeReason::FastPathMismatch => "FAST_PATH_MISMATCH",
            DegradeReason::CacheMismatch => "CACHE_MISMATCH",
            DegradeReason::AutofitCap => "AUTOFIT_CAP",
        }
    }
}

/// One degradation note. `page` is the 0-based index of the page being
/// filled when the degradation was applied (for document-level reasons,
/// the page count at that moment).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayoutDegradation {
    pub reason: DegradeReason,
    pub page: u32,
}

/// Content fingerprint of one placement attempt — what "progress" is
/// measured against. `units` is the block's consumable content count
/// (lines for a paragraph, rows for a table); `fresh` records whether
/// the attempt started on an empty page. A re-push that consumed nothing
/// has the same `units`; the same content attempted twice on a fresh
/// page is a proof of non-progress, because a fresh page is the most
/// room the paginator can ever offer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BlockFingerprint {
    pub units: u64,
    pub is_table: bool,
    pub fresh: bool,
}

impl BlockFingerprint {
    pub fn of(block: &LayoutBlock, fresh: bool) -> Self {
        match block {
            LayoutBlock::Paragraph(p) => Self {
                units: p.lines.len() as u64,
                is_table: false,
                fresh,
            },
            LayoutBlock::Table(t) => Self {
                units: t.rows.len() as u64,
                is_table: true,
                fresh,
            },
        }
    }
}

/// Churn counter + escalation ladder for one flow. See the module docs
/// for the doctrine.
#[derive(Debug, Clone)]
pub struct Watchdog {
    /// Churn rounds a *fresh-page* fingerprint may accumulate before
    /// stage (a). Rounds beyond it step one stage each.
    fresh_threshold: u32,
    /// Churn rounds a *non-fresh* fingerprint may accumulate before
    /// stage (a). The nominal column walk re-pushes identical content
    /// once per remaining column, so callers set this to the column
    /// count plus a margin.
    walk_threshold: u32,
    /// Per-fingerprint round counters for the current block. Tiny: a
    /// block spans a handful of distinct fingerprints at most.
    attempts: Vec<(BlockFingerprint, u32)>,
    stage: DegradeStage,
    notes: Vec<LayoutDegradation>,
    /// Test-only switch: every note becomes a hard failure so CI catches
    /// a new loop instead of a silent recovery masking it.
    strict: bool,
}

impl Watchdog {
    /// Default fresh-page churn threshold. One identical fresh-page
    /// attempt is already a proof of non-progress; the second such round
    /// escalates.
    pub const DEFAULT_FRESH_THRESHOLD: u32 = 1;
    /// Default non-fresh churn threshold: a single-column walk plus a
    /// margin of one.
    pub const DEFAULT_WALK_THRESHOLD: u32 = 2;

    pub fn new(fresh_threshold: u32, walk_threshold: u32) -> Self {
        Self {
            fresh_threshold: fresh_threshold.max(1),
            walk_threshold: walk_threshold.max(1),
            attempts: Vec::new(),
            stage: DegradeStage::Nominal,
            notes: Vec::new(),
            strict: false,
        }
    }

    /// Test-only: turn silent recoveries into panics.
    pub fn strict(mut self, on: bool) -> Self {
        self.strict = on;
        self
    }

    pub fn is_strict(&self) -> bool {
        self.strict
    }

    /// Re-tune the non-fresh threshold (the paginator does this when a
    /// section's column count changes).
    pub fn set_walk_threshold(&mut self, t: u32) {
        self.walk_threshold = t.max(1);
    }

    pub fn stage(&self) -> DegradeStage {
        self.stage
    }

    /// Rounds accumulated by `fp` in the current block (0 = first attempt
    /// or never seen).
    pub fn rounds(&self, fp: BlockFingerprint) -> u32 {
        self.attempts
            .iter()
            .find(|(f, _)| *f == fp)
            .map_or(0, |(_, r)| *r)
    }

    /// A new top-level block enters the flow: forget churn state. Notes
    /// are kept — they describe the whole flow.
    pub fn begin_block(&mut self) {
        self.attempts.clear();
        self.stage = DegradeStage::Nominal;
    }

    /// Feed one placement attempt. Returns the stage in force for it.
    pub fn observe(&mut self, fp: BlockFingerprint) -> DegradeStage {
        let rounds = match self.attempts.iter_mut().find(|(f, _)| *f == fp) {
            Some((_, r)) => {
                *r += 1;
                *r
            }
            None => {
                self.attempts.push((fp, 0));
                0
            }
        };
        let threshold = if fp.fresh {
            self.fresh_threshold
        } else {
            self.walk_threshold
        };
        if rounds >= threshold {
            let target = if rounds - threshold == 0 {
                DegradeStage::DropOptional
            } else {
                DegradeStage::Freeze
            };
            if target > self.stage {
                self.stage = target;
            }
        }
        self.stage
    }

    /// Force the ladder to at least `stage` (the document-level cap, or a
    /// test that wants to exercise a branch directly).
    pub fn escalate_to(&mut self, stage: DegradeStage) {
        if stage > self.stage {
            self.stage = stage;
        }
    }

    /// Record a degradation. Panics in strict mode.
    pub fn note(&mut self, reason: DegradeReason, page: u32) {
        assert!(
            !self.strict,
            "layout watchdog (strict): {} on page {page}",
            reason.as_str()
        );
        self.notes.push(LayoutDegradation { reason, page });
    }

    pub fn notes(&self) -> &[LayoutDegradation] {
        &self.notes
    }

    pub fn take_notes(&mut self) -> Vec<LayoutDegradation> {
        std::mem::take(&mut self.notes)
    }
}

impl Default for Watchdog {
    fn default() -> Self {
        Self::new(Self::DEFAULT_FRESH_THRESHOLD, Self::DEFAULT_WALK_THRESHOLD)
    }
}

/* ===================================================================
Verified fast paths
==================================================================== */

/// Why an incremental band was rejected against its reference band.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FastPathMismatch {
    /// The shorter band has pages the longer one lacks — impossible for
    /// a prefix; reported defensively.
    PageCount { shorter: usize, longer: usize },
    /// Page `page` differs in size or margins.
    PageGeometry { page: usize },
    /// A fully-committed page differs in block count.
    BlockCount {
        page: usize,
        shorter: usize,
        longer: usize,
    },
    /// Block `block` on page `page` differs in origin, size or content
    /// count.
    BlockGeometry { page: usize, block: usize },
    /// The shorter band's last page ends at a different position than
    /// the longer band's same page (its blocks are not a prefix).
    EndPosition { page: usize },
}

/// Verify that `shorter` is a geometric prefix of `longer`: every page of
/// `shorter` except its last matches the same page of `longer` exactly
/// (size, margins, block count, per-block origin + size + content count);
/// the last page's blocks are a prefix of the longer band's same page.
///
/// Both inputs are real layouts of the same document at the same inputs,
/// so equality is exact (bit-identical `f32`s) — the layout is
/// deterministic; any difference is a bug, never rounding.
pub fn verify_prefix(shorter: &[PageBox], longer: &[PageBox]) -> Result<(), FastPathMismatch> {
    if shorter.len() > longer.len() {
        return Err(FastPathMismatch::PageCount {
            shorter: shorter.len(),
            longer: longer.len(),
        });
    }
    let last = shorter.len().saturating_sub(1);
    for (page, (s, l)) in shorter.iter().zip(longer.iter()).enumerate() {
        if s.size != l.size || !margins_eq(&s.margins, &l.margins) {
            return Err(FastPathMismatch::PageGeometry { page });
        }
        let is_last = page == last;
        if !is_last && s.blocks.len() != l.blocks.len() {
            return Err(FastPathMismatch::BlockCount {
                page,
                shorter: s.blocks.len(),
                longer: l.blocks.len(),
            });
        }
        if is_last && s.blocks.len() > l.blocks.len() {
            return Err(FastPathMismatch::EndPosition { page });
        }
        for (block, (sb, lb)) in s.blocks.iter().zip(l.blocks.iter()).enumerate() {
            if !block_geometry_eq(sb, lb) {
                return Err(FastPathMismatch::BlockGeometry { page, block });
            }
        }
    }
    Ok(())
}

fn margins_eq(a: &crate::page::Margins, b: &crate::page::Margins) -> bool {
    a.top == b.top && a.right == b.right && a.bottom == b.bottom && a.left == b.left
}

fn block_geometry_eq(a: &LayoutBlock, b: &LayoutBlock) -> bool {
    match (a, b) {
        (LayoutBlock::Paragraph(p), LayoutBlock::Paragraph(q)) => {
            p.origin == q.origin && p.size == q.size && p.lines.len() == q.lines.len()
        }
        (LayoutBlock::Table(s), LayoutBlock::Table(t)) => {
            s.origin == t.origin && s.size == t.size && s.rows.len() == t.rows.len()
        }
        _ => false,
    }
}

/* ===================================================================
Geometry fingerprint — the regression anchor for "output-identical"
==================================================================== */

/// A stable hash of every geometric fact in `pages`: page size / margins /
/// number, block origins + sizes, line origins / baselines / heights /
/// widths / source starts, run source ranges + glyph advances, table rows
/// and cells (recursively), footnote bands and header / footer content
/// heights. Two page lists with equal fingerprints paint identically.
///
/// `DefaultHasher::new()` uses fixed SipHash keys, so the value is stable
/// across runs and machines for one Rust release — the tests pin the
/// values recorded on the pre-watchdog paginator.
pub fn geometry_fingerprint(pages: &[PageBox]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (pages.len() as u64).hash(&mut h);
    for page in pages {
        hash_f32(&mut h, page.size.width);
        hash_f32(&mut h, page.size.height);
        hash_f32(&mut h, page.margins.top);
        hash_f32(&mut h, page.margins.right);
        hash_f32(&mut h, page.margins.bottom);
        hash_f32(&mut h, page.margins.left);
        hash_f32(&mut h, page.header_offset);
        hash_f32(&mut h, page.footer_offset);
        page.page_number.hash(&mut h);
        hash_f32(
            &mut h,
            page.header.as_ref().map_or(-1.0, |b| b.content_height()),
        );
        hash_f32(
            &mut h,
            page.footer.as_ref().map_or(-1.0, |b| b.content_height()),
        );
        (page.blocks.len() as u64).hash(&mut h);
        for b in &page.blocks {
            hash_block(&mut h, b);
        }
        (page.footnotes.entries.len() as u64).hash(&mut h);
        /* Issue #80 — band placement + entry geometry are pinned only
        when a band exists, so note-free documents keep their pre-#80
        fingerprints bit-for-bit. */
        hash_note_band(&mut h, &page.footnotes);
        if !page.endnotes.is_empty() {
            (page.endnotes.entries.len() as u64).hash(&mut h);
            hash_note_band(&mut h, &page.endnotes);
        }
    }
    h.finish()
}

fn hash_note_band(h: &mut impl Hasher, band: &NoteBand) {
    if band.is_empty() {
        return;
    }
    hash_f32(h, band.y);
    band.continuation.hash(h);
    for f in &band.entries {
        hash_note_entry(h, f);
    }
}

fn hash_note_entry(h: &mut impl Hasher, f: &FootnoteEntry) {
    f.id.hash(h);
    f.first_block_index.hash(h);
    f.marker.hash(h);
    (f.kind == engine::NoteKind::Endnote).hash(h);
    hash_f32(h, f.origin.x);
    hash_f32(h, f.origin.y);
    f.continued_from_previous.hash(h);
    f.continues_on_next.hash(h);
    (f.blocks.len() as u64).hash(h);
    for b in &f.blocks {
        hash_block(h, b);
    }
}

fn hash_f32(h: &mut impl Hasher, v: f32) {
    v.to_bits().hash(h);
}

fn hash_block(h: &mut impl Hasher, b: &LayoutBlock) {
    match b {
        LayoutBlock::Paragraph(p) => {
            0u8.hash(h);
            hash_paragraph(h, p);
        }
        LayoutBlock::Table(t) => {
            1u8.hash(h);
            hash_table(h, t);
        }
    }
}

fn hash_paragraph(h: &mut impl Hasher, p: &ParagraphBox) {
    hash_f32(h, p.origin.x);
    hash_f32(h, p.origin.y);
    hash_f32(h, p.size.width);
    hash_f32(h, p.size.height);
    p.source_paragraph_id.hash(h);
    p.marker.is_some().hash(h);
    if let Some(m) = &p.marker {
        hash_f32(h, m.origin.x);
        hash_f32(h, m.origin.y);
        hash_f32(h, m.width);
    }
    (p.lines.len() as u64).hash(h);
    for l in &p.lines {
        hash_f32(h, l.origin.x);
        hash_f32(h, l.origin.y);
        hash_f32(h, l.baseline);
        hash_f32(h, l.height);
        hash_f32(h, l.width);
        l.source_start.hash(h);
        (l.runs.len() as u64).hash(h);
        for r in &l.runs {
            r.source_range.start.hash(h);
            r.source_range.end.hash(h);
            (r.glyphs.len() as u64).hash(h);
            for g in &r.glyphs {
                g.id.hash(h);
                g.cluster.hash(h);
                hash_f32(h, g.x_advance);
                hash_f32(h, g.x_offset);
                hash_f32(h, g.y_offset);
            }
        }
    }
    (p.page_break_after_line.len() as u64).hash(h);
    for i in &p.page_break_after_line {
        (*i as u64).hash(h);
    }
    (p.fields.len() as u64).hash(h);
    for f in &p.fields {
        f.evaluated_text.hash(h);
    }
}

fn hash_table(h: &mut impl Hasher, t: &TableBox) {
    hash_f32(h, t.origin.x);
    hash_f32(h, t.origin.y);
    hash_f32(h, t.size.width);
    hash_f32(h, t.size.height);
    (t.columns.len() as u64).hash(h);
    for c in &t.columns {
        hash_f32(h, *c);
    }
    (t.rows.len() as u64).hash(h);
    for r in &t.rows {
        hash_f32(h, r.origin.x);
        hash_f32(h, r.origin.y);
        hash_f32(h, r.size.width);
        hash_f32(h, r.size.height);
        r.header.hash(h);
        (r.cells.len() as u64).hash(h);
        for c in &r.cells {
            hash_f32(h, c.origin.x);
            hash_f32(h, c.origin.y);
            hash_f32(h, c.size.width);
            hash_f32(h, c.size.height);
            (c.content.len() as u64).hash(h);
            for b in &c.content {
                hash_block(h, b);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::boxes::{LineBox, Point, Size};
    use crate::page::Margins;
    use text_pipeline::{Alignment, ShapingDirection};

    fn fp(units: u64, fresh: bool) -> BlockFingerprint {
        BlockFingerprint {
            units,
            is_table: false,
            fresh,
        }
    }

    #[test]
    fn first_attempt_is_nominal_and_progress_starts_a_fresh_counter() {
        let mut w = Watchdog::default();
        assert_eq!(w.observe(fp(10, true)), DegradeStage::Nominal);
        /* A smaller tail is progress: its own counter starts at zero. */
        assert_eq!(w.observe(fp(6, true)), DegradeStage::Nominal);
        assert_eq!(w.observe(fp(3, true)), DegradeStage::Nominal);
        assert_eq!(w.rounds(fp(10, true)), 0);
        assert_eq!(w.stage(), DegradeStage::Nominal);
    }

    #[test]
    fn fresh_page_churn_escalates_drop_then_freeze() {
        let mut w = Watchdog::default();
        assert_eq!(w.observe(fp(10, true)), DegradeStage::Nominal);
        /* Same content on a fresh page again — proof of non-progress. */
        assert_eq!(w.observe(fp(10, true)), DegradeStage::DropOptional);
        assert_eq!(w.observe(fp(10, true)), DegradeStage::Freeze);
        /* The ladder is monotone within a block. */
        assert_eq!(w.observe(fp(10, true)), DegradeStage::Freeze);
        assert_eq!(w.rounds(fp(10, true)), 3);
    }

    #[test]
    fn column_walk_tolerates_walk_threshold_rounds() {
        /* A 3-column section re-pushes identical content up to three
        times on non-fresh columns before a fresh page — nominal. */
        let mut w = Watchdog::new(1, 4);
        for _ in 0..4 {
            assert_eq!(w.observe(fp(10, false)), DegradeStage::Nominal);
        }
        assert_eq!(w.observe(fp(10, false)), DegradeStage::DropOptional);
        assert_eq!(w.observe(fp(10, false)), DegradeStage::Freeze);
    }

    #[test]
    fn begin_block_resets_counter_and_stage_but_keeps_notes() {
        let mut w = Watchdog::default();
        w.observe(fp(10, true));
        w.observe(fp(10, true));
        w.observe(fp(10, true));
        w.note(DegradeReason::FrozenPlacement, 4);
        assert_eq!(w.stage(), DegradeStage::Freeze);
        w.begin_block();
        assert_eq!(w.stage(), DegradeStage::Nominal);
        assert_eq!(w.rounds(fp(10, true)), 0);
        assert_eq!(
            w.notes(),
            &[LayoutDegradation {
                reason: DegradeReason::FrozenPlacement,
                page: 4
            }]
        );
        assert_eq!(w.take_notes().len(), 1);
        assert!(w.notes().is_empty());
    }

    /// Issue #87 acceptance — "wrap-oscillation": an object that moves
    /// text which moves the object back. Modelled on the primitive (no
    /// wrap objects exist yet — #82 feeds this same watchdog): two
    /// fingerprints alternating forever must escalate to `Freeze` within
    /// a bounded number of rounds instead of oscillating.
    #[test]
    fn wrap_oscillation_pattern_is_frozen_within_bounded_rounds() {
        let mut w = Watchdog::default();
        let a = fp(7, true);
        let b = fp(9, true);
        let mut rounds = 0;
        let stage = loop {
            rounds += 1;
            let s = w.observe(if rounds % 2 == 0 { a } else { b });
            if s >= DegradeStage::Freeze || rounds > 1000 {
                break s;
            }
        };
        assert_eq!(stage, DegradeStage::Freeze);
        assert!(
            rounds <= 6,
            "alternating A/B churn must freeze within 6 rounds, took {rounds}"
        );
    }

    #[test]
    fn escalate_to_never_lowers_the_stage() {
        let mut w = Watchdog::default();
        w.escalate_to(DegradeStage::Freeze);
        w.escalate_to(DegradeStage::DropOptional);
        assert_eq!(w.stage(), DegradeStage::Freeze);
        w.escalate_to(DegradeStage::ForceValidate);
        assert_eq!(w.stage(), DegradeStage::ForceValidate);
    }

    #[test]
    #[should_panic(expected = "layout watchdog (strict): OVERSIZE_LINE on page 2")]
    fn strict_mode_turns_a_note_into_a_hard_failure() {
        let mut w = Watchdog::default().strict(true);
        w.note(DegradeReason::OversizeLine, 2);
    }

    #[test]
    fn reason_names_are_screaming_snake_case() {
        assert_eq!(
            DegradeReason::KeepChainDropped.as_str(),
            "KEEP_CHAIN_DROPPED"
        );
        assert_eq!(
            DegradeReason::FastPathMismatch.as_str(),
            "FAST_PATH_MISMATCH"
        );
    }

    /* ---------- verify_prefix ---------- */

    fn para(y: f32, h: f32, lines: usize) -> LayoutBlock {
        LayoutBlock::Paragraph(ParagraphBox {
            origin: Point { x: 0.0, y },
            size: Size {
                width: 100.0,
                height: h,
            },
            lines: (0..lines)
                .map(|i| LineBox {
                    origin: Point {
                        x: 0.0,
                        y: i as f32 * 10.0,
                    },
                    baseline: 8.0,
                    height: 10.0,
                    width: 100.0,
                    runs: Vec::new(),
                    alignment: Alignment::Start,
                    source_start: 0,
                })
                .collect(),
            direction: ShapingDirection::Ltr,
            marker: None,
            source_paragraph_id: ParagraphBox::NO_SOURCE_ID,
            fields: Vec::new(),
            page_break_after_line: Vec::new(),
            borders: None,
            shading: None,
            keep_next: false,
        })
    }

    fn page(blocks: Vec<LayoutBlock>) -> PageBox {
        PageBox {
            size: Size {
                width: 500.0,
                height: 800.0,
            },
            margins: Margins::uniform(50.0),
            blocks,
            header: None,
            footer: None,
            header_offset: 20.0,
            footer_offset: 20.0,
            footnotes: NoteBand::default(),
            endnotes: NoteBand::default(),
            hf_role: crate::boxes::HeaderRole::Default,
            page_number: 1,
        }
    }

    #[test]
    fn identical_bands_verify() {
        let a = vec![page(vec![para(0.0, 30.0, 3), para(30.0, 20.0, 2)])];
        let b = a.clone();
        assert_eq!(verify_prefix(&a, &b), Ok(()));
        assert_eq!(geometry_fingerprint(&a), geometry_fingerprint(&b));
    }

    #[test]
    fn shallower_band_is_a_prefix_of_the_deeper_one() {
        let shallow = vec![
            page(vec![para(0.0, 30.0, 3)]),
            page(vec![para(0.0, 20.0, 2)]),
        ];
        let deep = vec![
            page(vec![para(0.0, 30.0, 3)]),
            page(vec![para(0.0, 20.0, 2), para(20.0, 40.0, 4)]),
            page(vec![para(0.0, 10.0, 1)]),
        ];
        assert_eq!(verify_prefix(&shallow, &deep), Ok(()));
    }

    #[test]
    fn committed_page_block_count_mismatch_is_rejected() {
        let shallow = vec![page(vec![para(0.0, 30.0, 3)]), page(vec![])];
        let deep = vec![
            page(vec![para(0.0, 30.0, 3), para(30.0, 5.0, 1)]),
            page(vec![]),
        ];
        assert_eq!(
            verify_prefix(&shallow, &deep),
            Err(FastPathMismatch::BlockCount {
                page: 0,
                shorter: 1,
                longer: 2
            })
        );
    }

    #[test]
    fn block_geometry_drift_is_rejected() {
        let shallow = vec![page(vec![para(0.0, 30.0, 3)])];
        let deep = vec![page(vec![para(1.0, 30.0, 3)])];
        assert_eq!(
            verify_prefix(&shallow, &deep),
            Err(FastPathMismatch::BlockGeometry { page: 0, block: 0 })
        );
        assert_ne!(geometry_fingerprint(&shallow), geometry_fingerprint(&deep));
    }

    #[test]
    fn page_geometry_and_page_count_are_checked() {
        let mut other = page(vec![]);
        other.size.height = 900.0;
        assert_eq!(
            verify_prefix(&[page(vec![])], &[other]),
            Err(FastPathMismatch::PageGeometry { page: 0 })
        );
        assert_eq!(
            verify_prefix(&[page(vec![]), page(vec![])], &[page(vec![])]),
            Err(FastPathMismatch::PageCount {
                shorter: 2,
                longer: 1
            })
        );
        /* The shorter band's last page holding MORE blocks than the
        deeper band's same page is not a prefix either. */
        assert_eq!(
            verify_prefix(
                &[page(vec![para(0.0, 30.0, 3), para(30.0, 5.0, 1)])],
                &[page(vec![para(0.0, 30.0, 3)])]
            ),
            Err(FastPathMismatch::EndPosition { page: 0 })
        );
    }
}
