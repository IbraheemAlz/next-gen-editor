//! Issue #152 — every block-level command keeps comment anchors on the
//! paragraph they were attached to, and undo restores the pre-command
//! anchors. Driven end to end through `Engine::apply`.

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
    block_on(e.apply(cmd))
}

/// `["first", "second", "target"]` with a comment over "target" and the
/// caret at the start of the document.
fn commented_engine() -> Engine {
    let doc = DocumentTree::from_paragraphs(["first".into(), "second".into(), "target".into()]);
    let mut e = assemble_engine(None, None);
    seed_layout(&mut e);
    e.undo = UndoStack::new(doc, 100);
    let caret = bpos_top(0, 0);
    e.selection = Some(SelectionState {
        anchor: caret.clone(),
        caret,
        ideal_x: None,
        kind: SelectionKind::Linear,
    });
    e.review_date = "2026-01-01T00:00:00Z".into();
    let evt = apply(
        &mut e,
        Command::InsertComment {
            range: BridgeLogicalRange {
                start: bpos_top(2, 0),
                end: bpos_top(2, 6),
            },
            text: "note".into(),
            author: "me".into(),
        },
    );
    assert!(matches!(evt, Event::SelectionChanged { .. }), "{evt:?}");
    assert_eq!(commented(&e), "target");
    e
}

/// The text the single comment covers in the current document.
fn commented(e: &Engine) -> String {
    let doc = e.undo.current();
    let r = &doc.comment_ranges[0];
    doc.text_range(r.start.clone(), r.end.clone())
}

fn start_block(e: &Engine) -> u32 {
    e.undo.current().comment_ranges[0]
        .start
        .path
        .last_block_index()
        .unwrap()
}

/// Run `cmd`, assert the comment still covers "target" and moved to
/// `expected_block`, then undo and assert the original anchor is back.
fn check(cmd: Command, expected_block: u32) {
    let mut e = commented_engine();
    let evt = apply(&mut e, cmd);
    assert!(!matches!(evt, Event::Error { .. }), "{evt:?}");
    assert_eq!(commented(&e), "target");
    assert_eq!(start_block(&e), expected_block);
    apply(&mut e, Command::Undo);
    assert_eq!(start_block(&e), 2);
    assert_eq!(commented(&e), "target");
    apply(&mut e, Command::Redo);
    assert_eq!(start_block(&e), expected_block);
    assert_eq!(commented(&e), "target");
}

#[test]
fn insert_table_above_keeps_the_comment() {
    check(
        Command::InsertTable {
            at: BridgeBlockPath::top(0),
            rows: 2,
            cols: 2,
        },
        3,
    );
}

#[test]
fn insert_toc_above_keeps_the_comment() {
    /* Caret at a paragraph start → the stub (then the regenerated
    "no entries" result) lands in front of it. */
    check(
        Command::InsertToc {
            at: bpos_top(1, 0),
            switches: bridge::TocSwitches::default(),
        },
        3,
    );
}

#[test]
fn insert_page_break_above_keeps_the_comment() {
    check(Command::InsertPageBreak { at: bpos_top(1, 0) }, 2);
}

#[test]
fn insert_section_break_above_keeps_the_comment() {
    check(
        Command::InsertSectionBreak {
            at: bpos_top(0, 2),
            kind: SectionBreakKind::NextPage,
        },
        3,
    );
}

#[test]
fn delete_table_above_keeps_the_comment() {
    let mut e = commented_engine();
    apply(
        &mut e,
        Command::InsertTable {
            at: BridgeBlockPath::top(1),
            rows: 1,
            cols: 1,
        },
    );
    assert_eq!(start_block(&e), 3);
    let evt = apply(
        &mut e,
        Command::DeleteTable {
            path: BridgeBlockPath::top(1),
        },
    );
    assert!(!matches!(evt, Event::Error { .. }), "{evt:?}");
    assert_eq!(start_block(&e), 2);
    assert_eq!(commented(&e), "target");
    apply(&mut e, Command::Undo);
    assert_eq!(start_block(&e), 3);
    assert_eq!(commented(&e), "target");
}

#[test]
fn update_fields_regenerating_a_toc_keeps_the_comment() {
    let mut e = commented_engine();
    apply(
        &mut e,
        Command::InsertToc {
            at: bpos_top(0, 0),
            switches: bridge::TocSwitches::default(),
        },
    );
    assert_eq!(commented(&e), "target");
    /* Promote "second" to a heading: F9 now grows the TOC result. */
    let before = e.undo.current().blocks.len();
    let mut doc = e.undo.current().clone();
    let idx = (0..doc.blocks.len())
        .find(|&i| {
            doc.paragraph_at_path(&EngineBlockPath::top(i as u32))
                .is_some_and(|p| p.text == "second")
        })
        .unwrap();
    if let Some(engine::Block::Paragraph(p)) = doc.blocks.get(idx) {
        let mut p = p.clone();
        p.style_id = Some("Heading1".into());
        doc.blocks.set(idx, engine::Block::Paragraph(p));
    }
    e.undo.push(doc);
    let evt = apply(&mut e, Command::UpdateFields);
    assert!(!matches!(evt, Event::Error { .. }), "{evt:?}");
    assert!(e.undo.current().blocks.len() >= before);
    assert_eq!(commented(&e), "target");
}
