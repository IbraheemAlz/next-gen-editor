//! Issue #387 — comment highlights (per-line rects through the body
//! geometry, like selection rects) and the `SelectionChanged.reveal_caret`
//! stamp the shell's caret scroller follows, driven through `Engine::apply`.

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

fn apply(e: &mut Engine, cmd: Command) -> Event {
    let evt = block_on(e.apply(cmd));
    assert!(!matches!(evt, Event::Error { .. }), "{evt:?}");
    evt
}

fn engine_of(texts: &[&str]) -> Engine {
    crate::tests::test_engine_with_doc(DocumentTree::from_paragraphs(
        texts.iter().map(|t| t.to_string()),
    ))
}

fn range(a: (u32, u32), b: (u32, u32)) -> BridgeLogicalRange {
    BridgeLogicalRange {
        start: bpos_top(a.0, a.1),
        end: bpos_top(b.0, b.1),
    }
}

fn comment(e: &mut Engine, a: (u32, u32), b: (u32, u32)) {
    apply(
        e,
        Command::InsertComment {
            range: range(a, b),
            text: "note".into(),
            author: "Ada".into(),
        },
    );
}

fn highlights(e: &Engine) -> Vec<bridge::CommentHighlight> {
    match e.comment_highlights_event() {
        Some(Event::CommentHighlights { highlights }) => highlights,
        other => panic!("expected CommentHighlights, got {other:?}"),
    }
}

/// The selection rects the engine reports for `r` — the reference the
/// highlight must equal.
fn selection_rects(e: &mut Engine, r: BridgeLogicalRange) -> Vec<BridgeRect> {
    let caret = r.end.clone();
    match apply(e, Command::SetSelection { range: r, caret }) {
        Event::SelectionChanged { rects, .. } => rects,
        other => panic!("{other:?}"),
    }
}

fn caret_to(e: &mut Engine, para: u32, offset: u32) {
    apply(
        e,
        Command::SetSelection {
            range: range((para, offset), (para, offset)),
            caret: bpos_top(para, offset),
        },
    );
}

#[test]
fn a_document_without_comments_broadcasts_nothing() {
    let e = engine_of(&["alpha beta gamma"]);
    assert!(e.comment_highlights_event().is_none());
}

#[test]
fn a_comment_highlights_exactly_its_selection_rects() {
    let mut e = engine_of(&["alpha beta gamma"]);
    comment(&mut e, (0, 6), (0, 10));
    let hs = highlights(&e);
    assert_eq!(hs.len(), 1);
    assert_eq!(hs[0].author, "Ada");
    assert!(!hs[0].resolved);
    assert_eq!(hs[0].rects.len(), 1);
    let want = selection_rects(&mut e, range((0, 6), (0, 10)));
    let got = &hs[0].rects;
    assert_eq!(want.len(), got.len());
    for (w, g) in want.iter().zip(got) {
        assert_eq!((w.x, w.y, w.w, w.h), (g.x, g.y, g.w, g.h));
    }
}

#[test]
fn the_highlight_follows_typing_before_the_anchor() {
    let mut e = engine_of(&["alpha beta gamma"]);
    comment(&mut e, (0, 6), (0, 10));
    let before = highlights(&e)[0].rects[0];
    caret_to(&mut e, 0, 0);
    apply(
        &mut e,
        Command::InsertText {
            at: None,
            text: "xx ".into(),
        },
    );
    let after = highlights(&e)[0].rects[0];
    assert!(after.x > before.x, "{before:?} -> {after:?}");
    assert!((after.w - before.w).abs() < 0.01, "{before:?} -> {after:?}");
    /* Still the commented word, now three bytes later. */
    let want = selection_rects(&mut e, range((0, 9), (0, 13)))[0];
    assert_eq!((want.x, want.w), (after.x, after.w));
}

#[test]
fn a_comment_across_paragraphs_gets_one_rect_per_line() {
    let mut e = engine_of(&["first line", "second line"]);
    comment(&mut e, (0, 6), (1, 6));
    let rects = &highlights(&e)[0].rects;
    assert_eq!(rects.len(), 2, "{rects:?}");
    assert!(rects[1].y > rects[0].y);
}

#[test]
fn replies_ride_their_roots_range() {
    let mut e = engine_of(&["alpha beta gamma"]);
    comment(&mut e, (0, 6), (0, 10));
    let root = highlights(&e)[0].id;
    apply(
        &mut e,
        Command::ReplyToComment {
            parent_id: root,
            text: "re".into(),
            author: "Bob".into(),
        },
    );
    let hs = highlights(&e);
    assert_eq!(hs.len(), 1, "a reply is not a second highlight");
    assert_eq!(hs[0].id, root);
}

#[test]
fn a_point_comment_gets_a_narrow_marker() {
    let mut e = engine_of(&["alpha beta gamma"]);
    comment(&mut e, (0, 6), (0, 6));
    let rects = &highlights(&e)[0].rects;
    assert_eq!(rects.len(), 1);
    assert!(rects[0].w > 0.0 && rects[0].w < 10.0, "{rects:?}");
    assert!(rects[0].h > 0.0);
}

#[test]
fn deleting_the_last_comment_returns_to_no_highlights() {
    let mut e = engine_of(&["alpha beta gamma"]);
    comment(&mut e, (0, 6), (0, 10));
    let id = highlights(&e)[0].id;
    apply(&mut e, Command::DeleteComment { id });
    assert!(e.comment_highlights_event().is_none());
}

fn reveal_of(evt: Event) -> bool {
    match evt {
        Event::SelectionChanged { reveal_caret, .. } => reveal_caret,
        other => panic!("{other:?}"),
    }
}

#[test]
fn keyboard_moves_and_edits_reveal_the_caret_pointer_gestures_do_not() {
    let mut e = engine_of(&["alpha beta gamma", "second"]);
    caret_to(&mut e, 0, 0);
    assert!(reveal_of(apply(
        &mut e,
        Command::MoveCaret {
            direction: bridge::MoveDirection::Down,
            extend: false,
        },
    )));
    assert!(reveal_of(apply(
        &mut e,
        Command::InsertText {
            at: None,
            text: "x".into(),
        },
    )));
    assert!(!reveal_of(apply(
        &mut e,
        Command::PlaceCaretAtPoint {
            page: 0,
            at: bridge::Point { x: 100.0, y: 80.0 },
        },
    )));
    assert!(!reveal_of(apply(&mut e, Command::SelectAll)));
    assert!(!reveal_of(apply(&mut e, Command::SetZoom { scale: 1.5 })));
}
