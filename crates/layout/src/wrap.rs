//! Issue #82 — text wrap around floating objects.
//!
//! Three pieces, all pure functions of box geometry:
//!
//! 1. **Cutouts.** A positioned [`FloatBox`] on a page becomes a list of
//!    [`WrapCutout`]s — horizontal exclusion intervals per y-band — in the
//!    coordinate space of one paragraph box ([`cutouts_for_float`]).
//!    Square wrap is the bounding box plus the four wrap distances;
//!    tight / through wrap slices the `<wp:wrapPolygon>` into per-band
//!    slabs (falling back to the bounding box, reported, when the polygon
//!    is missing); top-and-bottom blocks the whole band; behind / in
//!    front (`WrapKind::None`) cuts nothing.
//! 2. **Segments.** [`segments_for_band`] turns the content box minus the
//!    cutouts intersecting a y-band into the available horizontal ranges
//!    ([`LineSegment`]s) — the model that unifies column bounds and float
//!    cutouts (a line is a *set of horizontal ranges*). The paragraph
//!    composer fills them in reading order.
//! 3. **The plan + convergence.** Object position depends on text flow
//!    (a float is placed against its anchor paragraph / line); text flow
//!    depends on object wrap. [`derive_plan`] reads a finished page list
//!    and produces the per-paragraph cutouts ([`WrapPlan`]) the *next*
//!    layout pass should use; [`WrapConvergence`] drives the
//!    anchor → position → wrap → reflow loop *around* the paginator to a
//!    fixpoint, bounded twice per the layout self-defense doctrine
//!    (render rules): a per-object **forward-move cap** (an object that
//!    keeps pushing its own anchor forward is frozen — its cutouts are
//!    dropped and it paints in front of the text, reported as
//!    [`DegradeReason::WrapObjectFrozen`]) and the [`Watchdog`] churn
//!    ladder over the whole float configuration (an oscillating
//!    configuration is a repeated fingerprint — stage (b) freezes every
//!    object that moved, [`DegradeReason::WrapOscillation`]), with a
//!    hard pass cap as the last backstop. Every escape hatch paints
//!    something and drops no content.
//!
//! Documents without wrapping floats never enter the loop: the engine
//! runs one pass with an empty plan, exactly the pre-#82 pipeline.

use crate::boxes::{
    FloatAnchorRef, FloatBox, LayoutBlock, LineSegment, PageBox, ParagraphBox, Point, WrapSide,
};
use crate::watchdog::{BlockFingerprint, DegradeReason, DegradeStage, LayoutDegradation, Watchdog};
use engine::WrapKind;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::hash::{Hash, Hasher};

/// One horizontal exclusion interval over a y-band, in
/// **paragraph-box-relative** px (x from the paragraph box's left edge,
/// y from the top of the *whole* paragraph — a split paragraph's tail
/// offsets its cutouts by the heights of the fragments before it, so one
/// plan entry describes the full paragraph). `x0 == -∞` / `x1 == +∞`
/// mark a band the text must skip entirely (top-and-bottom wrap, or a
/// one-sided wrap rule).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WrapCutout {
    pub x0: f32,
    pub x1: f32,
    pub y0: f32,
    pub y1: f32,
}

impl WrapCutout {
    /// Does this cutout touch the band `[y0, y1)`?
    pub fn intersects_band(&self, y0: f32, y1: f32) -> bool {
        self.y0 < y1 && self.y1 > y0
    }

    fn approx_eq(&self, o: &WrapCutout) -> bool {
        approx(self.x0, o.x0)
            && approx(self.x1, o.x1)
            && approx(self.y0, o.y0)
            && approx(self.y1, o.y1)
    }
}

/// Tolerance for "the plan did not change" — well under a device px.
const EPS: f32 = 0.01;

fn approx(a: f32, b: f32) -> bool {
    a == b || (a - b).abs() <= EPS
}

/// A segment narrower than this many nominal line heights is skipped by
/// the composer (a sliver next to an image would otherwise hold one or
/// two letters per line). Word applies a comparable minimum.
pub const MIN_SEGMENT_LINE_HEIGHTS: f32 = 1.5;

/// The available horizontal ranges of the band `[y0, y1)` inside the
/// content box `[x0, x1)`, after subtracting every cutout that touches the
/// band. Sorted left → right; zero-width ranges dropped. An empty result
/// means the band is fully blocked (top-and-bottom wrap).
pub fn segments_for_band(
    x0: f32,
    x1: f32,
    cutouts: &[WrapCutout],
    y0: f32,
    y1: f32,
) -> Vec<LineSegment> {
    let mut blocked: Vec<(f32, f32)> = cutouts
        .iter()
        .filter(|c| c.intersects_band(y0, y1))
        .map(|c| (c.x0.max(x0), c.x1.min(x1)))
        .filter(|(a, b)| b > a)
        .collect();
    if blocked.is_empty() {
        return vec![LineSegment { x0, x1 }];
    }
    blocked.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut out = Vec::new();
    let mut cursor = x0;
    for (a, b) in blocked {
        if a > cursor {
            out.push(LineSegment { x0: cursor, x1: a });
        }
        cursor = cursor.max(b);
    }
    if cursor < x1 {
        out.push(LineSegment { x0: cursor, x1 });
    }
    out
}

/// Where the band situation next changes below `y0`: the lowest bottom
/// edge among the cutouts intersecting `[y0, y1)`. `None` when no cutout
/// touches the band. Always `> y0`, so a composer that skips a blocked
/// band by jumping here makes strict progress.
pub fn next_band_edge(cutouts: &[WrapCutout], y0: f32, y1: f32) -> Option<f32> {
    cutouts
        .iter()
        .filter(|c| c.intersects_band(y0, y1))
        .map(|c| c.y1)
        .filter(|&e| e > y0)
        .min_by(|a, b| a.total_cmp(b))
}

/// Shape space of `<wp:wrapPolygon>`: `(21600, 21600)` is the object's
/// bottom-right corner (ECMA-376 §20.4.2.16).
const POLY_UNITS: f32 = 21600.0;

/// The cutouts one positioned float registers against a paragraph box
/// whose page-relative left edge is `para_x`, width `para_w`, and whose
/// (whole-paragraph) top is `para_top` — for a split paragraph's tail
/// the caller passes the tail's page top minus the head heights, so the
/// result is in whole-paragraph space. `slab_h` is the band height used
/// to slice a tight / through polygon (the paragraph's nominal line
/// height). `polygon_fallback` is set when a tight / through object
/// carried no usable polygon and the bounding box was used instead.
pub fn cutouts_for_float(
    f: &FloatBox,
    para_x: f32,
    para_w: f32,
    para_top: f32,
    slab_h: f32,
    polygon_fallback: &mut bool,
) -> Vec<WrapCutout> {
    if f.hidden || !f.wrap.cuts_text() {
        return Vec::new();
    }
    let w = &f.wrap;
    /* Object rect in paragraph space, expanded by the wrap distances. */
    let ox0 = f.origin.x - para_x;
    let ox1 = f.origin.x + f.size.width - para_x;
    let oy0 = f.origin.y - para_top;
    let oy1 = f.origin.y + f.size.height - para_top;
    let (dl, dr, dt, db) = (
        w.dist_left.max(0.0),
        w.dist_right.max(0.0),
        w.dist_top.max(0.0),
        w.dist_bottom.max(0.0),
    );
    let mut out: Vec<WrapCutout> = Vec::new();
    match w.kind {
        WrapKind::None => return out,
        WrapKind::TopAndBottom => {
            out.push(WrapCutout {
                x0: f32::NEG_INFINITY,
                x1: f32::INFINITY,
                y0: oy0 - dt,
                y1: oy1 + db,
            });
            return out;
        }
        WrapKind::Square => out.push(WrapCutout {
            x0: ox0 - dl,
            x1: ox1 + dr,
            y0: oy0 - dt,
            y1: oy1 + db,
        }),
        WrapKind::Tight | WrapKind::Through => {
            match w.polygon.as_deref().filter(|p| p.len() >= 3) {
                Some(poly) => {
                    let sx = f.size.width / POLY_UNITS;
                    let sy = f.size.height / POLY_UNITS;
                    let slab = if slab_h > 0.5 { slab_h } else { 12.0 };
                    let mut y = oy0 - dt;
                    let end = oy1 + db;
                    /* Bounded: each slab advances `slab`; the object is
                    finite. The polygon is sampled at the slab clamped to
                    the object's own rows, so the distance bands above /
                    below the object take the extent of the nearest row. */
                    while y < end {
                        let y_next = (y + slab).min(end);
                        let sample_lo = (y.max(oy0)).min(oy1);
                        let sample_hi = (y_next.min(oy1)).max(oy0);
                        let (lo, hi) = if sample_hi > sample_lo {
                            (sample_lo, sample_hi)
                        } else {
                            /* A pure distance band: sample the object row
                            it abuts (top row above, bottom row below). */
                            if y < oy0 {
                                (oy0, (oy0 + slab).min(oy1))
                            } else {
                                ((oy1 - slab).max(oy0), oy1)
                            }
                        };
                        if let Some((px0, px1)) = polygon_x_extent(poly, sx, sy, oy0, lo, hi) {
                            out.push(WrapCutout {
                                x0: ox0 + px0 - dl,
                                x1: ox0 + px1 + dr,
                                y0: y,
                                y1: y_next,
                            });
                        }
                        y = y_next;
                    }
                }
                None => {
                    *polygon_fallback = true;
                    out.push(WrapCutout {
                        x0: ox0 - dl,
                        x1: ox1 + dr,
                        y0: oy0 - dt,
                        y1: oy1 + db,
                    });
                }
            }
        }
    }
    /* Side rule. `Largest` compares the free widths beside the expanded
    object inside the paragraph box and blocks the smaller side. */
    let side = match w.side {
        WrapSide::Largest => {
            let left_gap = (ox0 - dl).max(0.0);
            let right_gap = (para_w - (ox1 + dr)).max(0.0);
            if left_gap >= right_gap {
                WrapSide::Left
            } else {
                WrapSide::Right
            }
        }
        s => s,
    };
    match side {
        WrapSide::Left => {
            /* Text only on the LEFT of the object: block everything to
            its right. */
            for c in &mut out {
                c.x1 = f32::INFINITY;
            }
        }
        WrapSide::Right => {
            for c in &mut out {
                c.x0 = f32::NEG_INFINITY;
            }
        }
        WrapSide::Both | WrapSide::Largest => {}
    }
    out
}

/// Horizontal extent `(min_x, max_x)` (object-relative px) of the polygon
/// clipped to the object-space row band `[lo, hi)` (paragraph-space y,
/// with `oy0` the object's top). `None` when no edge crosses the band.
fn polygon_x_extent(
    poly: &[Point],
    sx: f32,
    sy: f32,
    oy0: f32,
    lo: f32,
    hi: f32,
) -> Option<(f32, f32)> {
    let n = poly.len();
    let mut min_x = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut hit = false;
    let band_lo = lo - oy0;
    let band_hi = hi - oy0;
    for i in 0..n {
        let a = poly[i];
        let b = poly[(i + 1) % n];
        let (ax, ay) = (a.x * sx, a.y * sy);
        let (bx, by) = (b.x * sx, b.y * sy);
        /* Clip the segment's parameter range to the band. */
        let (mut t0, mut t1) = (0.0_f32, 1.0_f32);
        let dy = by - ay;
        if dy.abs() < f32::EPSILON {
            if ay < band_lo || ay > band_hi {
                continue;
            }
        } else {
            let ta = (band_lo - ay) / dy;
            let tb = (band_hi - ay) / dy;
            let (tmin, tmax) = if ta < tb { (ta, tb) } else { (tb, ta) };
            t0 = t0.max(tmin);
            t1 = t1.min(tmax);
            if t0 > t1 {
                continue;
            }
        }
        for t in [t0, t1] {
            let x = ax + (bx - ax) * t;
            min_x = min_x.min(x);
            max_x = max_x.max(x);
            hit = true;
        }
    }
    hit.then_some((min_x, max_x))
}

/// The per-paragraph cutout plan for one layout pass, keyed by
/// `ParagraphBox::source_paragraph_id` (stable across passes — the
/// engine assigns it in walk order). A paragraph absent from the plan
/// lays out at full width.
pub type WrapPlan = BTreeMap<u32, Vec<WrapCutout>>;

/// Two plans agree when every paragraph has the same cutouts within
/// [`EPS`] — the fixpoint test.
pub fn plans_equal(a: &WrapPlan, b: &WrapPlan) -> bool {
    a.len() == b.len()
        && a.iter().zip(b.iter()).all(|((ka, va), (kb, vb))| {
            ka == kb && va.len() == vb.len() && va.iter().zip(vb).all(|(x, y)| x.approx_eq(y))
        })
}

/// Identity of a float across passes: the anchor paragraph's source id
/// plus the sentinel byte. Header / footer anchors use the two reserved
/// ids below (bands are laid per page, never split).
pub type FloatKey = (u32, u32);
pub const HEADER_ANCHOR_ID: u32 = u32::MAX - 1;
pub const FOOTER_ANCHOR_ID: u32 = u32::MAX - 2;

/// Resolve a float's [`FloatKey`] on its page.
pub fn float_key(page: &PageBox, f: &FloatBox) -> Option<FloatKey> {
    let id = match f.anchor {
        FloatAnchorRef::Header => HEADER_ANCHOR_ID,
        FloatAnchorRef::Footer => FOOTER_ANCHOR_ID,
        FloatAnchorRef::Body { block, cell } => {
            let b = page.blocks.get(block)?;
            match (b, cell) {
                (LayoutBlock::Paragraph(p), None) => p.source_paragraph_id,
                (LayoutBlock::Table(t), Some(c)) => {
                    let inner = t.rows.get(c.row)?.cells.get(c.col)?.content.get(c.inner)?;
                    inner.as_paragraph()?.source_paragraph_id
                }
                _ => return None,
            }
        }
    };
    Some((id, f.at))
}

/// Read a finished page list and produce the plan the next pass should
/// lay out against. Only top-level body paragraphs are wrapped (table
/// cell text and the bands themselves are not cut — tracked as a gap);
/// every float on a page — body, header or footer anchored — registers
/// against every body paragraph of that page, in that paragraph's own
/// (whole-paragraph) coordinate space. Floats in `frozen` register
/// nothing. Split paragraphs: fragment `j` of a paragraph shifts its
/// cutouts by the heights of fragments `0..j`, and cutouts are clipped
/// to the fragment's own rows on the side that faces another fragment
/// (a page-1 float must not cut rows that live on page 2).
///
/// `slab_h` slices tight / through polygons; `notes` receives one
/// [`DegradeReason::WrapPolygonFallback`] per page whose tight / through
/// object lacked a polygon.
pub fn derive_plan(
    pages: &[PageBox],
    slab_h: f32,
    frozen: &BTreeSet<FloatKey>,
    notes: &mut Vec<LayoutDegradation>,
) -> WrapPlan {
    /* Pass 1 — fragment inventory per paragraph id, in page order. */
    let mut fragments: HashMap<u32, Vec<(usize, usize)>> = HashMap::new();
    for (pi, page) in pages.iter().enumerate() {
        for (bi, block) in page.blocks.iter().enumerate() {
            if let LayoutBlock::Paragraph(p) = block
                && p.source_paragraph_id != ParagraphBox::NO_SOURCE_ID
            {
                fragments
                    .entry(p.source_paragraph_id)
                    .or_default()
                    .push((pi, bi));
            }
        }
    }
    let mut plan: WrapPlan = BTreeMap::new();
    let mut ids: Vec<u32> = fragments.keys().copied().collect();
    ids.sort_unstable();
    for id in ids {
        let frags = &fragments[&id];
        let last = frags.len() - 1;
        let mut offset = 0.0_f32;
        let mut cuts: Vec<WrapCutout> = Vec::new();
        for (j, &(pi, bi)) in frags.iter().enumerate() {
            let page = &pages[pi];
            let LayoutBlock::Paragraph(p) = &page.blocks[bi] else {
                continue;
            };
            let para_x = page.margins.left + p.origin.x;
            let para_y = page.margins.top + p.origin.y;
            let frag_top = offset;
            let frag_bottom = offset + p.size.height;
            let mut fallback = false;
            for f in &page.floats {
                if !f.wrap.cuts_text() || f.hidden {
                    continue;
                }
                if let Some(key) = float_key(page, f)
                    && frozen.contains(&key)
                {
                    continue;
                }
                for mut c in cutouts_for_float(
                    f,
                    para_x,
                    p.size.width,
                    para_y - offset,
                    slab_h,
                    &mut fallback,
                ) {
                    if j > 0 {
                        c.y0 = c.y0.max(frag_top);
                    }
                    if j < last {
                        c.y1 = c.y1.min(frag_bottom);
                    }
                    if c.y1 > c.y0 {
                        cuts.push(c);
                    }
                }
            }
            if fallback {
                notes.push(LayoutDegradation {
                    reason: DegradeReason::WrapPolygonFallback,
                    page: pi as u32,
                });
            }
            offset = frag_bottom;
        }
        if !cuts.is_empty() {
            plan.insert(id, cuts);
        }
    }
    plan
}

/// `(key, page index, page-relative y)` of every wrapping float.
fn float_positions(pages: &[PageBox]) -> Vec<(FloatKey, usize, f32)> {
    let mut out = Vec::new();
    for (pi, page) in pages.iter().enumerate() {
        for f in &page.floats {
            if !f.wrap.cuts_text() || f.hidden {
                continue;
            }
            if let Some(key) = float_key(page, f) {
                out.push((key, pi, f.origin.y));
            }
        }
    }
    out.sort_by_key(|a| a.0);
    out
}

/// What the engine should do after a pass.
#[derive(Debug, Clone, PartialEq)]
pub enum WrapVerdict {
    /// The plan the pass was laid with reproduces itself: fixpoint.
    Converged,
    /// Lay out again with this plan.
    Continue(WrapPlan),
    /// The pass cap was hit; accept the pages as they are (reported).
    Capped,
}

/// The anchor → position → wrap → reflow loop's bookkeeping. One
/// instance per layout build; the engine calls [`Self::observe`] after
/// every pass and follows the verdict. See the module docs for the two
/// termination rules and the hard cap.
#[derive(Debug)]
pub struct WrapConvergence {
    pass: u32,
    max_passes: u32,
    forward_cap: u32,
    slab_h: f32,
    prev: HashMap<FloatKey, (usize, f32)>,
    forward_moves: HashMap<FloatKey, u32>,
    frozen: BTreeSet<FloatKey>,
    watchdog: Watchdog,
}

impl WrapConvergence {
    /// Hard cap on layout passes (the baseline pass counts). The watchdog
    /// ladder freezes an A/B oscillation on its 5th pass; a converging
    /// document needs 2–3.
    pub const DEFAULT_MAX_PASSES: u32 = 8;
    /// Forward moves (down the flow, or onto a later page) an object may
    /// make before it is frozen.
    pub const DEFAULT_FORWARD_CAP: u32 = 3;

    pub fn new(slab_h: f32) -> Self {
        Self {
            pass: 0,
            max_passes: Self::DEFAULT_MAX_PASSES,
            forward_cap: Self::DEFAULT_FORWARD_CAP,
            slab_h,
            prev: HashMap::new(),
            forward_moves: HashMap::new(),
            frozen: BTreeSet::new(),
            watchdog: Watchdog::default(),
        }
    }

    pub fn with_limits(mut self, max_passes: u32, forward_cap: u32) -> Self {
        self.max_passes = max_passes.max(1);
        self.forward_cap = forward_cap.max(1);
        self
    }

    /// Test-only: turn every recovery into a hard failure.
    pub fn strict(mut self, on: bool) -> Self {
        self.watchdog = std::mem::take(&mut self.watchdog).strict(on);
        self
    }

    pub fn passes(&self) -> u32 {
        self.pass
    }

    pub fn frozen(&self) -> &BTreeSet<FloatKey> {
        &self.frozen
    }

    pub fn take_notes(&mut self) -> Vec<LayoutDegradation> {
        self.watchdog.take_notes()
    }

    /// Feed the pages of a pass laid out against `current`.
    pub fn observe(&mut self, pages: &[PageBox], current: &WrapPlan) -> WrapVerdict {
        self.pass += 1;
        let positions = float_positions(pages);
        let page_of = |pi: usize| pi as u32;

        /* Rule 1 — per-object forward-move cap. */
        for (key, pi, y) in &positions {
            if let Some(&(ppi, py)) = self.prev.get(key) {
                let forward = *pi > ppi || (*pi == ppi && *y > py + EPS);
                if forward && !self.frozen.contains(key) {
                    let n = self.forward_moves.entry(*key).or_insert(0);
                    *n += 1;
                    if *n >= self.forward_cap {
                        self.frozen.insert(*key);
                        self.watchdog
                            .note(DegradeReason::WrapObjectFrozen, page_of(*pi));
                    }
                }
            }
        }

        /* Fixpoint test first — a converged pass must not be counted as
        churn. */
        let mut notes = Vec::new();
        let next = derive_plan(pages, self.slab_h, &self.frozen, &mut notes);
        for n in notes {
            /* One report per page, not one per pass. */
            if !self.watchdog.notes().contains(&n) {
                self.watchdog.note(n.reason, n.page);
            }
        }
        if plans_equal(&next, current) {
            self.prev = positions.into_iter().map(|(k, p, y)| (k, (p, y))).collect();
            return WrapVerdict::Converged;
        }

        /* Rule 2 — churn ladder over the whole configuration. The same
        set of positions seen again is an oscillation, not progress. */
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for (key, pi, y) in &positions {
            key.hash(&mut h);
            pi.hash(&mut h);
            y.to_bits().hash(&mut h);
        }
        let fp = BlockFingerprint {
            units: h.finish(),
            is_table: false,
            fresh: true,
        };
        let stage = self.watchdog.observe(fp);
        let mut refrozen = false;
        if stage >= DegradeStage::Freeze {
            for (key, pi, y) in &positions {
                let moved = self
                    .prev
                    .get(key)
                    .is_none_or(|&(ppi, py)| ppi != *pi || !approx(py, *y));
                if moved && self.frozen.insert(*key) {
                    refrozen = true;
                    self.watchdog
                        .note(DegradeReason::WrapOscillation, page_of(*pi));
                }
            }
        }
        self.prev = positions.into_iter().map(|(k, p, y)| (k, (p, y))).collect();

        if self.pass >= self.max_passes {
            let page = pages.len().saturating_sub(1) as u32;
            self.watchdog.note(DegradeReason::WrapOscillation, page);
            return WrapVerdict::Capped;
        }
        if refrozen {
            /* The frozen set changed: re-derive so the next pass drops
            the frozen objects' cutouts. */
            let mut notes = Vec::new();
            let next = derive_plan(pages, self.slab_h, &self.frozen, &mut notes);
            return WrapVerdict::Continue(next);
        }
        WrapVerdict::Continue(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::boxes::{FloatWrap, HeaderRole, LineBox, Size};
    use crate::page::Margins;
    use text_pipeline::{Alignment, ShapingDirection};

    fn cut(x0: f32, x1: f32, y0: f32, y1: f32) -> WrapCutout {
        WrapCutout { x0, x1, y0, y1 }
    }

    #[test]
    fn band_without_cutouts_is_one_full_segment() {
        let s = segments_for_band(0.0, 400.0, &[], 0.0, 16.0);
        assert_eq!(s, vec![LineSegment { x0: 0.0, x1: 400.0 }]);
        /* A cutout that misses the band changes nothing. */
        let s = segments_for_band(0.0, 400.0, &[cut(100.0, 200.0, 50.0, 90.0)], 0.0, 16.0);
        assert_eq!(s, vec![LineSegment { x0: 0.0, x1: 400.0 }]);
    }

    #[test]
    fn cutout_splits_band_into_left_and_right_ranges() {
        let s = segments_for_band(0.0, 400.0, &[cut(100.0, 200.0, 0.0, 40.0)], 10.0, 26.0);
        assert_eq!(
            s,
            vec![
                LineSegment { x0: 0.0, x1: 100.0 },
                LineSegment {
                    x0: 200.0,
                    x1: 400.0
                }
            ]
        );
        /* Overlapping cutouts merge; one flush with the left edge yields a
        single right-hand range. */
        let s = segments_for_band(
            0.0,
            400.0,
            &[cut(-20.0, 120.0, 0.0, 40.0), cut(100.0, 150.0, 0.0, 40.0)],
            0.0,
            16.0,
        );
        assert_eq!(
            s,
            vec![LineSegment {
                x0: 150.0,
                x1: 400.0
            }]
        );
    }

    #[test]
    fn full_width_cutout_blocks_the_band_and_reports_its_edge() {
        let c = [cut(f32::NEG_INFINITY, f32::INFINITY, 10.0, 60.0)];
        assert!(segments_for_band(0.0, 400.0, &c, 20.0, 36.0).is_empty());
        assert_eq!(next_band_edge(&c, 20.0, 36.0), Some(60.0));
        assert_eq!(next_band_edge(&c, 70.0, 86.0), None);
    }

    fn float_at(x: f32, y: f32, w: f32, h: f32, wrap: FloatWrap) -> FloatBox {
        FloatBox {
            origin: Point { x, y },
            size: Size {
                width: w,
                height: h,
            },
            rel_id: "r".into(),
            at: 1,
            anchor: FloatAnchorRef::Body {
                block: 0,
                cell: None,
            },
            z_order: 0,
            behind_doc: false,
            hidden: false,
            frame_origin: Point::default(),
            wrap,
            text_box: None,
        }
    }

    fn wrap(kind: WrapKind) -> FloatWrap {
        FloatWrap {
            kind,
            side: WrapSide::Both,
            dist_top: 2.0,
            dist_bottom: 3.0,
            dist_left: 4.0,
            dist_right: 5.0,
            polygon: None,
        }
    }

    #[test]
    fn square_cutout_is_the_box_plus_distances_in_paragraph_space() {
        let f = float_at(150.0, 220.0, 100.0, 50.0, wrap(WrapKind::Square));
        let mut fb = false;
        let c = cutouts_for_float(&f, 100.0, 400.0, 200.0, 16.0, &mut fb);
        assert_eq!(c, vec![cut(46.0, 155.0, 18.0, 73.0)]);
        assert!(!fb);
    }

    #[test]
    fn none_hidden_and_top_and_bottom_kinds() {
        let mut fb = false;
        let none = float_at(0.0, 0.0, 10.0, 10.0, wrap(WrapKind::None));
        assert!(cutouts_for_float(&none, 0.0, 400.0, 0.0, 16.0, &mut fb).is_empty());
        let mut hidden = float_at(0.0, 0.0, 10.0, 10.0, wrap(WrapKind::Square));
        hidden.hidden = true;
        assert!(cutouts_for_float(&hidden, 0.0, 400.0, 0.0, 16.0, &mut fb).is_empty());
        let tb = float_at(150.0, 220.0, 100.0, 50.0, wrap(WrapKind::TopAndBottom));
        let c = cutouts_for_float(&tb, 100.0, 400.0, 200.0, 16.0, &mut fb);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].x0, f32::NEG_INFINITY);
        assert_eq!(c[0].x1, f32::INFINITY);
        assert_eq!((c[0].y0, c[0].y1), (18.0, 73.0));
    }

    #[test]
    fn side_rules_block_one_side() {
        let mut fb = false;
        let mut w = wrap(WrapKind::Square);
        w.side = WrapSide::Left;
        let f = float_at(150.0, 220.0, 100.0, 50.0, w.clone());
        let c = cutouts_for_float(&f, 100.0, 400.0, 200.0, 16.0, &mut fb);
        assert_eq!((c[0].x0, c[0].x1), (46.0, f32::INFINITY));
        w.side = WrapSide::Right;
        let f = float_at(150.0, 220.0, 100.0, 50.0, w.clone());
        let c = cutouts_for_float(&f, 100.0, 400.0, 200.0, 16.0, &mut fb);
        assert_eq!((c[0].x0, c[0].x1), (f32::NEG_INFINITY, 155.0));
        /* Largest: the object sits near the left edge → the right side
        is larger → the left sliver is blocked. */
        w.side = WrapSide::Largest;
        let f = float_at(150.0, 220.0, 100.0, 50.0, w);
        let c = cutouts_for_float(&f, 100.0, 400.0, 200.0, 16.0, &mut fb);
        assert_eq!((c[0].x0, c[0].x1), (f32::NEG_INFINITY, 155.0));
    }

    #[test]
    fn tight_without_polygon_falls_back_to_square_and_reports() {
        let mut fb = false;
        let f = float_at(150.0, 220.0, 100.0, 50.0, wrap(WrapKind::Tight));
        let c = cutouts_for_float(&f, 100.0, 400.0, 200.0, 16.0, &mut fb);
        assert!(fb, "missing polygon is reported");
        assert_eq!(c, vec![cut(46.0, 155.0, 18.0, 73.0)]);
    }

    #[test]
    fn tight_polygon_slices_per_band_following_the_shape() {
        /* A downward-pointing triangle: full width at the top, a point at
        the bottom centre. Lower slabs must be narrower than upper ones. */
        let mut w = wrap(WrapKind::Tight);
        w.dist_left = 0.0;
        w.dist_right = 0.0;
        w.dist_top = 0.0;
        w.dist_bottom = 0.0;
        w.polygon = Some(vec![
            Point { x: 0.0, y: 0.0 },
            Point { x: 21600.0, y: 0.0 },
            Point {
                x: 10800.0,
                y: 21600.0,
            },
        ]);
        let f = float_at(100.0, 0.0, 100.0, 100.0, w);
        let mut fb = false;
        let c = cutouts_for_float(&f, 0.0, 400.0, 0.0, 25.0, &mut fb);
        assert!(!fb);
        assert_eq!(c.len(), 4, "100 px object / 25 px slabs");
        let widths: Vec<f32> = c.iter().map(|k| k.x1 - k.x0).collect();
        assert!((widths[0] - 100.0).abs() < 0.01, "top slab spans the base");
        assert!(widths[0] > widths[1] && widths[1] > widths[2] && widths[2] > widths[3]);
        assert!((widths[3] - 25.0).abs() < 0.01, "bottom slab is the tip");
        /* Every slab stays centred on the apex. */
        for k in &c {
            assert!(((k.x0 + k.x1) / 2.0 - 150.0).abs() < 0.01);
        }
    }

    /* ---- plan derivation + convergence ---- */

    fn para(id: u32, y: f32, h: f32, lines: usize) -> LayoutBlock {
        LayoutBlock::Paragraph(ParagraphBox {
            origin: Point { x: 0.0, y },
            size: Size {
                width: 400.0,
                height: h,
            },
            lines: (0..lines)
                .map(|i| LineBox {
                    origin: Point {
                        x: 0.0,
                        y: i as f32 * 16.0,
                    },
                    baseline: 12.0,
                    height: 16.0,
                    width: 400.0,
                    runs: Vec::new(),
                    alignment: Alignment::Start,
                    source_start: 0,
                    segments: Vec::new(),
                    segment: 0,
                })
                .collect(),
            direction: ShapingDirection::Ltr,
            marker: None,
            source_paragraph_id: id,
            fields: Vec::new(),
            page_break_after_line: Vec::new(),
            borders: None,
            shading: None,
            keep_next: false,
        })
    }

    fn page(blocks: Vec<LayoutBlock>, floats: Vec<FloatBox>) -> PageBox {
        PageBox {
            size: Size {
                width: 600.0,
                height: 800.0,
            },
            margins: Margins::uniform(50.0),
            blocks,
            header: None,
            footer: None,
            header_offset: 20.0,
            footer_offset: 20.0,
            footnotes: crate::NoteBand::default(),
            endnotes: crate::NoteBand::default(),
            hf_role: HeaderRole::Default,
            page_number: 1,
            floats,
        }
    }

    #[test]
    fn plan_registers_page_floats_against_every_body_paragraph() {
        let mut w = wrap(WrapKind::Square);
        w.dist_left = 0.0;
        w.dist_right = 0.0;
        w.dist_top = 0.0;
        w.dist_bottom = 0.0;
        /* Float anchored in paragraph 1 (block 1), page-relative
        (100, 130) 80×40; paragraph 0 at content y 0 (page 50), paragraph
        1 at content y 64 (page 114). */
        let mut f = float_at(100.0, 130.0, 80.0, 40.0, w);
        f.anchor = FloatAnchorRef::Body {
            block: 1,
            cell: None,
        };
        let pages = vec![page(
            vec![para(7, 0.0, 64.0, 4), para(9, 64.0, 64.0, 4)],
            vec![f],
        )];
        let mut notes = Vec::new();
        let plan = derive_plan(&pages, 16.0, &BTreeSet::new(), &mut notes);
        assert!(notes.is_empty());
        assert_eq!(plan.len(), 2, "both paragraphs see the float");
        /* Paragraph 7: x from 100 − 50 = 50 to 130; y from 130 − 50 = 80
        to 120 (below its own 64 px — harmless, the composer ignores it). */
        assert_eq!(plan[&7], vec![cut(50.0, 130.0, 80.0, 120.0)]);
        /* Paragraph 9: y from 130 − 114 = 16 to 56. */
        assert_eq!(plan[&9], vec![cut(50.0, 130.0, 16.0, 56.0)]);
        /* Frozen float registers nothing. */
        let frozen: BTreeSet<FloatKey> = [(9u32, 1u32)].into_iter().collect();
        let plan = derive_plan(&pages, 16.0, &frozen, &mut notes);
        assert!(plan.is_empty());
    }

    #[test]
    fn split_paragraph_tail_offsets_and_clips_its_cutouts() {
        let mut w = wrap(WrapKind::Square);
        w.dist_left = 0.0;
        w.dist_right = 0.0;
        w.dist_top = 0.0;
        w.dist_bottom = 0.0;
        /* Paragraph 3: head (3 lines, 48 px) at the bottom of page 0, tail
        (2 lines, 32 px) at the top of page 1. A float on page 1 at
        page-relative y 60 (content y 10) cuts the tail: in
        whole-paragraph space that is 48 + 10 = 58. A float on page 0
        extending below the head's bottom is clipped to the head. */
        let mut f1 = float_at(100.0, 60.0, 80.0, 40.0, w.clone());
        f1.anchor = FloatAnchorRef::Body {
            block: 0,
            cell: None,
        };
        let f0 = float_at(100.0, 50.0 + 700.0 + 40.0, 80.0, 40.0, w);
        let pages = vec![
            page(vec![para(3, 700.0, 48.0, 3)], vec![f0]),
            page(vec![para(3, 0.0, 32.0, 2)], vec![f1]),
        ];
        let mut notes = Vec::new();
        let plan = derive_plan(&pages, 16.0, &BTreeSet::new(), &mut notes);
        let cuts = &plan[&3];
        assert_eq!(cuts.len(), 2);
        /* Page-0 float: y 40..80 in head space, clipped at the head's
        bottom (48). */
        assert_eq!(cuts[0], cut(50.0, 130.0, 40.0, 48.0));
        /* Page-1 float: 58..98 in whole-paragraph space, floor at the
        tail's top (48) — already above it. */
        assert_eq!(cuts[1], cut(50.0, 130.0, 58.0, 98.0));
    }

    #[test]
    fn convergence_stops_when_the_plan_reproduces_itself() {
        let mut f = float_at(100.0, 130.0, 80.0, 40.0, wrap(WrapKind::Square));
        f.anchor = FloatAnchorRef::Body {
            block: 0,
            cell: None,
        };
        let pages = vec![page(vec![para(1, 0.0, 64.0, 4)], vec![f])];
        let mut conv = WrapConvergence::new(16.0).strict(true);
        let plan0 = WrapPlan::new();
        let v = conv.observe(&pages, &plan0);
        let WrapVerdict::Continue(plan1) = v else {
            panic!("first pass must ask for a wrapped pass: {v:?}");
        };
        assert_eq!(plan1.len(), 1);
        /* Same pages again (the float did not move): fixpoint. */
        assert_eq!(conv.observe(&pages, &plan1), WrapVerdict::Converged);
        assert_eq!(conv.passes(), 2);
        assert!(conv.take_notes().is_empty());
    }

    /// The oscillation the issue names: an object that pushes its own
    /// anchor forward and follows it. Modelled as alternating positions;
    /// the loop must freeze it within the forward cap and report.
    #[test]
    fn oscillating_float_is_frozen_within_bounded_passes_with_a_note() {
        let mk = |y: f32| {
            let mut f = float_at(100.0, y, 80.0, 40.0, wrap(WrapKind::Square));
            f.anchor = FloatAnchorRef::Body {
                block: 0,
                cell: None,
            };
            vec![page(vec![para(1, 0.0, 64.0, 4)], vec![f])]
        };
        let a = mk(130.0);
        let b = mk(146.0);
        let mut conv = WrapConvergence::new(16.0);
        let mut plan = WrapPlan::new();
        let mut passes = 0;
        let mut frozen_seen = false;
        loop {
            passes += 1;
            let pages = if passes % 2 == 1 { &a } else { &b };
            match conv.observe(pages, &plan) {
                WrapVerdict::Converged => break,
                WrapVerdict::Capped => break,
                WrapVerdict::Continue(next) => {
                    if !conv.frozen().is_empty() {
                        frozen_seen = true;
                        /* Once frozen the object registers nothing: the
                        next plan is empty and the following pass
                        converges. */
                        assert!(next.is_empty());
                    }
                    plan = next;
                }
            }
            assert!(passes <= 20, "must terminate");
        }
        assert!(frozen_seen, "the oscillating object was frozen");
        assert!(passes <= WrapConvergence::DEFAULT_MAX_PASSES as usize);
        let notes = conv.take_notes();
        assert!(
            notes.iter().any(|n| matches!(
                n.reason,
                DegradeReason::WrapObjectFrozen | DegradeReason::WrapOscillation
            )),
            "{notes:?}"
        );
    }

    #[test]
    fn pass_cap_is_the_last_backstop() {
        /* Positions that never repeat and never move forward: a float
        drifting UP each pass defeats both the forward cap and the
        churn fingerprint, so the hard cap must end it. */
        let mut conv = WrapConvergence::new(16.0).with_limits(3, 2);
        let mut plan = WrapPlan::new();
        let mut y = 300.0;
        let mut verdicts = Vec::new();
        for _ in 0..3 {
            let mut f = float_at(100.0, y, 80.0, 40.0, wrap(WrapKind::Square));
            f.anchor = FloatAnchorRef::Body {
                block: 0,
                cell: None,
            };
            let pages = vec![page(vec![para(1, 0.0, 64.0, 4)], vec![f])];
            let v = conv.observe(&pages, &plan);
            if let WrapVerdict::Continue(n) = &v {
                plan = n.clone();
            }
            verdicts.push(v);
            y -= 16.0;
        }
        assert_eq!(verdicts.last(), Some(&WrapVerdict::Capped));
        assert!(
            conv.take_notes()
                .iter()
                .any(|n| n.reason == DegradeReason::WrapOscillation)
        );
    }

    #[test]
    fn float_key_resolves_body_cell_and_band_anchors() {
        let mut f = float_at(0.0, 0.0, 1.0, 1.0, wrap(WrapKind::Square));
        let pg = page(vec![para(42, 0.0, 16.0, 1)], vec![]);
        assert_eq!(float_key(&pg, &f), Some((42, 1)));
        f.anchor = FloatAnchorRef::Header;
        assert_eq!(float_key(&pg, &f), Some((HEADER_ANCHOR_ID, 1)));
        f.anchor = FloatAnchorRef::Footer;
        assert_eq!(float_key(&pg, &f), Some((FOOTER_ANCHOR_ID, 1)));
        f.anchor = FloatAnchorRef::Body {
            block: 5,
            cell: None,
        };
        assert_eq!(float_key(&pg, &f), None, "dangling block index");
    }
}
