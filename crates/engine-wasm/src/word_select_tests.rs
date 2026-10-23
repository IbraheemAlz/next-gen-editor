//! Issue #553 — a double-click selects the word at a LOGICAL place.
//!
//! In `Hello world مرحبا بالعالم` (an LTR paragraph that ends in an RTL
//! run) a double-click on the page below the last line selected `مرحبا` —
//! the visually rightmost word, but the FIRST Arabic word — instead of
//! `بالعالم`, the word before the logical end. The pointer's nearest caret
//! SLOT is a visual fact: the slot nearest the empty space right of the
//! ink is the RTL run's logical START. Two things were wrong:
//!
//! 1. `hit_test_geom` answered that slot for a click past the line's ink
//!    (it now answers the line's logical end on that side, from the
//!    paragraph's base direction — `line_edge_at`);
//! 2. a double-click classified the character to the right of the nearest
//!    slot, so the right half of a word's last letter selected the space
//!    after it (it now probes the character UNDER the pointer, and the
//!    line's last word past its end — `word_target_geom`).
//!
//! The tests drive real `Command`s through `Engine::apply` over a layout
//! with a Latin and an Arabic face loaded, and read what the selection
//! covers off the paragraph TEXT, so none of them assumes a pixel.

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

fn engine_for(text: &str) -> Engine {
    engine_with(DocumentTree::from_text(text))
}

/// One paragraph `text` with `bold` byte ranges styled bold (each one a
/// run boundary for the layout).
fn doc_with_bold(text: &str, bold: &[(u32, u32)]) -> DocumentTree {
    let spans = bold
        .iter()
        .map(|&(start, end)| engine::StyleRun {
            start,
            end,
            style: engine::SpanStyle {
                bold: Some(true),
                ..Default::default()
            },
        })
        .collect();
    let mut doc = DocumentTree::new();
    doc.blocks.clear();
    doc.blocks.push_back(engine::Block::Paragraph(engine::Paragraph {
        text: text.to_string(),
        spans,
        ..Default::default()
    }));
    doc
}

fn lines(e: &Engine) -> Vec<LineGeom> {
    e.document_geometry().expect("geometry")
}

/// Outermost caret stops of a line's ink: `(left, right)` in device px.
fn ink(line: &LineGeom) -> (f32, f32) {
    line.slots
        .iter()
        .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), s| {
            (lo.min(s.x), hi.max(s.x))
        })
}

fn right_of_ink(line: &LineGeom) -> f32 {
    ink(line).1 + 20.0
}

fn left_of_ink(line: &LineGeom) -> f32 {
    ink(line).0 - 20.0
}

/// A point `gap` px under the line (inside the page body, below the last
/// line of the paragraph).
fn below(line: &LineGeom, x: f32) -> BridgePoint {
    BridgePoint {
        x,
        y: line.y_top + line.height + 40.0,
    }
}

/// A point on the line's own band.
fn on(line: &LineGeom, x: f32) -> BridgePoint {
    BridgePoint {
        x,
        y: line.y_top + line.height / 2.0,
    }
}

/// Visual x of the caret stop at `byte` (the first one when two runs
/// meet there).
fn stop_x(line: &LineGeom, byte: u32) -> f32 {
    line.slots
        .iter()
        .find(|s| s.byte == byte)
        .unwrap_or_else(|| panic!("a caret stop at byte {byte}"))
        .x
}

fn hit_offset(e: &mut Engine, at: BridgePoint) -> u32 {
    match apply(e, Command::HitTest { at }) {
        Event::HitResult { pos } => pos.offset,
        other => panic!("HitTest: {other:?}"),
    }
}

fn text_of(e: &Engine) -> String {
    e.undo
        .current()
        .nth_paragraph(0)
        .expect("paragraph")
        .text
        .clone()
}

/// What `SelectWordAt` at `at` selects, as `(text, start, end)`.
fn select_word(e: &mut Engine, at: BridgePoint) -> (String, u32, u32) {
    match apply(e, Command::SelectWordAt { at }) {
        Event::SelectionChanged { range, .. } => {
            let text = text_of(e);
            let (lo, hi) = (range.start.offset, range.end.offset);
            (text[lo as usize..hi as usize].to_string(), lo, hi)
        }
        other => panic!("SelectWordAt: {other:?}"),
    }
}

fn word_at(e: &mut Engine, at: BridgePoint) -> String {
    select_word(e, at).0
}

const MIXED: &str = "Hello world مرحبا بالعالم";

/// The issue's reproduction, through the commands the shell dispatches: a
/// double-click below the last line hit-tests to the paragraph END and
/// selects the word before it.
#[test]
fn double_click_below_the_last_line_selects_the_logical_last_word() {
    let mut e = engine_for(MIXED);
    let geom = lines(&e);
    let line = &geom[0];
    assert_eq!(geom.len(), 1, "one line");
    assert!(!line.rtl, "precondition: an LTR paragraph");
    /* Precondition (the bug's shape): the visually rightmost caret stop is
    the RTL run's logical START — not the end of the paragraph. */
    let rightmost = line
        .slots
        .iter()
        .max_by(|a, b| a.x.total_cmp(&b.x))
        .expect("slots");
    assert_eq!(rightmost.byte, "Hello world ".len() as u32);
    assert!(line.runs.len() >= 2, "an LTR run then an RTL run");

    let len = MIXED.len() as u32;
    let at = below(line, right_of_ink(line));
    assert_eq!(hit_offset(&mut e, at), len, "the hit is the paragraph end");
    let (word, lo, hi) = select_word(&mut e, at);
    assert_eq!(word, "بالعالم", "the logical last word, not the visual last");
    assert_eq!(hi, len);
    assert_eq!(lo, "Hello world مرحبا ".len() as u32);

    /* The far side of the line is its logical START. */
    let at = below(line, left_of_ink(line));
    assert_eq!(hit_offset(&mut e, at), 0);
    assert_eq!(word_at(&mut e, at), "Hello");
}

/// The same past the ink but ON the line's own band (the empty space
/// right of the text), and far past the page content as well.
#[test]
fn double_click_right_of_the_ink_on_the_line_selects_the_logical_last_word() {
    let mut e = engine_for(MIXED);
    let geom = lines(&e);
    let line = &geom[0];
    for x in [
        right_of_ink(line),
        line.hit_left + line.hit_width - 2.0,
        line.hit_left + line.hit_width + 300.0,
    ] {
        let at = on(line, x);
        assert_eq!(hit_offset(&mut e, at), MIXED.len() as u32, "x = {x}");
        assert_eq!(word_at(&mut e, at), "بالعالم", "x = {x}");
    }
    for x in [left_of_ink(line), line.hit_left - 50.0] {
        assert_eq!(word_at(&mut e, on(line, x)), "Hello", "x = {x}");
    }
}

/// Over the ink the pointer still decides: every word of the line is
/// selected by a double-click on its own letters, whichever way the run
/// reads (a stop strictly inside each word).
#[test]
fn double_click_over_each_word_of_a_mixed_line_selects_that_word() {
    let mut e = engine_for(MIXED);
    let geom = lines(&e);
    let line = &geom[0];
    for word in ["Hello", "world", "مرحبا", "بالعالم"] {
        let start = MIXED.find(word).expect("word") as u32;
        let first = word.chars().next().expect("char").len_utf8() as u32;
        let at = on(line, stop_x(line, start + first));
        assert_eq!(word_at(&mut e, at), word, "on {word:?}");
    }
}

/// The word under the pointer is chosen from the CHARACTER under it: the
/// right half of a word's last letter belongs to the word, the left half
/// of the space after it to the space. (The nearest caret slot is the
/// same one for both and used to pick the space for both.) All-Latin
/// paragraph, so the LTR case agrees with the mixed one.
#[test]
fn the_right_half_of_a_words_last_letter_selects_the_word() {
    let text = "Hello world";
    let mut e = engine_for(text);
    let geom = lines(&e);
    let line = &geom[0];
    let between = |a: u32, b: u32, t: f32| {
        let (xa, xb) = (stop_x(line, a), stop_x(line, b));
        on(line, xa + (xb - xa) * t)
    };
    assert_eq!(word_at(&mut e, between(4, 5, 0.75)), "Hello", "right half of `o`");
    assert_eq!(word_at(&mut e, between(4, 5, 0.25)), "Hello", "left half of `o`");
    assert_eq!(word_at(&mut e, between(5, 6, 0.25)), " ", "left half of the space");
    assert_eq!(word_at(&mut e, between(5, 6, 0.75)), " ", "right half of the space");
    assert_eq!(word_at(&mut e, between(6, 7, 0.25)), "world", "left half of `w`");
    assert_eq!(word_at(&mut e, between(10, 11, 0.75)), "world", "right half of `d`");
    /* Past the ends: the logical first / last word. */
    assert_eq!(word_at(&mut e, on(line, right_of_ink(line))), "world");
    assert_eq!(word_at(&mut e, below(line, right_of_ink(line))), "world");
    assert_eq!(word_at(&mut e, below(line, left_of_ink(line))), "Hello");
    assert_eq!(hit_offset(&mut e, below(line, right_of_ink(line))), 11);
}

/// An RTL paragraph (Arabic first): its logical START is the RIGHT edge.
#[test]
fn an_rtl_paragraph_reads_its_edges_from_its_own_direction() {
    let text = "مرحبا بالعالم";
    let mut e = engine_for(text);
    let geom = lines(&e);
    let line = &geom[0];
    assert!(line.rtl, "precondition: an RTL paragraph");
    let end = text.len() as u32;
    /* Right of the ink = the start; left of it = the end. */
    for at in [
        below(line, right_of_ink(line)),
        on(line, right_of_ink(line)),
    ] {
        assert_eq!(hit_offset(&mut e, at), 0);
        assert_eq!(word_at(&mut e, at), "مرحبا");
    }
    for at in [below(line, left_of_ink(line)), on(line, left_of_ink(line))] {
        assert_eq!(hit_offset(&mut e, at), end);
        assert_eq!(word_at(&mut e, at), "بالعالم");
    }
    /* The right half of the last letter of the first word — visually its
    LEFT side — still belongs to it. */
    let between = |a: u32, b: u32, t: f32| {
        let (xa, xb) = (stop_x(line, a), stop_x(line, b));
        on(line, xa + (xb - xa) * t)
    };
    let sp = "مرحبا".len() as u32;
    let last = sp - 2;
    assert_eq!(word_at(&mut e, between(last, sp, 0.5)), "مرحبا");
}

/// An RTL paragraph that ends in a Latin phrase: the visually leftmost
/// run is the LTR one, whose own left edge is its START — the logical
/// end of the paragraph is its right side, near the seam.
#[test]
fn an_rtl_paragraph_with_a_trailing_latin_phrase_selects_the_logical_last_word() {
    let text = "مرحبا بالعالم Hello world";
    let mut e = engine_for(text);
    let geom = lines(&e);
    let line = &geom[0];
    assert!(line.rtl, "precondition: an RTL paragraph");
    /* Precondition: the visually leftmost stop is a START of the Latin
    run, not the paragraph end. */
    let leftmost = line
        .slots
        .iter()
        .min_by(|a, b| a.x.total_cmp(&b.x))
        .expect("slots");
    assert_eq!(leftmost.byte, "مرحبا بالعالم ".len() as u32);

    let len = text.len() as u32;
    let at = below(line, left_of_ink(line));
    assert_eq!(hit_offset(&mut e, at), len);
    assert_eq!(word_at(&mut e, at), "world", "the logical last word");
    let at = below(line, right_of_ink(line));
    assert_eq!(hit_offset(&mut e, at), 0);
    assert_eq!(word_at(&mut e, at), "مرحبا", "the logical first word");
}

/// A run boundary INSIDE the Arabic run (a style change between two
/// pieces of one word, and between two words): the last logical word is
/// still the one before the end, and a double-click at the seam between
/// the two pieces selects the whole word, not half of it.
#[test]
fn a_style_run_boundary_inside_the_arabic_run_does_not_move_the_word() {
    let first_piece = ("Hello world مرحبا ".len() as u32, "Hello world مرحبا بال".len() as u32);
    let mut e = engine_with(doc_with_bold(MIXED, &[first_piece]));
    let geom = lines(&e);
    let line = &geom[0];
    assert!(
        line.runs.len() >= 4,
        "precondition: the style change splits the Arabic text into runs, got {}",
        line.runs.len()
    );
    let len = MIXED.len() as u32;
    for at in [
        below(line, right_of_ink(line)),
        on(line, right_of_ink(line)),
    ] {
        assert_eq!(hit_offset(&mut e, at), len);
        assert_eq!(word_at(&mut e, at), "بالعالم");
    }
    /* On the seam between the bold and the plain piece. */
    let seam = first_piece.1;
    let at = on(line, stop_x(line, seam));
    assert_eq!(word_at(&mut e, at), "بالعالم", "on the style seam");
    /* The seam between two words of the Arabic run (the bold piece ends
    at the end of `مرحبا `). */
    let mut e = engine_with(doc_with_bold(MIXED, &[(0, "Hello world مرحبا ".len() as u32)]));
    let geom = lines(&e);
    let line = &geom[0];
    assert_eq!(word_at(&mut e, below(line, right_of_ink(line))), "بالعالم");
}

/// Past the end of a WRAPPED line, the target is that line's last word —
/// not the first word of the next line (the line's end offset is the next
/// line's start) and not the trailing space.
#[test]
fn double_click_past_a_wrapped_lines_end_selects_that_lines_last_word() {
    for unit in ["alpha beta gamma delta epsilon ", "Hello world مرحبا بالعالم "] {
        let text = unit.repeat(8);
        let mut e = engine_for(&text);
        let geom = lines(&e);
        assert!(geom.len() > 2, "precondition: the paragraph wraps");
        for (i, line) in geom.iter().enumerate() {
            let slice = &text[line.start_byte as usize..line.end_byte as usize];
            let expected = slice.split_whitespace().next_back().expect("a word");
            let at = on(line, line.hit_left + line.hit_width + 10.0);
            assert_eq!(
                word_at(&mut e, at),
                expected,
                "line {i} {slice:?} of {text:?}"
            );
        }
    }
}

/// Triple-click past the end of a mixed line selects the whole paragraph
/// (it never read the offset's direction).
#[test]
fn triple_click_below_the_last_line_selects_the_whole_mixed_paragraph() {
    let mut e = engine_for(MIXED);
    let geom = lines(&e);
    let line = &geom[0];
    match apply(
        &mut e,
        Command::SelectParagraphAt {
            at: below(line, right_of_ink(line)),
        },
    ) {
        Event::SelectionChanged { range, .. } => {
            assert_eq!(range.start.offset, 0);
            assert_eq!(range.end.offset, MIXED.len() as u32);
        }
        other => panic!("SelectParagraphAt: {other:?}"),
    }
}

fn caret_offset(e: &Engine) -> u32 {
    e.selection.as_ref().expect("selection").caret.offset
}

/// Ctrl+Arrow word motion is logical too: a click past the end of the
/// mixed line puts the caret at the paragraph end, and each Ctrl+Left
/// lands on the start of the previous LOGICAL word — through the seam,
/// never along the visual order. The RTL paragraph steps the other way
/// round (its Ctrl+Right is the visual-right = logical-backward motion).
#[test]
fn word_motion_from_a_click_past_the_end_walks_the_logical_words() {
    let mut e = engine_for(MIXED);
    let geom = lines(&e);
    let line = &geom[0];
    apply(
        &mut e,
        Command::PlaceCaretAtPoint {
            page: 0,
            at: below(line, right_of_ink(line)),
        },
    );
    assert_eq!(caret_offset(&e), MIXED.len() as u32);
    let mut stops = Vec::new();
    for _ in 0..4 {
        apply(
            &mut e,
            Command::MoveCaret {
                direction: MoveDirection::WordLeft,
                extend: false,
            },
        );
        stops.push(caret_offset(&e));
    }
    let start = |w: &str| MIXED.find(w).expect("word") as u32;
    assert_eq!(
        stops,
        vec![start("بالعالم"), start("مرحبا"), start("world"), 0]
    );

    let text = "مرحبا بالعالم Hello world";
    let mut e = engine_for(text);
    let geom = lines(&e);
    let line = &geom[0];
    apply(
        &mut e,
        Command::PlaceCaretAtPoint {
            page: 0,
            at: below(line, left_of_ink(line)),
        },
    );
    assert_eq!(caret_offset(&e), text.len() as u32);
    let mut stops = Vec::new();
    for _ in 0..4 {
        apply(
            &mut e,
            Command::MoveCaret {
                direction: MoveDirection::WordRight,
                extend: false,
            },
        );
        stops.push(caret_offset(&e));
    }
    let start = |w: &str| text.find(w).expect("word") as u32;
    assert_eq!(
        stops,
        vec![start("world"), start("Hello"), start("بالعالم"), 0]
    );
}

/// A drag that ends past the ink extends to the logical end of the line.
#[test]
fn a_drag_past_the_end_of_a_mixed_line_extends_to_the_logical_end() {
    let mut e = engine_for(MIXED);
    let geom = lines(&e);
    let line = &geom[0];
    apply(
        &mut e,
        Command::PlaceCaretAtPoint {
            page: 0,
            at: on(line, stop_x(line, 0)),
        },
    );
    apply(
        &mut e,
        Command::ExtendSelectionToPoint {
            page: 0,
            at: below(line, right_of_ink(line)),
        },
    );
    let sel = e.selection.clone().expect("selection");
    assert_eq!(
        (sel.anchor.offset, sel.caret.offset),
        (0, MIXED.len() as u32)
    );
}

fn synthetic_line(rtl: bool, slots: &[(f32, u32)]) -> LineGeom {
    LineGeom {
        path: BridgeBlockPath::top(0),
        start_x: 0.0,
        hit_left: 0.0,
        hit_width: 200.0,
        y_top: 0.0,
        height: 20.0,
        start_byte: 0,
        end_byte: 9,
        rtl,
        slots: slots
            .iter()
            .map(|&(x, byte)| CaretSlot { x, byte })
            .collect(),
        runs: Vec::new(),
    }
}

/// `line_edge_at` reads the logical end from the base direction alone.
#[test]
fn line_edge_at_follows_the_paragraph_direction() {
    /* Visual order is the same in both: an LTR run at 10..40, an RTL run
    at 40..70 (slots in visual order, the RTL start at its right edge). */
    let slots = [
        (10.0, 0),
        (25.0, 1),
        (40.0, 3),
        (55.0, 5),
        (70.0, 3),
        (40.0, 9),
    ];
    let ltr = synthetic_line(false, &slots);
    assert_eq!(line_edge_at(&ltr, 100.0), Some(LineEdge::End));
    assert_eq!(line_edge_at(&ltr, 70.0), Some(LineEdge::End));
    assert_eq!(line_edge_at(&ltr, 5.0), Some(LineEdge::Start));
    assert_eq!(line_edge_at(&ltr, 10.0), Some(LineEdge::Start));
    assert_eq!(line_edge_at(&ltr, 40.0), None, "over the ink");
    assert_eq!(line_edge_at(&ltr, 69.9), None, "still over the ink");
    let rtl = synthetic_line(true, &slots);
    assert_eq!(line_edge_at(&rtl, 100.0), Some(LineEdge::Start));
    assert_eq!(line_edge_at(&rtl, 5.0), Some(LineEdge::End));
    assert_eq!(line_edge_at(&rtl, 40.0), None);
    /* An empty line has no ink to be past; a single stop has no side. */
    assert_eq!(line_edge_at(&synthetic_line(false, &[]), 50.0), None);
    let one = synthetic_line(true, &[(30.0, 0)]);
    assert_eq!(line_edge_at(&one, 30.0), Some(LineEdge::Start));
    assert_eq!(line_edge_at(&one, 99.0), Some(LineEdge::Start), "right of RTL");
    assert_eq!(line_edge_at(&one, 1.0), Some(LineEdge::End), "left of RTL");
}

/// `hit_test_geom` over a synthetic mixed line: a point past the right
/// edge names the line's logical end in an LTR paragraph and its logical
/// start in an RTL one; over the ink it is the nearest stop.
#[test]
fn hit_test_geom_resolves_a_point_past_the_ink_to_a_logical_end() {
    let slots = [(10.0, 0), (40.0, 3), (70.0, 3), (40.0, 9)];
    let at = |line: &LineGeom, x: f32| hit_test_geom(std::slice::from_ref(line), x, 10.0).offset;
    let ltr = synthetic_line(false, &slots);
    assert_eq!(at(&ltr, 150.0), 9, "right of an LTR line = its end");
    assert_eq!(at(&ltr, 0.0), 0);
    assert_eq!(at(&ltr, 41.0), 3, "over the ink: the nearest stop");
    let rtl = synthetic_line(true, &slots);
    assert_eq!(at(&rtl, 150.0), 0, "right of an RTL line = its start");
    assert_eq!(at(&rtl, 0.0), 9);
}

/// `word_probe_offset`: a line end probes the last character of the line's
/// last word; a character target probes itself.
#[test]
fn word_probe_offset_skips_trailing_whitespace_at_a_line_end() {
    let text = "alpha beta  gamma ";
    let end = text.len() as u32;
    assert_eq!(
        word_probe_offset(text, WordTarget::LineEnd { start: 0, end }),
        "alpha beta  gamm".len() as u32,
        "the last non-space character"
    );
    assert_eq!(
        word_probe_offset(text, WordTarget::LineEnd { start: 0, end: 6 }),
        4,
        "a line ending at a space probes the word before it"
    );
    /* An all-whitespace line probes its own last character. */
    assert_eq!(
        word_probe_offset("a   b", WordTarget::LineEnd { start: 1, end: 4 }),
        3
    );
    assert_eq!(word_probe_offset(text, WordTarget::Char(7)), 7);
    /* Out-of-range bytes cannot panic. */
    assert_eq!(
        word_probe_offset("é", WordTarget::LineEnd { start: 1, end: 99 }),
        1
    );
}

/// The same reproduction through the `fuzz-native` surface
/// (`Engine::new_headless` + `apply_sync`, a Latin-only face: the Arabic
/// run lays out with the face's `.notdef` glyphs but is still an RTL run).
#[cfg(feature = "fuzz-native")]
#[test]
fn headless_engine_selects_the_logical_last_word() {
    let mut e = Engine::new_headless(DocumentTree::from_text(MIXED));
    let geom = lines(&e);
    let line = &geom[0];
    let at = below(line, right_of_ink(line));
    match e.apply_sync(Command::HitTest { at }) {
        Event::HitResult { pos } => assert_eq!(pos.offset, MIXED.len() as u32),
        other => panic!("HitTest: {other:?}"),
    }
    match e.apply_sync(Command::SelectWordAt { at }) {
        Event::SelectionChanged { range, .. } => {
            let text = text_of(&e);
            assert_eq!(
                &text[range.start.offset as usize..range.end.offset as usize],
                "بالعالم"
            );
        }
        other => panic!("SelectWordAt: {other:?}"),
    }
}
