//! Issue #76 — Tab / Shift+Tab walk the cells of a table inside a
//! header story exactly like a body table, and Tab past the last cell
//! appends a row to the HEADER part through the story adapter (one undo
//! step, body untouched).

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
    e.undo = UndoStack::new(doc, 100);
    e.selection = Some(SelectionState {
        anchor: bpos_top(0, 0),
        caret: bpos_top(0, 0),
        ideal_x: None,
        kind: SelectionKind::Linear,
    });
    e.review_date = "2026-01-01T00:00:00Z".into();
    e
}

fn tab(e: &mut Engine, forward: bool) -> Event {
    apply(
        e,
        Command::MoveCaret {
            direction: if forward {
                MoveDirection::NextCell
            } else {
                MoveDirection::PrevCell
            },
            extend: false,
        },
    )
}

/// The `(row, col)` of the caret's cell — the caret path is story-rooted
/// (`Block(t) Cell{r,c} Block(0)`).
fn caret_cell(e: &Engine) -> (u32, u32) {
    let sel = e.selection.as_ref().expect("selection");
    match sel.caret.path.steps.get(1) {
        Some(BridgePathStep::Cell { row, col }) => (*row, *col),
        other => panic!("caret not in a cell: {other:?}"),
    }
}

fn header_rid(e: &Engine) -> String {
    match &e.active_story {
        StoryTarget::Header { rid, .. } => rid.clone(),
        _ => panic!("expected a header story"),
    }
}

fn header_table_rows(e: &Engine, rid: &str) -> usize {
    e.undo.current().headers[rid]
        .iter()
        .find_map(|b| b.as_table())
        .expect("header table")
        .rows
        .len()
}

/// Body `"body text"`; enter the page-0 header and insert a 1×2 table
/// there (the caret lands in its first cell).
fn header_table_engine() -> (Engine, String) {
    let mut e = engine_with(DocumentTree::from_text("body text"));
    let evt = apply(
        &mut e,
        Command::EnterHeaderFooter {
            page: 0,
            area: HeaderFooterArea::Header,
        },
    );
    assert!(!matches!(evt, Event::Error { .. }), "{evt:?}");
    let evt = apply(
        &mut e,
        Command::InsertTable {
            at: BridgeBlockPath::top(0),
            rows: 1,
            cols: 2,
        },
    );
    assert!(!matches!(evt, Event::Error { .. }), "{evt:?}");
    let rid = header_rid(&e);
    assert_eq!(header_table_rows(&e, &rid), 1);
    assert_eq!(caret_cell(&e), (0, 0));
    (e, rid)
}

/// Body block count + text (the model types carry no `PartialEq`).
fn body_fingerprint(e: &Engine) -> (usize, String) {
    let d = e.undo.current();
    (d.blocks.len(), d.to_plain_text())
}

#[test]
fn tab_and_shift_tab_walk_a_header_tables_cells() {
    let (mut e, _rid) = header_table_engine();
    let evt = tab(&mut e, true);
    assert!(matches!(evt, Event::SelectionChanged { .. }), "{evt:?}");
    assert_eq!(caret_cell(&e), (0, 1));
    let evt = tab(&mut e, false);
    assert!(matches!(evt, Event::SelectionChanged { .. }), "{evt:?}");
    assert_eq!(caret_cell(&e), (0, 0));
    /* Shift+Tab in the first cell is a no-op, still inside the story. */
    tab(&mut e, false);
    assert_eq!(caret_cell(&e), (0, 0));
    assert!(e.story_active());
}

#[test]
fn tab_selects_the_destination_cells_content_in_a_story() {
    let (mut e, _rid) = header_table_engine();
    tab(&mut e, true);
    apply(
        &mut e,
        Command::InsertText {
            at: None,
            text: "abc".into(),
        },
    );
    tab(&mut e, false);
    tab(&mut e, true);
    /* Word parity: Tab selects the destination cell's whole content —
    resolved against the header part, not the body. */
    let sel = e.selection.clone().unwrap();
    assert_eq!(sel.anchor.offset, 0);
    assert_eq!(sel.caret.offset, 3);
    assert_eq!(caret_cell(&e), (0, 1));
}

#[test]
fn tab_past_the_last_cell_appends_a_row_to_the_header_only() {
    let (mut e, rid) = header_table_engine();
    let body_before = body_fingerprint(&e);
    tab(&mut e, true);
    let revision = e.undo.revision();
    let evt = tab(&mut e, true);
    assert!(matches!(evt, Event::SelectionChanged { .. }), "{evt:?}");
    assert_eq!(header_table_rows(&e, &rid), 2, "row appended in the part");
    assert_eq!(caret_cell(&e), (1, 0));
    assert_eq!(e.undo.revision(), revision + 1, "exactly one undo step");
    assert_eq!(body_fingerprint(&e), body_before, "the body is untouched");
    assert!(
        e.undo
            .current()
            .blocks
            .iter()
            .all(|b| b.as_table().is_none()),
        "no row/table leaked into the body"
    );
    /* Typing lands in the new row's cell, in the header. */
    apply(
        &mut e,
        Command::InsertText {
            at: None,
            text: "x".into(),
        },
    );
    let t = e.undo.current().headers[&rid]
        .iter()
        .find_map(|b| b.as_table())
        .unwrap()
        .clone();
    assert_eq!(
        t.rows[1].cells[0].blocks[0].as_paragraph().unwrap().text,
        "x"
    );
    /* One undo drops the typing, the next drops the appended row —
    still inside the header story. */
    apply(&mut e, Command::Undo);
    apply(&mut e, Command::Undo);
    assert_eq!(header_table_rows(&e, &rid), 1);
    assert!(e.story_active());
    assert_eq!(body_fingerprint(&e), body_before);
}

#[test]
fn body_tab_path_is_unchanged() {
    let doc = DocumentTree::from_text("tail").insert_table(EngineBlockPath::top(0), 1, 2);
    let mut e = engine_with(doc);
    let first = BridgeLogicalPos {
        path: BridgeBlockPath {
            steps: vec![
                BridgePathStep::Block { idx: 0 },
                BridgePathStep::Cell { row: 0, col: 0 },
                BridgePathStep::Block { idx: 0 },
            ],
        },
        offset: 0,
    };
    e.selection = Some(SelectionState {
        anchor: first.clone(),
        caret: first,
        ideal_x: None,
        kind: SelectionKind::Linear,
    });
    tab(&mut e, true);
    assert_eq!(caret_cell(&e), (0, 1));
    let revision = e.undo.revision();
    tab(&mut e, true);
    assert_eq!(caret_cell(&e), (1, 0));
    assert_eq!(e.undo.revision(), revision + 1);
    assert_eq!(e.undo.current().blocks[0].as_table().unwrap().rows.len(), 2);
    tab(&mut e, false);
    assert_eq!(caret_cell(&e), (0, 1));
}
