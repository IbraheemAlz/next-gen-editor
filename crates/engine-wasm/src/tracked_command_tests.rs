//! Issues #301 / #298 — structural tracked edits end to end through
//! `Engine::apply`: Enter with review mode on records an inserted
//! paragraph mark, a delete across paragraph marks records deleted marks,
//! and Accept / Reject (all, or the single mark row) resolve them as one
//! undo step each.

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

/// A laid-out engine over `paras`, review mode ON, caret at `(0, 0)`.
fn tracking_engine(paras: &[&str]) -> Engine {
    let doc = DocumentTree::from_paragraphs(paras.iter().map(|s| s.to_string()));
    let mut e = assemble_engine(None, None);
    seed_layout(&mut e);
    e.undo = UndoStack::new(doc, 100);
    caret(&mut e, 0, 0);
    apply(&mut e, Command::ToggleTrackChanges { enabled: true });
    e
}

fn caret(e: &mut Engine, block: u32, offset: u32) {
    select(e, (block, offset), (block, offset));
}

fn select(e: &mut Engine, anchor: (u32, u32), caret: (u32, u32)) {
    e.selection = Some(SelectionState {
        anchor: bpos_top(anchor.0, anchor.1),
        caret: bpos_top(caret.0, caret.1),
        ideal_x: None,
        kind: SelectionKind::Linear,
    });
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

fn mark_kinds(e: &Engine) -> Vec<Vec<engine::RevisionKind>> {
    e.undo
        .current()
        .blocks
        .iter()
        .filter_map(engine::Block::as_paragraph)
        .map(|p| p.mark_revisions.iter().map(|r| r.kind).collect())
        .collect()
}

fn caret_pos(e: &Engine) -> (u32, u32) {
    let c = &e.selection.as_ref().expect("selection").caret;
    (c.path.last_block_index().unwrap_or(0), c.offset)
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

/* ======================= issue #301 — tracked Enter ==== */

#[test]
fn tracked_enter_marks_the_new_paragraph_mark_inserted() {
    let mut e = tracking_engine(&["alpha beta"]);
    caret(&mut e, 0, 5);
    let depth = e.undo.depth();
    apply(&mut e, Command::SplitParagraph { at: None });
    assert_eq!(e.undo.depth(), depth + 1);
    assert_eq!(texts(&e), vec!["alpha", " beta"]);
    assert_eq!(
        mark_kinds(&e),
        vec![vec![engine::RevisionKind::Insert], vec![]]
    );
    let mark = &e.undo.current().nth_paragraph(0).unwrap().mark_revisions[0];
    assert_eq!(mark.author, e.review_author);
    assert_eq!(caret_pos(&e), (1, 0));
    /* Reject-all merges the halves back as ONE undo step. */
    apply(&mut e, Command::RejectAllRevisions);
    assert_eq!(texts(&e), vec!["alpha beta"]);
    assert!(!e.undo.current().has_revisions());
    assert!(selection_valid(&e));
    apply(&mut e, Command::Undo);
    assert_eq!(texts(&e), vec!["alpha", " beta"]);
    /* Accept-all keeps the break. */
    apply(&mut e, Command::AcceptAllRevisions);
    assert_eq!(texts(&e), vec!["alpha", " beta"]);
    assert!(!e.undo.current().has_revisions());
}

#[test]
fn rejecting_the_mark_row_merges_a_tracked_enter() {
    let mut e = tracking_engine(&["alpha beta"]);
    caret(&mut e, 0, 5);
    apply(&mut e, Command::SplitParagraph { at: None });
    apply(
        &mut e,
        Command::RejectRevision {
            block: 0,
            start: 5,
            end: 5,
            revision_id: None,
        },
    );
    assert_eq!(texts(&e), vec!["alpha beta"]);
    assert!(selection_valid(&e));
}

#[test]
fn tracked_enter_over_a_selection_marks_it_deleted_then_breaks() {
    let mut e = tracking_engine(&["alpha beta gamma"]);
    select(&mut e, (0, 6), (0, 10));
    apply(&mut e, Command::SplitParagraph { at: None });
    /* The selection stays (struck), the break lands at its start. */
    assert_eq!(texts(&e), vec!["alpha ", "beta gamma"]);
    assert_eq!(
        mark_kinds(&e),
        vec![vec![engine::RevisionKind::Insert], vec![]]
    );
    let p1 = e.undo.current().nth_paragraph(1).unwrap();
    assert_eq!(
        p1.revisions
            .iter()
            .map(|r| (r.kind, r.start, r.end))
            .collect::<Vec<_>>(),
        vec![(engine::RevisionKind::Delete, 0, 4)]
    );
    apply(&mut e, Command::RejectAllRevisions);
    assert_eq!(texts(&e), vec!["alpha beta gamma"]);
    assert!(!e.undo.current().has_revisions());
}

#[test]
fn enter_with_review_mode_off_stays_plain() {
    let mut e = tracking_engine(&["alpha beta"]);
    apply(&mut e, Command::ToggleTrackChanges { enabled: false });
    caret(&mut e, 0, 5);
    apply(&mut e, Command::SplitParagraph { at: None });
    assert_eq!(texts(&e), vec!["alpha", " beta"]);
    assert!(!e.undo.current().has_revisions());
}

/* ======================= issue #298 — cross-paragraph tracked delete ==== */

fn delete_at_caret(e: &mut Engine, forward: bool) -> Event {
    apply(
        e,
        Command::DeleteAtCaret {
            forward,
            by_word: false,
        },
    )
}

#[test]
fn deleting_a_selection_across_paragraphs_marks_it_instead_of_doing_nothing() {
    let mut e = tracking_engine(&["alpha beta", "middle", "gamma delta"]);
    select(&mut e, (0, 6), (2, 6));
    let depth = e.undo.depth();
    delete_at_caret(&mut e, false);
    assert_eq!(e.undo.depth(), depth + 1, "one undo step");
    assert_eq!(texts(&e), vec!["alpha beta", "middle", "gamma delta"]);
    assert_eq!(
        mark_kinds(&e),
        vec![
            vec![engine::RevisionKind::Delete],
            vec![engine::RevisionKind::Delete],
            vec![]
        ]
    );
    assert_eq!(
        caret_pos(&e),
        (2, 6),
        "the caret lands after the struck text"
    );
    apply(&mut e, Command::AcceptAllRevisions);
    assert_eq!(texts(&e), vec!["alpha delta"]);
    assert!(selection_valid(&e));
    apply(&mut e, Command::Undo);
    apply(&mut e, Command::RejectAllRevisions);
    assert_eq!(texts(&e), vec!["alpha beta", "middle", "gamma delta"]);
    assert!(!e.undo.current().has_revisions());
}

#[test]
fn backspace_at_a_paragraph_start_marks_the_mark_and_steps_left() {
    let mut e = tracking_engine(&["one", "two"]);
    caret(&mut e, 1, 0);
    delete_at_caret(&mut e, false);
    assert_eq!(texts(&e), vec!["one", "two"]);
    assert_eq!(
        mark_kinds(&e),
        vec![vec![engine::RevisionKind::Delete], vec![]]
    );
    /* Word-like: the caret steps over the struck mark, so the next
    Backspace reaches the character before it (not the same mark). */
    assert_eq!(caret_pos(&e), (0, 3));
    delete_at_caret(&mut e, false);
    assert_eq!(caret_pos(&e), (0, 2));
    let p0 = e.undo.current().nth_paragraph(0).unwrap();
    assert_eq!(
        p0.revisions
            .iter()
            .map(|r| (r.kind, r.start, r.end))
            .collect::<Vec<_>>(),
        vec![(engine::RevisionKind::Delete, 2, 3)]
    );
    apply(&mut e, Command::AcceptAllRevisions);
    assert_eq!(texts(&e), vec!["ontwo"]);
}

#[test]
fn forward_delete_at_a_paragraph_end_marks_the_mark() {
    let mut e = tracking_engine(&["one", "two"]);
    caret(&mut e, 0, 3);
    delete_at_caret(&mut e, true);
    assert_eq!(
        mark_kinds(&e),
        vec![vec![engine::RevisionKind::Delete], vec![]]
    );
    assert_eq!(caret_pos(&e), (1, 0));
    apply(
        &mut e,
        Command::RejectRevision {
            block: 0,
            start: 3,
            end: 3,
            revision_id: None,
        },
    );
    assert_eq!(texts(&e), vec!["one", "two"]);
    assert!(!e.undo.current().has_revisions());
}

#[test]
fn backspace_over_your_own_tracked_enter_removes_it() {
    let mut e = tracking_engine(&["alpha beta"]);
    caret(&mut e, 0, 5);
    apply(&mut e, Command::SplitParagraph { at: None });
    assert_eq!(caret_pos(&e), (1, 0));
    delete_at_caret(&mut e, false);
    assert_eq!(texts(&e), vec!["alpha beta"]);
    assert!(!e.undo.current().has_revisions());
    assert_eq!(caret_pos(&e), (0, 5));
}

#[test]
fn typing_over_a_cross_paragraph_selection_marks_and_inserts() {
    let mut e = tracking_engine(&["one", "two"]);
    select(&mut e, (0, 1), (1, 1));
    apply(
        &mut e,
        Command::InsertText {
            at: None,
            text: "X".into(),
        },
    );
    assert_eq!(texts(&e), vec!["oXne", "two"]);
    apply(&mut e, Command::AcceptAllRevisions);
    assert_eq!(texts(&e), vec!["oXwo"]);
}

#[test]
fn a_tracked_delete_over_a_table_answers_an_error() {
    let doc = DocumentTree::from_paragraphs(["before".to_string(), "after".to_string()])
        .insert_table(engine::BlockPath::top(1), 1, 1);
    let after = doc
        .blocks
        .iter()
        .rposition(|b| b.as_paragraph().is_some())
        .expect("paragraph") as u32;
    let mut e = tracking_engine(&["x"]);
    e.undo = UndoStack::new(doc, 100);
    let depth = e.undo.depth();
    let evt = block_on(e.apply(Command::DeleteRange {
        range: BridgeLogicalRange {
            start: bpos_top(0, 2),
            end: bpos_top(after, 2),
        },
    }));
    match evt {
        Event::Error { message, .. } => {
            assert!(message.starts_with("DeleteRange: "), "{message}");
            assert!(message.contains("table"), "{message}");
        }
        other => panic!("expected a typed error, got {other:?}"),
    }
    assert_eq!(e.undo.depth(), depth, "nothing pushed");
}
