//! Epic #555 — the TABLE rows of the cursor-behaviour matrix, pinned
//! natively (the real-UI twin is `ts/e2e/table-cursor-matrix.spec.ts`).
//! Expected behaviour: the clean-room methods memo
//! `plans/cleanroom/memos/onlyoffice-cursor-memo.md`.
//!
//! - Row 18 (§4): Shift+Up / Shift+Down past the story's first / last line
//!   extends the selection to its start / end (a plain Up / Down there
//!   stays put); Shift across a cell boundary is a cell selection.
//! - Row 28 (§7): Delete at the end of the paragraph before a table joins
//!   the table's first cell; Backspace at the start of the paragraph after
//!   a table, and every collapsed delete at a cell's edge, deletes nothing
//!   and leaves the caret (and the undo history) alone.
//!
//! Every test drives real `Command`s through `Engine::apply` over a laid-out
//! document.

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
    let evt = block_on(e.apply(cmd));
    assert!(matches!(evt, Event::SelectionChanged { .. }), "{evt:?}");
    evt
}

fn cell_path(table: u32, row: u32, col: u32) -> BridgeBlockPath {
    BridgeBlockPath {
        steps: vec![
            BridgePathStep::Block { idx: table },
            BridgePathStep::Cell { row, col },
            BridgePathStep::Block { idx: 0 },
        ],
    }
}

fn cell_pos(table: u32, row: u32, col: u32, offset: u32) -> BridgeLogicalPos {
    BridgeLogicalPos {
        path: cell_path(table, row, col),
        offset,
    }
}

/// `head` paragraphs, a 3 x 3 table whose cells read `a1 b1 c1 / a2 …`, and
/// `tail` paragraphs after it.
fn table_doc(head: &[&str], tail: &[&str]) -> DocumentTree {
    let texts = head.iter().chain(tail).map(|t| t.to_string());
    let t = head.len() as u32;
    let mut doc = DocumentTree::from_paragraphs(texts).insert_table(EngineBlockPath::top(t), 3, 3);
    for r in 0..3u32 {
        for c in 0..3u32 {
            let label = format!("{}{}", ["a", "b", "c"][c as usize], r + 1);
            doc = doc.insert_text(to_engine_pos(cell_pos(t, r, c, 0)), &label);
        }
    }
    doc
}

fn engine_at(doc: DocumentTree, caret: BridgeLogicalPos) -> Engine {
    let mut e = tests::test_engine_with_doc(doc);
    e.selection = Some(SelectionState {
        anchor: caret.clone(),
        caret,
        ideal_x: None,
        kind: SelectionKind::Linear,
    });
    e
}

fn text_at(e: &Engine, path: &BridgeBlockPath) -> String {
    e.undo
        .current()
        .paragraph_at_path(&bridge_to_engine_path(path.clone()))
        .map(|p| p.text.clone())
        .unwrap_or_else(|| panic!("no paragraph at {path:?}"))
}

fn sel(e: &Engine) -> SelectionState {
    e.selection.clone().expect("a selection")
}

fn key(e: &mut Engine, direction: MoveDirection, extend: bool) -> Event {
    apply(e, Command::MoveCaret { direction, extend })
}

fn delete(e: &mut Engine, forward: bool) -> Event {
    apply(
        e,
        Command::DeleteAtCaret {
            forward,
            by_word: false,
        },
    )
}

#[test]
fn shift_up_on_the_first_line_extends_to_the_document_start() {
    let mut e = engine_at(table_doc(&["Intro"], &["Outro"]), bpos_top(0, 3));
    key(&mut e, MoveDirection::Up, true);
    let s = sel(&e);
    assert_eq!(s.anchor, bpos_top(0, 3));
    assert_eq!(s.caret, bpos_top(0, 0));
    /* A plain Up on the first line stays put. */
    let mut e = engine_at(table_doc(&["Intro"], &["Outro"]), bpos_top(0, 3));
    key(&mut e, MoveDirection::Up, false);
    assert_eq!(sel(&e).caret, bpos_top(0, 3));
}

#[test]
fn shift_down_on_the_last_line_extends_to_the_document_end() {
    let mut e = engine_at(table_doc(&["Intro"], &["Outro"]), bpos_top(2, 3));
    key(&mut e, MoveDirection::Down, true);
    let s = sel(&e);
    assert_eq!(s.anchor, bpos_top(2, 3));
    assert_eq!(s.caret, bpos_top(2, 5));
    let mut e = engine_at(table_doc(&["Intro"], &["Outro"]), bpos_top(2, 3));
    key(&mut e, MoveDirection::Down, false);
    assert_eq!(sel(&e).caret, bpos_top(2, 3));
}

#[test]
fn shift_up_from_the_top_row_of_a_leading_table_reaches_its_first_cell() {
    /* A table-led document: no line above the top row, and the story's
    start is the first cell's paragraph (never a bare `Block(table)`). */
    let mut e = engine_at(table_doc(&[], &["Outro"]), cell_pos(0, 0, 1, 1));
    key(&mut e, MoveDirection::Up, true);
    let s = sel(&e);
    assert_eq!(s.anchor, cell_pos(0, 0, 1, 1));
    assert_eq!(s.caret, cell_pos(0, 0, 0, 0));
    assert!(
        matches!(
            s.kind,
            SelectionKind::TableCells {
                from_row: 0,
                from_col: 0,
                to_row: 0,
                to_col: 1,
                ..
            }
        ),
        "{:?}",
        s.kind
    );
}

#[test]
fn shift_across_a_cell_boundary_is_a_cell_selection() {
    let mut e = engine_at(table_doc(&["Intro"], &["Outro"]), cell_pos(1, 0, 0, 2));
    key(&mut e, MoveDirection::Right, true);
    assert!(
        matches!(
            sel(&e).kind,
            SelectionKind::TableCells {
                from_row: 0,
                from_col: 0,
                to_row: 0,
                to_col: 1,
                ..
            }
        ),
        "{:?}",
        sel(&e).kind
    );
    key(&mut e, MoveDirection::Down, true);
    assert!(
        matches!(
            sel(&e).kind,
            SelectionKind::TableCells {
                from_row: 0,
                from_col: 0,
                to_row: 1,
                to_col: 1,
                ..
            }
        ),
        "{:?}",
        sel(&e).kind
    );
}

#[test]
fn delete_at_the_end_of_the_paragraph_before_a_table_joins_its_first_cell() {
    let mut e = engine_at(table_doc(&["Intro"], &["Outro"]), bpos_top(0, 5));
    let depth = e.undo.depth();
    delete(&mut e, true);
    let doc = e.undo.current();
    assert!(
        matches!(doc.blocks.front(), Some(engine::Block::Table(_))),
        "the paragraph left the body: the table is block 0"
    );
    assert_eq!(doc.blocks.len(), 2);
    assert_eq!(text_at(&e, &cell_path(0, 0, 0)), "Introa1");
    assert_eq!(text_at(&e, &cell_path(0, 0, 1)), "b1");
    assert_eq!(text_at(&e, &BridgeBlockPath::top(1)), "Outro");
    assert_eq!(
        sel(&e).caret,
        cell_pos(0, 0, 0, 5),
        "the caret sits at the join"
    );
    assert_eq!(sel(&e).anchor, sel(&e).caret);
    assert_eq!(e.undo.depth(), depth + 1, "one undo step");
    apply(&mut e, Command::Undo);
    assert_eq!(text_at(&e, &BridgeBlockPath::top(0)), "Intro");
    assert_eq!(text_at(&e, &cell_path(1, 0, 0)), "a1");
}

#[test]
fn delete_in_an_empty_paragraph_before_a_table_removes_it() {
    let mut e = engine_at(table_doc(&["Intro", ""], &["Outro"]), bpos_top(1, 0));
    delete(&mut e, true);
    let doc = e.undo.current();
    assert_eq!(doc.blocks.len(), 3);
    assert!(matches!(doc.blocks.get(1), Some(engine::Block::Table(_))));
    assert_eq!(text_at(&e, &cell_path(1, 0, 0)), "a1");
    assert_eq!(sel(&e).caret, cell_pos(1, 0, 0, 0));
}

#[test]
fn delete_before_a_table_keeps_a_section_break() {
    /* The paragraph's mark ends a section: joining would drop the break,
    so nothing is deleted. */
    let mut doc = table_doc(&["Intro"], &["Outro"]);
    if let Some(engine::Block::Paragraph(p)) = doc.blocks.get_mut(0) {
        p.section_end = Some(Box::default());
    }
    let mut e = engine_at(doc, bpos_top(0, 5));
    let revision = e.undo.revision();
    delete(&mut e, true);
    assert_eq!(e.undo.revision(), revision, "nothing committed");
    assert_eq!(text_at(&e, &BridgeBlockPath::top(0)), "Intro");
    assert_eq!(sel(&e).caret, bpos_top(0, 5));
}

#[test]
fn delete_before_a_table_in_review_mode_deletes_nothing() {
    let mut e = engine_at(table_doc(&["Intro"], &["Outro"]), bpos_top(0, 5));
    e.tracking_changes = true;
    let revision = e.undo.revision();
    delete(&mut e, true);
    assert_eq!(e.undo.revision(), revision);
    assert_eq!(text_at(&e, &BridgeBlockPath::top(0)), "Intro");
    assert_eq!(text_at(&e, &cell_path(1, 0, 0)), "a1");
}

/// A collapsed delete at `caret` changes nothing: same text everywhere,
/// same caret, no undo entry.
fn assert_deletes_nothing(caret: BridgeLogicalPos, forward: bool) {
    let mut e = engine_at(table_doc(&["Intro"], &["Outro"]), caret.clone());
    let revision = e.undo.revision();
    let depth = e.undo.depth();
    delete(&mut e, forward);
    assert_eq!(e.undo.revision(), revision, "{caret:?} forward={forward}");
    assert_eq!(e.undo.depth(), depth);
    assert_eq!(sel(&e).caret, caret, "the caret stays (forward={forward})");
    assert_eq!(sel(&e).anchor, caret);
    assert_eq!(text_at(&e, &BridgeBlockPath::top(0)), "Intro");
    assert_eq!(text_at(&e, &BridgeBlockPath::top(2)), "Outro");
    for r in 0..3u32 {
        for c in 0..3u32 {
            let label = format!("{}{}", ["a", "b", "c"][c as usize], r + 1);
            assert_eq!(text_at(&e, &cell_path(1, r, c)), label);
        }
    }
}

#[test]
fn backspace_at_the_start_of_the_paragraph_after_a_table_deletes_nothing() {
    assert_deletes_nothing(bpos_top(2, 0), false);
}

#[test]
fn a_collapsed_delete_never_crosses_a_cell_boundary() {
    /* Backspace at a cell's start (mid-table, and the first cell after
    `Intro`), Delete at a cell's end (mid-table, and the last cell before
    `Outro`). */
    assert_deletes_nothing(cell_pos(1, 1, 0, 0), false);
    assert_deletes_nothing(cell_pos(1, 0, 0, 0), false);
    assert_deletes_nothing(cell_pos(1, 1, 0, 2), true);
    assert_deletes_nothing(cell_pos(1, 2, 2, 2), true);
}

#[test]
fn deletes_inside_a_cell_still_edit_it() {
    let mut e = engine_at(table_doc(&["Intro"], &["Outro"]), cell_pos(1, 1, 1, 1));
    delete(&mut e, false);
    assert_eq!(text_at(&e, &cell_path(1, 1, 1)), "2");
    delete(&mut e, true);
    assert_eq!(text_at(&e, &cell_path(1, 1, 1)), "");
    assert_eq!(sel(&e).caret, cell_pos(1, 1, 1, 0));
}
