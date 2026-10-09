//! Issue #366 — a paste with review mode on is a tracked insertion: the
//! pasted text is `Insert`ed and every paragraph mark the paste creates is
//! an inserted mark, so reject restores the document and accept keeps it.

use crate::{
    Block, BlockPath, DocumentTree, LogicalPos, Paragraph, Revision, RevisionKind, SourceMarkup,
    SourceRun, UndoStack,
};

const ME: &str = "R";
const DATE: &str = "2026-10-09T00:00:00Z";

fn pos(block: u32, offset: u32) -> LogicalPos {
    LogicalPos::new(BlockPath::top(block), offset)
}

fn texts(d: &DocumentTree) -> Vec<String> {
    d.blocks
        .iter()
        .filter_map(Block::as_paragraph)
        .map(|p| p.text.clone())
        .collect()
}

fn marks(d: &DocumentTree) -> Vec<Vec<RevisionKind>> {
    d.blocks
        .iter()
        .filter_map(Block::as_paragraph)
        .map(|p| p.mark_revisions.iter().map(|r| r.kind).collect())
        .collect()
}

fn ranges(d: &DocumentTree) -> Vec<Vec<(RevisionKind, u32, u32, String)>> {
    d.blocks
        .iter()
        .filter_map(Block::as_paragraph)
        .map(|p| {
            let mut v: Vec<_> = p
                .revisions
                .iter()
                .map(|r| (r.kind, r.start, r.end, r.author.clone()))
                .collect();
            v.sort_by_key(|r| (r.1, r.2));
            v
        })
        .collect()
}

fn ins(start: u32, end: u32) -> (RevisionKind, u32, u32, String) {
    (RevisionKind::Insert, start, end, ME.to_string())
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

fn para(text: &str) -> Block {
    Block::Paragraph(Paragraph {
        text: text.into(),
        ..Paragraph::default()
    })
}

/// Push every state through an undo stack (test builds assert the source
/// markup stayed in step on every push).
fn in_step(states: &[&DocumentTree]) {
    let mut undo = UndoStack::new(states[0].clone(), 100);
    for s in &states[1..] {
        undo.push((*s).clone());
    }
}

/* ======================= plain multi-line paste ==== */

/// Two pasted lines mid-paragraph: the text of each line is the
/// reviewer's insertion, the new mark (ending the first half) an inserted
/// mark; reject restores the paragraph, accept keeps the paste clean.
#[test]
fn a_multiline_paste_records_inserted_text_and_marks() {
    let d = with_markup(DocumentTree::from_paragraphs(["alpha beta".to_string()]));
    let (pasted, caret) = d.tracked_insert_multiline(pos(0, 5), "one\ntwo", ME, DATE);
    assert_eq!(texts(&pasted), vec!["alphaone", "two beta"]);
    assert_eq!(caret, pos(1, 3));
    assert_eq!(ranges(&pasted), vec![vec![ins(5, 8)], vec![ins(0, 3)]]);
    assert_eq!(marks(&pasted), vec![vec![RevisionKind::Insert], vec![]]);
    let mark = &pasted.nth_paragraph(0).unwrap().mark_revisions[0];
    assert_eq!((mark.author.as_str(), mark.date.as_str()), (ME, DATE));
    let rejected = pasted.resolve_all_revisions(false);
    assert_eq!(texts(&rejected), vec!["alpha beta"]);
    assert!(!rejected.has_revisions());
    let accepted = pasted.resolve_all_revisions(true);
    assert_eq!(texts(&accepted), vec!["alphaone", "two beta"]);
    assert!(!accepted.has_revisions());
    in_step(&[&d, &pasted, &rejected]);
    in_step(&[&d, &pasted, &accepted]);
}

/// An empty middle line still creates a paragraph — its mark is an
/// inserted one too; three paragraphs reject back to one.
#[test]
fn an_empty_pasted_line_is_an_inserted_paragraph() {
    let d = with_markup(DocumentTree::from_paragraphs(["xy".to_string()]));
    let (pasted, caret) = d.tracked_insert_multiline(pos(0, 1), "a\r\n\r\nb", ME, DATE);
    assert_eq!(texts(&pasted), vec!["xa", "", "by"]);
    assert_eq!(caret, pos(2, 1));
    assert_eq!(
        marks(&pasted),
        vec![
            vec![RevisionKind::Insert],
            vec![RevisionKind::Insert],
            vec![]
        ]
    );
    let rejected = pasted.resolve_all_revisions(false);
    assert_eq!(texts(&rejected), vec!["xy"]);
    assert!(!rejected.has_revisions());
    in_step(&[&d, &pasted, &rejected]);
}

/// The tracked typing rules apply per line: a paste right after the
/// reviewer's own pending insertion grows it; a paste inside someone
/// else's deletion splits the deletion around the new text.
#[test]
fn pasted_lines_follow_the_tracked_typing_rules() {
    let d = DocumentTree::from_paragraphs(["abcdef".to_string()]);
    let typed = d.tracked_insert_text(pos(0, 3), "XY", ME.into(), DATE.into());
    let (pasted, _) = typed.tracked_insert_multiline(pos(0, 5), "1\n2", ME, DATE);
    assert_eq!(texts(&pasted), vec!["abcXY1", "2def"]);
    assert_eq!(ranges(&pasted), vec![vec![ins(3, 6)], vec![ins(0, 1)]]);

    let mut struck = DocumentTree::from_paragraphs(["abcdef".to_string()]);
    let mut blocks = struck.blocks.clone();
    if let Block::Paragraph(p) = &mut blocks[0] {
        p.revisions = vec![Revision {
            start: 1,
            end: 5,
            kind: RevisionKind::Delete,
            author: "Other".into(),
            date: "d".into(),
            id: None,
            prev_attrs: None,
            move_name: None,
        }];
    }
    struck.blocks = blocks;
    let (pasted, _) = struck.tracked_insert_multiline(pos(0, 3), "1\n2", ME, DATE);
    assert_eq!(texts(&pasted), vec!["abc1", "2def"]);
    let del = |s, e| (RevisionKind::Delete, s, e, "Other".to_string());
    assert_eq!(
        ranges(&pasted),
        vec![vec![del(1, 3), ins(3, 4)], vec![ins(0, 1), del(1, 3)]]
    );
    /* Rejecting the paste leaves the other reviewer's deletion pending,
    whole again over the same text. */
    let mut pick = std::collections::HashSet::new();
    for e in pasted.revision_entries() {
        if e.revision.author == ME {
            pick.insert(e.at);
        }
    }
    let rejected = pasted.resolve_revisions(false, &crate::RevisionPick::Only(pick));
    assert_eq!(texts(&rejected), vec!["abcdef"]);
    assert_eq!(
        rejected.resolve_all_revisions(true).paragraph_text(0),
        Some("af")
    );
}

/* ======================= rich paste ==== */

/// Two pasted paragraphs: one insertion over each, the first one's mark
/// (which now ends the head) inserted; the last one's mark gives way to
/// the target's original mark.
#[test]
fn a_rich_paste_records_inserted_text_and_marks() {
    let d = with_markup(DocumentTree::from_paragraphs(["alpha beta".to_string()]));
    let (pasted, caret) =
        d.tracked_insert_rich_blocks(pos(0, 5), &[para("one"), para("two")], ME, DATE);
    assert_eq!(texts(&pasted), vec!["alphaone", "two beta"]);
    assert_eq!(caret, pos(1, 3));
    assert_eq!(ranges(&pasted), vec![vec![ins(5, 8)], vec![ins(0, 3)]]);
    assert_eq!(marks(&pasted), vec![vec![RevisionKind::Insert], vec![]]);
    let rejected = pasted.resolve_all_revisions(false);
    assert_eq!(texts(&rejected), vec!["alpha beta"]);
    assert!(!rejected.has_revisions());
    let accepted = pasted.resolve_all_revisions(true);
    assert_eq!(texts(&accepted), vec!["alphaone", "two beta"]);
    assert!(!accepted.has_revisions());
    in_step(&[&d, &pasted, &rejected]);
    in_step(&[&d, &pasted, &accepted]);
}

/// A one-paragraph rich paste creates no paragraph mark: text only.
#[test]
fn a_single_paragraph_rich_paste_is_text_only() {
    let d = DocumentTree::from_paragraphs(["alpha beta".to_string()]);
    let (pasted, caret) = d.tracked_insert_rich_blocks(pos(0, 6), &[para("new ")], ME, DATE);
    assert_eq!(texts(&pasted), vec!["alpha new beta"]);
    assert_eq!(caret, pos(0, 10));
    assert_eq!(ranges(&pasted), vec![vec![ins(6, 10)]]);
    assert_eq!(marks(&pasted), vec![Vec::<RevisionKind>::new()]);
    assert_eq!(
        pasted.resolve_all_revisions(false).paragraph_text(0),
        Some("alpha beta")
    );
}

/// Three pasted paragraphs at the very start of a document: the middle
/// one is whole new text with an inserted mark.
#[test]
fn a_rich_paste_at_the_start_rejects_to_the_original() {
    let d = with_markup(DocumentTree::from_paragraphs([
        "first".to_string(),
        "second".to_string(),
    ]));
    let (pasted, _) =
        d.tracked_insert_rich_blocks(pos(0, 0), &[para("a"), para("b"), para("c")], ME, DATE);
    assert_eq!(texts(&pasted), vec!["a", "b", "cfirst", "second"]);
    assert_eq!(
        marks(&pasted),
        vec![
            vec![RevisionKind::Insert],
            vec![RevisionKind::Insert],
            vec![],
            vec![]
        ]
    );
    let rejected = pasted.resolve_all_revisions(false);
    assert_eq!(texts(&rejected), vec!["first", "second"]);
    assert!(!rejected.has_revisions());
    in_step(&[&d, &pasted, &rejected]);
}

/// A comment on the text after the paste point keeps its text through the
/// tracked paste and its reject.
#[test]
fn comments_follow_their_text_through_a_tracked_paste() {
    let d = with_markup(DocumentTree::from_paragraphs(["alpha beta".to_string()]));
    let (d, _) = d.insert_comment(pos(0, 6), pos(0, 10), "c".into(), "A".into(), "d".into());
    let covered = |d: &DocumentTree| {
        let r = &d.comment_ranges[0];
        d.text_range(r.start.clone(), r.end.clone())
    };
    for pasted in [
        d.tracked_insert_multiline(pos(0, 5), "one\ntwo", ME, DATE)
            .0,
        d.tracked_insert_rich_blocks(pos(0, 5), &[para("one"), para("two")], ME, DATE)
            .0,
    ] {
        assert_eq!(covered(&pasted), "beta");
        let rejected = pasted.resolve_all_revisions(false);
        assert_eq!(covered(&rejected), "beta");
        assert_eq!(texts(&rejected), vec!["alpha beta"]);
    }
}

/// Snapshot round trip of a tree carrying a tracked paste.
#[test]
fn a_tracked_paste_snapshots_byte_stably() {
    use crate::snapshot::{decode, encode};
    let d = DocumentTree::from_paragraphs(["alpha beta".to_string()]);
    let (pasted, _) = d.tracked_insert_multiline(pos(0, 5), "one\ntwo\nthree", ME, DATE);
    let bytes = encode(&pasted).unwrap();
    let back: DocumentTree = decode(&bytes).unwrap().payload;
    assert_eq!(marks(&back), marks(&pasted));
    assert_eq!(ranges(&back), ranges(&pasted));
    assert_eq!(encode(&back).unwrap(), bytes);
}
