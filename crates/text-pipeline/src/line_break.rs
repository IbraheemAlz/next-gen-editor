//! Line break opportunities via `icu_segmenter::LineSegmenter`.

use icu_segmenter::{LineSegmenter, LineSegmenterBorrowed};

thread_local! {
    /// The segmenter is built once and reused across every paragraph rather
    /// than reconstructed per call (Backlog #13 — incremental relayout). One
    /// segmenter handles any number of strings.
    static SEGMENTER: LineSegmenterBorrowed<'static> =
        LineSegmenter::new_auto(Default::default());
}

/// Returns the byte offsets where a line break is allowed.
///
/// The first offset is always 0 (start of string) and the last is `text.len()`
/// (end of string); per UAX #14, these are "line break opportunities" the
/// caller can use as break candidates.
pub fn break_opportunities(text: &str) -> Vec<usize> {
    SEGMENTER.with(|segmenter| segmenter.segment_str(text).collect())
}
