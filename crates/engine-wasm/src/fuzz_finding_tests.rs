//! Issues #477–#481 — the nightly `snapshot_decode` findings, spelled out
//! without the fuzz generator (the raw reproducers are committed under
//! `fuzz/corpus/snapshot_decode/repro_*`):
//!
//! * #477 / #481 — typing right after an endnote reference inserted
//!   mid-span left the straddling span unsplit; it ended inside the typed
//!   text, mid-character, and the repaint's span slicing panicked.
//! * #479 / #480 — `clamp_pos` collapsed every position of a table-only
//!   tree to the first cell's start, so a valid word / paragraph selection
//!   never survived its own clamp (the selection invariant failed).
//! * #478 — F9 inside a note story restamped the body without clamping
//!   the stashed body selection.

use super::*;
use bridge::PathStep;

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

fn open(e: &mut Engine, body: &str) {
    let evt = apply(
        e,
        Command::OpenDocument {
            bytes: format_docx::test_fixtures::docx_with_body(body),
            format: bridge::DocFormat::Docx,
            name: Some("finding.docx".into()),
            defaults: None,
            limits: None,
            password: None,
        },
    );
    assert!(
        matches!(evt, Event::DocumentLoaded { .. }),
        "open: {}",
        format!("{evt:?}").chars().take(200).collect::<String>()
    );
}

fn caret_at(e: &mut Engine, pos: BridgeLogicalPos) {
    let evt = apply(
        e,
        Command::SetSelection {
            range: BridgeLogicalRange {
                start: pos.clone(),
                end: pos.clone(),
            },
            caret: pos,
        },
    );
    assert!(!matches!(evt, Event::Error { .. }), "{evt:?}");
}

/// Issues #477 / #481 — the reproducers' session: an endnote reference
/// inserted INSIDE an italic run, the caret right after it, then typed
/// text with a combining mark. The span holding the reference used to keep
/// its old end, inside the typed text and mid-`U+0301`: the auto-repaint
/// panicked slicing the paragraph at it.
#[test]
fn typing_after_a_mid_span_endnote_reference_keeps_spans_on_char_boundaries() {
    let mut e = engine_with(DocumentTree::from_text("seed"));
    open(
        &mut e,
        concat!(
            "<w:p/><w:p><w:r><w:rPr><w:i/></w:rPr><w:t>السلام عليكم</w:t></w:r>",
            "<w:r><w:rPr><w:b/></w:rPr><w:t>world</w:t></w:r></w:p>"
        ),
    );
    let evt = apply(
        &mut e,
        Command::InsertEndnote {
            at: bpos_top(1, 17),
        },
    );
    assert!(!matches!(evt, Event::Error { .. }), "{evt:?}");
    let evt = apply(&mut e, Command::ExitHeaderFooter);
    assert!(!matches!(evt, Event::Error { .. }), "{evt:?}");
    let anchor = {
        let p = e.undo.current().nth_paragraph(1).expect("paragraph 1");
        assert_eq!(p.inline_objects.len(), 1, "the endnote reference");
        p.inline_objects[0].at
    };
    let after = anchor + '\u{FFFC}'.len_utf8() as u32;
    caret_at(&mut e, bpos_top(1, after));
    let typed = "ل a \u{301}1سab";
    let evt = apply(
        &mut e,
        Command::InsertText {
            at: None,
            text: typed.to_string(),
        },
    );
    assert!(
        matches!(evt, Event::SelectionChanged { .. }),
        "{}",
        format!("{evt:?}").chars().take(200).collect::<String>()
    );
    let p = e.undo.current().nth_paragraph(1).expect("paragraph 1");
    for s in &p.spans {
        assert!(
            p.text.is_char_boundary(s.start as usize) && p.text.is_char_boundary(s.end as usize),
            "span {}..{} cuts a character of {:?}",
            s.start,
            s.end,
            p.text
        );
    }
    let len = typed.len() as u32;
    assert_eq!(p.style_at(after).italic, None, "the typed text is plain");
    assert_eq!(
        p.style_at(after + len).italic,
        Some(true),
        "the rest of the italic run keeps its formatting"
    );
}

/// A body whose only top-level block is a 2×1 table, cell (1, 0) holding
/// `text` — a table-only tree (`paragraph_count() == 0`).
fn table_only(text: &str) -> DocumentTree {
    let mut d = DocumentTree::from_text("tail").insert_table(EngineBlockPath::top(0), 2, 1);
    d.blocks.pop_back();
    assert_eq!(d.paragraph_count(), 0, "table-only");
    d.insert_text(
        engine::LogicalPos {
            path: bridge_to_engine_path(cell_path(1)),
            offset: 0,
        },
        text,
    )
}

fn cell_path(row: u32) -> bridge::BlockPath {
    bridge::BlockPath {
        steps: vec![
            PathStep::Block { idx: 0 },
            PathStep::Cell { row, col: 0 },
            PathStep::Block { idx: 0 },
        ],
    }
}

fn cpos(row: u32, offset: u32) -> BridgeLogicalPos {
    BridgeLogicalPos {
        path: cell_path(row),
        offset,
    }
}

/// Issues #479 / #480 — in a table-only tree a position inside any cell
/// survives its own clamp (only its offset snaps to a char boundary); an
/// unresolvable path still falls back to the first cell's start (design
/// review B6).
#[test]
fn clamp_pos_keeps_a_valid_position_in_a_table_only_tree() {
    let text = "b1س\u{301}hello";
    let d = table_only(text);
    let end = text.len() as u32;
    assert_eq!(clamp_pos(&d, cpos(1, end)), cpos(1, end));
    assert_eq!(clamp_pos(&d, cpos(1, 2)), cpos(1, 2));
    assert_eq!(clamp_pos(&d, cpos(1, 3)), cpos(1, 2), "snapped into `س`");
    assert_eq!(clamp_pos(&d, cpos(1, 99)), cpos(1, end), "capped");
    assert_eq!(clamp_pos(&d, cpos(0, 0)), cpos(0, 0));
    assert_eq!(clamp_pos(&d, bpos_top(7, 3)), cpos(0, 0), "B6 fallback");
}

/// Issues #479 / #480 — the engine-level symptom: a selection set inside a
/// table-only document's second cell is kept, and it is valid (the
/// reproducers' `SelectWordAt` / `SelectParagraphAt` selected exactly
/// such a range, then failed the selection invariant).
#[test]
fn a_selection_in_a_table_only_document_stays_valid() {
    let text = "\u{301}a & b\u{301}";
    let mut e = engine_with(table_only(text));
    let evt = apply(
        &mut e,
        Command::SetSelection {
            range: BridgeLogicalRange {
                start: cpos(1, 0),
                end: cpos(1, 2),
            },
            caret: cpos(1, 2),
        },
    );
    assert!(!matches!(evt, Event::Error { .. }), "{evt:?}");
    let sel = e.selection.clone().expect("selection");
    assert_eq!(
        (sel.anchor.clone(), sel.caret.clone()),
        (cpos(1, 0), cpos(1, 2))
    );
    let doc = e.undo.current();
    assert_eq!(
        clamp_pos(doc, sel.anchor.clone()),
        sel.anchor,
        "valid anchor"
    );
    assert_eq!(clamp_pos(doc, sel.caret.clone()), sel.caret, "valid caret");
}
