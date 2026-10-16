//! Issue #440 — tracked changes inside tables nested in table cells are
//! listed (`revisions_snapshot`, with their full path and depth), resolved
//! by Accept / Reject All, and addressable one by one by `revision_id`,
//! at depth 2 and 3 — end to end through real `Command`s on the headless
//! `fuzz-native` engine, with `Engine::check_invariants_for_fuzzing`
//! after every command.

use super::*;

const AUTHOR: &str = "Bob";

fn para(text: &str) -> engine::Paragraph {
    engine::Paragraph {
        text: text.to_string(),
        ..Default::default()
    }
}

fn change(kind: engine::RevisionKind, start: u32, end: u32) -> engine::Revision {
    engine::Revision {
        start,
        end,
        kind,
        author: AUTHOR.to_string(),
        date: "2026-01-01T00:00:00Z".to_string(),
        id: None,
        prev_attrs: None,
        move_name: None,
    }
}

/// `intro` / a `depth`-level tower of one-cell tables / `outro`. Level `i`
/// (a table at nesting depth `i + 1`) holds the paragraph `"level i INS"`
/// and the next level. The paragraph of level `depth - 1` carries a
/// tracked insertion over `INS`, and that level's row a tracked row
/// insertion.
fn tower_doc(depth: usize) -> DocumentTree {
    let mut inner: Option<engine::Table> = None;
    for i in (0..depth).rev() {
        let mut p = para(&format!("level {i} INS"));
        let mut row = engine::TableRow::default();
        if i + 1 == depth {
            let s = p.text.len() as u32 - 3;
            p.revisions
                .push(change(engine::RevisionKind::Insert, s, s + 3));
            row.props
                .revisions
                .push(change(engine::RevisionKind::Insert, 0, 0));
        }
        let mut blocks = vec![engine::Block::Paragraph(p)];
        if let Some(t) = inner.take() {
            blocks.push(engine::Block::Table(t));
        }
        row.cells = vec![engine::TableCell {
            blocks,
            ..Default::default()
        }];
        inner = Some(engine::Table {
            grid: vec![2400],
            rows: vec![row],
            dirty: true,
            ..Default::default()
        });
    }
    let mut doc = DocumentTree::new();
    doc.blocks.clear();
    doc.blocks
        .push_back(engine::Block::Paragraph(para("intro")));
    doc.blocks
        .push_back(engine::Block::Table(inner.expect("depth >= 1")));
    doc.blocks
        .push_back(engine::Block::Paragraph(para("outro")));
    doc
}

fn run(e: &mut Engine, cmd: Command) -> Event {
    let name = format!("{cmd:?}");
    let evt = e.apply_sync(cmd);
    assert!(!matches!(evt, Event::Error { .. }), "{name}: {evt:?}");
    if let Err(why) = e.check_invariants_for_fuzzing() {
        panic!("after {name}: {why}");
    }
    evt
}

/// Every paragraph text of the document, depth-first, with its depth.
fn nested_texts(doc: &DocumentTree) -> Vec<(u32, String)> {
    let mut out = Vec::new();
    engine::for_each_paragraph_nested(&doc.blocks, &mut |path, p| {
        let depth = path
            .steps
            .iter()
            .filter(|s| matches!(s, engine::PathStep::Cell { .. }))
            .count() as u32;
        out.push((depth, p.text.clone()));
    });
    out
}

fn no_revisions_left(doc: &DocumentTree) -> bool {
    !doc.has_revisions()
}

#[test]
fn nested_revisions_are_listed_with_their_path_and_depth() {
    for depth in [2_usize, 3] {
        let e = Engine::new_headless(tower_doc(depth));
        let rows = revision_rows(e.undo.current());
        assert_eq!(rows.len(), 2, "depth {depth}: {rows:?}");
        let row = rows.iter().find(|r| r.row.is_some()).expect("row change");
        let text = rows.iter().find(|r| r.row.is_none()).expect("text change");
        assert_eq!(row.depth, depth as u32);
        assert_eq!(text.depth, depth as u32);
        assert_eq!(row.block, 1);
        assert_eq!(text.block, 1);
        /* The table path: Block + (Cell, Block) per level above it; the
        paragraph path one Cell + Block deeper. */
        assert_eq!(row.path.steps.len(), 1 + 2 * (depth - 1));
        assert_eq!(text.path.steps.len(), 1 + 2 * depth);
        assert_eq!((text.start, text.end), (8, 11));
        assert_ne!(row.revision_id, text.revision_id);
        e.check_invariants_for_fuzzing().unwrap();
    }
}

#[test]
fn accept_all_resolves_nested_revisions_at_depth_two_and_three() {
    for depth in [2_usize, 3] {
        let mut e = Engine::new_headless(tower_doc(depth));
        run(&mut e, Command::AcceptAllRevisions);
        let doc = e.undo.current();
        assert!(no_revisions_left(doc), "depth {depth}");
        assert!(revision_rows(doc).is_empty());
        let deepest = format!("level {} INS", depth - 1);
        assert!(
            nested_texts(doc)
                .iter()
                .any(|(d, t)| *d == depth as u32 && *t == deepest),
            "depth {depth}: {:?}",
            nested_texts(doc)
        );
    }
}

#[test]
fn reject_all_removes_nested_insertions_at_depth_two_and_three() {
    for depth in [2_usize, 3] {
        let mut e = Engine::new_headless(tower_doc(depth));
        run(&mut e, Command::RejectAllRevisions);
        let doc = e.undo.current();
        assert!(no_revisions_left(doc), "depth {depth}");
        /* The inserted row was the deepest table's only row: the table
        goes, its text with it; the level above keeps its text. */
        let texts = nested_texts(doc);
        assert!(
            texts.iter().all(|(d, _)| *d < depth as u32),
            "depth {depth}: {texts:?}"
        );
        assert!(
            texts
                .iter()
                .any(|(d, t)| *d == depth as u32 - 1 && *t == format!("level {} INS", depth - 2)),
            "depth {depth}: {texts:?}"
        );
    }
}

#[test]
fn single_nested_revisions_resolve_by_id() {
    for depth in [2_usize, 3] {
        let mut e = Engine::new_headless(tower_doc(depth));
        let rows = revision_rows(e.undo.current());
        let text = rows.iter().find(|r| r.row.is_none()).unwrap().clone();
        let row = rows.iter().find(|r| r.row.is_some()).unwrap().clone();
        /* Reject the text insertion alone: its bytes go, the row change
        stays listed. */
        run(
            &mut e,
            Command::RejectRevision {
                block: text.block,
                start: text.start,
                end: text.end,
                revision_id: Some(text.revision_id),
            },
        );
        let left = revision_rows(e.undo.current());
        assert_eq!(left.len(), 1, "depth {depth}: {left:?}");
        /* The row change's id hashes the row's text (issue #304), which
        the rejection just changed: same change, fresh id. */
        assert_eq!((left[0].row, left[0].depth), (row.row, row.depth));
        let row = left[0].clone();
        let deepest = format!("level {} ", depth - 1);
        assert!(
            nested_texts(e.undo.current())
                .iter()
                .any(|(d, t)| *d == depth as u32 && *t == deepest),
            "depth {depth}: {:?}",
            nested_texts(e.undo.current())
        );
        /* Accept the row insertion by id: nothing left, the row stays. */
        run(
            &mut e,
            Command::AcceptRevision {
                block: row.block,
                start: 0,
                end: 0,
                revision_id: Some(row.revision_id),
            },
        );
        assert!(no_revisions_left(e.undo.current()), "depth {depth}");
        assert!(
            nested_texts(e.undo.current())
                .iter()
                .any(|(d, _)| *d == depth as u32),
            "depth {depth}"
        );
    }
}

#[test]
fn a_tracked_delete_over_a_nested_table_marks_its_text_at_every_depth() {
    for depth in [2_usize, 3] {
        let mut doc = tower_doc(depth);
        /* Start clean: this test is about what the deletion records. */
        doc = doc.resolve_all_revisions(true);
        let mut e = Engine::new_headless(doc);
        run(&mut e, Command::ToggleTrackChanges { enabled: true });
        run(
            &mut e,
            Command::DeleteRange {
                range: BridgeLogicalRange {
                    start: bpos_top(0, 2),
                    end: bpos_top(2, 2),
                },
            },
        );
        /* Every nested paragraph's text is now covered by a deletion. */
        let mut uncovered = Vec::new();
        engine::for_each_paragraph_nested(&e.undo.current().blocks, &mut |path, p| {
            if path.steps.len() > 1 {
                let covered: u32 = p
                    .revisions
                    .iter()
                    .filter(|r| r.kind == engine::RevisionKind::Delete)
                    .map(|r| r.end - r.start)
                    .sum();
                if covered != p.text.len() as u32 {
                    uncovered.push((path.steps.len(), p.text.clone()));
                }
            }
        });
        assert!(uncovered.is_empty(), "depth {depth}: {uncovered:?}");
        let listed = revision_rows(e.undo.current());
        assert!(
            listed
                .iter()
                .any(|r| r.depth == depth as u32 && r.row.is_none() && r.kind == "delete"),
            "depth {depth}: {listed:?}"
        );
        /* Accept: the table goes whole; nothing tracked is left. */
        run(&mut e, Command::AcceptAllRevisions);
        let doc = e.undo.current();
        assert!(no_revisions_left(doc), "depth {depth}");
        assert!(
            doc.blocks.iter().all(|b| b.as_table().is_none()),
            "depth {depth}"
        );
    }
}
