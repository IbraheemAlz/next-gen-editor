//! Issues #252 / #253 — text-level and table-interior commands keep comment
//! anchors on their text, driven end to end through `Engine::apply` (the
//! IME commit, tracked typing, find/replace and HTML paste paths included).
//! The engine's `markup-assert` feature (dev-dependency) additionally
//! checks every committed `UndoStack::push` for stale source markup (#250).

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

fn caret_at(e: &mut Engine, block: u32, offset: u32) {
    let caret = bpos_top(block, offset);
    e.selection = Some(SelectionState {
        anchor: caret.clone(),
        caret,
        ideal_x: None,
        kind: SelectionKind::Linear,
    });
}

/// `["first", "hello target world", "third"]`, comment over "target".
fn commented_engine() -> Engine {
    let doc = DocumentTree::from_paragraphs([
        "first".into(),
        "hello target world".into(),
        "third".into(),
    ]);
    let mut e = assemble_engine(None, None);
    seed_layout(&mut e);
    e.undo = UndoStack::new(doc, 100);
    caret_at(&mut e, 0, 0);
    e.review_date = "2026-01-01T00:00:00Z".into();
    apply(
        &mut e,
        Command::InsertComment {
            range: BridgeLogicalRange {
                start: bpos_top(1, 6),
                end: bpos_top(1, 12),
            },
            text: "note".into(),
            author: "me".into(),
        },
    );
    assert_eq!(commented(&e), "target");
    e
}

fn commented(e: &Engine) -> String {
    let doc = e.undo.current();
    let r = &doc.comment_ranges[0];
    doc.text_range(r.start.clone(), r.end.clone())
}

fn start(e: &Engine) -> (u32, u32) {
    let s = &e.undo.current().comment_ranges[0].start;
    (s.path.last_block_index().unwrap(), s.offset)
}

#[test]
fn typing_before_the_comment_keeps_it_and_undo_restores() {
    let mut e = commented_engine();
    caret_at(&mut e, 1, 0);
    apply(
        &mut e,
        Command::InsertText {
            at: None,
            text: "abc ".into(),
        },
    );
    assert_eq!(commented(&e), "target");
    assert_eq!(start(&e), (1, 10));
    apply(&mut e, Command::Undo);
    assert_eq!(start(&e), (1, 6));
    assert_eq!(commented(&e), "target");
}

#[test]
fn ime_commit_before_the_comment_keeps_it() {
    let mut e = commented_engine();
    caret_at(&mut e, 1, 2);
    apply(&mut e, Command::BeginComposition { at: None });
    apply(
        &mut e,
        Command::UpdateComposition {
            text: "\u{6f22}\u{5b57}".into(),
            target_range: None,
        },
    );
    apply(&mut e, Command::EndComposition { commit: true });
    assert_eq!(commented(&e), "target");
    assert_eq!(start(&e), (1, 12));
}

#[test]
fn replace_range_before_the_comment_keeps_it() {
    let mut e = commented_engine();
    apply(
        &mut e,
        Command::ReplaceRange {
            range: BridgeLogicalRange {
                start: bpos_top(1, 0),
                end: bpos_top(1, 5),
            },
            text: "HI".into(),
        },
    );
    assert_eq!(e.undo.current().paragraph_text(1), Some("HI target world"));
    assert_eq!(commented(&e), "target");
}

#[test]
fn tracked_typing_and_backspace_keep_the_comment() {
    let mut e = commented_engine();
    apply(&mut e, Command::ToggleTrackChanges { enabled: true });
    caret_at(&mut e, 1, 0);
    apply(
        &mut e,
        Command::InsertText {
            at: None,
            text: "xyz".into(),
        },
    );
    assert_eq!(commented(&e), "target");
    /* Backspace inside the reviewer's own pending insertion removes it. */
    apply(
        &mut e,
        Command::DeleteAtCaret {
            forward: false,
            by_word: false,
        },
    );
    assert_eq!(
        e.undo.current().paragraph_text(1),
        Some("xyhello target world")
    );
    assert_eq!(commented(&e), "target");
}

#[test]
fn html_paste_before_the_comment_keeps_it() {
    let mut e = commented_engine();
    caret_at(&mut e, 1, 0);
    apply(
        &mut e,
        Command::PasteHtml {
            html: "<p>one</p><p>two</p>".into(),
        },
    );
    assert_eq!(commented(&e), "target");
    assert_eq!(start(&e).0, 2);
}

#[test]
fn deleting_across_a_paragraph_break_keeps_the_comment() {
    let mut e = commented_engine();
    apply(
        &mut e,
        Command::DeleteRange {
            range: BridgeLogicalRange {
                start: bpos_top(0, 2),
                end: bpos_top(1, 3),
            },
        },
    );
    assert_eq!(
        e.undo.current().paragraph_text(0),
        Some("filo target world")
    );
    assert_eq!(commented(&e), "target");
    assert_eq!(start(&e), (0, 5));
}

#[test]
fn deleting_the_anchor_row_moves_the_comment_to_the_next_row() {
    let doc = DocumentTree::from_paragraphs(["before".into(), "after".into()]).insert_table(
        EngineBlockPath::top(1),
        3,
        2,
    );
    let cell = |row: u32, col: u32| EngineBlockPath {
        steps: vec![
            engine::PathStep::Block(1),
            engine::PathStep::Cell { row, col },
            engine::PathStep::Block(0),
        ],
    };
    let mut doc = doc;
    for r in 0..3 {
        for c in 0..2 {
            doc = doc.insert_text(EnginePos::new(cell(r, c), 0), &format!("r{r}c{c}"));
        }
    }
    let (doc, _) = doc.insert_comment(
        EnginePos::new(cell(1, 1), 0),
        EnginePos::new(cell(1, 1), 4),
        "c".into(),
        "A".into(),
        "d".into(),
    );
    let mut e = assemble_engine(None, None);
    seed_layout(&mut e);
    e.undo = UndoStack::new(doc, 100);
    caret_at(&mut e, 0, 0);
    apply(
        &mut e,
        Command::DeleteRow {
            table_path: BridgeBlockPath::top(1),
            row: 1,
        },
    );
    let r = &e.undo.current().comment_ranges[0];
    assert_eq!(r.start.path, cell(1, 0));
    assert_eq!(
        e.undo
            .current()
            .paragraph_at_path(&r.start.path)
            .unwrap()
            .text,
        "r2c0"
    );
    apply(&mut e, Command::Undo);
    let r = &e.undo.current().comment_ranges[0];
    assert_eq!(r.start.path, cell(1, 1));
}
