//! Issue #305 — `Command::AcceptRevision` / `RejectRevision` end to end
//! through `Engine::apply`: the single revision resolves through the
//! accept-all resolver (one undo step, selection clamped like
//! accept-all's), and an address that names no revision pushes nothing.

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

/// `["gone", "kept"]`: the first paragraph — text and mark — is a
/// tracked deletion; the caret sits at the end of "gone".
fn engine_with_deleted_paragraph() -> Engine {
    let mut doc = DocumentTree::from_paragraphs(["gone".into(), "kept".into()]);
    if let Some(engine::Block::Paragraph(p)) = doc.blocks.get_mut(0) {
        p.revisions.push(rev(engine::RevisionKind::Delete, 0, 4));
        p.mark_revision = Some(rev(engine::RevisionKind::Delete, 0, 0));
    }
    let mut e = assemble_engine(None, None);
    seed_layout(&mut e);
    e.undo = UndoStack::new(doc, 100);
    let caret = bpos_top(1, 4);
    e.selection = Some(SelectionState {
        anchor: bpos_top(0, 4),
        caret,
        ideal_x: None,
        kind: SelectionKind::Linear,
    });
    e
}

#[test]
fn single_accepts_are_one_undo_step_each_and_clamp_the_selection() {
    let mut e = engine_with_deleted_paragraph();
    let depth = e.undo.depth();
    apply(
        &mut e,
        Command::AcceptRevision {
            block: 0,
            start: 0,
            end: 4,
        },
    );
    assert_eq!(e.undo.depth(), depth + 1);
    assert_eq!(texts(&e), vec!["", "kept"]);
    /* The emptied paragraph's mark now sits at its (empty) end. */
    apply(
        &mut e,
        Command::AcceptRevision {
            block: 0,
            start: 0,
            end: 0,
        },
    );
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
    for cmd in [
        Command::AcceptRevision {
            block: 0,
            start: 1,
            end: 3,
        },
        Command::RejectRevision {
            block: 7,
            start: 0,
            end: 4,
        },
        Command::RejectRevision {
            block: 1,
            start: 4,
            end: 4,
        },
    ] {
        apply(&mut e, cmd);
    }
    assert_eq!(e.undo.depth(), depth);
    assert_eq!(texts(&e), vec!["gone", "kept"]);
    let p = e.undo.current().nth_paragraph(0).unwrap();
    assert_eq!(p.revisions.len(), 1);
    assert!(p.mark_revision.is_some());
}
