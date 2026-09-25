//! Issues #247 / #262 — tracked moves, paragraph-mark revisions and the
//! document-wide accept / reject.

use crate::{Block, BlockPath, DocumentTree, LogicalPos, Revision, RevisionKind};

fn pos(block: u32, offset: u32) -> LogicalPos {
    LogicalPos::new(BlockPath::top(block), offset)
}

fn rev(kind: RevisionKind, start: u32, end: u32) -> Revision {
    Revision {
        start,
        end,
        kind,
        author: "A".into(),
        date: "d".into(),
        id: None,
        prev_attrs: None,
        move_name: matches!(kind, RevisionKind::MoveFrom | RevisionKind::MoveTo)
            .then(|| "m".to_string()),
    }
}

/// `texts[i]` in top-level paragraph `i`, carrying `revs[i]`.
fn doc_with(texts: &[&str], revs: &[Vec<Revision>]) -> DocumentTree {
    let mut d = DocumentTree::from_paragraphs(texts.iter().map(|t| t.to_string()));
    let mut blocks = d.blocks.clone();
    for (i, rs) in revs.iter().enumerate() {
        if let Block::Paragraph(p) = &mut blocks[i] {
            p.revisions = rs.clone();
        }
    }
    d.blocks = blocks;
    d
}

fn ranges(d: &DocumentTree, block: u32) -> Vec<(RevisionKind, u32, u32)> {
    d.nth_paragraph(block)
        .map(|p| {
            p.revisions
                .iter()
                .map(|r| (r.kind, r.start, r.end))
                .collect()
        })
        .unwrap_or_default()
}

/// Issue #247 — an untracked edit beside a tracked change moves the
/// change with its text (it used to stay at its old offsets and cover
/// the typed bytes instead).
#[test]
fn insert_text_carries_revisions_with_their_text() {
    let d = doc_with(&["ab moved cd"], &[vec![rev(RevisionKind::MoveTo, 3, 8)]]);
    /* Before: shift. */
    let before = d.insert_text(pos(0, 1), "XY");
    assert_eq!(ranges(&before, 0), vec![(RevisionKind::MoveTo, 5, 10)]);
    /* At the start: stays outside (shift). */
    let at_start = d.insert_text(pos(0, 3), "XY");
    assert_eq!(ranges(&at_start, 0), vec![(RevisionKind::MoveTo, 5, 10)]);
    /* Strictly inside: grows. */
    let inside = d.insert_text(pos(0, 5), "XY");
    assert_eq!(ranges(&inside, 0), vec![(RevisionKind::MoveTo, 3, 10)]);
    /* At the end: stays outside. */
    let at_end = d.insert_text(pos(0, 8), "XY");
    assert_eq!(ranges(&at_end, 0), vec![(RevisionKind::MoveTo, 3, 8)]);
    assert_eq!(at_end.paragraph_text(0), Some("ab movedXY cd"));
}

/// Issue #247 — accept keeps a move's destination and drops its source;
/// reject the reverse.
#[test]
fn move_accept_keeps_destination_reject_keeps_source() {
    let d = doc_with(
        &["moved stay moved"],
        &[vec![
            rev(RevisionKind::MoveFrom, 0, 5),
            rev(RevisionKind::MoveTo, 11, 16),
        ]],
    );
    let accepted = d.accept_revision_at(0, 11, 16).accept_revision_at(0, 0, 5);
    assert_eq!(accepted.paragraph_text(0), Some(" stay moved"));
    assert!(ranges(&accepted, 0).is_empty());
    let rejected = d.reject_revision_at(0, 11, 16).reject_revision_at(0, 0, 5);
    assert_eq!(rejected.paragraph_text(0), Some("moved stay "));
    assert!(ranges(&rejected, 0).is_empty());
}

#[test]
fn revision_kind_outcomes() {
    use RevisionKind::*;
    for (kind, on_accept, on_reject) in [
        (Insert, false, true),
        (Delete, true, false),
        (MoveFrom, true, false),
        (MoveTo, false, true),
        (FormatChange, false, false),
    ] {
        assert_eq!(kind.removes_text(true), on_accept, "{kind:?} accept");
        assert_eq!(kind.removes_text(false), on_reject, "{kind:?} reject");
        assert_eq!(kind.wraps_text(), kind != FormatChange);
    }
}

/* ======================= issue #262 — paragraph-mark revisions ==== */

use crate::{Paragraph, SourceMarkup, SourceRun, SpanStyle, StyleRun, UndoStack};

/// Every top-level paragraph gets one source run over its text, so the
/// #250 in-step assertion (`UndoStack::push`, test builds) has markup to
/// check.
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

fn set_mark(d: &mut DocumentTree, block: usize, kind: RevisionKind) {
    let mut blocks = d.blocks.clone();
    if let Block::Paragraph(p) = &mut blocks[block] {
        p.mark_revision = Some(rev(kind, 0, 0));
    }
    d.blocks = blocks;
}

fn commented(d: DocumentTree, s: LogicalPos, e: LogicalPos) -> DocumentTree {
    d.insert_comment(s, e, "c".into(), "A".into(), "d".into()).0
}

fn covered(d: &DocumentTree) -> String {
    let r = &d.comment_ranges[0];
    d.text_range(r.start.clone(), r.end.clone())
}

fn texts(d: &DocumentTree) -> Vec<String> {
    d.blocks
        .iter()
        .filter_map(Block::as_paragraph)
        .map(|p| p.text.clone())
        .collect()
}

fn in_step(p: &Paragraph) -> bool {
    p.source_markup
        .as_deref()
        .is_none_or(|m| m.offsets_valid(p.text.len()))
}

/// Issue #253's case: accept-all with a paragraph-mark deletion merges
/// the paragraphs and keeps the comment on its text — through ONE undo
/// step, source markup in step.
#[test]
fn accept_all_with_a_paragraph_mark_deletion_keeps_the_comment() {
    let mut d = with_markup(doc_with(
        &["drop hello ", "world tail", "last"],
        &[vec![rev(RevisionKind::Delete, 0, 5)], vec![], vec![]],
    ));
    set_mark(&mut d, 0, RevisionKind::Delete);
    /* The comment covers "world" in the paragraph that merges up. */
    let d = commented(d, pos(1, 0), pos(1, 5));
    assert_eq!(covered(&d), "world");
    let mut undo = UndoStack::new(d.clone(), 100);
    let accepted = d.resolve_all_revisions(true);
    undo.push(accepted.clone());
    assert_eq!(texts(&accepted), vec!["hello world tail", "last"]);
    assert_eq!(covered(&accepted), "world");
    let r = &accepted.comment_ranges[0];
    assert_eq!(
        (r.start.path.clone(), r.start.offset),
        (BlockPath::top(0), 6)
    );
    assert!(
        accepted
            .blocks
            .iter()
            .filter_map(Block::as_paragraph)
            .all(in_step)
    );
    assert!(!accepted.has_revisions());
    /* One step back restores everything. */
    assert!(undo.undo());
    assert_eq!(texts(undo.current()), texts(&d));
    assert_eq!(covered(undo.current()), "world");
}

/// A comment reaching across the merged boundary still covers the same
/// text; reject-all keeps both paragraphs.
#[test]
fn reject_all_of_a_mark_deletion_keeps_both_paragraphs() {
    let mut d = with_markup(DocumentTree::from_paragraphs([
        "alpha".to_string(),
        "beta".to_string(),
    ]));
    set_mark(&mut d, 0, RevisionKind::Delete);
    let d = commented(d, pos(0, 2), pos(1, 2));
    let rejected = d.resolve_all_revisions(false);
    assert_eq!(texts(&rejected), vec!["alpha", "beta"]);
    assert!(rejected.nth_paragraph(0).unwrap().mark_revision.is_none());
    assert_eq!(covered(&rejected), covered(&d));
    let accepted = d.resolve_all_revisions(true);
    assert_eq!(texts(&accepted), vec!["alphabeta"]);
    assert_eq!(covered(&accepted), "phabe");
}

/// A tracked split (inserted mark): accept keeps the break, reject
/// merges.
#[test]
fn inserted_mark_accept_keeps_reject_merges() {
    let mut d = with_markup(DocumentTree::from_paragraphs([
        "one ".to_string(),
        "two".to_string(),
    ]));
    set_mark(&mut d, 0, RevisionKind::Insert);
    let accepted = d.resolve_all_revisions(true);
    assert_eq!(texts(&accepted), vec!["one ", "two"]);
    assert!(!accepted.has_revisions());
    let rejected = d.resolve_all_revisions(false);
    assert_eq!(texts(&rejected), vec!["one two"]);
}

/// A chain of deleted marks merges in one pass; a moved-away mark merges
/// like a deletion; the last paragraph has nothing to merge with.
#[test]
fn chains_and_moved_marks() {
    let mut d = DocumentTree::from_paragraphs(["a".to_string(), "b".to_string(), "c".to_string()]);
    set_mark(&mut d, 0, RevisionKind::Delete);
    set_mark(&mut d, 1, RevisionKind::MoveFrom);
    assert_eq!(texts(&d.resolve_all_revisions(true)), vec!["abc"]);
    assert_eq!(texts(&d.resolve_all_revisions(false)), vec!["a", "b", "c"]);
    let mut last = DocumentTree::from_paragraphs(["x".to_string()]);
    set_mark(&mut last, 0, RevisionKind::Delete);
    let out = last.resolve_all_revisions(true);
    assert_eq!(texts(&out), vec!["x"]);
    assert!(!out.has_revisions());
}

/// A paragraph deleted whole (text + mark): accept removes the block and
/// leaves the next paragraph untouched — still clean, its own style.
#[test]
fn a_paragraph_deleted_whole_vanishes_and_the_next_stays_clean() {
    let mut d = doc_with(
        &["gone", "kept"],
        &[vec![rev(RevisionKind::Delete, 0, 4)], vec![]],
    );
    set_mark(&mut d, 0, RevisionKind::Delete);
    let mut blocks = d.blocks.clone();
    if let Block::Paragraph(p) = &mut blocks[1] {
        p.dirty = false;
        p.style_id = Some("Heading1".into());
    }
    d.blocks = blocks;
    let d = commented(d, pos(1, 0), pos(1, 4));
    let out = d.resolve_all_revisions(true);
    assert_eq!(texts(&out), vec!["kept"]);
    let p = out.nth_paragraph(0).unwrap();
    assert!(!p.dirty, "the surviving paragraph was not touched");
    assert_eq!(p.style_id.as_deref(), Some("Heading1"));
    assert_eq!(covered(&out), "kept");
}

/// The single-revision path addresses a mark as the empty range at the
/// paragraph end.
#[test]
fn a_mark_revision_resolves_through_accept_revision_at() {
    let mut d = DocumentTree::from_paragraphs(["ab".to_string(), "cd".to_string()]);
    set_mark(&mut d, 0, RevisionKind::Delete);
    assert_eq!(texts(&d.accept_revision_at(0, 2, 2)), vec!["abcd"]);
    let rejected = d.reject_revision_at(0, 2, 2);
    assert_eq!(texts(&rejected), vec!["ab", "cd"]);
    assert!(!rejected.has_revisions());
}

/// Accept-all over moves and nested wrappers (the Tika-792 shape): the
/// destination stays unless deleted inside the move, the source goes.
#[test]
fn accept_and_reject_all_over_moves() {
    let d = doc_with(
        &["s.", "b"],
        &[
            vec![
                rev(RevisionKind::Delete, 0, 1),
                rev(RevisionKind::Delete, 1, 2),
                rev(RevisionKind::MoveTo, 1, 2),
            ],
            vec![
                rev(RevisionKind::Insert, 0, 1),
                rev(RevisionKind::MoveFrom, 0, 1),
            ],
        ],
    );
    assert_eq!(texts(&d.resolve_all_revisions(true)), vec!["", ""]);
    assert_eq!(texts(&d.resolve_all_revisions(false)), vec!["s", ""]);
    let plain = doc_with(
        &["moved stay moved"],
        &[vec![
            rev(RevisionKind::MoveFrom, 0, 5),
            rev(RevisionKind::MoveTo, 11, 16),
        ]],
    );
    assert_eq!(
        texts(&plain.resolve_all_revisions(true)),
        vec![" stay moved"]
    );
    assert_eq!(
        texts(&plain.resolve_all_revisions(false)),
        vec!["moved stay "]
    );
}

/// Reject restores a tracked formatting change's recorded style (the
/// single path and reject-all); accept keeps the new one.
#[test]
fn reject_restores_a_format_change() {
    let mut d = DocumentTree::from_paragraphs(["bold".to_string()]);
    let bold = SpanStyle {
        bold: Some(true),
        ..SpanStyle::default()
    };
    let mut blocks = d.blocks.clone();
    if let Block::Paragraph(p) = &mut blocks[0] {
        p.spans = vec![StyleRun {
            start: 0,
            end: 4,
            style: bold.clone(),
        }];
        p.revisions = vec![Revision {
            prev_attrs: Some(SpanStyle::default()),
            ..rev(RevisionKind::FormatChange, 0, 4)
        }];
    }
    d.blocks = blocks;
    assert!(
        d.resolve_all_revisions(false)
            .nth_paragraph(0)
            .unwrap()
            .spans
            .is_empty()
    );
    assert!(
        d.reject_revision_at(0, 0, 4)
            .nth_paragraph(0)
            .unwrap()
            .spans
            .is_empty()
    );
    let accepted = d.resolve_all_revisions(true);
    assert_eq!(accepted.nth_paragraph(0).unwrap().spans[0].style, bold);
}

/// A mark revision inside a table cell merges within the cell.
#[test]
fn a_mark_revision_in_a_table_cell_merges_in_the_cell() {
    let mut d =
        DocumentTree::from_paragraphs(["before".to_string()]).insert_table(BlockPath::top(1), 1, 1);
    let cell = |d: &DocumentTree| -> Vec<String> {
        d.blocks
            .iter()
            .find_map(|b| match b {
                Block::Table(t) => Some(
                    t.rows[0].cells[0]
                        .blocks
                        .iter()
                        .filter_map(Block::as_paragraph)
                        .map(|p| p.text.clone())
                        .collect(),
                ),
                _ => None,
            })
            .unwrap_or_default()
    };
    let mut blocks = d.blocks.clone();
    for b in blocks.iter_mut() {
        if let Block::Table(t) = b {
            t.rows[0].cells[0].blocks = vec![
                Block::Paragraph(Paragraph {
                    text: "x".into(),
                    mark_revision: Some(rev(RevisionKind::Delete, 0, 0)),
                    ..Paragraph::default()
                }),
                Block::Paragraph(Paragraph {
                    text: "y".into(),
                    ..Paragraph::default()
                }),
            ];
        }
    }
    d.blocks = blocks;
    assert!(d.has_revisions());
    assert_eq!(cell(&d.resolve_all_revisions(true)), vec!["xy"]);
    assert_eq!(cell(&d.resolve_all_revisions(false)), vec!["x", "y"]);
}
