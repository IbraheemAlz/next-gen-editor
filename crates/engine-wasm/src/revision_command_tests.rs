//! Issues #262 / #247 — `Command::AcceptAllRevisions` /
//! `RejectAllRevisions` end to end through `Engine::apply` (one undo step,
//! comment anchors kept, selection clamped), the paragraph-mark pilcrow
//! (paint-only: geometry unchanged) and the revisions snapshot rows.

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

/// `["drop hello ", "world tail", "last"]`: "drop " is a tracked
/// deletion, the first paragraph's MARK is deleted (a tracked merge), a
/// comment covers "world", the caret sits at the end of "last".
fn review_engine() -> Engine {
    let mut doc =
        DocumentTree::from_paragraphs(["drop hello ".into(), "world tail".into(), "last".into()]);
    if let Some(engine::Block::Paragraph(p)) = doc.blocks.get_mut(0) {
        p.revisions.push(rev(engine::RevisionKind::Delete, 0, 5));
        p.mark_revision = Some(rev(engine::RevisionKind::Delete, 0, 0));
    }
    let (doc, _) = doc.insert_comment(
        EnginePos::new(EngineBlockPath::top(1), 0),
        EnginePos::new(EngineBlockPath::top(1), 5),
        "c".into(),
        "A".into(),
        "d".into(),
    );
    let mut e = assemble_engine(None, None);
    seed_layout(&mut e);
    e.undo = UndoStack::new(doc, 100);
    let caret = bpos_top(2, 4);
    e.selection = Some(SelectionState {
        anchor: caret.clone(),
        caret,
        ideal_x: None,
        kind: SelectionKind::Linear,
    });
    e
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

fn texts(e: &Engine) -> Vec<String> {
    e.undo
        .current()
        .blocks
        .iter()
        .filter_map(engine::Block::as_paragraph)
        .map(|p| p.text.clone())
        .collect()
}

fn commented(e: &Engine) -> String {
    let doc = e.undo.current();
    let r = &doc.comment_ranges[0];
    doc.text_range(r.start.clone(), r.end.clone())
}

#[test]
fn accept_all_is_one_undo_step_and_keeps_the_comment() {
    let mut e = review_engine();
    let depth = e.undo.depth();
    apply(&mut e, Command::AcceptAllRevisions);
    assert_eq!(e.undo.depth(), depth + 1, "exactly one undo step");
    assert_eq!(texts(&e), vec!["hello world tail", "last"]);
    assert_eq!(commented(&e), "world");
    assert!(!e.undo.current().has_revisions());
    assert!(selection_valid(&e));
    apply(&mut e, Command::Undo);
    assert_eq!(texts(&e), vec!["drop hello ", "world tail", "last"]);
    assert_eq!(commented(&e), "world");
    assert!(e.undo.current().has_revisions());
}

#[test]
fn reject_all_keeps_the_text_and_the_break() {
    let mut e = review_engine();
    apply(&mut e, Command::RejectAllRevisions);
    assert_eq!(texts(&e), vec!["drop hello ", "world tail", "last"]);
    assert!(!e.undo.current().has_revisions());
    assert_eq!(commented(&e), "world");
}

#[test]
fn accept_all_without_revisions_pushes_nothing() {
    let mut e = review_engine();
    apply(&mut e, Command::RejectAllRevisions);
    let depth = e.undo.depth();
    apply(&mut e, Command::AcceptAllRevisions);
    apply(&mut e, Command::RejectAllRevisions);
    assert_eq!(e.undo.depth(), depth);
}

#[test]
fn the_selection_is_clamped_after_a_merge() {
    let mut e = review_engine();
    /* The caret in the paragraph that merges away. */
    let caret = bpos_top(2, 4);
    e.selection = Some(SelectionState {
        anchor: bpos_top(1, 10),
        caret,
        ideal_x: None,
        kind: SelectionKind::Linear,
    });
    apply(&mut e, Command::AcceptAllRevisions);
    assert!(selection_valid(&e));
}

/// The review pilcrow is paint-only: the mark revision adds fill
/// commands, the geometry fingerprint is unchanged.
#[test]
fn a_mark_revision_paints_a_pilcrow_without_moving_geometry() {
    let with_mark = review_engine();
    let mut without = review_engine();
    let mut doc = without.undo.current().clone();
    if let Some(engine::Block::Paragraph(p)) = doc.blocks.get_mut(0) {
        p.mark_revision = None;
    }
    without.undo = UndoStack::new(doc, 100);
    let (pages_a, ..) = with_mark.build_pages(1.0, false, None).expect("layout");
    let (pages_b, ..) = without.build_pages(1.0, false, None).expect("layout");
    assert_eq!(
        layout::geometry_fingerprint(&pages_a),
        layout::geometry_fingerprint(&pages_b)
    );
    let fills = |pages: &[layout::PageBox]| {
        render::scene::build_document_scene(pages, 0.0)
            .cmds
            .iter()
            .filter(|c| matches!(c, render::scene::DisplayCmd::FillRect { .. }))
            .count()
    };
    /* Everything else paints the same; the pilcrow adds 4 rects. */
    assert_eq!(fills(&pages_a), fills(&pages_b) + 4);
}

#[test]
fn the_snapshot_lists_the_mark_revision_at_the_paragraph_end() {
    let e = review_engine();
    let doc = e.undo.current();
    let p = doc.nth_paragraph(0).unwrap();
    /* What `revisions_snapshot` emits for the mark row, and what the
    single accept resolves. */
    let end = p.text.len() as u32;
    let accepted = doc.accept_revision_at(0, end, end);
    assert_eq!(
        accepted.nth_paragraph(0).map(|p| p.text.as_str()),
        Some("drop hello world tail")
    );
}
