//! Issue #365 — tracked deletions over tables: whole rows (`<w:trPr>
//! <w:del/>`), per-cell sub-ranges inside one row, and body ↔ table
//! ranges; accept / reject (all, or one row by its id) remove or restore
//! the rows, comment anchors following.

use crate::{
    Block, BlockPath, DocumentTree, LogicalPos, Paragraph, PathStep, Revision, RevisionKind,
    RevisionSlot, UndoStack,
};

const ME: &str = "R";
const DATE: &str = "2026-10-09T00:00:00Z";

fn pos(block: u32, offset: u32) -> LogicalPos {
    LogicalPos::new(BlockPath::top(block), offset)
}

fn cell_path(table: u32, row: u32, col: u32, block: u32) -> BlockPath {
    BlockPath {
        steps: vec![
            PathStep::Block(table),
            PathStep::Cell { row, col },
            PathStep::Block(block),
        ],
    }
}

fn cpos(table: u32, row: u32, col: u32, offset: u32) -> LogicalPos {
    LogicalPos::new(cell_path(table, row, col, 0), offset)
}

/// `["before", <3 × 2 table "r{r}c{c}">, "after"]` — the table at block 1.
fn doc() -> DocumentTree {
    let mut d = DocumentTree::from_paragraphs(["before".to_string(), "after".to_string()])
        .insert_table(BlockPath::top(1), 3, 2);
    assert!(matches!(d.blocks[1], Block::Table(_)));
    for r in 0..3 {
        for c in 0..2 {
            d = d.insert_text(cpos(1, r, c, 0), &format!("r{r}c{c}"));
        }
    }
    d
}

fn table(d: &DocumentTree, block: u32) -> &crate::Table {
    d.table_at_path(&BlockPath::top(block)).expect("table")
}

/// Per row of the table at `block`: its revision kinds.
fn row_revs(d: &DocumentTree, block: u32) -> Vec<Vec<RevisionKind>> {
    table(d, block)
        .rows
        .iter()
        .map(|r| r.props.revisions.iter().map(|x| x.kind).collect())
        .collect()
}

/// Per row: its cells' texts.
fn cells(d: &DocumentTree, block: u32) -> Vec<Vec<String>> {
    table(d, block)
        .rows
        .iter()
        .map(|r| {
            r.cells
                .iter()
                .map(|c| {
                    c.blocks
                        .iter()
                        .filter_map(Block::as_paragraph)
                        .map(|p| p.text.clone())
                        .collect::<Vec<_>>()
                        .join("|")
                })
                .collect()
        })
        .collect()
}

/// Per row: the text revisions of its cells' paragraphs.
fn cell_revs(d: &DocumentTree, block: u32) -> Vec<Vec<Vec<(RevisionKind, u32, u32)>>> {
    table(d, block)
        .rows
        .iter()
        .map(|r| {
            r.cells
                .iter()
                .map(|c| {
                    c.blocks
                        .iter()
                        .filter_map(Block::as_paragraph)
                        .flat_map(|p| p.revisions.iter().map(|x| (x.kind, x.start, x.end)))
                        .collect()
                })
                .collect()
        })
        .collect()
}

/// The top-level shape: paragraph texts, `<table>` for a table.
fn shape(d: &DocumentTree) -> Vec<String> {
    d.blocks
        .iter()
        .map(|b| match b {
            Block::Paragraph(p) => p.text.clone(),
            Block::Table(t) => format!("<table {}>", t.rows.len()),
        })
        .collect()
}

fn del(d: &DocumentTree, start: LogicalPos, end: LogicalPos) -> crate::TrackedDeletion {
    d.try_tracked_delete_range(start, end, ME, DATE)
        .expect("recorded")
}

fn in_undo(states: &[&DocumentTree]) {
    let mut undo = UndoStack::new(states[0].clone(), 100);
    for s in &states[1..] {
        undo.push((*s).clone());
    }
}

const DEL: RevisionKind = RevisionKind::Delete;

/// A range from body text over a whole table into the next paragraph:
/// the body text and the swallowed mark are marked, every row is marked
/// deleted with its contents. Accept gives exactly the untracked delete's
/// result; reject restores everything.
#[test]
fn a_range_over_a_whole_table_marks_its_rows_and_contents() {
    let d = doc();
    let t = del(&d, pos(0, 2), pos(2, 2));
    assert_eq!(shape(&t.doc), shape(&d), "nothing removed yet");
    assert_eq!(row_revs(&t.doc, 1), vec![vec![DEL]; 3]);
    assert_eq!(cell_revs(&t.doc, 1), vec![vec![vec![(DEL, 0, 4)]; 2]; 3]);
    let before = t.doc.nth_paragraph(0).unwrap();
    assert_eq!(
        before
            .mark_revisions
            .iter()
            .map(|r| r.kind)
            .collect::<Vec<_>>(),
        vec![DEL]
    );
    assert_eq!(t.start, pos(0, 2));
    assert_eq!(t.end, pos(2, 2));
    let accepted = t.doc.resolve_all_revisions(true);
    let plain = d.delete_range(pos(0, 2), pos(2, 2));
    assert_eq!(shape(&accepted), vec!["beter"]);
    assert_eq!(shape(&accepted), shape(&plain));
    assert!(!accepted.has_revisions());
    let rejected = t.doc.resolve_all_revisions(false);
    assert_eq!(shape(&rejected), shape(&d));
    assert_eq!(cells(&rejected, 1), cells(&d, 1));
    assert!(!rejected.has_revisions());
    in_undo(&[&d, &t.doc, &accepted]);
    in_undo(&[&d, &t.doc, &rejected]);
}

/// Two rows selected (first cell of one to the end of the last cell of
/// the next): both rows are deleted whole; accept removes them, reject
/// keeps them.
#[test]
fn a_range_across_rows_deletes_them_whole() {
    let d = doc();
    let t = del(&d, cpos(1, 0, 0, 0), cpos(1, 1, 1, 4));
    assert_eq!(row_revs(&t.doc, 1), vec![vec![DEL], vec![DEL], vec![]]);
    assert_eq!(
        cell_revs(&t.doc, 1),
        vec![
            vec![vec![(DEL, 0, 4)]; 2],
            vec![vec![(DEL, 0, 4)]; 2],
            vec![vec![]; 2]
        ]
    );
    let accepted = t.doc.resolve_all_revisions(true);
    assert_eq!(cells(&accepted, 1), vec![vec!["r2c0", "r2c1"]]);
    assert!(!accepted.has_revisions());
    let rejected = t.doc.resolve_all_revisions(false);
    assert_eq!(cells(&rejected, 1), cells(&d, 1));
    assert!(!rejected.has_revisions());
    /* Rows are deleted WHOLE even when the range only touches them. */
    let partial = del(&d, cpos(1, 0, 1, 2), cpos(1, 2, 0, 1));
    assert_eq!(row_revs(&partial.doc, 1), vec![vec![DEL]; 3]);
    assert_eq!(
        shape(&partial.doc.resolve_all_revisions(true)),
        vec!["before", "after"]
    );
}

/// Across cells of ONE row: each cell's sub-range, no row revision.
#[test]
fn a_range_across_cells_of_one_row_deletes_each_sub_range() {
    let d = doc();
    let t = del(&d, cpos(1, 0, 0, 1), cpos(1, 0, 1, 2));
    assert_eq!(row_revs(&t.doc, 1), vec![Vec::<RevisionKind>::new(); 3]);
    assert_eq!(
        cell_revs(&t.doc, 1)[0],
        vec![vec![(DEL, 1, 4)], vec![(DEL, 0, 2)]]
    );
    assert_eq!(t.end, cpos(1, 0, 1, 2));
    let accepted = t.doc.resolve_all_revisions(true);
    assert_eq!(cells(&accepted, 1)[0], vec!["r", "c1"]);
    assert_eq!(cells(&t.doc.resolve_all_revisions(false), 1), cells(&d, 1));
}

/// From body text into a table: the body part plus the whole rows the
/// range reaches. The body paragraph's mark is not swallowed (the range
/// ends inside the table: nothing to merge with).
#[test]
fn a_range_from_body_text_into_a_table_deletes_whole_rows() {
    let d = doc();
    let t = del(&d, pos(0, 2), cpos(1, 1, 0, 1));
    assert_eq!(row_revs(&t.doc, 1), vec![vec![DEL], vec![DEL], vec![]]);
    let p0 = t.doc.nth_paragraph(0).unwrap();
    assert!(p0.mark_revisions.is_empty());
    assert_eq!(
        p0.revisions
            .iter()
            .map(|r| (r.kind, r.start, r.end))
            .collect::<Vec<_>>(),
        vec![(DEL, 2, 6)]
    );
    let accepted = t.doc.resolve_all_revisions(true);
    assert_eq!(shape(&accepted), vec!["be", "<table 1>", "after"]);
    assert_eq!(shape(&t.doc.resolve_all_revisions(false)), shape(&d));
}

/// From inside a table out into body text: the rows from the start's
/// row down, then the body part.
#[test]
fn a_range_from_a_table_into_body_text_deletes_rows_and_text() {
    let d = doc();
    let t = del(&d, cpos(1, 1, 1, 0), pos(2, 3));
    assert_eq!(row_revs(&t.doc, 1), vec![vec![], vec![DEL], vec![DEL]]);
    assert_eq!(t.end, pos(2, 3));
    let accepted = t.doc.resolve_all_revisions(true);
    assert_eq!(shape(&accepted), vec!["before", "<table 1>", "er"]);
    assert_eq!(cells(&accepted, 1), vec![vec!["r0c0", "r0c1"]]);
}

/// A row the reviewer inserted (a tracked paste of theirs) is removed
/// outright by their own deletion; a table left without rows goes, and
/// the paragraphs around it merge when the range swallows the mark.
#[test]
fn deleting_your_own_inserted_rows_removes_them() {
    let mut d = doc();
    let mut t = table(&d, 1).clone();
    for row in &mut t.rows {
        row.props.revisions = vec![Revision {
            start: 0,
            end: 0,
            kind: RevisionKind::Insert,
            author: ME.into(),
            date: DATE.into(),
            id: None,
            prev_attrs: None,
            move_name: None,
        }];
    }
    let mut blocks = d.blocks.clone();
    blocks.set(1, Block::Table(t));
    d.blocks = blocks;
    let two = del(&d, cpos(1, 0, 0, 0), cpos(1, 1, 1, 4));
    assert_eq!(cells(&two.doc, 1), vec![vec!["r2c0", "r2c1"]]);
    assert_eq!(row_revs(&two.doc, 1), vec![vec![RevisionKind::Insert]]);
    let all = del(&d, pos(0, 6), pos(2, 0));
    assert_eq!(shape(&all.doc), vec!["before", "after"]);
    assert_eq!(all.end, pos(1, 0));
    assert_eq!(
        shape(&all.doc.resolve_all_revisions(true)),
        vec!["beforeafter"]
    );
}

/// The review UI lists each row change under its own id (ahead of the
/// row's cell paragraphs); a single accept resolves exactly that row.
#[test]
fn a_single_row_resolves_by_its_id() {
    let d = doc();
    let t = del(&d, cpos(1, 0, 0, 0), cpos(1, 1, 1, 4));
    let entries = t.doc.revision_entries();
    let rows: Vec<_> = entries
        .iter()
        .filter(|e| matches!(e.at.slot, RevisionSlot::Row { .. }))
        .collect();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|e| e.paragraph.is_none()));
    assert_eq!(rows[0].at.path, BlockPath::top(1));
    let second = t.doc.revision_by_id(rows[1].id).expect("listed");
    assert_eq!(second.slot, RevisionSlot::Row { row: 1, index: 0 });
    let one = t.doc.resolve_revision(&second, true).expect("resolved");
    assert_eq!(cells(&one, 1).len(), 2);
    assert_eq!(cells(&one, 1)[1], vec!["r2c0", "r2c1"]);
    assert_eq!(row_revs(&one, 1), vec![vec![DEL], vec![]]);
    /* The first row's id is unchanged by resolving its neighbour. */
    assert!(one.revision_by_id(rows[0].id).is_some());
    let rejected = t
        .doc
        .resolve_revision(&t.doc.revision_by_id(rows[0].id).unwrap(), false)
        .unwrap();
    assert_eq!(row_revs(&rejected, 1), vec![vec![], vec![DEL], vec![]]);
}

/// A comment in the paragraph after the table keeps its text when
/// accepting removes the rows and the table; a comment inside a removed
/// row collapses onto the next surviving row.
#[test]
fn comments_follow_through_row_removal() {
    let d = doc();
    let (d, _) = d.insert_comment(pos(2, 1), pos(2, 4), "c".into(), "A".into(), "d".into());
    let (d, _) = d.insert_comment(
        cpos(1, 0, 1, 0),
        cpos(1, 0, 1, 2),
        "in".into(),
        "A".into(),
        "d".into(),
    );
    let covered = |d: &DocumentTree, i: usize| {
        let r = &d.comment_ranges[i];
        d.text_range(r.start.clone(), r.end.clone())
    };
    let rows = del(&d, cpos(1, 0, 0, 0), cpos(1, 0, 1, 4)).doc;
    /* One row selected whole across its cells → per-cell sub-ranges. */
    assert!(row_revs(&rows, 1).iter().all(Vec::is_empty));
    let rows = del(&d, cpos(1, 0, 0, 0), cpos(1, 1, 0, 0)).doc;
    let accepted = rows.resolve_all_revisions(true);
    assert_eq!(covered(&accepted, 0), "fte");
    let inner = &accepted.comment_ranges[1];
    assert_eq!(inner.start, cpos(1, 0, 0, 0));
    assert_eq!(inner.start, inner.end);
    let over = del(&d, pos(0, 0), pos(2, 0))
        .doc
        .resolve_all_revisions(true);
    assert_eq!(shape(&over), vec!["after"]);
    assert_eq!(covered(&over, 0), "fte");
}

/// A deleted row holding a nested table: the nested rows are marked
/// too; reject leaves nothing pending, accept removes the row.
#[test]
fn a_deleted_row_marks_a_nested_tables_rows() {
    let d = doc();
    /* A 1 × 1 table nested in cell (1, 0), in front of its paragraph. */
    let inner =
        DocumentTree::from_paragraphs(["x".to_string()]).insert_table(BlockPath::top(0), 1, 1);
    let nested = inner.blocks[0].clone();
    let mut t = table(&d, 1).clone();
    t.rows[1].cells[0].blocks.insert(0, nested);
    let mut blocks = d.blocks.clone();
    blocks.set(1, Block::Table(t));
    let mut d = d;
    d.blocks = blocks;
    let path = cell_path(1, 1, 0, 1);
    assert_eq!(
        d.paragraph_at_path(&path).map(|p| p.text.as_str()),
        Some("r1c0")
    );
    let t = del(&d, cpos(1, 0, 0, 0), LogicalPos::new(path, 4));
    let nested_revs = |d: &DocumentTree| -> Vec<Vec<RevisionKind>> {
        d.table_at_path(&cell_path(1, 1, 0, 0))
            .map(|t| {
                t.rows
                    .iter()
                    .map(|r| r.props.revisions.iter().map(|x| x.kind).collect())
                    .collect()
            })
            .unwrap_or_default()
    };
    assert_eq!(nested_revs(&t.doc), vec![vec![DEL]]);
    assert!(t.doc.has_revisions());
    let rejected = t.doc.resolve_all_revisions(false);
    assert!(!rejected.has_revisions());
    assert_eq!(nested_revs(&rejected), vec![Vec::<RevisionKind>::new()]);
    let accepted = t.doc.resolve_all_revisions(true);
    assert_eq!(cells(&accepted, 1), vec![vec!["r2c0", "r2c1"]]);
}

/// Issue #366 × #365 — a tracked rich paste of a table records its rows
/// as inserted: rejecting the paste removes the table and merges the
/// paragraphs back.
#[test]
fn a_tracked_table_paste_rejects_to_the_original() {
    let d = DocumentTree::from_paragraphs(["alpha beta".to_string()]);
    let fragment = DocumentTree::from_paragraphs(["".to_string()])
        .insert_table(BlockPath::top(0), 2, 1)
        .insert_text(cpos(0, 0, 0, 0), "cell");
    let pasted_table = fragment.blocks[0].clone();
    let blocks = [
        pasted_table,
        Block::Paragraph(Paragraph {
            text: "x".into(),
            ..Paragraph::default()
        }),
    ];
    let (pasted, _) = d.tracked_insert_rich_blocks(pos(0, 5), &blocks, ME, DATE);
    assert_eq!(shape(&pasted), vec!["alpha", "<table 2>", "x beta"]);
    assert_eq!(
        row_revs(&pasted, 1),
        vec![vec![RevisionKind::Insert], vec![RevisionKind::Insert]]
    );
    assert_eq!(
        pasted.nth_paragraph(0).unwrap().mark_revisions[0].kind,
        RevisionKind::Insert
    );
    let rejected = pasted.resolve_all_revisions(false);
    assert_eq!(shape(&rejected), vec!["alpha beta"]);
    assert!(!rejected.has_revisions());
    let accepted = pasted.resolve_all_revisions(true);
    assert_eq!(shape(&accepted), vec!["alpha", "<table 2>", "x beta"]);
    assert!(!accepted.has_revisions());
}

/// Snapshot round trip of a tree carrying row revisions; row properties
/// without any encode exactly as before (no key).
#[test]
fn row_revisions_snapshot_byte_stably() {
    use crate::snapshot::{decode, encode};
    let none = rmp_serde::to_vec_named(&crate::RowProperties::default()).unwrap();
    assert!(!none.windows(9).any(|w| w == b"revisions"));
    let d = doc();
    let t = del(&d, cpos(1, 0, 0, 0), cpos(1, 1, 1, 4));
    let bytes = encode(&t.doc).unwrap();
    let back: DocumentTree = decode(&bytes).unwrap().payload;
    assert_eq!(row_revs(&back, 1), row_revs(&t.doc, 1));
    assert_eq!(encode(&back).unwrap(), bytes);
}
