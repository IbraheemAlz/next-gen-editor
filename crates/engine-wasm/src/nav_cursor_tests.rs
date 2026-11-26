//! Epic #555 — the navigation rows of the cursor-behaviour matrix, pinned
//! natively (the real-UI twin is `ts/e2e/nav-cursor-matrix.spec.ts`).
//! Expected behaviour is the clean-room methods memo
//! `plans/cleanroom/memos/onlyoffice-cursor-memo.md`:
//!
//! - row 13 (§3): with a selection, Left / Right only collapse it — to the
//!   logical start / end, the key mapping flipped in an RTL paragraph;
//! - row 16 (§4): Up / Down probe at the sticky x, so inside a table they
//!   stay in the grid column under it (a taller neighbour cell of the same
//!   row is not "the next line");
//! - row 19 (§1, §5): the LINE HINT — a caret at a wrap point (the end of
//!   one line is the start of the next) is drawn on, and walks from, the
//!   line that put it there: Home on a wrapped line stays on that line,
//!   End stays at the end of its own line, Right past a line's last
//!   character shows the caret at the start of the next line;
//! - row 33 (§10): Enter in an empty list item leaves the list (a nested
//!   item outdents) instead of splitting;
//! - row 35 (§10): Undo / Redo restore the caret of their snapshot.
//!
//! Every test drives real `Command`s through `Engine::apply` over a laid
//! out page (`tests::test_engine_with_doc`), and reads the caret off the
//! `SelectionChanged` replies.

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

/// `doc` in an engine with a Latin AND an Arabic face loaded.
fn engine_with(doc: DocumentTree) -> Engine {
    let mut e = tests::test_engine_with_doc(doc);
    let bytes = include_bytes!("../../../ts/fonts/NotoNaskhArabic-Regular.ttf").to_vec();
    let font = LoadedFont::parse("test-arabic".to_string(), bytes).expect("arabic font");
    e.fonts.insert("test-arabic".to_string(), Arc::new(font));
    e
}

/// One paragraph per line of `text`.
fn doc_of(text: &str) -> DocumentTree {
    let mut lines = text.split('\n');
    let mut d = DocumentTree::from_text(lines.next().unwrap_or(""));
    for l in lines {
        d.blocks
            .push_back(engine::Block::Paragraph(engine::Paragraph {
                text: l.to_string(),
                ..Default::default()
            }));
    }
    d
}

fn at(steps: Vec<BridgePathStep>, offset: u32) -> BridgeLogicalPos {
    BridgeLogicalPos {
        path: BridgeBlockPath { steps },
        offset,
    }
}

fn top(idx: u32, offset: u32) -> BridgeLogicalPos {
    at(vec![BridgePathStep::Block { idx }], offset)
}

fn cell(row: u32, col: u32, offset: u32) -> BridgeLogicalPos {
    at(
        vec![
            BridgePathStep::Block { idx: 1 },
            BridgePathStep::Cell { row, col },
            BridgePathStep::Block { idx: 0 },
        ],
        offset,
    )
}

/// What a `SelectionChanged` reply says: `(range, caret rect)`.
fn selection_of(evt: Event) -> (BridgeLogicalRange, BridgeRect) {
    match evt {
        Event::SelectionChanged { range, caret, .. } => (range, caret),
        other => panic!("expected SelectionChanged, got {other:?}"),
    }
}

fn park(e: &mut Engine, pos: BridgeLogicalPos) -> BridgeRect {
    selection_of(apply(
        e,
        Command::SetSelection {
            range: BridgeLogicalRange {
                start: pos.clone(),
                end: pos.clone(),
            },
            caret: pos,
        },
    ))
    .1
}

fn press(
    e: &mut Engine,
    direction: MoveDirection,
    extend: bool,
) -> (BridgeLogicalRange, BridgeRect) {
    selection_of(apply(e, Command::MoveCaret { direction, extend }))
}

/// The collapsed caret's position after `press`.
fn caret_after(e: &mut Engine, direction: MoveDirection) -> BridgeLogicalPos {
    let (range, _) = press(e, direction, false);
    assert_eq!(range.start, range.end, "a collapsed caret");
    range.end
}

/* ----------------------------------------------------------- row 13 */

#[test]
fn left_and_right_collapse_a_selection_to_its_logical_edges() {
    let mut e = engine_with(doc_of("Hello brave world\nمرحبا بالعالم الجميل"));
    /* LTR, forward selection 6..11: Left -> its start, without moving on. */
    park(&mut e, top(0, 6));
    for _ in 0..5 {
        press(&mut e, MoveDirection::Right, true);
    }
    assert_eq!(caret_after(&mut e, MoveDirection::Left), top(0, 6));
    /* Backward selection (caret at the start): Right -> its end. */
    park(&mut e, top(0, 11));
    for _ in 0..5 {
        press(&mut e, MoveDirection::Left, true);
    }
    assert_eq!(caret_after(&mut e, MoveDirection::Right), top(0, 11));
    /* Shift still extends from the anchor; a collapsed caret still steps. */
    park(&mut e, top(0, 6));
    let (range, _) = press(&mut e, MoveDirection::Right, true);
    assert_eq!((range.start.offset, range.end.offset), (6, 7));
    park(&mut e, top(0, 6));
    assert_eq!(caret_after(&mut e, MoveDirection::Right), top(0, 7));

    /* RTL paragraph: Right is logically backward -> the logical START,
    Left logically forward -> the logical END. */
    let word = "مرحبا ".len() as u32;
    let word_end = word + "بالعالم".len() as u32;
    select(&mut e, top(1, word), top(1, word_end));
    assert_eq!(caret_after(&mut e, MoveDirection::Right), top(1, word));
    select(&mut e, top(1, word_end), top(1, word));
    assert_eq!(caret_after(&mut e, MoveDirection::Left), top(1, word_end));
}

/// A selection from `anchor` to `caret` (setup).
fn select(e: &mut Engine, anchor: BridgeLogicalPos, caret: BridgeLogicalPos) {
    let (start, end) = ordered(anchor.clone(), caret.clone());
    apply(
        e,
        Command::SetSelection {
            range: BridgeLogicalRange { start, end },
            caret: caret.clone(),
        },
    );
    let sel = e.selection.as_ref().expect("selection");
    assert_eq!((&sel.anchor, &sel.caret), (&anchor, &caret));
}

/* ----------------------------------------------------------- row 19 */

/// The caret rect sits inside `line`'s vertical band (#557 sizes it to
/// the text, so its top is not the line's top).
fn on_line(rect: &BridgeRect, line: &LineGeom) -> bool {
    rect.y >= line.y_top - 0.5 && rect.y + rect.h <= line.y_top + line.height + 0.5
}

/// A paragraph that wraps over several lines.
fn wrapped() -> String {
    (0..40)
        .map(|i| format!("word{i}"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn home_and_end_stay_on_the_line_the_caret_is_drawn_on() {
    let mut e = engine_with(doc_of(&format!("{}\nnext", wrapped())));
    let geom = e.document_geometry().expect("geometry");
    let lines: Vec<&LineGeom> = geom.iter().filter(|l| l.path == top(0, 0).path).collect();
    assert!(lines.len() >= 3, "precondition: the paragraph wraps");
    let (l0, l1, l2) = (lines[0], lines[1], lines[2]);
    assert_eq!(l0.end_byte, l1.start_byte, "precondition: a wrap point");

    /* Mid second line, Home: the line's start — drawn on THAT line. */
    park(&mut e, top(0, l1.start_byte + 3));
    let (range, rect) = press(&mut e, MoveDirection::LineHome, false);
    assert_eq!(range.end.offset, l1.start_byte);
    assert!(on_line(&rect, l1), "Home stays on the second line");
    assert!((rect.x - l1.start_x).abs() < 1.0, "at the line's left edge");
    /* Home again: nowhere else to go (not the previous line's start). */
    let (range, rect) = press(&mut e, MoveDirection::LineHome, false);
    assert_eq!(range.end.offset, l1.start_byte);
    assert!(on_line(&rect, l1));

    /* End: the same line's end, drawn at the end of THAT line. */
    let (range, rect) = press(&mut e, MoveDirection::LineEnd, false);
    assert_eq!(range.end.offset, l1.end_byte);
    assert!(
        on_line(&rect, l1),
        "End stays at the end of the second line"
    );
    /* End again stays; Home from there goes to this line's start. */
    let (range, rect) = press(&mut e, MoveDirection::LineEnd, false);
    assert_eq!(range.end.offset, l1.end_byte);
    assert!(on_line(&rect, l1));
    let (range, rect) = press(&mut e, MoveDirection::LineHome, false);
    assert_eq!(range.end.offset, l1.start_byte);
    assert!(on_line(&rect, l1));
    /* ... and Down from the line start walks from THAT line. */
    let (_, rect) = press(&mut e, MoveDirection::Down, false);
    assert!(
        on_line(&rect, l2),
        "Down from the second line reaches the third"
    );

    /* A logical move past a line's last character shows the caret at the
    start of the next line (no end-of-line stop). */
    park(&mut e, top(0, l0.end_byte - 1));
    let (range, rect) = press(&mut e, MoveDirection::Right, false);
    assert_eq!(range.end.offset, l0.end_byte);
    assert!(on_line(&rect, l1), "drawn at the start of the next line");

    /* A click right of the first line's ink lands on its end and stays
    drawn there. */
    let rect0 = selection_of(apply(
        &mut e,
        Command::PlaceCaretAtPoint {
            page: 0,
            at: BridgePoint {
                x: l0.hit_left + l0.hit_width - 2.0,
                y: l0.y_top + l0.height / 2.0,
            },
        },
    ))
    .1;
    let sel = e.selection.as_ref().expect("selection").caret.clone();
    assert_eq!(sel.offset, l0.end_byte);
    assert!(on_line(&rect0, l0), "a click past the ink keeps its line");

    /* Ctrl+End / Ctrl+Home: the document edges. */
    assert_eq!(caret_after(&mut e, MoveDirection::DocEnd), top(1, 4));
    assert_eq!(caret_after(&mut e, MoveDirection::DocHome), top(0, 0));
}

#[test]
fn home_and_end_in_an_rtl_paragraph_are_its_logical_line_edges() {
    let arabic = std::iter::repeat_n("كلمة", 8).collect::<Vec<_>>().join(" ");
    let mut e = engine_with(doc_of(&arabic));
    park(&mut e, top(0, 10));
    let (range, home) = press(&mut e, MoveDirection::LineHome, false);
    assert_eq!(range.end.offset, 0, "Home = the logical start");
    let (range, end) = press(&mut e, MoveDirection::LineEnd, false);
    assert_eq!(
        range.end.offset,
        arabic.len() as u32,
        "End = the logical end"
    );
    assert_eq!(home.y, end.y);
    assert!(home.x > end.x, "an RTL line starts at its RIGHT edge");
}

/* ----------------------------------------------------------- row 16 */

/// `above`, a 2 x 2 table whose cell (0, 1) is three lines tall, `below`.
fn table_engine() -> Engine {
    let mut e = engine_with(doc_of(
        "above the table: one line that reaches the second column\nbelow the table",
    ));
    apply(
        &mut e,
        Command::InsertTable {
            at: BridgeBlockPath {
                steps: vec![BridgePathStep::Block { idx: 1 }],
            },
            rows: 2,
            cols: 2,
        },
    );
    for (row, col, text) in [
        (0, 0, "a"),
        (0, 1, "b1\u{2028}b2\u{2028}b3"),
        (1, 0, "c"),
        (1, 1, "d"),
    ] {
        park(&mut e, cell(row, col, 0));
        apply(
            &mut e,
            Command::InsertText {
                at: None,
                text: text.to_string(),
            },
        );
    }
    e
}

/// Where a position is: `p<block>` or `c<row><col>`.
fn label(pos: &BridgeLogicalPos) -> String {
    match pos.path.steps.as_slice() {
        [_, BridgePathStep::Cell { row, col }, ..] => format!("c{row}{col}"),
        [BridgePathStep::Block { idx }] => format!("p{idx}"),
        other => format!("{other:?}"),
    }
}

fn walk(e: &mut Engine, direction: MoveDirection, n: usize) -> Vec<String> {
    (0..n).map(|_| label(&caret_after(e, direction))).collect()
}

#[test]
fn up_and_down_stay_in_the_grid_column_under_the_sticky_x() {
    let mut e = table_engine();
    /* Column 0: the taller neighbour (0, 1) is not the next line down. */
    park(&mut e, top(0, 1));
    assert_eq!(walk(&mut e, MoveDirection::Down, 3), ["c00", "c10", "p2"]);
    assert_eq!(walk(&mut e, MoveDirection::Up, 3), ["c10", "c00", "p0"]);

    /* Column 1: line by line through the tall cell, then the row below. */
    park(&mut e, top(0, 52));
    let mut offsets = Vec::new();
    let mut seen = Vec::new();
    for _ in 0..5 {
        let pos = caret_after(&mut e, MoveDirection::Down);
        seen.push(label(&pos));
        offsets.push(pos.offset);
    }
    assert_eq!(seen, ["c01", "c01", "c01", "c11", "p2"]);
    assert!(
        offsets[0] < offsets[1] && offsets[1] < offsets[2],
        "{offsets:?}"
    );
    assert_eq!(
        walk(&mut e, MoveDirection::Up, 5),
        ["c11", "c01", "c01", "c01", "p0"]
    );
}

#[test]
fn reachable_lines_follow_the_cell_under_x() {
    let e = table_engine();
    let geom = e.document_geometry().expect("geometry");
    let c00 = geom
        .iter()
        .find(|l| label(&at(l.path.steps.clone(), 0)) == "c00")
        .expect("cell (0, 0) line");
    let x = c00.hit_left + 1.0;
    let reach = reachable_at_x(&geom, x);
    for (l, ok) in geom.iter().zip(&reach) {
        let name = label(&at(l.path.steps.clone(), 0));
        let expect = !name.ends_with('1');
        assert_eq!(*ok, expect, "{name} reachable at the first column's x");
    }
}

/* ----------------------------------------------------------- row 33 */

fn list_ilvl(evt: &Event) -> Option<u8> {
    match evt {
        Event::SelectionChanged { list_ilvl, .. } => *list_ilvl,
        other => panic!("expected SelectionChanged, got {other:?}"),
    }
}

#[test]
fn enter_in_an_empty_list_item_leaves_the_list_instead_of_splitting() {
    let mut e = engine_with(doc_of("item"));
    park(&mut e, top(0, 4));
    apply(
        &mut e,
        Command::ToggleList {
            range: BridgeLogicalRange {
                start: top(0, 4),
                end: top(0, 4),
            },
            kind: bridge::ListKind::Bullet,
        },
    );
    /* Enter after text continues the list. */
    let evt = apply(&mut e, Command::SplitParagraph { at: None });
    assert_eq!(list_ilvl(&evt), Some(0));
    assert_eq!(e.undo.current().paragraph_count(), 2);
    /* Enter in the empty item: no new paragraph, the item leaves the list. */
    let evt = apply(&mut e, Command::SplitParagraph { at: None });
    assert_eq!(list_ilvl(&evt), None, "the empty item left the list");
    assert_eq!(e.undo.current().paragraph_count(), 2, "nothing was split");
    assert_eq!(selection_of(evt).0.end, top(1, 0));
    /* Now an ordinary empty paragraph: Enter splits again. */
    apply(&mut e, Command::SplitParagraph { at: None });
    assert_eq!(e.undo.current().paragraph_count(), 3);

    /* A nested empty item outdents one level first. */
    let mut e = engine_with(doc_of("item"));
    park(&mut e, top(0, 4));
    let range = BridgeLogicalRange {
        start: top(0, 4),
        end: top(0, 4),
    };
    apply(
        &mut e,
        Command::ToggleList {
            range: range.clone(),
            kind: bridge::ListKind::Number,
        },
    );
    apply(&mut e, Command::SplitParagraph { at: None });
    let range1 = BridgeLogicalRange {
        start: top(1, 0),
        end: top(1, 0),
    };
    let evt = apply(
        &mut e,
        Command::ChangeListLevel {
            range: range1,
            delta: 1,
        },
    );
    assert_eq!(list_ilvl(&evt), Some(1));
    let evt = apply(&mut e, Command::SplitParagraph { at: None });
    assert_eq!(list_ilvl(&evt), Some(0), "outdented, still a list item");
    let evt = apply(&mut e, Command::SplitParagraph { at: None });
    assert_eq!(list_ilvl(&evt), None);
    assert_eq!(e.undo.current().paragraph_count(), 2);
}

/* ----------------------------------------------------------- row 35 */

fn caret_pos(e: &Engine) -> BridgeLogicalPos {
    let sel = e.selection.as_ref().expect("selection");
    assert_eq!(sel.anchor, sel.caret, "a collapsed caret");
    sel.caret.clone()
}

#[test]
fn undo_and_redo_restore_the_caret_of_their_step() {
    let mut e = engine_with(doc_of("Hello world"));
    park(&mut e, top(0, 5));
    apply(
        &mut e,
        Command::PastePlain {
            text: "PASTED".to_string(),
        },
    );
    assert_eq!(caret_pos(&e), top(0, 11), "paste leaves the caret after it");
    /* Move away, then Undo: the caret is back where the paste started. */
    press(&mut e, MoveDirection::DocEnd, false);
    apply(&mut e, Command::Undo);
    assert_eq!(e.undo.current().paragraph_text(0), Some("Hello world"));
    assert_eq!(caret_pos(&e), top(0, 5));
    /* Move away, then Redo: the caret the paste left. */
    press(&mut e, MoveDirection::DocHome, false);
    apply(&mut e, Command::Redo);
    assert_eq!(
        e.undo.current().paragraph_text(0),
        Some("HelloPASTED world")
    );
    assert_eq!(caret_pos(&e), top(0, 11));

    /* Typing: each step's own caret. */
    park(&mut e, top(0, 0));
    for ch in ["a", "b"] {
        apply(
            &mut e,
            Command::InsertText {
                at: None,
                text: ch.to_string(),
            },
        );
    }
    press(&mut e, MoveDirection::DocEnd, false);
    apply(&mut e, Command::Undo);
    assert_eq!(caret_pos(&e), top(0, 1), "before the undone `b`");
    apply(&mut e, Command::Undo);
    assert_eq!(caret_pos(&e), top(0, 0), "before the undone `a`");
    apply(&mut e, Command::Redo);
    assert_eq!(caret_pos(&e), top(0, 1), "after the redone `a`");

    /* Nothing to undo: the selection is left alone. */
    let mut fresh = engine_with(doc_of("x"));
    park(&mut fresh, top(0, 1));
    apply(&mut fresh, Command::Undo);
    assert_eq!(caret_pos(&fresh), top(0, 1));
}
