//! Line break opportunities via `icu_segmenter::LineSegmenter`.

use icu_segmenter::LineSegmenter;

/// Returns the byte offsets where a line break is allowed.
///
/// The first offset is always 0 (start of string) and the last is `text.len()`
/// (end of string); per UAX #14, these are "line break opportunities" the
/// caller can use as break candidates.
pub fn break_opportunities(text: &str) -> Vec<usize> {
    let segmenter = LineSegmenter::new_auto();
    segmenter.segment_str(text).collect()
}
