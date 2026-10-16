//! Issues #462 / #65 — navigation that reaches past the lazily laid-out
//! band expands the band SYNCHRONOUSLY, inside the command that needs
//! it, before the move or the hit resolves.
//!
//! Lazy pagination (`LazyLayoutState` + `Command::ExpandLayout`) lays out
//! only the viewport band plus a runway; the shell's scroll asks for more.
//! A burst of ArrowDown presses outruns that scroll, so the caret used to
//! park on the band's last line (#462), and a click on a page past the
//! band guessed the missing page tops from A4 and could clamp onto the
//! wrong page (#65). Both now go through [`Engine::lazy_nav_expand_until`]:
//! raise `min_target_y`, rebuild the band through
//! `ensure_layout_snapshot` — so every growth is the verified fast path of
//! `.claude/rules/render.md` (`verify_prefix_open`, the keep-with-next cull
//! deferral and provisional pages all apply unchanged) — and repeat with a
//! geometrically growing target until the predicate holds or the layout is
//! full. Bounded: the target at least doubles past the band each round, a
//! full layout satisfies every predicate, and the round cap falls back to
//! one full layout. The reply of the command is then built over the grown
//! band, so it already carries the final caret and geometry — one
//! round-trip, no shell retry.

use super::*;

/// Settled pages the band must hold BELOW the caret's page: the next line
/// a vertical step can reach is at most one page away (a block's lines
/// are all laid once its last page is, and the following block starts on
/// that page or the next).
const NAV_PAGES_AHEAD: usize = 1;

/// Expansion rounds before the backstop full layout. Each round at least
/// doubles the target, so 24 rounds span far more than any real document
/// (the cold-open band × 2^24); a full layout always ends the loop sooner.
const MAX_EXPAND_ROUNDS: u32 = 24;

/// A cull target (layout pt at scale 1) no document reaches: the band
/// becomes a full layout. Finite so `× scale` stays finite.
const FULL_LAYOUT_TARGET_PT: f32 = 1.0e30;

/// Height of a laid band in device px (inter-page gaps included).
fn band_height_px(s: &LayoutSnapshot, gap: f32) -> f32 {
    if s.pages.is_empty() {
        return 0.0;
    }
    s.pages.iter().map(|p| p.size.height + gap).sum::<f32>() - gap
}

/// Bottom edge (device px from the document top) of band page `k`.
fn page_bottom_px(s: &LayoutSnapshot, k: usize, gap: f32) -> f32 {
    s.pages
        .iter()
        .take(k + 1)
        .map(|p| p.size.height + gap)
        .sum::<f32>()
        - gap
}

/// The top-level body block an engine path starts in.
fn top_block_of(path: &EngineBlockPath) -> Option<u32> {
    match path.steps.first() {
        Some(engine::PathStep::Block(i)) => Some(*i),
        _ => None,
    }
}

/// Pages of the band that are final: everything before the first
/// provisional page (issue #93 / #181 — a pending column balance or note
/// continuation that a longer band rewrites).
fn settled_pages(s: &LayoutSnapshot) -> usize {
    s.info
        .open_from_page
        .map_or(s.pages.len(), |o| o.min(s.pages.len()))
}

/// Pages of the band that are final AND complete: a culled band's last
/// page is the one the cull stopped in, so it holds only part of what a
/// longer band puts there.
fn complete_pages(s: &LayoutSnapshot) -> usize {
    if s.info.is_full_layout {
        s.pages.len()
    } else {
        settled_pages(s).min(s.pages.len().saturating_sub(1))
    }
}

/// The last band page holding (part of) top-level block `block`.
fn last_page_of_block(s: &LayoutSnapshot, block: u32) -> Option<usize> {
    s.page_paths
        .iter()
        .rposition(|paths| paths.iter().any(|p| top_block_of(p) == Some(block)))
}

/// Whether the band covers a caret inside top-level block `block`: the
/// block's last page plus [`NAV_PAGES_AHEAD`] page(s) below it are laid
/// and settled.
fn band_covers_block(s: &LayoutSnapshot, block: u32) -> bool {
    last_page_of_block(s, block).is_some_and(|k| k + NAV_PAGES_AHEAD < settled_pages(s))
}

impl Engine {
    /// Lazy navigation runs for the BODY story under a cached layout
    /// config. Header / footer / note / text-box stories lay out their
    /// own geometry (the page they live on is already painted).
    fn lazy_nav_active(&self) -> bool {
        self.layout_cfg.is_some() && matches!(self.active_story, StoryTarget::Body)
    }

    /// Grow the laid band until `covered` holds for it (or every block is
    /// laid). `first_target_pt` is the first round's cull target in layout
    /// pt at scale 1; it is floored at one `LAYOUT_BUFFER_PT` past both the
    /// band's end and the current target, so a round is never a no-op bump
    /// (the #65 retry was one whenever its A4 guess fell short of the
    /// existing target). Later rounds double the target. Returns whether
    /// `min_target_y` moved — the caller then repaints so the new pages
    /// reach the shell.
    ///
    /// After a round that ends in a FULL layout, `min_target_y` is lowered
    /// back to the real document height (never below where it started):
    /// it stays the honest high-water mark of how deep the session went,
    /// exactly as if the user had scrolled there, and the full snapshot
    /// keeps serving every target (`ensure_layout_snapshot` short-circuits
    /// on `is_full_layout`), so lowering it costs no rebuild.
    fn lazy_nav_expand_until(
        &mut self,
        first_target_pt: f32,
        covered: impl Fn(&LayoutSnapshot) -> bool,
    ) -> Result<bool, Box<Event>> {
        let scale = self.scale().max(f32::MIN_POSITIVE);
        let gap = render::scene::PAGE_GAP_PT * scale;
        let start = self.lazy_layout.min_target_y;
        let mut want = first_target_pt;
        let mut rounds = 0u32;
        loop {
            let target = Some(self.lazy_layout.min_target_y * scale);
            self.ensure_layout_snapshot(scale, false, target)?;
            let (done, full, band_pt) = {
                let snap = self.layout_snapshot.borrow();
                let s = snap
                    .as_ref()
                    .expect("ensure_layout_snapshot populated the memo");
                let full = s.info.is_full_layout;
                (full || covered(s), full, band_height_px(s, gap) / scale)
            };
            if done {
                if full && self.lazy_layout.min_target_y > band_pt.max(start) {
                    self.lazy_layout.min_target_y = band_pt.max(start);
                }
                return Ok(self.lazy_layout.min_target_y != start);
            }
            rounds += 1;
            if rounds > MAX_EXPAND_ROUNDS {
                /* Backstop: one full layout ends the loop. */
                want = FULL_LAYOUT_TARGET_PT;
            }
            let floor = band_pt.max(self.lazy_layout.min_target_y) + LAYOUT_BUFFER_PT;
            self.lazy_layout.min_target_y = want.max(floor);
            want = (2.0 * self.lazy_layout.min_target_y).min(FULL_LAYOUT_TARGET_PT);
        }
    }

    /// Issue #462 — make the band cover a caret at `pos`: its block's last
    /// page plus [`NAV_PAGES_AHEAD`] settled page(s) below it. The first
    /// round aims one `LAYOUT_BUFFER_PT` past the bottom of the caret's
    /// page, so a walk down the document re-expands about once per buffer
    /// of travel, not once per line.
    fn lazy_nav_reach_pos(&mut self, pos: &BridgeLogicalPos) -> Result<bool, Box<Event>> {
        if !self.lazy_nav_active() {
            return Ok(false);
        }
        let Some(BridgePathStep::Block { idx: block }) = pos.path.steps.first().cloned() else {
            return Ok(false);
        };
        let scale = self.scale().max(f32::MIN_POSITIVE);
        let gap = render::scene::PAGE_GAP_PT * scale;
        self.ensure_layout_snapshot(scale, false, Some(self.lazy_layout.min_target_y * scale))?;
        let first = {
            let snap = self.layout_snapshot.borrow();
            let s = snap
                .as_ref()
                .expect("ensure_layout_snapshot populated the memo");
            if s.info.is_full_layout || band_covers_block(s, block) {
                return Ok(false);
            }
            last_page_of_block(s, block).map_or(0.0, |k| page_bottom_px(s, k, gap) / scale)
                + LAYOUT_BUFFER_PT
        };
        self.lazy_nav_expand_until(first, |s| band_covers_block(s, block))
    }

    /// Issue #462 — `Ctrl+End`: lay out the whole document (one full
    /// layout; the paginator's own page-count / watchdog bounds apply).
    fn lazy_nav_reach_end(&mut self) -> Result<bool, Box<Event>> {
        if !self.lazy_nav_active() {
            return Ok(false);
        }
        self.lazy_nav_expand_until(FULL_LAYOUT_TARGET_PT, |_| false)
    }

    /// Issue #501 — scroll-driven growth through the same routine. A paint
    /// (`RequestPaint`, and `ExpandLayout`, which paints) asks for the band
    /// to reach `min_target_y` — the high-water mark `SetViewport` /
    /// `ExpandLayout` raise. The paginator's cull normally satisfies that
    /// in the one build the paint makes anyway; when it cannot (a running
    /// height that passes every target: a page or a line no finite target
    /// clears — a restored tree can hold one) the band did not grow and
    /// nothing said so. Here the band is checked first and, only when it
    /// falls short, grown by [`Self::lazy_nav_expand_until`] — its
    /// progress bound, full-layout fallback and degradation note apply
    /// unchanged. Returns whether `min_target_y` moved.
    pub(crate) fn lazy_expand_to_viewport(&mut self) -> Result<bool, Box<Event>> {
        if !self.lazy_nav_active() {
            return Ok(false);
        }
        let scale = self.scale().max(f32::MIN_POSITIVE);
        let gap = render::scene::PAGE_GAP_PT * scale;
        let want_pt = self.lazy_layout.min_target_y;
        /* The same build `render_document` makes next (composition
        included), so the common path costs a cache hit. */
        self.ensure_layout_snapshot(scale, true, Some(want_pt * scale))?;
        let reaches = |s: &LayoutSnapshot| band_height_px(s, gap) / scale >= want_pt;
        {
            let snap = self.layout_snapshot.borrow();
            let s = snap
                .as_ref()
                .expect("ensure_layout_snapshot populated the memo");
            if s.info.is_full_layout || reaches(s) {
                return Ok(false);
            }
        }
        self.lazy_nav_expand_until(want_pt, reaches)
    }

    /// Issue #65 — make the band reach body page `page_idx` (0-based). The
    /// first round estimates the missing pages from the TALLEST section's
    /// real page height (never an A4 constant), and the loop keeps growing
    /// until the page is laid and complete or the layout is full, so a
    /// taller-than-estimated page can no longer leave the click on the
    /// previous page.
    /// On return the layout snapshot is populated (the caller borrows it).
    pub(crate) fn lazy_nav_reach_page(&mut self, page_idx: u32) -> Result<bool, Box<Event>> {
        let want_pages = page_idx as usize + 1;
        let scale = self.scale().max(f32::MIN_POSITIVE);
        let gap = render::scene::PAGE_GAP_PT * scale;
        self.ensure_layout_snapshot(scale, false, Some(self.lazy_layout.min_target_y * scale))?;
        let page_pt = self.tallest_section_page_pt() + render::scene::PAGE_GAP_PT;
        let first = {
            let snap = self.layout_snapshot.borrow();
            let s = snap
                .as_ref()
                .expect("ensure_layout_snapshot populated the memo");
            if complete_pages(s) >= want_pages {
                return Ok(false);
            }
            /* At least one page: a clicked page that is the partial last
            one needs the band to run past it. */
            let missing = want_pages.saturating_sub(s.pages.len()).max(1);
            band_height_px(s, gap) / scale + missing as f32 * page_pt
        };
        /* The clicked page must be COMPLETE (not the page the cull
        stopped in), or a click low on it would resolve onto the last
        laid line above. */
        self.lazy_nav_expand_until(first, |s| complete_pages(s) >= want_pages)
    }

    /// The tallest page (layout pt) any section of the document uses.
    fn tallest_section_page_pt(&self) -> f32 {
        let tallest = self.with_cached_sections(|sections| {
            sections
                .iter()
                .map(|s| s.geometry.height.max(s.geometry.width))
                .fold(0.0f32, f32::max)
        });
        if tallest.is_finite() && tallest > 0.0 {
            tallest
        } else {
            engine::PageGeometry::a4().height
        }
    }

    /// Issue #462 — `Command::MoveCaret` over a band that reaches the
    /// move's target. Before the move, the band is grown to cover the
    /// caret's page and the settled page below it (`Ctrl+End`: the whole
    /// document), so a vertical walk always finds the next line
    /// (`PageDown` grows it further inside [`Self::page_step`]); after it,
    /// the same rule is applied to the NEW caret. When the band grew the
    /// document is repainted (the worker's `paint_geometry_seq` broadcast
    /// then carries the new pages to the shell) and, if it grew after the
    /// move, the reply is rebuilt over the grown band so the caret rect is
    /// exact. Shift-extension takes the same path (`extend`).
    pub(crate) fn do_move_caret_reaching(
        &mut self,
        direction: MoveDirection,
        extend: bool,
    ) -> Event {
        let start = self.lazy_layout.min_target_y;
        let pre = match direction {
            MoveDirection::DocEnd => self.lazy_nav_reach_end(),
            _ => match self.selection.as_ref().map(|s| s.caret.clone()) {
                Some(caret) => self.lazy_nav_reach_pos(&caret),
                None => Ok(false),
            },
        };
        if let Err(e) = pre {
            return *e;
        }
        let evt = self.do_move_caret(direction, extend);
        let grew_after = if matches!(evt, Event::SelectionChanged { .. }) {
            match self.selection.as_ref().map(|s| s.caret.clone()) {
                Some(caret) => match self.lazy_nav_reach_pos(&caret) {
                    Ok(g) => g,
                    Err(e) => return *e,
                },
                None => false,
            }
        } else {
            false
        };
        let grew = self.lazy_layout.min_target_y != start;
        if grew && let Err(e) = self.maybe_repaint_result() {
            return *e;
        }
        if grew_after {
            return self.selection_changed();
        }
        evt
    }

    /// Issue #462 — the `PageDown` / `PageUp` target: the line one
    /// viewport height (`LazyLayoutState::viewport_h`, the shell's last
    /// `SetViewport`) below / above the caret's line, snapped to the
    /// carried ideal-x column like `Down` / `Up`. `PageDown` first grows
    /// the band until its settled pages reach that depth, so the target
    /// line is laid before it is chosen. Past the document's end / start
    /// the last / first line is the target (Word parity).
    pub(crate) fn page_step(
        &mut self,
        sel: &SelectionState,
        down: bool,
    ) -> Result<(BridgeLogicalPos, Option<f32>), Box<Event>> {
        let scale = self.scale().max(f32::MIN_POSITIVE);
        let gap = render::scene::PAGE_GAP_PT * scale;
        let caret = &sel.caret;
        let on_caret_line = |l: &LineGeom| {
            l.path == caret.path && caret.offset >= l.start_byte && caret.offset <= l.end_byte
        };
        let (cur_y, cur_h, ideal) = {
            let geom = self.document_geometry()?;
            let Some(cur) = geom.iter().find(|l| on_caret_line(l)) else {
                return Ok((caret.clone(), sel.ideal_x));
            };
            let ideal = sel
                .ideal_x
                .unwrap_or_else(|| slot_x_for_byte(cur, caret.offset));
            (cur.y_top, cur.height, ideal)
        };
        let step = (self.lazy_layout.viewport_h * scale).max(cur_h);
        let target_y = if down { cur_y + step } else { cur_y - step };
        if down && self.lazy_nav_active() {
            /* Settled pages down to one line past the target depth. */
            let need = target_y + cur_h;
            self.lazy_nav_expand_until(need / scale + LAYOUT_BUFFER_PT, |s| {
                let complete = complete_pages(s);
                complete > 0 && page_bottom_px(s, complete - 1, gap) >= need
            })?;
        }
        let geom = self.document_geometry()?;
        let beyond = |l: &LineGeom| {
            if down {
                l.y_top > cur_y + 0.5
            } else {
                l.y_top + 0.5 < cur_y
            }
        };
        /* The line whose band holds the target depth, else the nearest by
        depth; ties (same-row table cells) by distance to the ideal x. */
        let dist = |l: &LineGeom| {
            if target_y >= l.y_top && target_y < l.y_top + l.height {
                0.0
            } else {
                (l.y_top - target_y)
                    .abs()
                    .min((l.y_top + l.height - target_y).abs())
            }
        };
        let best = geom.iter().filter(|l| beyond(l)).min_by(|a, b| {
            dist(a)
                .partial_cmp(&dist(b))
                .unwrap_or(core::cmp::Ordering::Equal)
                .then_with(|| {
                    x_distance_to_line(a, ideal)
                        .partial_cmp(&x_distance_to_line(b, ideal))
                        .unwrap_or(core::cmp::Ordering::Equal)
                })
        });
        let pos = match best {
            Some(line) => BridgeLogicalPos {
                path: line.path.clone(),
                offset: nearest_slot_by_x(&line.slots, ideal).map_or(line.start_byte, |s| s.byte),
            },
            None => caret.clone(),
        };
        Ok((pos, Some(ideal)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::test_engine_with_doc;

    /// `lines` one-line paragraphs ("Line N"), like the #462 report's
    /// 90-line plain-text document.
    fn lines_doc(lines: usize) -> DocumentTree {
        let mut d = DocumentTree::from_text("Line 1");
        for i in 2..=lines {
            d.blocks
                .push_back(engine::Block::Paragraph(engine::Paragraph {
                    text: format!("Line {i}"),
                    ..Default::default()
                }));
        }
        d
    }

    /// Wrapping prose, many pages long.
    fn prose(paragraphs: usize) -> DocumentTree {
        let mut d = DocumentTree::from_text("Opening paragraph of a long document.");
        for i in 1..paragraphs {
            d.blocks.push_back(engine::Block::Paragraph(engine::Paragraph {
                text: format!(
                    "Paragraph {i}: sphinx of black quartz, judge my vow; pack my box with five dozen liquor jugs."
                ),
                ..Default::default()
            }));
        }
        d
    }

    /// A small viewport keeps the lazy runway at its floor, so the band is
    /// as short as the shell can make it.
    fn small_band(engine: &mut Engine) {
        engine.lazy_layout = LazyLayoutState {
            viewport_y: 0.0,
            viewport_h: 100.0,
            min_target_y: INITIAL_COLD_OPEN_BUDGET_PT,
        };
        engine.invalidate_layout_snapshot();
    }

    fn caret_block(engine: &Engine) -> u32 {
        match engine
            .selection
            .as_ref()
            .expect("selection")
            .caret
            .path
            .steps
            .first()
        {
            Some(BridgePathStep::Block { idx }) => *idx,
            other => panic!("caret not in a body block: {other:?}"),
        }
    }

    fn snapshot_pages(engine: &Engine) -> (usize, bool) {
        let snap = engine.layout_snapshot.borrow();
        let s = snap.as_ref().expect("snapshot");
        (s.pages.len(), s.info.is_full_layout)
    }

    fn move_via_dispatch(engine: &mut Engine, direction: MoveDirection, extend: bool) -> Event {
        engine.do_move_caret_reaching(direction, extend)
    }

    /// The document's line pitch in points, measured through the engine:
    /// the caret-rect distance between two consecutive `Down` presses on a
    /// short throwaway document. The pitch is font-derived (issue #475),
    /// so a test that counts lines per viewport reads it instead of
    /// hard-coding a number.
    fn measured_line_pitch_pt() -> f32 {
        let mut engine = test_engine_with_doc(lines_doc(4));
        let mut ys = Vec::new();
        for _ in 0..3 {
            let Event::SelectionChanged { caret, .. } =
                move_via_dispatch(&mut engine, MoveDirection::Down, false)
            else {
                panic!("Down answers SelectionChanged");
            };
            ys.push(caret.y);
        }
        (ys[2] - ys[1]) / engine.scale()
    }

    /// Acceptance (#462): a run of ArrowDown never stops at the band's
    /// edge — 600 steps on a 900-line document land on line 601, and the
    /// band grew synchronously (no `ExpandLayout`, no viewport change).
    #[test]
    fn arrow_down_run_crosses_the_lazy_band_edge() {
        let mut engine = test_engine_with_doc(lines_doc(900));
        small_band(&mut engine);
        engine
            .ensure_layout_snapshot(
                engine.scale(),
                false,
                Some(INITIAL_COLD_OPEN_BUDGET_PT * engine.scale()),
            )
            .expect("cold band");
        let (cold_pages, cold_full) = snapshot_pages(&engine);
        assert!(
            !cold_full,
            "the cold band must be culled for this test to mean anything"
        );
        for i in 0..600 {
            let evt = move_via_dispatch(&mut engine, MoveDirection::Down, false);
            assert!(
                matches!(evt, Event::SelectionChanged { .. }),
                "step {i}: {evt:?}"
            );
        }
        assert_eq!(
            caret_block(&engine),
            600,
            "600 × Down from line 1 lands on line 601"
        );
        let (pages, _) = snapshot_pages(&engine);
        assert!(
            pages > cold_pages,
            "the band grew past the cold band ({cold_pages} → {pages})"
        );
        let snap = engine.layout_snapshot.borrow();
        assert!(
            snap.as_ref()
                .expect("snapshot")
                .info
                .degradations
                .is_empty(),
            "every growth verified as a prefix: {:?}",
            snap.as_ref().expect("snapshot").info.degradations
        );
    }

    /// The pre-#462 engine parks the caret on the band's last line: the
    /// bare `do_move_caret` (no reach) stops short on the same document.
    #[test]
    fn bare_move_without_reach_stops_at_the_band_edge() {
        let mut engine = test_engine_with_doc(lines_doc(900));
        small_band(&mut engine);
        for _ in 0..600 {
            engine.do_move_caret(MoveDirection::Down, false);
        }
        assert!(caret_block(&engine) < 600, "the bare walk is band-bounded");
    }

    /// The reply of a move that crossed the band edge carries a caret rect
    /// on the caret's real line (not the page-margin fallback).
    #[test]
    fn reply_caret_rect_is_exact_after_growth() {
        let mut engine = test_engine_with_doc(lines_doc(400));
        small_band(&mut engine);
        let mut last_y = -1.0f32;
        for _ in 0..300 {
            let evt = move_via_dispatch(&mut engine, MoveDirection::Down, false);
            let Event::SelectionChanged { caret, .. } = evt else {
                panic!("{evt:?}");
            };
            assert!(
                caret.y > last_y,
                "every Down moves the caret rect down ({} → {})",
                last_y,
                caret.y
            );
            last_y = caret.y;
        }
    }

    /// Shift+Down extends across the edge too: the anchor stays put.
    #[test]
    fn shift_arrow_down_extends_across_the_band_edge() {
        let mut engine = test_engine_with_doc(lines_doc(900));
        small_band(&mut engine);
        for _ in 0..500 {
            move_via_dispatch(&mut engine, MoveDirection::Down, true);
        }
        let sel = engine.selection.clone().expect("selection");
        assert_eq!(sel.anchor, bpos_top(0, 0));
        assert_eq!(caret_block(&engine), 500);
    }

    /// Ctrl+End lays out the whole document and lands on the last line
    /// with an exact caret rect inside the last page; Ctrl+Home returns.
    /// `min_target_y` ends at the real document height (an honest
    /// high-water mark), and later geometry queries reuse the full band.
    #[test]
    fn ctrl_end_reaches_the_last_page_and_ctrl_home_returns() {
        let mut engine = test_engine_with_doc(prose(1200));
        small_band(&mut engine);
        let evt = move_via_dispatch(&mut engine, MoveDirection::DocEnd, false);
        let Event::SelectionChanged { caret, .. } = evt else {
            panic!("{evt:?}");
        };
        assert_eq!(caret_block(&engine), 1199);
        let scale = engine.scale();
        let gap = render::scene::PAGE_GAP_PT * scale;
        let (last_top, band_pt) = {
            let snap = engine.layout_snapshot.borrow();
            let s = snap.as_ref().expect("snapshot");
            assert!(s.info.is_full_layout, "Ctrl+End lays out every block");
            assert!(
                s.pages.len() > 20,
                "a long document: {} pages",
                s.pages.len()
            );
            let last = s.pages.len() - 1;
            let top = if last == 0 {
                0.0
            } else {
                page_bottom_px(s, last - 1, gap) + gap
            };
            (top, band_height_px(s, gap) / scale)
        };
        assert!(
            caret.y >= last_top,
            "caret rect y {} lies on the last page (top {last_top})",
            caret.y
        );
        assert!(
            (engine.lazy_layout.min_target_y - band_pt).abs() < 1.0,
            "min_target_y settles at the document height ({} vs {band_pt})",
            engine.lazy_layout.min_target_y
        );
        let evt = move_via_dispatch(&mut engine, MoveDirection::DocHome, true);
        let Event::SelectionChanged { caret, .. } = evt else {
            panic!("{evt:?}");
        };
        assert_eq!(caret_block(&engine), 0);
        assert!(caret.y < last_top);
    }

    /// `PageDown` moves one viewport height per press, crossing the band
    /// edge synchronously; `PageUp` comes back the same way; the ends of
    /// the document clamp to the last / first line.
    #[test]
    fn page_down_and_up_step_one_viewport_through_the_band() {
        let mut engine = test_engine_with_doc(lines_doc(900));
        small_band(&mut engine);
        /* A 520 pt viewport: `520 / pitch` lines per press (the pitch is
        font-derived since #475), less what a page break spends on margins
        + the page gap. */
        engine.lazy_layout.viewport_h = 520.0;
        let pitch = measured_line_pitch_pt();
        assert!((10.0..=40.0).contains(&pitch), "a plausible pitch: {pitch}");
        let per_press = (520.0 / pitch).ceil() as u32 + 1;
        let mut blocks = vec![caret_block(&engine)];
        for _ in 0..30 {
            let evt = move_via_dispatch(&mut engine, MoveDirection::PageDown, false);
            assert!(matches!(evt, Event::SelectionChanged { .. }), "{evt:?}");
            blocks.push(caret_block(&engine));
        }
        for w in blocks.windows(2) {
            let d = w[1] - w[0];
            assert!(
                (8..=per_press).contains(&d),
                "a press moves ~one viewport ({per_press} lines at {pitch} pt): {blocks:?}"
            );
        }
        assert!(
            caret_block(&engine) >= 300,
            "30 presses go deep: {blocks:?}"
        );
        let deep = caret_block(&engine);
        for _ in 0..5 {
            move_via_dispatch(&mut engine, MoveDirection::PageUp, true);
        }
        let back = caret_block(&engine);
        assert!(
            (40..=5 * per_press).contains(&(deep - back)),
            "{deep} → {back} ({per_press} lines per press)"
        );
        assert_eq!(
            engine.selection.as_ref().unwrap().anchor.path,
            bpos_top(deep, 0).path,
            "Shift+PageUp extends"
        );
        for _ in 0..60 {
            move_via_dispatch(&mut engine, MoveDirection::PageDown, false);
        }
        assert_eq!(
            caret_block(&engine),
            899,
            "PageDown past the end lands on the last line"
        );
        for _ in 0..80 {
            move_via_dispatch(&mut engine, MoveDirection::PageUp, false);
        }
        assert_eq!(
            caret_block(&engine),
            0,
            "PageUp past the start lands on the first line"
        );
    }

    /// ISO A3 portrait with Word's stock 1-inch margins.
    fn a3() -> engine::PageGeometry {
        engine::PageGeometry::from_twips(16838, 23811, 1440, 1440, 1440, 1440, 720, 720)
    }

    /// US Legal portrait.
    fn legal() -> engine::PageGeometry {
        engine::PageGeometry::from_twips(12240, 20160, 1440, 1440, 1440, 1440, 720, 720)
    }

    fn tall_page_engine(geometry: engine::PageGeometry, paragraphs: usize) -> Engine {
        let mut doc = prose(paragraphs);
        doc.body_section.geometry = geometry;
        let mut engine = test_engine_with_doc(doc);
        small_band(&mut engine);
        engine
    }

    /// The top-level blocks the full layout puts on page `idx`.
    fn blocks_on_page(engine: &Engine, idx: usize) -> Vec<u32> {
        let (_, _, paths, _) = engine
            .build_pages(engine.scale(), false, None)
            .expect("reference layout");
        paths[idx].iter().filter_map(top_block_of).collect()
    }

    /// Acceptance (#65): a click on a page past the stale band of a
    /// taller-than-A4 document resolves on THAT page, never the last laid
    /// one — A3 and US Legal.
    #[test]
    fn hit_test_beyond_band_lands_on_the_clicked_tall_page() {
        for (name, geometry, paragraphs, page_idx) in
            [("A3", a3(), 900usize, 7u32), ("Legal", legal(), 900, 11)]
        {
            let mut engine = tall_page_engine(geometry, paragraphs);
            let scale = engine.scale();
            engine
                .ensure_layout_snapshot(scale, false, Some(engine.lazy_layout.min_target_y * scale))
                .expect("cold band");
            let (cold_pages, cold_full) = snapshot_pages(&engine);
            assert!(
                !cold_full && cold_pages <= page_idx as usize,
                "{name}: page {page_idx} is beyond the band ({cold_pages} laid)"
            );
            let expected = blocks_on_page(&engine, page_idx as usize);
            assert!(
                !expected.is_empty(),
                "{name}: page {page_idx} exists in the full layout"
            );
            /* A point a little below the top margin of the page. */
            let at = BridgePoint {
                x: geometry.margin_left * scale + 20.0,
                y: (geometry.margin_top + 30.0) * scale,
            };
            let evt = engine.do_hit_test_in_page(page_idx, at);
            let Event::HitResult { pos } = evt else {
                panic!("{name}: {evt:?}");
            };
            let Some(BridgePathStep::Block { idx }) = pos.path.steps.first().cloned() else {
                panic!("{name}: hit outside the body: {pos:?}");
            };
            assert!(
                expected.contains(&idx),
                "{name}: the hit lands on block {idx}, not on page {page_idx}'s blocks {expected:?}"
            );
            let (pages, _) = snapshot_pages(&engine);
            assert!(
                pages > page_idx as usize,
                "{name}: the band grew to the clicked page"
            );
        }
    }

    /// The first-round estimate uses the section's real page height.
    #[test]
    fn tallest_section_page_reads_the_real_geometry() {
        let engine = tall_page_engine(a3(), 4);
        assert!((engine.tallest_section_page_pt() - a3().height).abs() < 0.01);
        let engine = test_engine_with_doc(lines_doc(4));
        assert!(
            (engine.tallest_section_page_pt() - engine::PageGeometry::a4().height).abs() < 0.01
        );
    }

    /// Manual timing harness (#462 acceptance: a 500-page document must
    /// still answer Ctrl+End). Run with:
    /// `cargo test -p engine-wasm --release --lib time_ctrl_end_500p -- --ignored --nocapture`
    #[test]
    #[ignore = "manual timing harness, not a correctness gate"]
    fn time_ctrl_end_500p() {
        use std::time::Instant;
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/perf/500p.docx");
        let bytes = std::fs::read(path).expect("read 500p.docx fixture");
        let archive = format_docx::read_docx(&bytes).expect("parse 500p.docx");
        let blocks = archive.document.blocks.len();
        {
            /* Reference: one raw full layout on a fresh engine (cold caches). */
            let fresh = test_engine_with_doc(archive.document.clone());
            let t = Instant::now();
            let (pages, ..) = fresh.build_pages(fresh.scale(), false, None).expect("full");
            eprintln!(
                "raw full build_pages: {:?}  ({} pages)",
                t.elapsed(),
                pages.len()
            );
        }
        let mut engine = test_engine_with_doc(archive.document);
        let scale = engine.scale();
        let t = Instant::now();
        engine
            .ensure_layout_snapshot(scale, false, Some(engine.lazy_layout.min_target_y * scale))
            .expect("cold band");
        eprintln!(
            "cold band:            {:?}  ({} pages)",
            t.elapsed(),
            snapshot_pages(&engine).0
        );
        let t = Instant::now();
        let evt = engine.do_move_caret_reaching(MoveDirection::DocEnd, false);
        let ctrl_end = t.elapsed();
        assert!(matches!(evt, Event::SelectionChanged { .. }), "{evt:?}");
        let (pages, full) = snapshot_pages(&engine);
        eprintln!(
            "Ctrl+End:             {ctrl_end:?}  ({pages} pages, full={full}, {blocks} blocks)"
        );
        assert!(full);
        assert_eq!(caret_block(&engine) as usize, blocks - 1);
        let t = Instant::now();
        engine.do_move_caret_reaching(MoveDirection::Up, false);
        eprintln!("ArrowUp after it:     {:?}", t.elapsed());
        let t = Instant::now();
        engine.do_move_caret_reaching(MoveDirection::DocHome, false);
        eprintln!("Ctrl+Home after it:   {:?}", t.elapsed());
    }
}
