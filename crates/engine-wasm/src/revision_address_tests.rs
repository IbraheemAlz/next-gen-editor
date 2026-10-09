//! Issues #305 / #304 — `Command::AcceptRevision` / `RejectRevision` end
//! to end through `Engine::apply`: the single revision resolves through
//! the accept-all resolver (one undo step, selection clamped like
//! accept-all's), an address that names no revision pushes nothing, and
//! the `revisions_snapshot` rows carry a stable `revision_id` that
//! addresses every listed revision — the outer of two wrappers over one
//! range included — with a tracked move resolving as a pair.

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

fn seed_layout(e: &mut Engine) {
    let bytes = include_bytes!("../../../ts/fonts/LiberationSans-Regular.ttf").to_vec();
    let font = LoadedFont::parse("test-latin".to_string(), bytes).expect("parse test font");
    e.fonts.insert("test-latin".to_string(), Arc::new(font));
    e.layout_cfg = Some(RenderConfig {
        font_id: "test-latin".to_string(),
        base_direction: ShapingDirection::Ltr,
        px_size: 16.0,
        line_height: 26.0,
        alignment: Alignment::Start,
        scale: 1.0,
        base_scale: 1.0,
        zoom: 1.0,
    });
}

fn apply(e: &mut Engine, cmd: Command) -> Event {
    let evt = block_on(e.apply(cmd));
    assert!(!matches!(evt, Event::Error { .. }), "{evt:?}");
    evt
}

fn accept(block: u32, start: u32, end: u32, revision_id: Option<u32>) -> Command {
    Command::AcceptRevision {
        block,
        start,
        end,
        revision_id,
    }
}

fn reject(block: u32, start: u32, end: u32, revision_id: Option<u32>) -> Command {
    Command::RejectRevision {
        block,
        start,
        end,
        revision_id,
    }
}

fn rev(kind: engine::RevisionKind, start: u32, end: u32) -> engine::Revision {
    engine::Revision {
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

fn texts(e: &Engine) -> Vec<String> {
    e.undo
        .current()
        .blocks
        .iter()
        .filter_map(engine::Block::as_paragraph)
        .map(|p| p.text.clone())
        .collect()
}

fn selection_valid(e: &Engine) -> bool {
    match &e.selection {
        None => true,
        Some(sel) => e.with_selection_doc(|d| {
            clamp_pos(d, sel.anchor.clone()) == sel.anchor
                && clamp_pos(d, sel.caret.clone()) == sel.caret
        }),
    }
}

/// An engine on `doc` with the caret at the start of the document.
fn engine_with(doc: DocumentTree) -> Engine {
    let mut e = assemble_engine(None, None);
    seed_layout(&mut e);
    e.undo = UndoStack::new(doc, 100);
    e.selection = Some(SelectionState {
        anchor: bpos_top(0, 0),
        caret: bpos_top(0, 0),
        ideal_x: None,
        kind: SelectionKind::Linear,
    });
    e
}

/// `["gone", "kept"]`: the first paragraph — text and mark — is a
/// tracked deletion; the selection runs from the end of "gone" into
/// "kept".
fn engine_with_deleted_paragraph() -> Engine {
    let mut doc = DocumentTree::from_paragraphs(["gone".into(), "kept".into()]);
    if let Some(engine::Block::Paragraph(p)) = doc.blocks.get_mut(0) {
        p.revisions.push(rev(engine::RevisionKind::Delete, 0, 4));
        p.mark_revision = Some(rev(engine::RevisionKind::Delete, 0, 0));
    }
    let mut e = engine_with(doc);
    e.selection = Some(SelectionState {
        anchor: bpos_top(0, 4),
        caret: bpos_top(1, 4),
        ideal_x: None,
        kind: SelectionKind::Linear,
    });
    e
}

#[test]
fn single_accepts_are_one_undo_step_each_and_clamp_the_selection() {
    let mut e = engine_with_deleted_paragraph();
    let depth = e.undo.depth();
    apply(&mut e, accept(0, 0, 4, None));
    assert_eq!(e.undo.depth(), depth + 1);
    assert_eq!(texts(&e), vec!["", "kept"]);
    /* The emptied paragraph's mark now sits at its (empty) end. */
    apply(&mut e, accept(0, 0, 0, None));
    assert_eq!(e.undo.depth(), depth + 2);
    assert_eq!(texts(&e), vec!["kept"]);
    assert!(!e.undo.current().has_revisions());
    assert!(selection_valid(&e), "the caret left the vanished paragraph");
    apply(&mut e, Command::Undo);
    apply(&mut e, Command::Undo);
    assert_eq!(texts(&e), vec!["gone", "kept"]);
    assert!(e.undo.current().has_revisions());
}

#[test]
fn an_address_naming_no_revision_pushes_no_undo_step() {
    let mut e = engine_with_deleted_paragraph();
    let depth = e.undo.depth();
    let stale = revision_rows(e.undo.current())
        .iter()
        .map(|r| r.revision_id)
        .max()
        .unwrap()
        .wrapping_add(7);
    for cmd in [
        accept(0, 1, 3, None),
        reject(7, 0, 4, None),
        reject(1, 4, 4, None),
        /* An id wins over the range: a stale id is a no-op even though
        (0, 0, 4) still names a revision. */
        accept(0, 0, 4, Some(stale)),
    ] {
        apply(&mut e, cmd);
    }
    assert_eq!(e.undo.depth(), depth);
    assert_eq!(texts(&e), vec!["gone", "kept"]);
    let p = e.undo.current().nth_paragraph(0).unwrap();
    assert_eq!(p.revisions.len(), 1);
    assert!(p.mark_revision.is_some());
}

/// Tika-792's shape: `"s."` — "s" deleted, "." moved here and deleted
/// inside the move destination (`<w:moveTo><w:del>`, inner recorded
/// first); `"b"` — the move source, holding an insertion.
fn tika_engine() -> Engine {
    let mv = |kind, s, e| engine::Revision {
        move_name: Some("move256509658".into()),
        ..rev(kind, s, e)
    };
    let mut doc = DocumentTree::from_paragraphs(["s.".into(), "b".into()]);
    if let Some(engine::Block::Paragraph(p)) = doc.blocks.get_mut(0) {
        p.revisions = vec![
            rev(engine::RevisionKind::Delete, 0, 1),
            rev(engine::RevisionKind::Delete, 1, 2),
            mv(engine::RevisionKind::MoveTo, 1, 2),
        ];
    }
    if let Some(engine::Block::Paragraph(p)) = doc.blocks.get_mut(1) {
        p.revisions = vec![
            rev(engine::RevisionKind::Insert, 0, 1),
            mv(engine::RevisionKind::MoveFrom, 0, 1),
        ];
    }
    engine_with(doc)
}

/// Every snapshot row carries its own id; the two rows over (0, 1..2)
/// share a range but not an id.
#[test]
fn snapshot_rows_carry_unique_revision_ids() {
    let e = tika_engine();
    let rows = revision_rows(e.undo.current());
    let shape: Vec<(u32, u32, u32, &str)> = rows
        .iter()
        .map(|r| (r.block, r.start, r.end, r.kind))
        .collect();
    assert_eq!(
        shape,
        vec![
            (0, 0, 1, "delete"),
            (0, 1, 2, "delete"),
            (0, 1, 2, "move-to"),
            (1, 0, 1, "insert"),
            (1, 0, 1, "move-from"),
        ]
    );
    let ids: std::collections::HashSet<u32> = rows.iter().map(|r| r.revision_id).collect();
    assert_eq!(ids.len(), rows.len());
}

/// Accepting the outer `move-to` row by id resolves the move as a pair
/// (the source "b" goes) in ONE undo step, and leaves the deletions —
/// the one nested under the same range included — listed and pending.
#[test]
fn the_outer_wrapper_is_addressable_and_the_move_resolves_as_a_pair() {
    let mut e = tika_engine();
    let rows = revision_rows(e.undo.current());
    let outer = rows.iter().find(|r| r.kind == "move-to").unwrap().clone();
    let depth = e.undo.depth();
    apply(
        &mut e,
        accept(outer.block, outer.start, outer.end, Some(outer.revision_id)),
    );
    assert_eq!(e.undo.depth(), depth + 1);
    assert_eq!(texts(&e), vec!["s.", ""]);
    let left = revision_rows(e.undo.current());
    let kinds: Vec<&str> = left.iter().map(|r| r.kind).collect();
    assert_eq!(kinds, vec!["delete", "delete"]);
    /* The surviving rows kept their ids. */
    assert_eq!(left[0].revision_id, rows[0].revision_id);
    assert_eq!(left[1].revision_id, rows[1].revision_id);
    /* The same id again (a double click) names nothing now. */
    apply(
        &mut e,
        accept(outer.block, outer.start, outer.end, Some(outer.revision_id)),
    );
    assert_eq!(e.undo.depth(), depth + 1);
    apply(&mut e, Command::Undo);
    assert_eq!(revision_rows(e.undo.current()), rows);
}

/// Rejecting the `move-from` half by id rejects the destination too.
#[test]
fn rejecting_the_source_half_rejects_the_destination() {
    let mut e = tika_engine();
    let rows = revision_rows(e.undo.current());
    let source = rows.iter().find(|r| r.kind == "move-from").unwrap().clone();
    apply(
        &mut e,
        reject(
            source.block,
            source.start,
            source.end,
            Some(source.revision_id),
        ),
    );
    /* The destination "." is gone (its nested deletion with it); the
    source keeps "b" and its pending insertion. */
    assert_eq!(texts(&e), vec!["s", "b"]);
    let kinds: Vec<&str> = revision_rows(e.undo.current())
        .iter()
        .map(|r| r.kind)
        .collect();
    assert_eq!(kinds, vec!["delete", "insert"]);
}
