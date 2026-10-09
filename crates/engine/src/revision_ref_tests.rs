//! Issue #305 — the single-revision accept / reject runs through the
//! accept-all resolver ([`DocumentTree::resolve_revisions`]): no inline
//! object is left pointing at removed text, pending neighbours travel
//! with their text, and every committed state passes the `markup-assert`
//! tree check (`UndoStack::push`, which now also checks inline-object
//! sentinels).

use crate::{
    Block, BlockPath, DocumentTree, ImageBlob, LogicalPos, Revision, RevisionKind, RevisionRef,
    RevisionSlot, UndoStack,
};

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

/// `(top-level paragraph, anchor byte)` of every inline object.
fn objects(d: &DocumentTree) -> Vec<(usize, u32)> {
    d.blocks
        .iter()
        .filter_map(Block::as_paragraph)
        .enumerate()
        .flat_map(|(i, p)| p.inline_objects.iter().map(move |o| (i, o.at)))
        .collect()
}

/// `"pic ￼ end"` (a picture at byte 4) followed by `"next"`; the whole
/// first paragraph — text AND mark — is one tracked change of `kind`.
fn picture_paragraph(kind: RevisionKind) -> DocumentTree {
    let mut d = DocumentTree::from_paragraphs(["pic  end".to_string(), "next".to_string()])
        .insert_inline_image_at(
            pos(0, 4),
            ImageBlob {
                content_type: "image/png".into(),
                data: vec![0x89, b'P', b'N', b'G'],
            },
            9525,
            9525,
        );
    let mut blocks = d.blocks.clone();
    if let Block::Paragraph(p) = &mut blocks[0] {
        let len = p.text.len() as u32;
        p.revisions = vec![rev(kind, 0, len)];
        p.mark_revision = Some(rev(kind, 0, 0));
    }
    d.blocks = blocks;
    assert_eq!(objects(&d), vec![(0, 4)]);
    d
}

fn first_len(d: &DocumentTree) -> u32 {
    d.nth_paragraph(0).map_or(0, |p| p.text.len() as u32)
}

/// Rejecting an inserted paragraph that holds a picture — text first,
/// then the (now empty) paragraph's mark — removes the picture with its
/// sentinel and then the paragraph; undo restores both.
#[test]
fn rejecting_an_inserted_paragraph_with_a_picture_leaves_no_dangling_object() {
    let d = picture_paragraph(RevisionKind::Insert);
    let mut undo = UndoStack::new(d.clone(), 100);
    let text_gone = d.reject_revision_at(0, 0, first_len(&d));
    undo.push(text_gone.clone());
    assert_eq!(texts(&text_gone), vec!["", "next"]);
    assert!(objects(&text_gone).is_empty());
    let para_gone = text_gone.reject_revision_at(0, 0, 0);
    undo.push(para_gone.clone());
    assert_eq!(texts(&para_gone), vec!["next"]);
    assert!(objects(&para_gone).is_empty());
    assert!(!para_gone.has_revisions());
    assert!(undo.undo() && undo.undo());
    assert_eq!(texts(undo.current()), texts(&d));
    assert_eq!(objects(undo.current()), vec![(0, 4)]);
    assert!(undo.current().has_revisions());
}

/// The same paragraph, mark first: the merge keeps the picture on its
/// sentinel (and the pending text revision on its text); rejecting the
/// text afterwards takes both out.
#[test]
fn rejecting_the_mark_first_keeps_the_picture_anchored() {
    let d = picture_paragraph(RevisionKind::Insert);
    let len = first_len(&d);
    let mut undo = UndoStack::new(d.clone(), 100);
    let merged = d.reject_revision_at(0, len, len);
    undo.push(merged.clone());
    assert_eq!(texts(&merged), vec!["pic \u{FFFC} endnext"]);
    assert_eq!(objects(&merged), vec![(0, 4)]);
    let gone = merged.reject_revision_at(0, 0, len);
    undo.push(gone.clone());
    assert_eq!(texts(&gone), vec!["next"]);
    assert!(objects(&gone).is_empty());
    assert!(!gone.has_revisions());
}

/// Accepting a deleted paragraph with a picture through the single path
/// ends exactly where accept-all does.
#[test]
fn accepting_a_deleted_paragraph_with_a_picture_matches_accept_all() {
    let d = picture_paragraph(RevisionKind::Delete);
    let mut undo = UndoStack::new(d.clone(), 100);
    let a = d.accept_revision_at(0, 0, first_len(&d));
    undo.push(a.clone());
    assert!(objects(&a).is_empty());
    let b = a.accept_revision_at(0, 0, 0);
    undo.push(b.clone());
    let all = d.resolve_all_revisions(true);
    assert_eq!(texts(&b), texts(&all));
    assert_eq!(texts(&b), vec!["next"]);
    assert!(objects(&b).is_empty() && objects(&all).is_empty());
}

/// A revision the single path leaves pending travels with its text.
#[test]
fn a_pending_revision_shifts_when_its_neighbour_resolves() {
    let mut d = DocumentTree::from_paragraphs(["abXYcdEFgh".to_string()]);
    let mut blocks = d.blocks.clone();
    if let Block::Paragraph(p) = &mut blocks[0] {
        p.revisions = vec![
            rev(RevisionKind::Insert, 2, 4),
            rev(RevisionKind::Delete, 6, 8),
        ];
    }
    d.blocks = blocks;
    let rejected = d.reject_revision_at(0, 2, 4);
    assert_eq!(texts(&rejected), vec!["abcdEFgh"]);
    let left: Vec<_> = rejected.nth_paragraph(0).unwrap().revisions.clone();
    assert_eq!(left.len(), 1);
    assert_eq!(
        (left[0].kind, left[0].start, left[0].end),
        (RevisionKind::Delete, 4, 6)
    );
    let accepted = rejected.accept_revision_at(0, 4, 6);
    assert_eq!(texts(&accepted), vec!["abcdgh"]);
    assert!(!accepted.has_revisions());
}

/// An address that names no revision changes nothing (the engine then
/// pushes no undo step).
#[test]
fn a_missing_revision_is_a_no_op() {
    let d = picture_paragraph(RevisionKind::Insert);
    let missing = RevisionRef {
        path: BlockPath::top(1),
        slot: RevisionSlot::Mark,
    };
    assert!(d.resolve_revision(&missing, true).is_none());
    let bad_index = RevisionRef {
        path: BlockPath::top(0),
        slot: RevisionSlot::Text(7),
    };
    assert!(d.resolve_revision(&bad_index, false).is_none());
    assert!(d.revision_at_range(0, 1, 2).is_none());
    assert_eq!(texts(&d.accept_revision_at(0, 1, 2)), texts(&d));
}

/// Resolving every revision one at a time lands where accept-all /
/// reject-all does — one implementation.
#[test]
fn one_at_a_time_matches_all_at_once() {
    let build = || {
        let mut d = DocumentTree::from_paragraphs([
            "keep gone stay ".to_string(),
            "new text".to_string(),
            "tail".to_string(),
        ]);
        let mut blocks = d.blocks.clone();
        if let Block::Paragraph(p) = &mut blocks[0] {
            p.revisions = vec![rev(RevisionKind::Delete, 5, 10)];
            p.mark_revision = Some(rev(RevisionKind::Delete, 0, 0));
        }
        if let Block::Paragraph(p) = &mut blocks[1] {
            p.revisions = vec![rev(RevisionKind::Insert, 0, 4)];
        }
        d.blocks = blocks;
        d
    };
    for accept in [true, false] {
        let all = build().resolve_all_revisions(accept);
        let mut one = build();
        let mut steps = 0;
        while one.has_revisions() {
            let (b, p) = one
                .blocks
                .iter()
                .enumerate()
                .find_map(|(b, blk)| {
                    blk.as_paragraph()
                        .filter(|p| !p.revisions.is_empty() || p.mark_revision.is_some())
                        .map(|p| (b as u32, p))
                })
                .expect("a paragraph with a revision");
            let (s, e) = p
                .revisions
                .first()
                .map_or((p.text.len() as u32, p.text.len() as u32), |r| {
                    (r.start, r.end)
                });
            one = if accept {
                one.accept_revision_at(b, s, e)
            } else {
                one.reject_revision_at(b, s, e)
            };
            steps += 1;
            assert!(steps < 10, "every step resolves one revision");
        }
        assert_eq!(texts(&one), texts(&all), "accept = {accept}");
    }
}
