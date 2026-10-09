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
