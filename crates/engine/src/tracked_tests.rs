//! Issues #301 / #298 — recording structural tracked changes: the tracked
//! paragraph break and the cross-paragraph tracked deletion, resolved by
//! accept / reject.

use crate::{
    Block, BlockPath, DocumentTree, LogicalPos, Paragraph, PathStep, Revision, RevisionKind,
    SourceMarkup, SourceRun, UndoStack,
};

const ME: &str = "R";
const DATE: &str = "2026-10-09T00:00:00Z";

fn pos(block: u32, offset: u32) -> LogicalPos {
    LogicalPos::new(BlockPath::top(block), offset)
}

fn rev(kind: RevisionKind, start: u32, end: u32, author: &str) -> Revision {
    Revision {
        start,
        end,
        kind,
        author: author.into(),
        date: "d".into(),
        id: None,
        prev_attrs: None,
        move_name: None,
    }
}

fn texts(d: &DocumentTree) -> Vec<String> {
    d.blocks
        .iter()
        .filter_map(Block::as_paragraph)
        .map(|p| p.text.clone())
        .collect()
}

fn marks(d: &DocumentTree) -> Vec<Vec<(RevisionKind, String)>> {
    d.blocks
        .iter()
        .filter_map(Block::as_paragraph)
        .map(|p| {
            p.mark_revisions
                .iter()
                .map(|r| (r.kind, r.author.clone()))
                .collect()
        })
        .collect()
}

fn ranges(d: &DocumentTree, block: u32) -> Vec<(RevisionKind, u32, u32)> {
    let mut v: Vec<_> = d
        .nth_paragraph(block)
        .map(|p| {
            p.revisions
                .iter()
                .map(|r| (r.kind, r.start, r.end))
                .collect()
        })
        .unwrap_or_default();
    v.sort_by_key(|&(_, s, e)| (s, e));
    v
}

/// One source run over each top-level paragraph's text, so the #250
/// in-step assertion (`UndoStack::push`, test builds) has markup to check.
fn with_markup(mut d: DocumentTree) -> DocumentTree {
    let mut blocks = d.blocks.clone();
    for b in blocks.iter_mut() {
        if let Block::Paragraph(p) = b {
            let len = p.text.len() as u32;
            p.source_markup = Some(Box::new(SourceMarkup {
                text_len: len,
                runs: vec![SourceRun {
                    start: 0,
                    end: len,
                    ..SourceRun::default()
                }],
                ..SourceMarkup::default()
            }));
        }
    }
    d.blocks = blocks;
    d
}

fn in_step(d: &DocumentTree) -> bool {
    d.blocks.iter().filter_map(Block::as_paragraph).all(|p| {
        p.source_markup
            .as_deref()
            .is_none_or(|m| m.offsets_valid(p.text.len()))
    })
}

fn covered(d: &DocumentTree) -> String {
    let r = &d.comment_ranges[0];
    d.text_range(r.start.clone(), r.end.clone())
}

/* ======================= issue #301 — the tracked paragraph break ==== */

/// Enter mid-paragraph with review mode on: the new mark (ending the left
/// half) is an insertion by the reviewer; accept keeps the break, reject
/// — all or the single mark row — merges the halves back.
#[test]
fn tracked_enter_records_an_inserted_mark_that_reject_merges() {
    let d = with_markup(DocumentTree::from_paragraphs(["alpha beta".to_string()]));
    let split = d.tracked_split_paragraph(pos(0, 5), ME, DATE);
    assert_eq!(texts(&split), vec!["alpha", " beta"]);
    assert_eq!(
        marks(&split),
        vec![vec![(RevisionKind::Insert, ME.to_string())], vec![]]
    );
    assert_eq!(split.nth_paragraph(0).unwrap().mark_revisions[0].date, DATE);
    assert!(split.has_revisions());
    let mut undo = UndoStack::new(d.clone(), 100);
    undo.push(split.clone());
    let rejected = split.resolve_all_revisions(false);
    undo.push(rejected.clone());
    assert_eq!(texts(&rejected), vec!["alpha beta"]);
    assert!(!rejected.has_revisions());
    assert!(in_step(&rejected));
    let accepted = split.resolve_all_revisions(true);
    assert_eq!(texts(&accepted), vec!["alpha", " beta"]);
    assert!(!accepted.has_revisions());
    /* The sidebar's single decision on the mark row (the empty range at
    the paragraph end). */
    assert_eq!(
        texts(&split.reject_revision_at(0, 5, 5)),
        vec!["alpha beta"]
    );
    let kept = split.accept_revision_at(0, 5, 5);
    assert_eq!(texts(&kept), vec!["alpha", " beta"]);
    assert!(!kept.has_revisions());
    /* Undo walks back to the unsplit paragraph. */
    assert!(undo.undo());
    assert!(undo.undo());
    assert_eq!(texts(undo.current()), vec!["alpha beta"]);
}

/// Enter at the start and at the end of a paragraph: reject restores the
/// one paragraph either way.
#[test]
fn tracked_enter_at_either_end_rejects_to_one_paragraph() {
    let d = DocumentTree::from_paragraphs(["alpha".to_string(), "next".to_string()]);
    let at_start = d.tracked_split_paragraph(pos(0, 0), ME, DATE);
    assert_eq!(texts(&at_start), vec!["", "alpha", "next"]);
    assert_eq!(
        texts(&at_start.resolve_all_revisions(false)),
        vec!["alpha", "next"]
    );
    let at_end = d.tracked_split_paragraph(pos(0, 5), ME, DATE);
    assert_eq!(texts(&at_end), vec!["alpha", "", "next"]);
    assert_eq!(
        texts(&at_end.resolve_all_revisions(false)),
        vec!["alpha", "next"]
    );
    /* Two Enters in a row: two inserted marks, both reverted. */
    let twice = at_end.tracked_split_paragraph(pos(1, 0), ME, DATE);
    assert_eq!(texts(&twice), vec!["alpha", "", "", "next"]);
    assert_eq!(
        texts(&twice.resolve_all_revisions(false)),
        vec!["alpha", "next"]
    );
}

/// A paragraph break inside the reviewer's own pending insertion keeps
/// both halves tracked (splitting used to drop every revision): reject
/// removes the typed text AND the break.
#[test]
fn enter_inside_a_pending_insertion_keeps_both_halves_tracked() {
    let d = DocumentTree::from_paragraphs(["base".to_string()]).tracked_insert_text(
        pos(0, 4),
        " typed",
        ME.into(),
        DATE.into(),
    );
    assert_eq!(ranges(&d, 0), vec![(RevisionKind::Insert, 4, 10)]);
    let split = d.tracked_split_paragraph(pos(0, 7), ME, DATE);
    assert_eq!(texts(&split), vec!["base ty", "ped"]);
    assert_eq!(ranges(&split, 0), vec![(RevisionKind::Insert, 4, 7)]);
    assert_eq!(ranges(&split, 1), vec![(RevisionKind::Insert, 0, 3)]);
    assert_eq!(texts(&split.resolve_all_revisions(false)), vec!["base"]);
    let accepted = split.resolve_all_revisions(true);
    assert_eq!(texts(&accepted), vec!["base ty", "ped"]);
    assert!(!accepted.has_revisions());
}

/// The untracked split carries revisions too; the right piece of a cut
/// change drops its source id (ids are document-unique).
#[test]
fn a_plain_split_cuts_a_straddling_revision_in_two() {
    let mut d = DocumentTree::from_paragraphs(["abcdef".to_string()]);
    let mut blocks = d.blocks.clone();
    if let Block::Paragraph(p) = &mut blocks[0] {
        p.revisions = vec![
            Revision {
                id: Some(7),
                ..rev(RevisionKind::Delete, 2, 4, "A")
            },
            Revision {
                id: Some(8),
                ..rev(RevisionKind::Insert, 4, 6, "A")
            },
        ];
    }
    d.blocks = blocks;
    let split = d.split_paragraph(pos(0, 3));
    assert_eq!(ranges(&split, 0), vec![(RevisionKind::Delete, 2, 3)]);
    assert_eq!(
        ranges(&split, 1),
        vec![(RevisionKind::Delete, 0, 1), (RevisionKind::Insert, 1, 3)]
    );
    let right = &split.nth_paragraph(1).unwrap().revisions;
    let ids: Vec<Option<u32>> = right.iter().map(|r| r.id).collect();
    assert_eq!(ids, vec![None, Some(8)]);
    assert_eq!(split.nth_paragraph(0).unwrap().revisions[0].id, Some(7));
}

/// A comment across the split point is restored by the reject.
#[test]
fn rejecting_a_tracked_enter_restores_the_comment() {
    let d = with_markup(DocumentTree::from_paragraphs([
        "alpha beta gamma".to_string()
    ]));
    let (d, _) = d.insert_comment(pos(0, 3), pos(0, 8), "c".into(), "A".into(), "d".into());
    assert_eq!(covered(&d), "ha be");
    let split = d.tracked_split_paragraph(pos(0, 6), ME, DATE);
    assert_eq!(texts(&split), vec!["alpha ", "beta gamma"]);
    let rejected = split.resolve_all_revisions(false);
    assert_eq!(texts(&rejected), vec!["alpha beta gamma"]);
    assert_eq!(covered(&rejected), "ha be");
    assert!(in_step(&rejected));
}

/// In a table cell the break and its reject stay inside the cell.
#[test]
fn a_tracked_enter_in_a_table_cell_rejects_inside_the_cell() {
    let d =
        DocumentTree::from_paragraphs(["before".to_string()]).insert_table(BlockPath::top(1), 1, 1);
    let table = d
        .blocks
        .iter()
        .position(|b| matches!(b, Block::Table(_)))
        .expect("table") as u32;
    let cell = |steps_block: u32| BlockPath {
        steps: vec![
            PathStep::Block(table),
            PathStep::Cell { row: 0, col: 0 },
            PathStep::Block(steps_block),
        ],
    };
    let typed = d.insert_text(LogicalPos::new(cell(0), 0), "xy");
    let split = typed.tracked_split_paragraph(LogicalPos::new(cell(0), 1), ME, DATE);
    let cell_texts = |d: &DocumentTree| -> Vec<String> {
        match &d.blocks[table as usize] {
            Block::Table(t) => t.rows[0].cells[0]
                .blocks
                .iter()
                .filter_map(Block::as_paragraph)
                .map(|p: &Paragraph| p.text.clone())
                .collect(),
            _ => Vec::new(),
        }
    };
    assert_eq!(cell_texts(&split), vec!["x", "y"]);
    assert!(split.has_revisions());
    assert_eq!(cell_texts(&split.resolve_all_revisions(false)), vec!["xy"]);
    assert_eq!(
        cell_texts(&split.resolve_all_revisions(true)),
        vec!["x", "y"]
    );
}

/* ======================= issue #298 — cross-paragraph tracked delete ==== */

fn del(d: &DocumentTree, s: (u32, u32), e: (u32, u32)) -> crate::TrackedDeletion {
    d.try_tracked_delete_range(pos(s.0, s.1), pos(e.0, e.1), ME, DATE)
        .expect("recorded")
}

fn end_of(t: &crate::TrackedDeletion) -> (u32, u32) {
    (t.end.path.last_block_index().unwrap(), t.end.offset)
}

/// The head's tail, the whole middle paragraph and the last one's head
/// are marked deleted, and so is every swallowed mark; accept merges what
/// is left, reject restores everything.
#[test]
fn a_cross_paragraph_tracked_delete_marks_text_and_marks() {
    let d = with_markup(DocumentTree::from_paragraphs([
        "alpha beta".to_string(),
        "middle".to_string(),
        "gamma delta".to_string(),
    ]));
    let t = del(&d, (0, 6), (2, 6));
    let doc = &t.doc;
    assert_eq!(texts(doc), texts(&d), "the text is kept, only marked");
    assert_eq!(ranges(doc, 0), vec![(RevisionKind::Delete, 6, 10)]);
    assert_eq!(ranges(doc, 1), vec![(RevisionKind::Delete, 0, 6)]);
    assert_eq!(ranges(doc, 2), vec![(RevisionKind::Delete, 0, 6)]);
    assert_eq!(
        marks(doc),
        vec![
            vec![(RevisionKind::Delete, ME.to_string())],
            vec![(RevisionKind::Delete, ME.to_string())],
            vec![]
        ]
    );
    assert_eq!(end_of(&t), (2, 6));
    let mut undo = UndoStack::new(d.clone(), 100);
    undo.push(t.doc.clone());
    let accepted = t.doc.resolve_all_revisions(true);
    undo.push(accepted.clone());
    assert_eq!(texts(&accepted), vec!["alpha delta"]);
    assert!(!accepted.has_revisions());
    assert!(in_step(&accepted));
    let rejected = t.doc.resolve_all_revisions(false);
    assert_eq!(texts(&rejected), texts(&d));
    assert!(!rejected.has_revisions());
    assert!(undo.undo() && undo.undo());
    assert_eq!(texts(undo.current()), texts(&d));
}

/// Backspace at a paragraph start (the range is just the previous mark):
/// the mark is marked deleted, no text; accept merges, reject keeps.
#[test]
fn deleting_just_a_paragraph_mark_marks_it() {
    let d = DocumentTree::from_paragraphs(["one".to_string(), "two".to_string()]);
    let t = del(&d, (0, 3), (1, 0));
    assert_eq!(texts(&t.doc), vec!["one", "two"]);
    assert!(ranges(&t.doc, 0).is_empty() && ranges(&t.doc, 1).is_empty());
    assert_eq!(
        marks(&t.doc),
        vec![vec![(RevisionKind::Delete, ME.to_string())], vec![]]
    );
    assert_eq!(texts(&t.doc.resolve_all_revisions(true)), vec!["onetwo"]);
    assert_eq!(
        texts(&t.doc.resolve_all_revisions(false)),
        vec!["one", "two"]
    );
    /* The sidebar's single mark row. */
    assert_eq!(texts(&t.doc.accept_revision_at(0, 3, 3)), vec!["onetwo"]);
    /* Deleting it again records nothing new. */
    let again = del(&t.doc, (0, 3), (1, 0));
    assert_eq!(marks(&again.doc), marks(&t.doc));
}

/// The reviewer's own pending insertions inside the range are removed
/// outright (text and the #301 inserted mark), the rest is marked; the
/// returned end follows the removals.
#[test]
fn own_insertions_inside_the_range_are_removed() {
    let d = DocumentTree::from_paragraphs(["abc".to_string(), "def".to_string()])
        .tracked_insert_text(pos(0, 3), "XY", ME.into(), DATE.into());
    assert_eq!(texts(&d), vec!["abcXY", "def"]);
    let t = del(&d, (0, 1), (1, 2));
    assert_eq!(texts(&t.doc), vec!["abc", "def"]);
    assert_eq!(ranges(&t.doc, 0), vec![(RevisionKind::Delete, 1, 3)]);
    assert_eq!(ranges(&t.doc, 1), vec![(RevisionKind::Delete, 0, 2)]);
    assert_eq!(end_of(&t), (1, 2));
    assert_eq!(texts(&t.doc.resolve_all_revisions(true)), vec!["af"]);
    assert_eq!(
        texts(&t.doc.resolve_all_revisions(false)),
        vec!["abc", "def"]
    );
    /* A range straddling the own insertion inside ONE paragraph: the
    insertion goes, the rest is marked, the two marks coalesce. */
    let one = DocumentTree::from_paragraphs(["abcd".to_string()]).tracked_insert_text(
        pos(0, 2),
        "XY",
        ME.into(),
        DATE.into(),
    );
    let t = del(&one, (0, 1), (0, 5));
    assert_eq!(texts(&t.doc), vec!["abcd"]);
    assert_eq!(ranges(&t.doc, 0), vec![(RevisionKind::Delete, 1, 3)]);
    assert_eq!(end_of(&t), (0, 3));
}

/// Backspace over the reviewer's own tracked Enter removes the break —
/// no deleted mark, no revision left; someone else's inserted break gets
/// a deletion next to the insertion (issue #303's two-change mark).
#[test]
fn deleting_a_tracked_break_removes_your_own_and_marks_someone_elses() {
    let d = DocumentTree::from_paragraphs(["alpha beta".to_string()]);
    let mine = d.tracked_split_paragraph(pos(0, 5), ME, DATE);
    let t = del(&mine, (0, 5), (1, 0));
    assert_eq!(texts(&t.doc), vec!["alpha beta"]);
    assert!(!t.doc.has_revisions());
    assert_eq!(end_of(&t), (0, 5));
    /* Enter typed and removed again inside a three-paragraph range: the
    end moves up onto the merged paragraph. */
    let three = DocumentTree::from_paragraphs(["ab".to_string(), "cd".to_string()])
        .tracked_split_paragraph(pos(0, 1), ME, DATE);
    assert_eq!(texts(&three), vec!["a", "b", "cd"]);
    let t = del(&three, (0, 0), (2, 1));
    assert_eq!(texts(&t.doc), vec!["ab", "cd"]);
    assert_eq!(end_of(&t), (1, 1));
    assert_eq!(texts(&t.doc.resolve_all_revisions(true)), vec!["d"]);

    let theirs = d.tracked_split_paragraph(pos(0, 5), "Other", DATE);
    let t = del(&theirs, (0, 5), (1, 0));
    assert_eq!(texts(&t.doc), vec!["alpha", " beta"]);
    assert_eq!(
        marks(&t.doc),
        vec![
            vec![
                (RevisionKind::Insert, "Other".to_string()),
                (RevisionKind::Delete, ME.to_string())
            ],
            vec![]
        ]
    );
    /* In order: accept keeps the insertion, then the deletion merges;
    reject drops the insertion — the mark goes either way. */
    assert_eq!(
        texts(&t.doc.resolve_all_revisions(true)),
        vec!["alpha beta"]
    );
    assert_eq!(
        texts(&t.doc.resolve_all_revisions(false)),
        vec!["alpha beta"]
    );
}

/// Already-deleted text (anyone's) is not marked a second time.
#[test]
fn already_deleted_text_is_left_alone() {
    let mut d = DocumentTree::from_paragraphs(["abcdef".to_string(), "gh".to_string()]);
    let mut blocks = d.blocks.clone();
    if let Block::Paragraph(p) = &mut blocks[0] {
        p.revisions = vec![rev(RevisionKind::Delete, 2, 4, "Other")];
    }
    d.blocks = blocks;
    let t = del(&d, (0, 1), (1, 1));
    assert_eq!(
        ranges(&t.doc, 0),
        vec![
            (RevisionKind::Delete, 1, 2),
            (RevisionKind::Delete, 2, 4),
            (RevisionKind::Delete, 4, 6)
        ]
    );
    assert_eq!(texts(&t.doc.resolve_all_revisions(true)), vec!["ah"]);
}

/// A comment on the surviving text keeps its text through accept and
/// reject; source markup stays in step.
#[test]
fn comments_follow_their_text_through_the_tracked_delete() {
    let d = with_markup(DocumentTree::from_paragraphs([
        "keep gone".to_string(),
        "gone too".to_string(),
        "gone tail".to_string(),
    ]));
    let (d, _) = d.insert_comment(pos(2, 5), pos(2, 9), "c".into(), "A".into(), "d".into());
    assert_eq!(covered(&d), "tail");
    let t = del(&d, (0, 5), (2, 5));
    assert_eq!(covered(&t.doc), "tail");
    let accepted = t.doc.resolve_all_revisions(true);
    assert_eq!(texts(&accepted), vec!["keep tail"]);
    assert_eq!(covered(&accepted), "tail");
    assert!(in_step(&accepted));
    let rejected = t.doc.resolve_all_revisions(false);
    assert_eq!(covered(&rejected), "tail");
}

/// No silent no-op: a range over a table or across a cell boundary is
/// refused with a typed error; the wrapper leaves the tree unchanged.
#[test]
fn a_range_over_a_table_or_across_a_cell_is_refused() {
    let d = DocumentTree::from_paragraphs(["before".to_string(), "after".to_string()])
        .insert_table(BlockPath::top(1), 1, 1);
    let kinds: Vec<bool> = d
        .blocks
        .iter()
        .map(|b| matches!(b, Block::Table(_)))
        .collect();
    let table = kinds.iter().position(|&t| t).expect("table") as u32;
    let after = (table + 1..d.blocks.len() as u32)
        .find(|&i| d.blocks[i as usize].as_paragraph().is_some())
        .expect("a paragraph after the table");
    assert_eq!(
        d.try_tracked_delete_range(pos(0, 2), pos(after, 2), ME, DATE)
            .err(),
        Some(crate::TrackedEditError::SpansTable)
    );
    let cell = LogicalPos::new(
        BlockPath {
            steps: vec![
                PathStep::Block(table),
                PathStep::Cell { row: 0, col: 0 },
                PathStep::Block(0),
            ],
        },
        0,
    );
    assert_eq!(
        d.try_tracked_delete_range(pos(0, 2), cell, ME, DATE).err(),
        Some(crate::TrackedEditError::CrossContainer)
    );
    let unchanged = d.tracked_delete_range(pos(0, 2), pos(after, 2), ME.into(), DATE.into());
    assert!(!unchanged.has_revisions());
}

/// Inside one table cell a cross-paragraph delete works like the body.
#[test]
fn a_cross_paragraph_delete_inside_a_cell() {
    let d =
        DocumentTree::from_paragraphs(["before".to_string()]).insert_table(BlockPath::top(1), 1, 1);
    let table = d
        .blocks
        .iter()
        .position(|b| matches!(b, Block::Table(_)))
        .expect("table") as u32;
    let cell = |b: u32| BlockPath {
        steps: vec![
            PathStep::Block(table),
            PathStep::Cell { row: 0, col: 0 },
            PathStep::Block(b),
        ],
    };
    let d = d
        .insert_text(LogicalPos::new(cell(0), 0), "xy")
        .split_paragraph(LogicalPos::new(cell(0), 1));
    let t = d
        .try_tracked_delete_range(
            LogicalPos::new(cell(0), 0),
            LogicalPos::new(cell(1), 1),
            ME,
            DATE,
        )
        .expect("recorded");
    let cell_texts = |d: &DocumentTree| -> Vec<String> {
        match &d.blocks[table as usize] {
            Block::Table(t) => t.rows[0].cells[0]
                .blocks
                .iter()
                .filter_map(Block::as_paragraph)
                .map(|p| p.text.clone())
                .collect(),
            _ => Vec::new(),
        }
    };
    assert_eq!(cell_texts(&t.doc), vec!["x", "y"]);
    assert_eq!(cell_texts(&t.doc.resolve_all_revisions(true)), vec![""]);
    assert_eq!(
        cell_texts(&t.doc.resolve_all_revisions(false)),
        vec!["x", "y"]
    );
}

/// Snapshot round trip of a tree carrying the recorded deletion.
#[test]
fn the_recorded_deletion_snapshots_byte_stably() {
    use crate::snapshot::{decode, encode};
    let d = DocumentTree::from_paragraphs(["one".to_string(), "two".to_string()])
        .tracked_split_paragraph(pos(1, 1), "Other", DATE);
    let t = del(&d, (0, 1), (1, 1));
    let bytes = encode(&t.doc).unwrap();
    let back: DocumentTree = decode(&bytes).unwrap().payload;
    assert_eq!(marks(&back), marks(&t.doc));
    assert_eq!(encode(&back).unwrap(), bytes);
}
