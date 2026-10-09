//! Issue #422 — the tiling bound shared by every painter that repeats a
//! small fill across a span: dotted / dashed / wavy underlines and dot /
//! hyphen tab leaders, in the display-list builder (`render::scene`) and
//! in its PDF twin (`format-pdf`).
//!
//! Those painters used to step `x += pitch` while `x < end`. The span is
//! the run's advance, which is untrusted: a run carrying an inline image
//! inserted at its "original" size of 640 730 085 px asked a wavy underline
//! for ~320 million fills — 46.5 GB of display list before the fuzz driver
//! was OOM-killed. An infinite end never terminates at all, and past 2^53
//! the step stops moving `x`. A visible page needs a few thousand tiles at
//! most, so a span that needs more than [`MAX_PATTERN_TILES`] (or has a
//! non-finite edge or pitch) is not tiled: the caller paints it as one
//! solid stroke instead — the decoration degrades, it never disappears.
//!
//! Under the cap the painters keep their exact accumulation (`x += pitch`
//! under the same loop condition), so every nominal span — every golden —
//! paints byte-identically; the cap only bounds the iteration count.

/// The most fills one patterned span may tile. A 22-inch page (the widest
/// Word accepts) at 4.5× zoom on a 3× display is ≈ 21 400 device px; at
/// the narrowest pitch any painter uses (2 px) that is 10 700 tiles.
pub const MAX_PATTERN_TILES: usize = 16_384;

/// An upper bound on the iterations a `x = lo; while x < hi { …; x += pitch }`
/// tiling loop runs, or `None` when the span must not be tiled (see the
/// module docs): a non-finite `lo` / `hi` / `pitch`, a non-positive
/// `pitch`, or more than [`MAX_PATTERN_TILES`] tiles. An empty span is
/// `Some(0)`. The bound carries two iterations of slack for the float
/// accumulation, so a loop that also checks its own condition every
/// iteration stops exactly where the unbounded loop would have.
pub fn pattern_tile_bound(lo: f64, hi: f64, pitch: f64) -> Option<usize> {
    if !(lo.is_finite() && hi.is_finite() && pitch.is_finite()) || pitch <= 0.0 {
        return None;
    }
    if hi <= lo {
        return Some(0);
    }
    let tiles = ((hi - lo) / pitch).ceil();
    if !tiles.is_finite() || tiles > MAX_PATTERN_TILES as f64 {
        return None;
    }
    Some(tiles as usize + 2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nominal_spans_get_a_bound_with_slack() {
        assert_eq!(pattern_tile_bound(0.0, 10.0, 2.0), Some(7));
        assert_eq!(pattern_tile_bound(10.0, 10.0, 2.0), Some(0));
        assert_eq!(pattern_tile_bound(10.0, 5.0, 2.0), Some(0));
        let n = pattern_tile_bound(0.0, 1584.0 * 13.5, 2.0).expect("a full page row tiles");
        assert!(n <= MAX_PATTERN_TILES + 2);
    }

    #[test]
    fn hostile_spans_are_not_tiled() {
        // The #422 reproducer: a 640 730 085 px run, 2 px pitch.
        assert_eq!(pattern_tile_bound(0.0, 640_730_085.0, 2.0), None);
        assert_eq!(pattern_tile_bound(0.0, f64::INFINITY, 2.0), None);
        assert_eq!(pattern_tile_bound(f64::NAN, 10.0, 2.0), None);
        assert_eq!(pattern_tile_bound(0.0, 10.0, 0.0), None);
        assert_eq!(pattern_tile_bound(0.0, 10.0, -1.0), None);
        assert_eq!(pattern_tile_bound(0.0, 10.0, f64::NAN), None);
        // Past 2^53 the accumulation stalls; the span is tiny but the
        // bound still caps the loop.
        let lo = 1e17;
        let n = pattern_tile_bound(lo, lo + 64.0, 2.0).expect("small span");
        assert!(n <= 40);
    }
}
