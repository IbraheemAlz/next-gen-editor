//! Issue #561 — typing over, deleting and copying a selection whose ends
//! sit in different containers (body ↔ cell, a Tab-selected cell whose
//! content starts with / ends with / is a nested table, a `TABLE_CELLS`
//! rectangle), through the real commands. The tree-level rules are pinned
//! in `engine::range_delete_tests`; here every caller of the one range
//! deletion (`Engine::delete_range_in`) is driven end to end, the copy
//! included, and review mode keeps the #365 row marking.

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

fn table(rows: Vec<Vec<engine::TableCell>>) -> engine::Block {
    let cols = rows.first().map_or(1, Vec::len);
    engine::Block::Table(engine::Table {
        grid: vec![2000; cols],
        rows: rows
            .into_iter()
            .map(|cells| engine::TableRow {
                cells,
                ..Default::default()
            })
            .collect(),
        dirty: true,
        ..Default::default()
    })
}

fn grid(texts: &[&[&str]]) -> engine::Block {
    table(
        texts
            .iter()
            .map(|row| row.iter().map(|t| cell(vec![para(t)])).collect())
            .collect(),
    )
}

fn doc_of(blocks: Vec<engine::Block>) -> DocumentTree {
    let mut d = DocumentTree::new();
    d.blocks.clear();
    for b in blocks {
        d.blocks.push_back(b);
    }
    d
}

/// The #551 document: `intro`, a 2×2 table — (0,0) `A`; (0,1) a nested
/// `N1 | N2` then `after`; (1,0) `before` then a nested `M1 | M2`; (1,1)
/// only a nested `O1 | O2` — then `outro`.
fn nested_doc() -> DocumentTree {
    doc_of(vec![
        para("intro"),
        table(vec![
            vec![
                cell(vec![para("A")]),
                cell(vec![grid(&[&["N1", "N2"]]), para("after")]),
            ],
            vec![
                cell(vec![para("before"), grid(&[&["M1", "M2"]])]),
                cell(vec![grid(&[&["O1", "O2"]])]),
            ],
        ]),
        para("outro"),
    ])
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

fn select(e: &mut Engine, anchor: BridgeLogicalPos, caret: BridgeLogicalPos) {
    match apply(
        e,
        Command::SetSelection {
            range: BridgeLogicalRange {
                start: anchor,
                end: caret.clone(),
            },
            caret,
        },
    ) {
        Event::SelectionChanged { .. } => {}
        other => panic!("SetSelection: {other:?}"),
    }
}

fn tab(e: &mut Engine) {
    match apply(
        e,
        Command::MoveCaret {
            direction: MoveDirection::NextCell,
            extend: false,
        },
    ) {
        Event::SelectionChanged { .. } => {}
        other => panic!("Tab: {other:?}"),
    }
}

fn type_text(e: &mut Engine, text: &str) {
    match apply(
        e,
        Command::InsertText {
            text: text.into(),
            at: None,
        },
    ) {
        Event::SelectionChanged { .. } => {}
        other => panic!("InsertText: {other:?}"),
    }
}

fn delete(e: &mut Engine) {
    match apply(
        e,
        Command::DeleteAtCaret {
            forward: true,
            by_word: false,
        },
    ) {
        Event::SelectionChanged { .. } => {}
        other => panic!("DeleteAtCaret: {other:?}"),
    }
}

fn copy_plain(e: &mut Engine) -> String {
    match apply(
        e,
        Command::GetSelectionAsClipboard {
            include_docx: Some(false),
        },
    ) {
        Event::ClipboardPayload { plain, .. } => plain,
        other => panic!("GetSelectionAsClipboard: {other:?}"),
    }
}

/// Every block of a cell's / the body's block list as text, nested
/// tables flattened to `[rows of cells]`.
fn shape(blocks: &[engine::Block]) -> String {
    blocks
        .iter()
        .map(|blk| match blk {
            engine::Block::Paragraph(p) => format!("{:?}", p.text),
            engine::Block::Table(t) => format!(
                "[{}]",
                t.rows
                    .iter()
                    .map(|r| r
                        .cells
                        .iter()
                        .map(|cl| shape(&cl.blocks))
                        .collect::<Vec<_>>()
                        .join(" | "))
                    .collect::<Vec<_>>()
                    .join(" / ")
            ),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn body(e: &Engine) -> String {
    let blocks: Vec<engine::Block> = e.undo.current().blocks.iter().cloned().collect();
    shape(&blocks)
}

fn caret(e: &Engine) -> (BridgeLogicalPos, BridgeLogicalPos) {
    let s = e.selection.as_ref().expect("a selection");
    (s.anchor.clone(), s.caret.clone())
}

/// The fuzz targets' invariants hold after every step.
fn assert_sound(e: &Engine, what: &str) {
    /* `selection_is_valid` is native-only; its check, read straight: both
    endpoints are fixed points of `clamp_pos`. */
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

/// Typing over a Tab-selected cell replaces its WHOLE content — the nested
/// table included — with the typed text (#561 acceptance; before, only the
/// span's first paragraph lost its tail: `Z`, `N2`, `after`).
#[test]
fn typing_over_a_tab_selected_cell_replaces_all_of_it() {
    let outer = |cell: &str| {
        let others = [
            ("00", "\"A\""),
            ("01", "[\"N1\" | \"N2\"], \"after\""),
            ("10", "\"before\", [\"M1\" | \"M2\"]"),
            ("11", "[\"O1\" | \"O2\"]"),
        ];
        let cells: Vec<String> = others
            .iter()
            .map(|(k, v)| {
                if *k == cell {
                    "\"Z\"".to_string()
                } else {
                    (*v).to_string()
                }
            })
            .collect();
        format!(
            "\"intro\", [{} | {} / {} | {}], \"outro\"",
            cells[0], cells[1], cells[2], cells[3]
        )
    };
    for (tabs, cell_key, copied) in [
        (1, "01", "N1\tN2\nafter"),
        (2, "10", "before\nM1\tM2"),
        (3, "11", "O1\tO2"),
    ] {
        let mut e = tests::test_engine_with_doc(nested_doc());
        select(
            &mut e,
            pos(vec![b(1), c(0, 0), b(0)], 1),
            pos(vec![b(1), c(0, 0), b(0)], 1),
        );
        for _ in 0..tabs {
            tab(&mut e);
        }
        assert_eq!(copy_plain(&mut e), copied, "copy of cell {cell_key}");
        type_text(&mut e, "Z");
        assert_sound(&e, "after typing");
        assert_eq!(body(&e), outer(cell_key), "cell {cell_key}");
        let (r, col) = (cell_key.as_bytes()[0] - b'0', cell_key.as_bytes()[1] - b'0');
        let home = pos(vec![b(1), c(u32::from(r), u32::from(col)), b(0)], 1);
        assert_eq!(caret(&e), (home.clone(), home), "the caret follows the Z");
        /* One undo step restores the cell. */
        let _ = apply(&mut e, Command::Undo);
        assert_eq!(
            body(&e),
            shape(&nested_doc().blocks.iter().cloned().collect::<Vec<_>>())
        );
        assert_sound(&e, "after undo");
    }
}

/// Delete over a Tab-selected cell empties it; the caret sits in the one
/// paragraph that is left.
#[test]
fn delete_over_a_tab_selected_cell_empties_it() {
    let mut e = tests::test_engine_with_doc(nested_doc());
    select(
        &mut e,
        pos(vec![b(1), c(0, 0), b(0)], 0),
        pos(vec![b(1), c(0, 0), b(0)], 0),
    );
    tab(&mut e);
    delete(&mut e);
    assert_sound(&e, "after delete");
    assert_eq!(
        body(&e),
        "\"intro\", [\"A\" | \"\" / \"before\", [\"M1\" | \"M2\"] | [\"O1\" | \"O2\"]], \"outro\""
    );
    let home = pos(vec![b(1), c(0, 1), b(0)], 0);
    assert_eq!(caret(&e), (home.clone(), home));
}

fn body_doc() -> DocumentTree {
    doc_of(vec![
        para("intro"),
        grid(&[&["A1", "B1"], &["C1", "D1"]]),
        para("outro"),
    ])
}

/// A body selection into a table: the head stays, no merge across the
/// table edge, the whole rows it covers go; the copy reads every cell.
#[test]
fn a_body_selection_into_a_table_deletes_and_copies_across_it() {
    let mut e = tests::test_engine_with_doc(body_doc());
    select(
        &mut e,
        pos(vec![b(0)], 2),
        pos(vec![b(1), c(1, 0), b(0)], 1),
    );
    assert_eq!(copy_plain(&mut e), "tro\nA1\tB1\nC");
    delete(&mut e);
    assert_sound(&e, "after delete");
    assert_eq!(body(&e), "\"in\", [\"1\" | \"D1\"], \"outro\"");
    let home = pos(vec![b(0)], 2);
    assert_eq!(caret(&e), (home.clone(), home));

    /* A body range over the whole table copies it row by row and deletes
    it (and the paragraphs merge, as in one container). */
    let mut e = tests::test_engine_with_doc(body_doc());
    select(&mut e, pos(vec![b(0)], 2), pos(vec![b(2)], 3));
    assert_eq!(copy_plain(&mut e), "tro\nA1\tB1\nC1\tD1\nout");
    type_text(&mut e, "-");
    assert_sound(&e, "after typing");
    assert_eq!(body(&e), "\"in-ro\"");
}

/// A cell into the body: the tail stays, the rows after the start's row
/// go, the start row's later cells empty.
#[test]
fn a_selection_from_a_cell_into_the_body() {
    let mut e = tests::test_engine_with_doc(body_doc());
    select(
        &mut e,
        pos(vec![b(1), c(0, 0), b(0)], 1),
        pos(vec![b(2)], 3),
    );
    assert_eq!(copy_plain(&mut e), "1\tB1\nC1\tD1\nout");
    let _ = apply(
        &mut e,
        Command::PastePlain {
            text: "x\ny".into(),
        },
    );
    assert_sound(&e, "after paste");
    assert_eq!(body(&e), "\"intro\", [\"Ax\", \"y\" | \"\"], \"ro\"");
}

/// A `TABLE_CELLS` rectangle: Delete empties exactly its cells, typing
/// lands in its first cell, the copy is its rows.
#[test]
fn a_cell_rectangle_is_cleared_and_copied_by_its_rows() {
    let doc = doc_of(vec![
        para("intro"),
        grid(&[&["a", "b", "c"], &["d", "e", "f"], &["g", "h", "i"]]),
        para("outro"),
    ]);
    let mut e = tests::test_engine_with_doc(doc);
    select(
        &mut e,
        pos(vec![b(1), c(0, 1), b(0)], 0),
        pos(vec![b(1), c(1, 2), b(0)], 1),
    );
    assert!(
        matches!(
            e.selection.as_ref().map(|s| &s.kind),
            Some(SelectionKind::TableCells { .. })
        ),
        "a cell rectangle"
    );
    assert_eq!(copy_plain(&mut e), "b\tc\ne\tf");
    type_text(&mut e, "Z");
    assert_sound(&e, "after typing");
    assert_eq!(
        body(&e),
        "\"intro\", [\"a\" | \"Z\" | \"\" / \"d\" | \"\" | \"\" / \"g\" | \"h\" | \"i\"], \"outro\""
    );
}

/// Review mode keeps the #365 rule: a deletion from the body into a table
/// marks every row it touches deleted and removes nothing.
#[test]
fn review_mode_still_marks_the_rows_a_body_selection_reaches() {
    let mut e = tests::test_engine_with_doc(body_doc());
    let _ = apply(&mut e, Command::ToggleTrackChanges { enabled: true });
    select(
        &mut e,
        pos(vec![b(0)], 2),
        pos(vec![b(1), c(1, 0), b(0)], 1),
    );
    delete(&mut e);
    assert_sound(&e, "after the tracked delete");
    assert_eq!(
        body(&e),
        shape(&body_doc().blocks.iter().cloned().collect::<Vec<_>>())
    );
    let doc = e.undo.current();
    let engine::Block::Table(t) = &doc.blocks[1] else {
        panic!("the table stays");
    };
    for (r, row) in t.rows.iter().enumerate() {
        assert!(
            row.props
                .revisions
                .iter()
                .any(|x| x.kind == engine::RevisionKind::Delete),
            "row {r} marked deleted"
        );
    }
    let intro = doc.blocks[0].as_paragraph().expect("intro");
    assert!(
        intro
            .revisions
            .iter()
            .any(|x| x.kind == engine::RevisionKind::Delete && x.start == 2 && x.end == 5),
        "the head's tail is marked: {:?}",
        intro.revisions
    );
}

/// `fuzz-native` — every cross-container edit, the copy and undo / redo
/// keep the `rpc_command` invariants, and a command that answers an error
/// leaves the session untouched (`state_fingerprint_for_fuzzing`).
#[cfg(feature = "fuzz-native")]
#[test]
fn cross_container_edits_keep_the_fuzz_invariants() {
    let ranges = [
        (pos(vec![b(0)], 2), pos(vec![b(1), c(1, 0), b(0)], 3)),
        (
            pos(vec![b(1), c(0, 1), b(0), c(0, 0), b(0)], 0),
            pos(vec![b(1), c(0, 1), b(1)], 5),
        ),
        (
            pos(vec![b(1), c(1, 0), b(0)], 0),
            pos(vec![b(1), c(1, 0), b(1), c(0, 1), b(0)], 2),
        ),
        (
            pos(vec![b(1), c(1, 1), b(0), c(0, 0), b(0)], 0),
            pos(vec![b(1), c(1, 1), b(0), c(0, 1), b(0)], 2),
        ),
        (
            pos(vec![b(1), c(0, 1), b(0), c(0, 1), b(0)], 1),
            pos(vec![b(2)], 3),
        ),
        (
            pos(vec![b(1), c(0, 0), b(0)], 0),
            pos(vec![b(1), c(1, 1), b(0), c(0, 1), b(0)], 2),
        ),
        (pos(vec![b(0)], 0), pos(vec![b(2)], 5)),
    ];
    let edits = [
        Command::InsertText {
            text: "Z".into(),
            at: None,
        },
        Command::DeleteAtCaret {
            forward: false,
            by_word: false,
        },
        Command::PastePlain {
            text: "p\nq\nr".into(),
        },
        Command::SplitParagraph { at: None },
        Command::PasteHtml {
            html: "<p>h</p><table><tr><td>t</td></tr></table>".into(),
        },
        Command::GetSelectionAsClipboard {
            include_docx: Some(true),
        },
    ];
    for tracking in [false, true] {
        for (start, end) in &ranges {
            for edit in &edits {
                let mut e = Engine::new_headless(nested_doc());
                let _ = e.apply_sync(Command::ToggleTrackChanges { enabled: tracking });
                let _ = e.apply_sync(Command::SetSelection {
                    range: BridgeLogicalRange {
                        start: start.clone(),
                        end: end.clone(),
                    },
                    caret: end.clone(),
                });
                for cmd in [edit.clone(), Command::Undo, Command::Redo] {
                    let before = e.state_fingerprint_for_fuzzing();
                    let reply = e.apply_sync(cmd.clone());
                    if matches!(reply, Event::Error { .. }) {
                        assert_eq!(
                            e.state_fingerprint_for_fuzzing(),
                            before,
                            "{cmd:?} answered an error and changed the session"
                        );
                    }
                    e.check_invariants_for_fuzzing().unwrap_or_else(|err| {
                        panic!("{cmd:?} over {start:?}..{end:?} (tracking {tracking}): {err}")
                    });
                }
            }
        }
    }
}
