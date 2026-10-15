//! Issue #442 — structural edits made by command with review mode on are
//! tracked like Word tracks them: `InsertRow` / `DeleteRow` record
//! `<w:trPr><w:ins/>` / `<w:del/>` row changes, `InsertTable` a table of
//! inserted rows, `InsertSectionBreak` an inserted paragraph mark carrying
//! the break — each listed in `revisions_snapshot`, resolved by Accept /
//! Reject (all, or by `revision_id`), and rejecting restores the document.
//! End to end through real `Command`s on the headless `fuzz-native`
//! engine, with `Engine::check_invariants_for_fuzzing` after every command.

use super::*;

fn run(e: &mut Engine, cmd: Command) -> Event {
    let name = format!("{cmd:?}");
    let evt = e.apply_sync(cmd);
    assert!(!matches!(evt, Event::Error { .. }), "{name}: {evt:?}");
    if let Err(why) = e.check_invariants_for_fuzzing() {
        panic!("after {name}: {why}");
    }
    evt
}

fn top(idx: u32) -> BridgeBlockPath {
    BridgeBlockPath {
        steps: vec![BridgePathStep::Block { idx }],
    }
}

/// `before` / a 2 × 2 table (cells `r{row}c{col}`) / `after`, review mode
/// OFF.
fn table_engine() -> Engine {
    let mut e = Engine::new_headless(DocumentTree::from_paragraphs([
        "before".to_string(),
        "after".to_string(),
    ]));
    run(
        &mut e,
        Command::InsertTable {
            at: top(1),
            rows: 2,
            cols: 2,
        },
    );
    for row in 0..2 {
        for col in 0..2 {
            let at = BridgeLogicalPos {
                path: BridgeBlockPath {
                    steps: vec![
                        BridgePathStep::Block { idx: 1 },
                        BridgePathStep::Cell { row, col },
                        BridgePathStep::Block { idx: 0 },
                    ],
                },
                offset: 0,
            };
            /* `InsertText` types at the live caret: put it in the cell. */
            run(
                &mut e,
                Command::SetSelection {
                    range: BridgeLogicalRange {
                        start: at.clone(),
                        end: at.clone(),
                    },
                    caret: at,
                },
            );
            run(
                &mut e,
                Command::InsertText {
                    at: None,
                    text: format!("r{row}c{col}"),
                },
            );
        }
    }
    assert_eq!(
        cell_texts(&e),
        vec![
            vec!["r0c0".to_string(), "r0c1".to_string()],
            vec!["r1c0".to_string(), "r1c1".to_string()],
        ]
    );
    e
}

fn track(e: &mut Engine) {
    run(e, Command::ToggleTrackChanges { enabled: true });
}

fn table(e: &Engine) -> engine::Table {
    e.undo
        .current()
        .blocks
        .iter()
        .find_map(engine::Block::as_table)
        .expect("a table")
        .clone()
}

fn row_kinds(e: &Engine) -> Vec<Vec<engine::RevisionKind>> {
    table(e)
        .rows
        .iter()
        .map(|r| r.props.revisions.iter().map(|x| x.kind).collect())
        .collect()
}

fn cell_texts(e: &Engine) -> Vec<Vec<String>> {
    table(e)
        .rows
        .iter()
        .map(|r| {
            r.cells
                .iter()
                .map(|c| {
                    c.blocks
                        .iter()
                        .filter_map(engine::Block::as_paragraph)
                        .map(|p| p.text.clone())
                        .collect::<String>()
                })
                .collect()
        })
        .collect()
}

fn paragraphs(e: &Engine) -> Vec<String> {
    e.undo
        .current()
        .blocks
        .iter()
        .filter_map(engine::Block::as_paragraph)
        .map(|p| p.text.clone())
        .collect()
}

#[test]
fn a_tracked_inserted_row_is_listed_and_resolves() {
    let mut e = table_engine();
    track(&mut e);
    run(
        &mut e,
        Command::InsertRow {
            table_path: top(1),
            row: 0,
            side: bridge::InsertSide::After,
        },
    );
    use engine::RevisionKind::Insert;
    assert_eq!(row_kinds(&e), vec![vec![], vec![Insert], vec![]]);
    let rows = revision_rows(e.undo.current());
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(
        (rows[0].row, rows[0].kind, rows[0].depth),
        (Some(1), "insert", 1)
    );
    /* Reject it alone, by id: the row goes. */
    let mut rejected = table_engine();
    track(&mut rejected);
    run(
        &mut rejected,
        Command::InsertRow {
            table_path: top(1),
            row: 0,
            side: bridge::InsertSide::After,
        },
    );
    let id = revision_rows(rejected.undo.current())[0].revision_id;
    run(
        &mut rejected,
        Command::RejectRevision {
            block: 1,
            start: 0,
            end: 0,
            revision_id: Some(id),
        },
    );
    assert_eq!(table(&rejected).rows.len(), 2);
    assert!(!rejected.undo.current().has_revisions());
    /* Accept all: the row stays, unmarked. */
    run(&mut e, Command::AcceptAllRevisions);
    assert_eq!(row_kinds(&e), vec![Vec::new(); 3]);
}

#[test]
fn a_tracked_deleted_row_is_marked_with_its_text_and_resolves() {
    let mut e = table_engine();
    track(&mut e);
    run(
        &mut e,
        Command::DeleteRow {
            table_path: top(1),
            row: 1,
        },
    );
    use engine::RevisionKind::Delete;
    assert_eq!(row_kinds(&e), vec![vec![], vec![Delete]]);
    /* The row's cell text is marked deleted, still there. */
    assert_eq!(
        cell_texts(&e)[1],
        vec!["r1c0".to_string(), "r1c1".to_string()]
    );
    let t = table(&e);
    for cell in &t.rows[1].cells {
        let p = cell.blocks[0].as_paragraph().unwrap();
        assert!(
            p.revisions
                .iter()
                .any(|r| r.kind == Delete && r.start == 0 && r.end == 4),
            "{:?}",
            p.revisions
        );
    }
    let listed = revision_rows(e.undo.current());
    assert!(
        listed
            .iter()
            .any(|r| r.row == Some(1) && r.kind == "delete")
    );
    let mut rejected = table_engine();
    track(&mut rejected);
    run(
        &mut rejected,
        Command::DeleteRow {
            table_path: top(1),
            row: 1,
        },
    );
    run(&mut rejected, Command::RejectAllRevisions);
    assert_eq!(cell_texts(&rejected), cell_texts(&table_engine()));
    assert!(!rejected.undo.current().has_revisions());
    run(&mut e, Command::AcceptAllRevisions);
    assert_eq!(
        cell_texts(&e),
        vec![vec!["r0c0".to_string(), "r0c1".to_string()]]
    );
    assert!(!e.undo.current().has_revisions());
}

#[test]
fn deleting_your_own_inserted_row_removes_it() {
    let mut e = table_engine();
    track(&mut e);
    run(
        &mut e,
        Command::InsertRow {
            table_path: top(1),
            row: 1,
            side: bridge::InsertSide::After,
        },
    );
    run(
        &mut e,
        Command::DeleteRow {
            table_path: top(1),
            row: 2,
        },
    );
    assert_eq!(table(&e).rows.len(), 2);
    assert!(!e.undo.current().has_revisions());
}

#[test]
fn a_tracked_inserted_table_rejects_back_to_the_original_document() {
    for at in [1_u32, 2] {
        let mut e = Engine::new_headless(DocumentTree::from_paragraphs([
            "one".to_string(),
            "two".to_string(),
        ]));
        let original = paragraphs(&e);
        track(&mut e);
        run(
            &mut e,
            Command::InsertTable {
                at: top(at),
                rows: 2,
                cols: 3,
            },
        );
        use engine::RevisionKind::Insert;
        assert_eq!(row_kinds(&e), vec![vec![Insert], vec![Insert]], "at {at}");
        let listed = revision_rows(e.undo.current());
        assert_eq!(
            listed.iter().filter(|r| r.row.is_some()).count(),
            2,
            "at {at}: {listed:?}"
        );
        let mut rejected = Engine::new_headless(DocumentTree::from_paragraphs([
            "one".to_string(),
            "two".to_string(),
        ]));
        track(&mut rejected);
        run(
            &mut rejected,
            Command::InsertTable {
                at: top(at),
                rows: 2,
                cols: 3,
            },
        );
        run(&mut rejected, Command::RejectAllRevisions);
        assert_eq!(paragraphs(&rejected), original, "at {at}");
        assert!(
            rejected
                .undo
                .current()
                .blocks
                .iter()
                .all(|b| b.as_table().is_none()),
            "at {at}"
        );
        run(&mut e, Command::AcceptAllRevisions);
        assert!(!e.undo.current().has_revisions(), "at {at}");
        assert_eq!(table(&e).rows.len(), 2);
    }
}

#[test]
fn a_tracked_section_break_is_an_inserted_mark_and_resolves() {
    let fresh = || {
        let mut e = Engine::new_headless(DocumentTree::from_paragraphs([
            "alpha beta".to_string(),
            "gamma".to_string(),
        ]));
        track(&mut e);
        run(
            &mut e,
            Command::InsertSectionBreak {
                at: bpos_top(0, 5),
                kind: bridge::SectionBreakKind::NextPage,
            },
        );
        e
    };
    let mut e = fresh();
    assert_eq!(paragraphs(&e), vec!["alpha", " beta", "gamma"]);
    let p0 = e.undo.current().nth_paragraph(0).unwrap().clone();
    assert!(p0.section_end.is_some());
    assert_eq!(
        p0.mark_revisions.iter().map(|r| r.kind).collect::<Vec<_>>(),
        vec![engine::RevisionKind::Insert]
    );
    let listed = revision_rows(e.undo.current());
    assert!(
        listed
            .iter()
            .any(|r| r.mark && r.kind == "insert" && r.block == 0),
        "{listed:?}"
    );
    assert_eq!(e.undo.current().effective_sections().len(), 2);
    let mut rejected = fresh();
    run(&mut rejected, Command::RejectAllRevisions);
    assert_eq!(paragraphs(&rejected), vec!["alpha beta", "gamma"]);
    assert_eq!(rejected.undo.current().effective_sections().len(), 1);
    assert!(!rejected.undo.current().has_revisions());
    run(&mut e, Command::AcceptAllRevisions);
    assert_eq!(e.undo.current().effective_sections().len(), 2);
    assert!(!e.undo.current().has_revisions());
}

#[test]
fn untracked_structural_edits_record_nothing() {
    let mut e = table_engine();
    run(
        &mut e,
        Command::InsertRow {
            table_path: top(1),
            row: 0,
            side: bridge::InsertSide::Before,
        },
    );
    run(
        &mut e,
        Command::DeleteRow {
            table_path: top(1),
            row: 2,
        },
    );
    run(
        &mut e,
        Command::InsertSectionBreak {
            at: bpos_top(0, 2),
            kind: bridge::SectionBreakKind::Continuous,
        },
    );
    assert!(!e.undo.current().has_revisions());
}
