//! Issue #471 - `MergeCells` and the merged regions that already exist.
//!
//! A rectangle is mergeable iff every merged region (a `gridSpan` cell, a
//! `vMerge` run, or both) it TOUCHES lies entirely inside it - those
//! regions are absorbed. A region it only partly covers is refused as
//! `TableError::CrossesMerge` -> `ErrorKind::UnsupportedHere`, naming the
//! region, with the document, selection and undo depth untouched. The
//! columns of the rectangle are LOGICAL GRID columns. After any accepted
//! merge every row's span sum still equals the grid width.
//!
//! Gated on `fuzz-native` like the other bridge-level tests that drive real
//! `Command`s: they use `Engine::apply_sync` and the fuzz fingerprint.

use super::*;
use bridge::{BlockPath as WirePath, ErrorKind};
use engine::VMergeRole;

fn engine_with(doc: DocumentTree) -> Engine {
    let mut e = Engine::new_headless(doc);
    e.selection = Some(SelectionState {
        anchor: bpos_top(1, 0),
        caret: bpos_top(1, 0),
        ideal_x: None,
        kind: SelectionKind::Linear,
    });
    e
}

/// `[Table rows x cols, Paragraph "tail"]`.
fn table_engine(rows: u32, cols: u32) -> Engine {
    engine_with(DocumentTree::from_text("tail").insert_table(EngineBlockPath::top(0), rows, cols))
}

fn merge(e: &mut Engine, (r0, c0): (u32, u32), (r1, c1): (u32, u32)) -> Event {
    e.apply_sync(Command::MergeCells {
        table_path: WirePath::top(0),
        from_row: r0,
        from_col: c0,
        to_row: r1,
        to_col: c1,
    })
}

fn table(e: &Engine) -> engine::Table {
    e.undo.current().blocks[0].as_table().unwrap().clone()
}

/// The spans of each row, e.g. `[[2, 1], [1, 1, 1]]`.
fn spans(t: &engine::Table) -> Vec<Vec<usize>> {
    t.rows
        .iter()
        .map(|r| {
            r.cells
                .iter()
                .map(|c| c.props.grid_span.max(1) as usize)
                .collect()
        })
        .collect()
}

fn assert_square(e: &Engine, label: &str) {
    let t = table(e);
    let width = t.grid.len();
    for (r, row) in t.rows.iter().enumerate() {
        let sum: usize = row
            .cells
            .iter()
            .map(|c| c.props.grid_span.max(1) as usize)
            .sum();
        assert_eq!(
            sum, width,
            "{label}: row {r} spans sum to {sum}, grid is {width}"
        );
    }
    assert_eq!(e.ragged_table_rows_for_fuzzing(), 0, "{label}");
    /* Every `vMerge` continue cell hangs under a run member that starts at
    the same grid column with the same span - a cut run (a restart merged
    sideways, a continue left behind) breaks exactly this. */
    let start_and_span = |row: &engine::TableRow, i: usize| {
        let start: usize = row.cells[..i]
            .iter()
            .map(|c| c.props.grid_span.max(1) as usize)
            .sum();
        (start, row.cells[i].props.grid_span.max(1) as usize)
    };
    for (r, row) in t.rows.iter().enumerate() {
        for (i, cell) in row.cells.iter().enumerate() {
            if cell.props.v_merge != VMergeRole::Continue {
                continue;
            }
            let (start, span) = start_and_span(row, i);
            let above = r.checked_sub(1).and_then(|p| {
                let prow = &t.rows[p];
                (0..prow.cells.len())
                    .find(|&k| start_and_span(prow, k) == (start, span))
                    .map(|k| prow.cells[k].props.v_merge)
            });
            assert!(
                matches!(above, Some(VMergeRole::Restart | VMergeRole::Continue)),
                "{label}: the continue cell at row {r}, grid column {start} (span {span}) has no \
                 run member above it"
            );
        }
    }
}

fn refusal(evt: &Event) -> (&Option<ErrorKind>, &str) {
    match evt {
        Event::Error { kind, message, .. } => (kind, message),
        other => panic!("expected a refusal, got {other:?}"),
    }
}

/// Assert `evt` is the #471 refusal and that nothing moved.
fn assert_crossing_refused(e: &mut Engine, evt: &Event, before: [u64; 5], region: &str) {
    let (kind, message) = refusal(evt);
    assert_eq!(*kind, Some(ErrorKind::UnsupportedHere), "{message}");
    assert!(message.contains("MergeCells"), "{message}");
    assert!(message.contains(region), "{message} should name {region}");
    assert_eq!(
        e.state_fingerprint_for_fuzzing(),
        before,
        "a refused merge changes nothing"
    );
}

/// The issue's repro: a horizontal merge, then a rectangle that holds the
/// merged cell's second grid column but not its first.
#[test]
fn rectangle_crossing_a_horizontal_merge_is_refused() {
    let mut e = table_engine(3, 3);
    assert!(matches!(
        merge(&mut e, (0, 0), (0, 1)),
        Event::SelectionChanged { .. }
    ));
    assert_eq!(
        spans(&table(&e)),
        vec![vec![2, 1], vec![1, 1, 1], vec![1, 1, 1]]
    );
    let before = e.state_fingerprint_for_fuzzing();
    let depth = e.undo_depth();

    let evt = merge(&mut e, (0, 1), (1, 2));
    assert_crossing_refused(&mut e, &evt, before, "rows 0-0, columns 0-1");
    assert_eq!(e.undo_depth(), depth, "no undo step for a refusal");
    assert_square(&e, "after the refusal");

    /* The same rectangle with the corners given the other way round, and
    one that cuts the merged cell from its LEFT edge instead. */
    let evt = merge(&mut e, (1, 2), (0, 1));
    assert_crossing_refused(&mut e, &evt, before, "rows 0-0, columns 0-1");
    let evt = merge(&mut e, (0, 0), (1, 0));
    assert_crossing_refused(&mut e, &evt, before, "rows 0-0, columns 0-1");
}

/// A horizontal region partly inside a rectangle that starts below it.
#[test]
fn rectangle_crossing_a_merge_in_a_lower_row_is_refused() {
    let mut e = table_engine(3, 3);
    assert!(matches!(
        merge(&mut e, (2, 1), (2, 2)),
        Event::SelectionChanged { .. }
    ));
    let before = e.state_fingerprint_for_fuzzing();
    /* Rows 0..=2, columns 0..=1: the merged (2, 1..=2) is cut in half. */
    let evt = merge(&mut e, (0, 0), (2, 1));
    assert_crossing_refused(&mut e, &evt, before, "rows 2-2, columns 1-2");
    assert_square(&e, "after the refusal");
}

#[test]
fn rectangle_crossing_a_vmerge_vertically_is_refused() {
    let mut e = table_engine(3, 3);
    /* A vertical run down column 0, rows 1-2. */
    assert!(matches!(
        merge(&mut e, (1, 0), (2, 0)),
        Event::SelectionChanged { .. }
    ));
    let t = table(&e);
    assert_eq!(t.rows[1].cells[0].props.v_merge, VMergeRole::Restart);
    assert_eq!(t.rows[2].cells[0].props.v_merge, VMergeRole::Continue);
    let before = e.state_fingerprint_for_fuzzing();
    let depth = e.undo_depth();

    /* From above: rows 0-1 take the run's first cell but not its second. */
    let evt = merge(&mut e, (0, 0), (1, 1));
    assert_crossing_refused(&mut e, &evt, before, "rows 1-2, columns 0-0");
    /* From below: the run's continue cell is in, its restart is not. */
    let evt = merge(&mut e, (2, 0), (2, 1));
    assert_crossing_refused(&mut e, &evt, before, "rows 1-2, columns 0-0");
    assert_eq!(e.undo_depth(), depth, "no undo step for a refusal");
    assert_square(&e, "after the refusals");

    /* Through the middle of a taller run: rows 1-2 of a four-row run. */
    let mut tall = table_engine(4, 2);
    assert!(matches!(
        merge(&mut tall, (0, 0), (3, 0)),
        Event::SelectionChanged { .. }
    ));
    let before = tall.state_fingerprint_for_fuzzing();
    let evt = merge(&mut tall, (1, 0), (2, 1));
    assert_crossing_refused(&mut tall, &evt, before, "rows 0-3, columns 0-0");
}

/// A rectangle that contains every merged region it touches is fine: the
/// regions are absorbed into the new one.
#[test]
fn rectangle_enclosing_merges_absorbs_them() {
    let mut e = table_engine(3, 3);
    /* A: (0, 0..=1) horizontal. B: (1..=2, 2) vertical. */
    assert!(matches!(
        merge(&mut e, (0, 0), (0, 1)),
        Event::SelectionChanged { .. }
    ));
    assert!(matches!(
        merge(&mut e, (1, 2), (2, 2)),
        Event::SelectionChanged { .. }
    ));
    assert_square(&e, "two merges");
    let depth = e.undo_depth();

    let evt = merge(&mut e, (0, 0), (2, 2));
    assert!(matches!(evt, Event::SelectionChanged { .. }), "{evt:?}");
    assert_eq!(e.undo_depth(), depth + 1);
    let t = table(&e);
    assert_eq!(spans(&t), vec![vec![3], vec![3], vec![3]]);
    assert_eq!(t.rows[0].cells[0].props.v_merge, VMergeRole::Restart);
    assert_eq!(t.rows[1].cells[0].props.v_merge, VMergeRole::Continue);
    assert_eq!(t.rows[2].cells[0].props.v_merge, VMergeRole::Continue);
    assert_eq!(t.grid.len(), 3, "the grid itself is not touched");
    assert_square(&e, "after absorbing");

    /* Undo puts both earlier merges back. */
    e.apply_sync(Command::Undo);
    assert_eq!(
        spans(&table(&e)),
        vec![vec![2, 1], vec![1, 1, 1], vec![1, 1, 1]]
    );
}

/// A rectangle that swallows only SOME of the merges, and all of the ones
/// it touches: the cells outside it stay as they were.
#[test]
fn rectangle_enclosing_one_merge_leaves_the_rest_alone() {
    let mut e = table_engine(3, 4);
    assert!(matches!(
        merge(&mut e, (0, 1), (0, 2)),
        Event::SelectionChanged { .. }
    ));
    assert!(matches!(
        merge(&mut e, (2, 0), (2, 1)),
        Event::SelectionChanged { .. }
    ));
    assert_eq!(
        spans(&table(&e)),
        vec![vec![1, 2, 1], vec![1, 1, 1, 1], vec![2, 1, 1]]
    );
    /* Rows 0-1, grid columns 1-3: holds row 0's (1..=2) cell whole, and
    touches nothing of row 2's merge. */
    let evt = merge(&mut e, (0, 1), (1, 3));
    assert!(matches!(evt, Event::SelectionChanged { .. }), "{evt:?}");
    assert_eq!(
        spans(&table(&e)),
        vec![vec![1, 3], vec![1, 3], vec![2, 1, 1]]
    );
    assert_square(&e, "after");
}

#[test]
fn rectangle_disjoint_from_merges_is_allowed() {
    let mut e = table_engine(3, 3);
    assert!(matches!(
        merge(&mut e, (0, 0), (0, 1)),
        Event::SelectionChanged { .. }
    ));
    /* The row below the merge, edge to edge: touches nothing merged. */
    let evt = merge(&mut e, (1, 0), (1, 2));
    assert!(matches!(evt, Event::SelectionChanged { .. }), "{evt:?}");
    /* And a row further down that shares columns with the first merge
    without sharing a row. */
    let evt = merge(&mut e, (2, 0), (2, 1));
    assert!(matches!(evt, Event::SelectionChanged { .. }), "{evt:?}");
    assert_eq!(spans(&table(&e)), vec![vec![2, 1], vec![3], vec![2, 1]]);
    assert_square(&e, "after");
}

/// The grid is in logical columns: once a row holds a `gridSpan` cell its
/// cell indices stop matching its grid columns, and a rectangle must still
/// mean the same columns in every row.
#[test]
fn columns_are_grid_columns_not_cell_indices() {
    let mut e = table_engine(2, 4);
    assert!(matches!(
        merge(&mut e, (0, 0), (0, 1)),
        Event::SelectionChanged { .. }
    ));
    /* Row 0 is now [A(2), C, D]; row 1 is [a, b, c, d]. Grid columns
    2..=3 over both rows: row 0's C and D, row 1's c and d. */
    let evt = merge(&mut e, (0, 2), (1, 3));
    assert!(matches!(evt, Event::SelectionChanged { .. }), "{evt:?}");
    assert_eq!(spans(&table(&e)), vec![vec![2, 2], vec![1, 1, 2]]);
    assert_square(&e, "after");

    /* A column past the grid is out of range, not a cell-index miss. */
    let evt = merge(&mut e, (0, 0), (0, 4));
    assert_eq!(*refusal(&evt).0, Some(ErrorKind::OutOfRange));
}

/// The cell content of every absorbed cell lands in the owner in row-major
/// order, and a vMerge placeholder adds no empty paragraph.
#[test]
fn absorbed_content_keeps_row_major_order_without_placeholder_blanks() {
    let mut doc = DocumentTree::from_text("tail").insert_table(EngineBlockPath::top(0), 2, 3);
    for (i, text) in ["a", "b", "c", "d", "e", "f"].iter().enumerate() {
        let path = EngineBlockPath {
            steps: vec![
                engine::PathStep::Block(0),
                engine::PathStep::Cell {
                    row: (i / 3) as u32,
                    col: (i % 3) as u32,
                },
                engine::PathStep::Block(0),
            ],
        };
        doc = doc.insert_text(engine::LogicalPos::new(path, 0), text);
    }
    let mut e = engine_with(doc);
    /* Vertically merge the last column: c's cell takes f, row 1's last
    cell becomes a blank continue placeholder. */
    assert!(matches!(
        merge(&mut e, (0, 2), (1, 2)),
        Event::SelectionChanged { .. }
    ));
    assert_eq!(
        table(&e).rows[1].cells[2].props.v_merge,
        VMergeRole::Continue
    );

    let evt = merge(&mut e, (0, 0), (1, 2));
    assert!(matches!(evt, Event::SelectionChanged { .. }), "{evt:?}");
    let t = table(&e);
    assert_eq!(spans(&t), vec![vec![3], vec![3]]);
    let owner: Vec<String> = t.rows[0].cells[0]
        .blocks
        .iter()
        .map(|b| b.as_paragraph().map(|p| p.text.clone()).unwrap_or_default())
        .collect();
    /* Row-major: the owner row's cells (a, b, then c's cell = c + the f it
    took), then row 1's (d, e); the placeholder under c is blank and
    contributes nothing - a blank paragraph per absorbed run would pile up. */
    assert_eq!(owner, vec!["a", "b", "c", "f", "d", "e"], "{owner:?}");
    assert_square(&e, "after");
}

/// Every rectangle of a table with merges is either refused or leaves
/// every row square; a refusal changes nothing. Exhaustive over a 4x4
/// grid - ~100 rectangles per starting shape.
#[test]
fn every_rectangle_is_refused_or_keeps_the_table_square() {
    /* A merge rectangle as its two corners, `(row, grid column)` each. */
    type Rect = ((u32, u32), (u32, u32));
    let shapes: [&[Rect]; 4] = [
        &[((0, 0), (0, 1))],
        &[((0, 0), (0, 1)), ((1, 2), (3, 2))],
        &[((1, 1), (2, 2)), ((0, 3), (1, 3))],
        &[((0, 1), (2, 1)), ((3, 0), (3, 3))],
    ];
    for (n, shape) in shapes.iter().enumerate() {
        for (r0, c0, r1, c1) in (0..4u32).flat_map(|a| {
            (0..4u32).flat_map(move |b| {
                (a..4u32).flat_map(move |c| (b..4u32).map(move |d| (a, b, c, d)))
            })
        }) {
            let mut e = table_engine(4, 4);
            for &(from, to) in shape.iter() {
                let evt = merge(&mut e, from, to);
                assert!(
                    matches!(evt, Event::SelectionChanged { .. }),
                    "shape {n}: {evt:?}"
                );
            }
            assert_square(&e, "shape");
            let before = e.state_fingerprint_for_fuzzing();
            let label = format!("shape {n}: rect ({r0},{c0})-({r1},{c1})");
            match merge(&mut e, (r0, c0), (r1, c1)) {
                Event::SelectionChanged { .. } => assert_square(&e, &label),
                Event::Error { kind, .. } => {
                    assert_eq!(kind, Some(ErrorKind::UnsupportedHere), "{label}");
                    assert_eq!(e.state_fingerprint_for_fuzzing(), before, "{label}");
                }
                other => panic!("{label}: {other:?}"),
            }
        }
    }
}

/// `SplitCell` is the inverse for an absorbing merge's owner too: the
/// table stays square through merge -> split.
#[test]
fn split_after_an_absorbing_merge_stays_square() {
    let mut e = table_engine(3, 3);
    assert!(matches!(
        merge(&mut e, (0, 0), (0, 1)),
        Event::SelectionChanged { .. }
    ));
    assert!(matches!(
        merge(&mut e, (0, 0), (1, 2)),
        Event::SelectionChanged { .. }
    ));
    let evt = e.apply_sync(Command::SplitCell {
        table_path: WirePath::top(0),
        row: 0,
        col: 0,
    });
    assert!(matches!(evt, Event::SelectionChanged { .. }), "{evt:?}");
    assert_square(&e, "after split");
}
