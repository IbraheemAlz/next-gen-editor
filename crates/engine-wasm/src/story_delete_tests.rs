//! Issue #565 — a collapsed Backspace / Delete inside a table cell of a
//! HEADER story edits the cell. `story_delete_at_caret` used to build its
//! range with top-level block arithmetic (`bpos_top(top_idx, …)`, where
//! `top_idx` is the caret path's first step — the TABLE for a story cell),
//! so the range straddled two containers and nothing was removed. The
//! story now plans the step with the body's `collapsed_delete_plan` over
//! the story tree, so the epic #555 row 28 rules (clean-room cursor memo
//! §7) hold there too: a collapsed delete never crosses a cell boundary,
//! Delete at the end of the paragraph before a table joins its first cell,
//! and Ctrl+Delete / Ctrl+Backspace use the row 20 word classes.
//!
//! Every test drives real `Command`s through `Engine::apply`. The real-UI
//! twin is `ts/e2e/story-table-delete.spec.ts`.

use super::story_tab_tests::{apply, header_table_engine};
use super::*;

/// A story-rooted cell paragraph: `Block(table) Cell{row, col} Block(0)`.
fn cell_pos(table: u32, row: u32, col: u32, offset: u32) -> BridgeLogicalPos {
    BridgeLogicalPos {
        path: BridgeBlockPath {
            steps: vec![
                BridgePathStep::Block { idx: table },
                BridgePathStep::Cell { row, col },
                BridgePathStep::Block { idx: 0 },
            ],
        },
        offset,
    }
}

fn put_caret(e: &mut Engine, caret: BridgeLogicalPos) {
    e.selection = Some(SelectionState {
        anchor: caret.clone(),
        caret,
        ideal_x: None,
        kind: SelectionKind::Linear,
    });
}

fn caret(e: &Engine) -> BridgeLogicalPos {
    e.selection.clone().expect("a selection").caret
}

fn type_text(e: &mut Engine, text: &str) {
    let evt = apply(
        e,
        Command::InsertText {
            at: None,
            text: text.into(),
        },
    );
    assert!(matches!(evt, Event::SelectionChanged { .. }), "{evt:?}");
}

fn delete(e: &mut Engine, forward: bool, by_word: bool) {
    let evt = apply(e, Command::DeleteAtCaret { forward, by_word });
    assert!(matches!(evt, Event::SelectionChanged { .. }), "{evt:?}");
}

/// The text of the header's paragraph at `pos`'s path.
fn header_text(e: &Engine, rid: &str, pos: &BridgeLogicalPos) -> String {
    let tree = DocumentTree::from_blocks(e.undo.current().headers[rid].clone());
    tree.paragraph_at_path(&bridge_to_engine_path(pos.path.clone()))
        .map(|p| p.text.clone())
        .unwrap_or_else(|| panic!("no header paragraph at {:?}", pos.path))
}

fn header_blocks(e: &Engine, rid: &str) -> usize {
    e.undo.current().headers[rid].len()
}

/// The header table's two cells hold `left` / `right`, caret in the left
/// one at `offset`.
fn engine_with_cells(left: &str, right: &str, offset: u32) -> (Engine, String) {
    let (mut e, rid) = header_table_engine();
    type_text(&mut e, left);
    put_caret(&mut e, cell_pos(0, 0, 1, 0));
    type_text(&mut e, right);
    put_caret(&mut e, cell_pos(0, 0, 0, offset));
    assert_eq!(header_text(&e, &rid, &cell_pos(0, 0, 0, 0)), left);
    assert_eq!(header_text(&e, &rid, &cell_pos(0, 0, 1, 0)), right);
    (e, rid)
}

/// A collapsed delete at `at` changes nothing: no undo step, the caret
/// stays, both cells keep their text and the body is untouched.
fn assert_deletes_nothing(at: BridgeLogicalPos, forward: bool, by_word: bool) {
    let (mut e, rid) = engine_with_cells("abc", "xyz", 0);
    put_caret(&mut e, at.clone());
    let revision = e.undo.revision();
    let depth = e.undo.depth();
    delete(&mut e, forward, by_word);
    let why = format!("{at:?} forward={forward} by_word={by_word}");
    assert_eq!(e.undo.revision(), revision, "{why}");
    assert_eq!(e.undo.depth(), depth, "{why}");
    assert_eq!(caret(&e), at, "the caret stays: {why}");
    assert_eq!(header_text(&e, &rid, &cell_pos(0, 0, 0, 0)), "abc", "{why}");
    assert_eq!(header_text(&e, &rid, &cell_pos(0, 0, 1, 0)), "xyz", "{why}");
    assert_eq!(e.undo.current().to_plain_text(), "body text", "{why}");
    assert!(e.story_active(), "{why}");
}

#[test]
fn delete_inside_a_header_cell_removes_one_character() {
    let (mut e, rid) = engine_with_cells("abc", "xyz", 1);
    let depth = e.undo.depth();
    delete(&mut e, true, false);
    assert_eq!(header_text(&e, &rid, &cell_pos(0, 0, 0, 0)), "ac");
    assert_eq!(caret(&e), cell_pos(0, 0, 0, 1));
    assert_eq!(header_text(&e, &rid, &cell_pos(0, 0, 1, 0)), "xyz");
    assert_eq!(e.undo.depth(), depth + 1, "one undo step");
    assert_eq!(e.undo.current().to_plain_text(), "body text");
    apply(&mut e, Command::Undo);
    assert_eq!(header_text(&e, &rid, &cell_pos(0, 0, 0, 0)), "abc");
    assert!(e.story_active());
}

#[test]
fn backspace_inside_a_header_cell_removes_one_character() {
    let (mut e, rid) = engine_with_cells("abc", "xyz", 2);
    delete(&mut e, false, false);
    assert_eq!(header_text(&e, &rid, &cell_pos(0, 0, 0, 0)), "ac");
    assert_eq!(caret(&e), cell_pos(0, 0, 0, 1));
    /* The second cell is edited the same way. */
    put_caret(&mut e, cell_pos(0, 0, 1, 3));
    delete(&mut e, false, false);
    assert_eq!(header_text(&e, &rid, &cell_pos(0, 0, 1, 0)), "xy");
    assert_eq!(caret(&e), cell_pos(0, 0, 1, 2));
}

#[test]
fn word_deletes_inside_a_header_cell_use_the_word_classes() {
    let (mut e, rid) = engine_with_cells("one two three", "xyz", 7);
    /* Ctrl+Backspace from the end of `two`: back to its start. */
    delete(&mut e, false, true);
    assert_eq!(header_text(&e, &rid, &cell_pos(0, 0, 0, 0)), "one  three");
    assert_eq!(caret(&e), cell_pos(0, 0, 0, 4));
    /* Ctrl+Delete from there: up to the start of `three`. */
    delete(&mut e, true, true);
    assert_eq!(header_text(&e, &rid, &cell_pos(0, 0, 0, 0)), "one three");
    assert_eq!(caret(&e), cell_pos(0, 0, 0, 4));
}

#[test]
fn a_header_cells_edges_delete_nothing() {
    /* Backspace at a cell's start (both cells), Delete at a cell's end
    (both cells) — the neighbour cell, and the paragraph around the table,
    are never touched. Word-wise too. */
    for by_word in [false, true] {
        assert_deletes_nothing(cell_pos(0, 0, 0, 0), false, by_word);
        assert_deletes_nothing(cell_pos(0, 0, 1, 0), false, by_word);
        assert_deletes_nothing(cell_pos(0, 0, 0, 3), true, by_word);
        assert_deletes_nothing(cell_pos(0, 0, 1, 3), true, by_word);
    }
}

#[test]
fn backspace_at_the_start_of_the_paragraph_after_a_header_table_deletes_nothing() {
    let (mut e, rid) = engine_with_cells("abc", "xyz", 0);
    /* The header keeps a paragraph after its table: `[table, paragraph]`. */
    assert_eq!(header_blocks(&e, &rid), 2);
    put_caret(&mut e, bpos_top(1, 0));
    type_text(&mut e, "tail");
    put_caret(&mut e, bpos_top(1, 0));
    let revision = e.undo.revision();
    delete(&mut e, false, false);
    assert_eq!(e.undo.revision(), revision);
    assert_eq!(caret(&e), bpos_top(1, 0));
    assert_eq!(header_text(&e, &rid, &cell_pos(0, 0, 1, 0)), "xyz");
    assert_eq!(header_text(&e, &rid, &bpos_top(1, 0)), "tail");
}

/// A paragraph block reading `text`.
fn para_block(text: &str) -> engine::Block {
    DocumentTree::from_text(text).blocks[0].clone()
}

/// Replace the active header's blocks (the story stays open on it).
fn set_header(e: &mut Engine, rid: &str, blocks: Vec<engine::Block>) {
    let doc = e.undo.current().with_updated_header_part(rid, blocks);
    e.undo = UndoStack::new(doc, 100);
}

#[test]
fn delete_at_the_end_of_the_paragraph_before_a_header_table_joins_its_first_cell() {
    let (mut e, rid) = header_table_engine();
    let table = e.undo.current().headers[&rid][0].clone();
    assert!(table.as_table().is_some());
    /* `[Head][table]` — a paragraph above the table. */
    set_header(&mut e, &rid, vec![para_block("Head"), table]);
    put_caret(&mut e, bpos_top(0, 4));
    let depth = e.undo.depth();
    delete(&mut e, true, false);
    assert_eq!(
        header_text(&e, &rid, &cell_pos(0, 0, 0, 0)),
        "Head",
        "the paragraph joined the first cell"
    );
    assert_eq!(header_blocks(&e, &rid), 1, "the paragraph left the band");
    assert_eq!(
        caret(&e),
        cell_pos(0, 0, 0, 4),
        "the caret sits at the join"
    );
    assert_eq!(e.undo.depth(), depth + 1, "one undo step");
    apply(&mut e, Command::Undo);
    assert_eq!(header_blocks(&e, &rid), 2);
    assert_eq!(header_text(&e, &rid, &bpos_top(0, 0)), "Head");
}

#[test]
fn review_mode_deletes_nothing_when_joining_a_header_table() {
    /* The join has no tracked-change form: review mode deletes nothing
    (the body twin is `delete_before_a_table_in_review_mode_deletes_nothing`). */
    let (mut e, rid) = header_table_engine();
    let table = e.undo.current().headers[&rid][0].clone();
    set_header(&mut e, &rid, vec![para_block("Head"), table]);
    put_caret(&mut e, bpos_top(0, 4));
    e.tracking_changes = true;
    let revision = e.undo.revision();
    delete(&mut e, true, false);
    assert_eq!(e.undo.revision(), revision);
    assert_eq!(header_blocks(&e, &rid), 2);
    assert_eq!(header_text(&e, &rid, &bpos_top(0, 0)), "Head");
}

#[test]
fn review_mode_never_deletes_across_a_header_cell_boundary() {
    /* Tracked deletions in stories are #368 (a story records no
    revisions yet), so this pins only the boundary rules: they hold with
    review mode on exactly as with it off. */
    let (mut e, rid) = engine_with_cells("abc", "xyz", 0);
    e.tracking_changes = true;
    let revision = e.undo.revision();
    delete(&mut e, false, false);
    assert_eq!(e.undo.revision(), revision, "Backspace at a cell start");
    put_caret(&mut e, cell_pos(0, 0, 0, 3));
    delete(&mut e, true, false);
    assert_eq!(e.undo.revision(), revision, "Delete at a cell end");
    assert_eq!(header_text(&e, &rid, &cell_pos(0, 0, 0, 0)), "abc");
    assert_eq!(header_text(&e, &rid, &cell_pos(0, 0, 1, 0)), "xyz");
}

#[test]
fn a_plain_header_paragraph_still_merges_with_its_neighbour() {
    /* Two top-level paragraphs in the band: Backspace at the second's
    start merges it into the first, Delete at the first's end likewise —
    the pre-#565 behaviour, now through the shared planner. */
    let (mut e, rid) = header_table_engine();
    set_header(&mut e, &rid, vec![para_block("one"), para_block("two")]);
    put_caret(&mut e, bpos_top(1, 0));
    delete(&mut e, false, false);
    assert_eq!(header_blocks(&e, &rid), 1);
    assert_eq!(header_text(&e, &rid, &bpos_top(0, 0)), "onetwo");
    assert_eq!(caret(&e), bpos_top(0, 3));
    set_header(&mut e, &rid, vec![para_block("one"), para_block("two")]);
    put_caret(&mut e, bpos_top(0, 3));
    delete(&mut e, true, false);
    assert_eq!(header_blocks(&e, &rid), 1);
    assert_eq!(header_text(&e, &rid, &bpos_top(0, 0)), "onetwo");
}
