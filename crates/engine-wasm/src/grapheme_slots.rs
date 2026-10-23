//! Epic #555 row 2 — the caret never rests inside a grapheme cluster, and
//! every grapheme cluster is one caret step (the clean-room cursor memo
//! §1 "Normalisation" and §3 "Steps").
//!
//! The line geometry ([`build_line_run_geom`](super::build_line_run_geom))
//! emits one caret stop per SHAPED glyph, at the glyph's cluster byte. A
//! shaped cluster is not a grapheme cluster:
//!
//! * a ligature (`fi` in a Latin face, `تب` in Amiri) is one glyph over
//!   several grapheme clusters, so the caret skipped them all in one
//!   press and no click could land between them;
//! * a base and its combining mark shape as two glyphs that share one
//!   cluster byte, so the mark's glyph added a second stop for the SAME
//!   byte at the base's far edge — where the next cluster starts;
//! * a run boundary can fall inside a cluster (an emoji ZWJ sequence
//!   whose pieces resolve to different faces), and a stop there let a
//!   click or an arrow rest between the pieces.
//!
//! [`snap_slots_to_graphemes`] is the ONE place that turns shaping stops
//! into caret stops: per run it keeps one stop per cluster byte at the
//! cluster's LOGICAL-START edge, splits a cluster spanning several
//! grapheme clusters evenly across its advance (no ligature caret table is
//! read — the even split is what a font without `GDEF` ligature carets
//! gets in Word too), and drops every stop that is not a UAX #29 extended
//! grapheme cluster boundary of the paragraph text. Every geometry
//! consumer (hit-test, Left / Right, Up / Down, Home / End, selection
//! rectangles, the caret rect) reads the result through
//! `Engine::document_geometry`, so none of them can produce a position
//! inside a cluster. The logical steps at a line edge (`step_left` /
//! `step_right`) were already grapheme-based.

use super::{CaretSlot, DocumentTree, LineGeom, RunGeom, bridge_to_engine_path};
use std::collections::BTreeMap;
use unicode_segmentation::UnicodeSegmentation;

/// Rewrite the caret stops of every line in `geom` onto grapheme cluster
/// boundaries (see the module docs). `doc` is the tree the lines' paths
/// address (the body, or the active story). While the field-code view is
/// on, a paragraph holding fields is laid out from its derived code text,
/// not from `Paragraph::text`, so its lines are left alone — the code view
/// already confines those stops to field boundaries.
pub(super) fn snap_slots_to_graphemes(
    geom: &mut [LineGeom],
    doc: &DocumentTree,
    field_code_view: bool,
) {
    /* Boundaries of the paragraph the previous line belonged to: a
    paragraph's lines are consecutive in the walk. */
    let mut cached: Option<(super::BridgeBlockPath, Option<Vec<u32>>)> = None;
    for line in geom.iter_mut() {
        if !line.runs.iter().any(run_needs_snap) {
            continue;
        }
        if cached.as_ref().is_none_or(|(p, _)| *p != line.path) {
            let bounds = doc
                .paragraph_at_path(&bridge_to_engine_path(line.path.clone()))
                .filter(|p| !field_code_view || p.fields.is_empty())
                .map(|p| grapheme_boundaries(&p.text));
            cached = Some((line.path.clone(), bounds));
        }
        let Some((_, Some(bounds))) = cached.as_ref() else {
            continue;
        };
        let mut changed = false;
        for run in line.runs.iter_mut() {
            changed |= snap_run(run, bounds);
        }
        if changed {
            line.slots = line
                .runs
                .iter()
                .flat_map(|r| r.slots.iter().copied())
                .collect();
        }
    }
}

/// UAX #29 extended grapheme cluster boundaries of `text` (byte offsets,
/// ascending, `0` and `text.len()` included).
pub(super) fn grapheme_boundaries(text: &str) -> Vec<u32> {
    text.grapheme_indices(true)
        .map(|(i, _)| i as u32)
        .chain(std::iter::once(text.len() as u32))
        .collect()
}

/// Whether a run's stops can be anything but one per grapheme: some
/// logical cluster spans more than one byte (a multi-byte character, a
/// ligature, a base with its marks). An all-ASCII run of single-glyph
/// clusters — the common Latin case — never needs the text.
fn run_needs_snap(run: &RunGeom) -> bool {
    /* Fast path, no allocation: an LTR run of one-byte clusters lists
    0, 1, 2, … in order. */
    if run.slots.windows(2).all(|w| w[1].byte == w[0].byte + 1) {
        return false;
    }
    let mut bytes: Vec<u32> = run.slots.iter().map(|s| s.byte).collect();
    bytes.sort_unstable();
    bytes.dedup();
    bytes.windows(2).any(|w| w[1] - w[0] > 1)
}

/// Rebuild one run's stops; `false` when it is left as it was (nothing to
/// change, or stops that do not address `bounds`' text).
fn snap_run(run: &mut RunGeom, bounds: &[u32]) -> bool {
    let len = *bounds.last().unwrap_or(&0);
    let Some(end) = run.slots.iter().find(|s| s.byte == run.src_end).copied() else {
        return false;
    };
    if run.src_end > len || run.slots.iter().any(|s| s.byte > run.src_end) {
        return false;
    }
    let others: Vec<CaretSlot> = run
        .slots
        .iter()
        .filter(|s| s.byte != run.src_end)
        .copied()
        .collect();
    /* A run is unidirectional: its logical end is its visual RIGHT edge
    when it reads left-to-right, its LEFT edge when right-to-left. */
    let rtl =
        others.iter().any(|s| s.x > end.x + 0.01) && others.iter().all(|s| s.x >= end.x - 0.01);
    /* One stop per cluster byte, at the cluster's logical-start edge: the
    leftmost of its glyphs' stops in an LTR run, the rightmost in an RTL
    one (a mark glyph sharing the base's cluster byte reports the base's
    far edge). */
    let mut edges: BTreeMap<u32, f32> = BTreeMap::new();
    for s in &others {
        edges
            .entry(s.byte)
            .and_modify(|x| *x = if rtl { x.max(s.x) } else { x.min(s.x) })
            .or_insert(s.x);
    }
    let mut seq: Vec<(u32, f32)> = edges.into_iter().collect();
    seq.push((end.byte, end.x));
    let is_boundary = |b: u32| bounds.binary_search(&b).is_ok();
    let mut body: Vec<CaretSlot> = Vec::with_capacity(seq.len());
    for w in seq.windows(2) {
        let ((b0, x0), (b1, x1)) = (w[0], w[1]);
        if is_boundary(b0) {
            body.push(CaretSlot { x: x0, byte: b0 });
        }
        /* Grapheme boundaries strictly inside the cluster: split its
        advance evenly between the pieces. */
        let lo = bounds.partition_point(|&g| g <= b0);
        let hi = bounds.partition_point(|&g| g < b1);
        let inner = &bounds[lo..hi.max(lo)];
        let pieces = (inner.len() + 1) as f32;
        for (k, &g) in inner.iter().enumerate() {
            let t = (k + 1) as f32 / pieces;
            body.push(CaretSlot {
                x: x0 + (x1 - x0) * t,
                byte: g,
            });
        }
    }
    /* Keep the emission convention of `build_line_run_geom`: visual
    left-to-right, an RTL run's logical-end stop last. */
    if rtl {
        body.reverse();
    }
    if is_boundary(end.byte) {
        body.push(end);
    }
    let unchanged = body.len() == run.slots.len()
        && body
            .iter()
            .zip(&run.slots)
            .all(|(a, b)| a.byte == b.byte && a.x.to_bits() == b.x.to_bits());
    if unchanged {
        return false;
    }
    run.slots = body;
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(src_start: u32, src_end: u32, slots: &[(f32, u32)]) -> RunGeom {
        RunGeom {
            src_start,
            src_end,
            slots: slots
                .iter()
                .map(|&(x, byte)| CaretSlot { x, byte })
                .collect(),
        }
    }

    fn stops(r: &RunGeom) -> Vec<(f32, u32)> {
        r.slots.iter().map(|s| (s.x, s.byte)).collect()
    }

    /// A two-letter ligature over `fi` (one glyph, cluster 0) gets a stop
    /// halfway through its advance.
    #[test]
    fn a_ligature_is_split_evenly_between_its_graphemes() {
        let text = "fix";
        let mut r = run(0, 3, &[(0.0, 0), (20.0, 2), (30.0, 3)]);
        assert!(snap_run(&mut r, &grapheme_boundaries(text)));
        assert_eq!(stops(&r), vec![(0.0, 0), (10.0, 1), (20.0, 2), (30.0, 3)]);
    }

    /// An RTL ligature (Amiri's `تب`): the advance runs right-to-left, the
    /// logical-end stop stays last.
    #[test]
    fn an_rtl_ligature_is_split_right_to_left() {
        /* Two bytes per letter. */
        let text = "كتب";
        /* Visual order: [تب ligature][ك]; ك starts at x 30 (its right
        edge), the ligature (cluster 2) at x 20, the run ends at x 0. */
        let mut r = run(0, 6, &[(20.0, 2), (30.0, 0), (0.0, 6)]);
        assert!(snap_run(&mut r, &grapheme_boundaries(text)));
        assert_eq!(stops(&r), vec![(10.0, 4), (20.0, 2), (30.0, 0), (0.0, 6)]);
    }

    /// A base and its combining mark (two glyphs, one cluster byte): the
    /// mark's stop at the base's far edge is dropped, the base keeps its
    /// logical-start edge, and no stop sits between them.
    #[test]
    fn a_mark_glyph_adds_no_stop_inside_its_cluster() {
        /* `e` (1 byte) + U+0301 (2) + `t` (1). */
        let text = "e\u{301}t";
        /* LTR: base `e` at 0, the mark glyph (same cluster 0) reported at
        the base's right edge 10, `t` at 10, end at 20. */
        let mut r = run(0, 4, &[(0.0, 0), (10.0, 0), (10.0, 3), (20.0, 4)]);
        assert!(snap_run(&mut r, &grapheme_boundaries(text)));
        assert_eq!(stops(&r), vec![(0.0, 0), (10.0, 3), (20.0, 4)]);

        /* RTL: an Arabic letter + fatha then a letter. The mark glyph
        reports the base's LEFT edge (the cluster's logical end). */
        /* Kaf (2 bytes) + fatha (2) + teh (2). */
        let text = "كَت";
        /* Visual order: ت [0, 10] (stop at its right edge), the zero-width
        fatha glyph, ك [10, 20]. */
        let mut r = run(0, 6, &[(10.0, 4), (10.0, 0), (20.0, 0), (0.0, 6)]);
        assert!(snap_run(&mut r, &grapheme_boundaries(text)));
        assert_eq!(stops(&r), vec![(10.0, 4), (20.0, 0), (0.0, 6)]);
    }

    /// A run that starts inside a grapheme cluster (an emoji ZWJ sequence
    /// whose pieces went to different faces) offers no stop at that seam.
    #[test]
    fn a_run_boundary_inside_a_cluster_is_no_caret_stop() {
        let text = "a\u{1F469}\u{200D}\u{1F467}b";
        let bounds = grapheme_boundaries(text);
        assert_eq!(bounds, vec![0, 1, 12, 13]);
        /* Run 1 covers `a` + the first pictograph [0, 5); run 2 the ZWJ +
        second pictograph [5, 12); run 3 `b`. */
        let mut r1 = run(0, 5, &[(0.0, 0), (10.0, 1), (30.0, 5)]);
        let mut r2 = run(5, 12, &[(30.0, 5), (40.0, 8), (60.0, 12)]);
        assert!(snap_run(&mut r1, &bounds));
        assert!(snap_run(&mut r2, &bounds));
        assert_eq!(stops(&r1), vec![(0.0, 0), (10.0, 1)]);
        assert_eq!(stops(&r2), vec![(60.0, 12)]);
    }

    /// Single-byte clusters need no text and are never touched.
    #[test]
    fn plain_ascii_runs_are_left_alone() {
        let r = run(0, 3, &[(0.0, 0), (5.0, 1), (11.0, 2), (16.0, 3)]);
        assert!(!run_needs_snap(&r));
    }
}
