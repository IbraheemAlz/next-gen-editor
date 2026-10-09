//! Document lifecycle commands: issue #338 (`CloseDocument` resets to the
//! seeded empty document instead of answering a `phase3_stub` error).

use super::*;
use bridge::HeaderFooterArea;

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
    block_on(e.apply(cmd))
}

fn engine_with(doc: DocumentTree) -> Engine {
    let mut e = assemble_engine(None, None);
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
    e.undo = UndoStack::new(doc, UNDO_CAP);
    e.selection = Some(SelectionState {
        anchor: bpos_top(0, 0),
        caret: bpos_top(0, 0),
        ideal_x: None,
        kind: SelectionKind::Linear,
    });
    e.review_date = "2026-01-01T00:00:00Z".into();
    e
}

fn range(p: u32, a: u32, b: u32) -> BridgeLogicalRange {
    BridgeLogicalRange {
        start: bpos_top(p, a),
        end: bpos_top(p, b),
    }
}

/// Issue #338 — typing, a comment, a name and track-changes all go; the
/// answer is a real `SelectionChanged` over the seeded empty document
/// with nothing to undo, at a moved document revision.
#[test]
fn close_document_resets_to_the_seeded_empty_document() {
    let mut e = engine_with(DocumentTree::from_text("hello world"));
    apply(
        &mut e,
        Command::InsertText {
            at: None,
            text: "abc".into(),
        },
    );
    let commented = apply(
        &mut e,
        Command::InsertComment {
            range: range(0, 0, 3),
            text: "note".into(),
            author: "Tester".into(),
        },
    );
    assert!(!matches!(commented, Event::Error { .. }), "{commented:?}");
    assert!(!e.undo.current().comment_defs.is_empty());
    assert!(e.undo.can_undo());
    e.document_name = Some("letter.docx".into());
    e.tracking_changes = true;
    let before = e.mutation_seq;

    let Event::SelectionChanged {
        range: sel,
        can_undo,
        can_redo,
        undo_depth,
        is_tracking_changes,
        editing_story,
        document_revision,
        ..
    } = apply(&mut e, Command::CloseDocument)
    else {
        panic!("CloseDocument answers SelectionChanged");
    };
    assert!(!can_undo && !can_redo, "undo history cleared");
    assert_eq!(undo_depth, e.undo.depth());
    assert!(!is_tracking_changes, "a new document records nothing");
    assert!(editing_story.is_none());
    assert_eq!(sel.start, bpos_top(0, 0));
    assert_eq!(sel.end, bpos_top(0, 0));
    assert!(e.mutation_seq > before, "the close is a document mutation");
    assert_eq!(document_revision, e.mutation_seq);

    let doc = e.undo.current();
    assert_eq!(doc.paragraph_count(), 1);
    assert_eq!(doc.to_plain_text(), "");
    assert!(doc.comment_defs.is_empty() && doc.comment_ranges.is_empty());
    assert!(doc.media.is_empty());
    assert!(doc.source_package.is_none());
    assert!(e.document_name.is_none());
    /* The session survives: fonts + layout config (zoom, direction). */
    assert!(e.layout_cfg.is_some());
    assert!(e.fonts.contains_key("test-latin"));
    /* And the engine keeps working on the fresh document. */
    let typed = apply(
        &mut e,
        Command::InsertText {
            at: None,
            text: "new".into(),
        },
    );
    assert!(matches!(typed, Event::SelectionChanged { .. }), "{typed:?}");
    assert_eq!(e.undo.current().to_plain_text(), "new");
}

/// Issue #338 — an opened `.docx`'s retained source package (#134) is
/// dropped: the next save goes through the minimal-package writer.
#[test]
fn close_document_drops_the_retained_source_package() {
    let mut e = engine_with(DocumentTree::from_text(""));
    let bytes = format_docx::build_minimal_docx(&DocumentTree::from_text("from a file"))
        .expect("build a .docx");
    let loaded = apply(&mut e, Command::LoadDocx { bytes });
    assert!(matches!(loaded, Event::DocumentLoaded { .. }), "{loaded:?}");
    assert!(e.undo.current().source_package.is_some());

    apply(&mut e, Command::CloseDocument);
    assert!(e.undo.current().source_package.is_none());
    assert!(e.detached_package.borrow().is_none());
    let saved = apply(&mut e, Command::SaveDocx);
    assert!(matches!(saved, Event::DocumentSaved { .. }), "{saved:?}");
}

/// Issue #338 — `StoryPolicy::ExitsStory`: closing from inside a header
/// leaves the story first instead of being rejected by the story gate.
#[test]
fn close_document_exits_an_active_story() {
    let mut e = engine_with(DocumentTree::from_text("body text"));
    apply(
        &mut e,
        Command::EnterHeaderFooter {
            page: 0,
            area: HeaderFooterArea::Header,
        },
    );
    assert!(e.story_active());
    let closed = apply(&mut e, Command::CloseDocument);
    let Event::SelectionChanged { editing_story, .. } = closed else {
        panic!("CloseDocument answers SelectionChanged, got {closed:?}");
    };
    assert!(editing_story.is_none());
    assert!(!e.story_active());
    assert_eq!(e.undo.current().to_plain_text(), "");
}
