//! Issue #551 — cell selection (`SelectCellAt`, Tab / Shift+Tab) over a
//! cell that starts with, ends with, or only holds a NESTED table.
//!
//! `cell_content_span` used to emit `Block(0)` / `Block(n - 1)` of the
//! hit cell as the selection's endpoints. A cell may start or end with a
//! table (legal in `.docx`), so an endpoint addressed a table block,
//! `clamp_pos` rewrote it, and the live selection escaped its document
//! (the `snapshot_decode` invariant, 17 nightly duplicates). The span
//! now runs from the first PARAGRAPH in document order inside the cell
//! to the end of the last one, through the engine's one deep descent
//! (`DocumentTree::cell_paragraph_bounds`); Tab names the cell it
//! entered, and the innermost table owns both gestures
//! (`onlyoffice-cursor-memo.md` §7).

use super::*;

fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    use std::task::{Context, Poll, Waker};
    let mut cx = Context::from_waker(Waker::noop());
    let mut fut = Box::pin(fut);
    match fut.as_mut().poll(&mut cx) {
        Poll::Ready(v) => v,
        Poll::Pending => panic!("Engine::apply suspended in a native test"),
    }
}

fn apply(e: &mut Engine, cmd: Command) -> Event {
    block_on(e.apply(cmd))
}

fn para(text: &str) -> engine::Block {
    engine::Block::Paragraph(engine::Paragraph {
        text: text.to_string(),
        ..Default::default()
    })
}

fn cell(blocks: Vec<engine::Block>) -> engine::TableCell {
    engine::TableCell {
        blocks,
        ..Default::default()
    }
}

fn table(rows: Vec<Vec<engine::TableCell>>, grid: Vec<i32>) -> engine::Table {
    engine::Table {
        grid,
        rows: rows
            .into_iter()
            .map(|cells| engine::TableRow {
                cells,
                ..Default::default()
            })
            .collect(),
        dirty: true,
        ..Default::default()
    }
}

/// A 1×2 table holding `a` | `b`.
fn pair(a: &str, b: &str) -> engine::Block {
    engine::Block::Table(table(
        vec![vec![cell(vec![para(a)]), cell(vec![para(b)])]],
        vec![1500, 1500],
    ))
}

/// `intro`, then a 2×2 table:
/// - (0,0) `A`
/// - (0,1) a nested `N1 | N2` table, then `after` — STARTS with a table
/// - (1,0) `before`, then a nested `M1 | M2` table — ENDS with a table
/// - (1,1) only a nested `O1 | O2` table
///
/// then `outro`.
fn nested_doc() -> DocumentTree {
    let outer = table(
        vec![
            vec![
                cell(vec![para("A")]),
                cell(vec![pair("N1", "N2"), para("after")]),
            ],
            vec![
                cell(vec![para("before"), pair("M1", "M2")]),
                cell(vec![pair("O1", "O2")]),
            ],
        ],
        vec![4000, 4000],
    );
    let mut doc = DocumentTree::new();
    doc.blocks.clear();
    doc.blocks.push_back(para("intro"));
    doc.blocks.push_back(engine::Block::Table(outer));
    doc.blocks.push_back(para("outro"));
    doc
}

fn b(idx: u32) -> BridgePathStep {
    BridgePathStep::Block { idx }
}

fn c(row: u32, col: u32) -> BridgePathStep {
    BridgePathStep::Cell { row, col }
}

fn pos(steps: Vec<BridgePathStep>, offset: u32) -> BridgeLogicalPos {
    BridgeLogicalPos {
        path: BridgeBlockPath { steps },
        offset,
    }
}

/// The live selection's `(anchor, caret)`.
fn selection(e: &Engine) -> (BridgeLogicalPos, BridgeLogicalPos) {
    let sel = e.selection.as_ref().expect("a selection");
    (sel.anchor.clone(), sel.caret.clone())
}

/// Put a collapsed caret at `at`.
fn caret_at(e: &mut Engine, at: BridgeLogicalPos) {
    match apply(
        e,
        Command::SetSelection {
            range: BridgeLogicalRange {
                start: at.clone(),
                end: at.clone(),
            },
            caret: at.clone(),
        },
    ) {
        Event::SelectionChanged { .. } => {}
        other => panic!("SetSelection: {other:?}"),
    }
    assert_eq!(selection(e).1, at, "the caret was accepted as given");
}

fn tab(e: &mut Engine, forward: bool) {
    let direction = if forward {
        MoveDirection::NextCell
    } else {
        MoveDirection::PrevCell
    };
    match apply(
        e,
        Command::MoveCaret {
            direction,
            extend: false,
        },
    ) {
        Event::SelectionChanged { .. } => {}
        other => panic!("MoveCaret {direction:?}: {other:?}"),
    }
}

/// The texts of the paragraphs the live selection covers, in document
/// order: every paragraph whose path lies between the (ordered)
/// endpoints' paths. (`GetSelectionAsClipboard`'s plain text cannot
/// serve here: `DocumentTree::text_range` reads only a same-container
/// range, issue #561.)
fn covered_texts(e: &Engine) -> Vec<String> {
    let sel = e.selection.as_ref().expect("a selection");
    let (start, end) = ordered(sel.anchor.clone(), sel.caret.clone());
    let (lo, hi) = (
        bridge_to_engine_path(start.path),
        bridge_to_engine_path(end.path),
    );
    e.with_selection_doc(|doc| {
        doc_paragraph_paths(doc)
            .into_iter()
            .filter(|p| lo.cmp_doc_order(p).is_le() && p.cmp_doc_order(&hi).is_le())
            .map(|p| doc.paragraph_at_path(&p).expect("a paragraph").text.clone())
            .collect()
    })
}

/// The live selection resolves inside the document it addresses — the
/// `snapshot_decode` invariant #551 broke (`selection_is_valid`: both
/// endpoints are fixed points of `clamp_pos`). Under `fuzz-native` the
/// fuzz target's own `check_invariants_for_fuzzing` runs too.
fn assert_in_document(e: &Engine, what: &str) {
    let sel = e.selection.as_ref().expect("a selection");
    e.with_selection_doc(|doc| {
        assert_eq!(
            clamp_pos(doc, sel.anchor.clone()),
            sel.anchor,
            "{what}: anchor escaped"
        );
        assert_eq!(
            clamp_pos(doc, sel.caret.clone()),
            sel.caret,
            "{what}: caret escaped"
        );
    });
    #[cfg(feature = "fuzz-native")]
    e.check_invariants_for_fuzzing()
        .unwrap_or_else(|err| panic!("{what}: invariants: {err}"));
}

/// The selection resolves inside the document and covers exactly
/// `expect`: `(anchor, caret)` and the paragraphs between them — no
/// other cell's text is in it.
fn assert_selects(e: &mut Engine, expect: (BridgeLogicalPos, BridgeLogicalPos), words: &[&str]) {
    assert_in_document(e, "selection");
    assert_eq!(selection(e), expect);
    assert_eq!(covered_texts(e), words);
}

fn line_for<'g>(geom: &'g [LineGeom], path: &BridgeBlockPath) -> &'g LineGeom {
    geom.iter()
        .find(|l| &l.path == path)
        .unwrap_or_else(|| panic!("no geometry line for {path:?}"))
}

/// A point inside the line's text (its start for an empty line).
fn inside(line: &LineGeom) -> BridgePoint {
    let (lo, hi) = line
        .slots
        .iter()
        .fold((line.start_x, line.start_x), |(lo, hi), s| {
            (lo.min(s.x), hi.max(s.x))
        });
    BridgePoint {
        x: (lo + hi) / 2.0,
        y: line.y_top + line.height / 2.0,
    }
}

fn select_cell_at(e: &mut Engine, path: Vec<BridgePathStep>) {
    let geom = e.document_geometry().expect("geometry");
    let at = inside(line_for(&geom, &BridgeBlockPath { steps: path }));
    match apply(e, Command::SelectCellAt { at }) {
        Event::SelectionChanged { .. } => {}
        other => panic!("SelectCellAt: {other:?}"),
    }
}

/// The engine crate's descent, read straight: first / last paragraph of
/// each outer cell, through a leading / trailing / sole nested table.
#[test]
fn cell_paragraph_bounds_descend_into_nested_tables() {
    let doc = nested_doc();
    let bounds = |r: u32, col: u32| {
        doc.cell_paragraph_bounds(&bridge_to_engine_path(BridgeBlockPath {
            steps: vec![b(1), c(r, col)],
        }))
        .map(|(f, l)| {
            (
                engine_to_bridge_path(f).steps,
                engine_to_bridge_path(l).steps,
            )
        })
    };
    assert_eq!(
        bounds(0, 0),
        Some((vec![b(1), c(0, 0), b(0)], vec![b(1), c(0, 0), b(0)]))
    );
    assert_eq!(
        bounds(0, 1),
        Some((
            vec![b(1), c(0, 1), b(0), c(0, 0), b(0)],
            vec![b(1), c(0, 1), b(1)]
        ))
    );
    assert_eq!(
        bounds(1, 0),
        Some((
            vec![b(1), c(1, 0), b(0)],
            vec![b(1), c(1, 0), b(1), c(0, 1), b(0)]
        ))
    );
    assert_eq!(
        bounds(1, 1),
        Some((
            vec![b(1), c(1, 1), b(0), c(0, 0), b(0)],
            vec![b(1), c(1, 1), b(0), c(0, 1), b(0)]
        ))
    );
    /* No such cell / not a cell path. */
    assert_eq!(bounds(2, 0), None);
    assert!(
        doc.cell_paragraph_bounds(&EngineBlockPath::top(1))
            .is_none()
    );
    /* A cell with no paragraph at any depth never yields a table path. */
    let mut empty = nested_doc();
    empty.blocks.set(
        1,
        engine::Block::Table(table(vec![vec![cell(Vec::new())]], vec![3000])),
    );
    assert!(
        empty
            .cell_paragraph_bounds(&bridge_to_engine_path(BridgeBlockPath {
                steps: vec![b(1), c(0, 0)],
            }))
            .is_none()
    );
}

/// `SelectCellAt` (quadruple-click) on the paragraph of a cell that
/// starts / ends with a nested table selects the whole outer cell,
/// nested table included; a click inside a nested table selects that
/// nested cell (the innermost table owns the gesture).
#[test]
fn select_cell_at_spans_a_leading_or_trailing_nested_table() {
    let mut e = tests::test_engine_with_doc(nested_doc());

    /* (0,1): [N1 | N2], after — clicked on `after`. */
    select_cell_at(&mut e, vec![b(1), c(0, 1), b(1)]);
    assert_selects(
        &mut e,
        (
            pos(vec![b(1), c(0, 1), b(0), c(0, 0), b(0)], 0),
            pos(vec![b(1), c(0, 1), b(1)], "after".len() as u32),
        ),
        &["N1", "N2", "after"],
    );

    /* (1,0): before, [M1 | M2] — clicked on `before`. */
    select_cell_at(&mut e, vec![b(1), c(1, 0), b(0)]);
    assert_selects(
        &mut e,
        (
            pos(vec![b(1), c(1, 0), b(0)], 0),
            pos(vec![b(1), c(1, 0), b(1), c(0, 1), b(0)], 2),
        ),
        &["before", "M1", "M2"],
    );

    /* A click on `O2`, inside the nested table that is (1,1)'s only
    content: the nested cell. */
    select_cell_at(&mut e, vec![b(1), c(1, 1), b(0), c(0, 1), b(0)]);
    assert_selects(
        &mut e,
        (
            pos(vec![b(1), c(1, 1), b(0), c(0, 1), b(0)], 0),
            pos(vec![b(1), c(1, 1), b(0), c(0, 1), b(0)], 2),
        ),
        &["O2"],
    );

    /* Typing over the (0,1) span lands IN that cell — before #551 the
    selection had escaped to the document's last paragraph, so the text
    went there — and leaves every other cell and the body alone.
    (Replacing the WHOLE span, nested table included, is the
    cross-container delete of issue #561; today the delete stops at the
    end of the span's first paragraph.) */
    select_cell_at(&mut e, vec![b(1), c(0, 1), b(1)]);
    let _ = apply(
        &mut e,
        Command::InsertText {
            text: "Z".into(),
            at: None,
        },
    );
    assert_in_document(&e, "after typing");
    let doc = e.undo.current().clone();
    let t = doc.blocks[1]
        .as_table()
        .expect("the outer table survives")
        .clone();
    let texts = |r: usize, col: usize| -> Vec<String> {
        let mut out = Vec::new();
        walk_block_texts_owned(&t.rows[r].cells[col].blocks, &mut out);
        out
    };
    assert!(
        texts(0, 1)
            .first()
            .is_some_and(|first| first.starts_with('Z')),
        "{:?}",
        texts(0, 1)
    );
    assert!(
        !texts(0, 1).iter().any(|t| t.contains("N1")),
        "{:?}",
        texts(0, 1)
    );
    assert_eq!(texts(0, 0), vec!["A".to_string()]);
    assert_eq!(texts(1, 0), vec!["before", "M1", "M2"]);
    assert_eq!(texts(1, 1), vec!["O1", "O2"]);
    let body = |i: usize| doc.blocks[i].as_paragraph().map(|p| p.text.clone());
    assert_eq!(body(0).as_deref(), Some("intro"));
    assert_eq!(body(2).as_deref(), Some("outro"));
}

/// Every paragraph text under `blocks`, in document order.
fn walk_block_texts_owned(blocks: &[engine::Block], out: &mut Vec<String>) {
    for blk in blocks {
        match blk {
            engine::Block::Paragraph(p) => out.push(p.text.clone()),
            engine::Block::Table(t) => {
                for row in &t.rows {
                    for cl in &row.cells {
                        walk_block_texts_owned(&cl.blocks, out);
                    }
                }
            }
        }
    }
}

/// Tab / Shift+Tab into a cell select its content (memo row 27):
/// through a leading nested table, a trailing one, and a cell that is
/// only a nested table (that table's whole content). Each step keeps
/// the selection inside the document.
#[test]
fn tab_selects_cells_that_start_end_or_are_a_nested_table() {
    let mut e = tests::test_engine_with_doc(nested_doc());
    caret_at(&mut e, pos(vec![b(1), c(0, 0), b(0)], 1));

    tab(&mut e, true);
    assert_selects(
        &mut e,
        (
            pos(vec![b(1), c(0, 1), b(0), c(0, 0), b(0)], 0),
            pos(vec![b(1), c(0, 1), b(1)], "after".len() as u32),
        ),
        &["N1", "N2", "after"],
    );

    tab(&mut e, true);
    assert_selects(
        &mut e,
        (
            pos(vec![b(1), c(1, 0), b(0)], 0),
            pos(vec![b(1), c(1, 0), b(1), c(0, 1), b(0)], 2),
        ),
        &["before", "M1", "M2"],
    );

    tab(&mut e, true);
    assert_selects(
        &mut e,
        (
            pos(vec![b(1), c(1, 1), b(0), c(0, 0), b(0)], 0),
            pos(vec![b(1), c(1, 1), b(0), c(0, 1), b(0)], 2),
        ),
        &["O1", "O2"],
    );

    /* Shift+Tab walks back over the same cells. */
    tab(&mut e, false);
    assert_selects(
        &mut e,
        (
            pos(vec![b(1), c(1, 0), b(0)], 0),
            pos(vec![b(1), c(1, 0), b(1), c(0, 1), b(0)], 2),
        ),
        &["before", "M1", "M2"],
    );
    tab(&mut e, false);
    assert_selects(
        &mut e,
        (
            pos(vec![b(1), c(0, 1), b(0), c(0, 0), b(0)], 0),
            pos(vec![b(1), c(0, 1), b(1)], "after".len() as u32),
        ),
        &["N1", "N2", "after"],
    );
    tab(&mut e, false);
    assert_selects(
        &mut e,
        (
            pos(vec![b(1), c(0, 0), b(0)], 0),
            pos(vec![b(1), c(0, 0), b(0)], 1),
        ),
        &["A"],
    );
}

/// The innermost table handles Tab: from `N1` it moves to `N2` and
/// selects that NESTED cell, not the outer cell around the table.
#[test]
fn tab_inside_a_nested_table_selects_the_next_nested_cell() {
    let mut e = tests::test_engine_with_doc(nested_doc());
    caret_at(&mut e, pos(vec![b(1), c(0, 1), b(0), c(0, 0), b(0)], 1));
    tab(&mut e, true);
    assert_selects(
        &mut e,
        (
            pos(vec![b(1), c(0, 1), b(0), c(0, 1), b(0)], 0),
            pos(vec![b(1), c(0, 1), b(0), c(0, 1), b(0)], 2),
        ),
        &["N2"],
    );
    tab(&mut e, false);
    assert_selects(
        &mut e,
        (
            pos(vec![b(1), c(0, 1), b(0), c(0, 0), b(0)], 0),
            pos(vec![b(1), c(0, 1), b(0), c(0, 0), b(0)], 2),
        ),
        &["N1"],
    );
}

/// The #551 document shape, generator-free: a top-level table whose
/// cell (0,0) is `[nested table, paragraph]`; `SelectCellAt` anywhere in
/// that cell leaves a selection inside the document.
#[test]
fn the_551_shape_select_cell_at_stays_in_the_document() {
    let outer = table(
        vec![vec![cell(vec![pair("x", "y"), para("tail")])]],
        vec![6000],
    );
    let mut doc = DocumentTree::new();
    doc.blocks.clear();
    doc.blocks.push_back(engine::Block::Table(outer));
    doc.blocks.push_back(para(""));
    let mut e = tests::test_engine_with_doc(doc);
    let geom = e.document_geometry().expect("geometry");
    let lines: Vec<BridgeBlockPath> = geom.iter().map(|l| l.path.clone()).collect();
    for path in lines {
        select_cell_at(&mut e, path.steps.clone());
        assert_in_document(&e, &format!("SelectCellAt on {path:?}"));
    }
    /* The paragraph after the nested table selects the whole cell. */
    select_cell_at(&mut e, vec![b(0), c(0, 0), b(1)]);
    assert_selects(
        &mut e,
        (
            pos(vec![b(0), c(0, 0), b(0), c(0, 0), b(0)], 0),
            pos(vec![b(0), c(0, 0), b(1)], "tail".len() as u32),
        ),
        &["x", "y", "tail"],
    );
}
