//! Issue #305 — the single-revision accept / reject runs through the
//! accept-all resolver ([`DocumentTree::resolve_revisions`]): no inline
//! object is left pointing at removed text, pending neighbours travel
//! with their text, and every committed state passes the `markup-assert`
//! tree check (`UndoStack::push`, which now also checks inline-object
//! sentinels).

use crate::{
    Block, BlockPath, DocumentTree, ImageBlob, LogicalPos, Revision, RevisionKind, RevisionRef,
    RevisionSlot, SourceMarker, SourceMarkup, UndoStack,
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

/* ============ issue #304 — stable ids, nested wrappers, move pairs ==== */

const MOVE: &str = "move256509658";

fn with_id(r: Revision, id: u32) -> Revision {
    Revision { id: Some(id), ..r }
}

fn moved(kind: RevisionKind, start: u32, end: u32, name: &str) -> Revision {
    Revision {
        move_name: Some(name.to_string()),
        ..rev(kind, start, end)
    }
}

/// Tika-792's shape (`tools/roundtrip` `TIKA_792_BODY`): `"s."` — "s"
/// deleted, "." moved here and deleted inside the move destination
/// (`<w:moveTo><w:del>`: one range, two wrappers, the inner one recorded
/// first); `"b"` — the move source, holding an insertion.
fn tika() -> DocumentTree {
    let mut d = DocumentTree::from_paragraphs(["s.".to_string(), "b".to_string()]);
    let mut blocks = d.blocks.clone();
    if let Block::Paragraph(p) = &mut blocks[0] {
        p.revisions = vec![
            with_id(rev(RevisionKind::Delete, 0, 1), 1),
            with_id(rev(RevisionKind::Delete, 1, 2), 4),
            with_id(moved(RevisionKind::MoveTo, 1, 2, MOVE), 3),
        ];
    }
    if let Block::Paragraph(p) = &mut blocks[1] {
        p.revisions = vec![
            with_id(rev(RevisionKind::Insert, 0, 1), 7),
            with_id(moved(RevisionKind::MoveFrom, 0, 1, MOVE), 6),
        ];
    }
    d.blocks = blocks;
    d
}

fn kinds(d: &DocumentTree, block: u32) -> Vec<(RevisionKind, u32, u32)> {
    d.nth_paragraph(block)
        .map(|p| {
            p.revisions
                .iter()
                .map(|r| (r.kind, r.start, r.end))
                .collect()
        })
        .unwrap_or_default()
}

/// The id of the listed revision at `(block, slot)`.
fn id_of(d: &DocumentTree, block: u32, slot: RevisionSlot) -> u32 {
    d.revision_entries()
        .into_iter()
        .find(|e| e.at.path == BlockPath::top(block) && e.at.slot == slot)
        .map(|e| e.id)
        .expect("listed")
}

#[test]
fn every_listed_revision_has_its_own_id() {
    let d = tika();
    let entries = d.revision_entries();
    assert_eq!(entries.len(), 5);
    let ids: std::collections::HashSet<u32> = entries.iter().map(|e| e.id).collect();
    assert_eq!(ids.len(), 5, "ids are unique");
    for e in &entries {
        assert_eq!(d.revision_by_id(e.id), Some(e.at.clone()));
    }
    /* The range cannot tell the two wrappers over "." apart: it names the
    inner deletion; the outer move destination is reachable by id only. */
    assert_eq!(
        d.revision_at_range(0, 1, 2).map(|r| r.slot),
        Some(RevisionSlot::Text(1))
    );
    assert_ne!(
        id_of(&d, 0, RevisionSlot::Text(1)),
        id_of(&d, 0, RevisionSlot::Text(2))
    );
}

/// Accepting the OUTER wrapper (the move destination) by id resolves the
/// move as a pair — the destination's text stays, the source's goes —
/// and leaves both deletions in the first paragraph pending.
#[test]
fn accepting_the_outer_wrapper_resolves_the_move_pair_only() {
    let d = tika();
    let outer = d
        .revision_by_id(id_of(&d, 0, RevisionSlot::Text(2)))
        .unwrap();
    let out = d.resolve_revision(&outer, true).unwrap();
    assert_eq!(texts(&out), vec!["s.", ""]);
    assert_eq!(
        kinds(&out, 0),
        vec![(RevisionKind::Delete, 0, 1), (RevisionKind::Delete, 1, 2)]
    );
    assert!(kinds(&out, 1).is_empty());
    /* Rejecting it instead drops the destination (and the deletion
    nested in it) and keeps the source with its pending insertion. */
    let out = d.resolve_revision(&outer, false).unwrap();
    assert_eq!(texts(&out), vec!["s", "b"]);
    assert_eq!(kinds(&out, 0), vec![(RevisionKind::Delete, 0, 1)]);
    assert_eq!(kinds(&out, 1), vec![(RevisionKind::Insert, 0, 1)]);
}

/// Either half of a move resolves the pair, the move's range markers go
/// with it, and another move's markers stay.
#[test]
fn either_half_of_a_move_resolves_the_pair_and_its_markers() {
    let build = || {
        let mut d = DocumentTree::from_paragraphs(["moved stay moved".to_string()]);
        let mut blocks = d.blocks.clone();
        if let Block::Paragraph(p) = &mut blocks[0] {
            p.revisions = vec![
                moved(RevisionKind::MoveFrom, 0, 5, "m"),
                moved(RevisionKind::MoveTo, 11, 16, "m"),
            ];
            let markers = [
                (
                    0,
                    r#"<w:moveFromRangeStart w:id="1" w:author="A" w:name="m"/>"#,
                ),
                (5, r#"<w:moveFromRangeEnd w:id="1"/>"#),
                (6, r#"<w:moveToRangeStart w:id="9" w:name="n"/>"#),
                (10, r#"<w:moveToRangeEnd w:id="9"/>"#),
                (11, r#"<w:moveToRangeStart w:id="2" w:name="m"/>"#),
                (16, r#"<w:moveToRangeEnd w:id="2"/>"#),
            ];
            p.source_markup = Some(Box::new(SourceMarkup {
                text_len: 16,
                markers: markers
                    .iter()
                    .map(|(at, xml)| SourceMarker::verbatim(*at, xml.as_bytes().to_vec()))
                    .collect(),
                ..SourceMarkup::default()
            }));
        }
        d.blocks = blocks;
        d
    };
    let markers = |d: &DocumentTree| -> Vec<String> {
        d.nth_paragraph(0)
            .and_then(|p| p.source_markup.as_deref())
            .map(|m| {
                m.markers
                    .iter()
                    .map(|mk| String::from_utf8_lossy(&mk.xml).into_owned())
                    .collect()
            })
            .unwrap_or_default()
    };
    for slot in [RevisionSlot::Text(0), RevisionSlot::Text(1)] {
        let d = build();
        let at = RevisionRef {
            path: BlockPath::top(0),
            slot,
        };
        let accepted = d.resolve_revision(&at, true).unwrap();
        assert_eq!(texts(&accepted), vec![" stay moved"], "{slot:?}");
        assert!(!accepted.has_revisions());
        assert_eq!(
            markers(&accepted),
            vec![
                r#"<w:moveToRangeStart w:id="9" w:name="n"/>"#.to_string(),
                r#"<w:moveToRangeEnd w:id="9"/>"#.to_string(),
            ],
            "only move m's markers go"
        );
        let rejected = d.resolve_revision(&at, false).unwrap();
        assert_eq!(texts(&rejected), vec!["moved stay "], "{slot:?}");
        assert!(!rejected.has_revisions());
    }
}

/// A move spanning paragraphs — moved paragraph marks included — resolves
/// as one, whichever piece is addressed.
#[test]
fn a_move_across_paragraphs_resolves_every_piece() {
    let mut d = DocumentTree::from_paragraphs([
        "from".to_string(),
        "x".to_string(),
        "to".to_string(),
        "end".to_string(),
    ]);
    let mut blocks = d.blocks.clone();
    if let Block::Paragraph(p) = &mut blocks[0] {
        p.revisions = vec![moved(RevisionKind::MoveFrom, 0, 4, "m")];
        p.mark_revision = Some(moved(RevisionKind::MoveFrom, 0, 0, "m"));
    }
    if let Block::Paragraph(p) = &mut blocks[2] {
        p.revisions = vec![moved(RevisionKind::MoveTo, 0, 2, "m")];
        p.mark_revision = Some(moved(RevisionKind::MoveTo, 0, 0, "m"));
    }
    d.blocks = blocks;
    let to = id_of(&d, 2, RevisionSlot::Text(0));
    let accepted = d
        .resolve_revision(&d.revision_by_id(to).unwrap(), true)
        .unwrap();
    assert_eq!(texts(&accepted), vec!["x", "to", "end"]);
    assert!(!accepted.has_revisions());
    let from_mark = d.revision_by_id(id_of(&d, 0, RevisionSlot::Mark)).unwrap();
    let rejected = d.resolve_revision(&from_mark, false).unwrap();
    /* The destination's text and its moved-in mark go: the emptied
    paragraph vanishes; the source keeps its text and its break. */
    assert_eq!(texts(&rejected), vec!["from", "x", "end"]);
    assert!(!rejected.has_revisions());
}

/// Ids describe the revision, not its position: an edit elsewhere and
/// resolving a neighbour leave the others' ids alone; identical
/// revisions still get distinct ids.
#[test]
fn ids_survive_unrelated_edits_and_stay_unique() {
    let d = tika();
    let before: Vec<u32> = d.revision_entries().iter().map(|e| e.id).collect();
    /* Typing at the start of the first paragraph shifts every range. */
    let typed = d.insert_text(pos(0, 0), "XY");
    let after: Vec<u32> = typed.revision_entries().iter().map(|e| e.id).collect();
    assert_eq!(before, after);
    /* Accepting the leading deletion leaves the other four ids. */
    let first = id_of(&d, 0, RevisionSlot::Text(0));
    let resolved = d
        .resolve_revision(&d.revision_by_id(first).unwrap(), true)
        .unwrap();
    let rest: Vec<u32> = resolved.revision_entries().iter().map(|e| e.id).collect();
    assert_eq!(rest, before[1..].to_vec());
    assert!(resolved.revision_by_id(first).is_none());
    /* Two identical insertions in two paragraphs: two ids. */
    let mut twins = DocumentTree::from_paragraphs(["a".to_string(), "a".to_string()]);
    let mut blocks = twins.blocks.clone();
    for b in blocks.iter_mut() {
        if let Block::Paragraph(p) = b {
            p.revisions = vec![rev(RevisionKind::Insert, 0, 1)];
        }
    }
    twins.blocks = blocks;
    let ids: Vec<u32> = twins.revision_entries().iter().map(|e| e.id).collect();
    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0], ids[1]);
}
