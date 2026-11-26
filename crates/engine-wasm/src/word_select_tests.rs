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
//! Issue #598 (epic #555 row 22) — the word itself is the ONE word model
//! (`engine::words`, shared with Ctrl+arrows): text / punctuation /
//! separator / object units, a word being a unit WITH the separators after
//! it (clean-room cursor memo §5 / §6). So a double-click on `Hello` in
//! `Hello world` selects `Hello ` (and a click on the space selects the
//! word it trails), punctuation is a word of its own, and a drag after the
//! double press grows by whole words (`SelectUnitsInPage`).
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
    doc.blocks
        .push_back(engine::Block::Paragraph(engine::Paragraph {
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
    assert_eq!(
        word, "بالعالم",
        "the logical last word, not the visual last"
    );
    assert_eq!(hi, len);
    assert_eq!(lo, "Hello world مرحبا ".len() as u32);

    /* The far side of the line is its logical START (issue #598: the
    first word with its trailing space). */
    let at = below(line, left_of_ink(line));
    assert_eq!(hit_offset(&mut e, at), 0);
    assert_eq!(word_at(&mut e, at), "Hello ");
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
        assert_eq!(word_at(&mut e, on(line, x)), "Hello ", "x = {x}");
    }
}

/// Over the ink the pointer still decides: every word of the line is
/// selected by a double-click on its own letters, whichever way the run
/// reads (a stop strictly inside each word) — with its trailing space
/// (issue #598).
#[test]
fn double_click_over_each_word_of_a_mixed_line_selects_that_word() {
    let mut e = engine_for(MIXED);
    let geom = lines(&e);
    let line = &geom[0];
    for (word, selected) in [
        ("Hello", "Hello "),
        ("world", "world "),
        ("مرحبا", "مرحبا "),
        ("بالعالم", "بالعالم"),
    ] {
        let start = MIXED.find(word).expect("word") as u32;
        let first = word.chars().next().expect("char").len_utf8() as u32;
        let at = on(line, stop_x(line, start + first));
        assert_eq!(word_at(&mut e, at), selected, "on {word:?}");
    }
}

/// The word under the pointer is chosen from the CHARACTER under it: the
/// right half of a word's last letter belongs to the word, the left half
/// of `w` to `world`. (The nearest caret slot is the same one for both
/// halves of a slot and used to pick the wrong side.) Issue #598: the
/// space after `Hello` belongs to `Hello` — a double-click on it selects
/// the word it trails, with it. All-Latin paragraph, so the LTR case
/// agrees with the mixed one.
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
    assert_eq!(
        word_at(&mut e, between(4, 5, 0.75)),
        "Hello ",
        "right half of `o`"
    );
    assert_eq!(
        word_at(&mut e, between(4, 5, 0.25)),
        "Hello ",
        "left half of `o`"
    );
    assert_eq!(
        word_at(&mut e, between(5, 6, 0.25)),
        "Hello ",
        "left half of the space"
    );
    assert_eq!(
        word_at(&mut e, between(5, 6, 0.75)),
        "Hello ",
        "right half of the space"
    );
    assert_eq!(
        word_at(&mut e, between(6, 7, 0.25)),
        "world",
        "left half of `w`"
    );
    assert_eq!(
        word_at(&mut e, between(10, 11, 0.75)),
        "world",
        "right half of `d`"
    );
    /* Past the ends: the logical first / last word. */
    assert_eq!(word_at(&mut e, on(line, right_of_ink(line))), "world");
    assert_eq!(word_at(&mut e, below(line, right_of_ink(line))), "world");
    assert_eq!(word_at(&mut e, below(line, left_of_ink(line))), "Hello ");
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
        assert_eq!(word_at(&mut e, at), "مرحبا ");
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
    assert_eq!(word_at(&mut e, between(last, sp, 0.5)), "مرحبا ");
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
    assert_eq!(word_at(&mut e, at), "مرحبا ", "the logical first word");
}

/// A run boundary INSIDE the Arabic run (a style change between two
/// pieces of one word, and between two words): the last logical word is
/// still the one before the end, and a double-click at the seam between
/// the two pieces selects the whole word, not half of it.
#[test]
fn a_style_run_boundary_inside_the_arabic_run_does_not_move_the_word() {
    let first_piece = (
        "Hello world مرحبا ".len() as u32,
        "Hello world مرحبا بال".len() as u32,
    );
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
    let mut e = engine_with(doc_with_bold(
        MIXED,
        &[(0, "Hello world مرحبا ".len() as u32)],
    ));
    let geom = lines(&e);
    let line = &geom[0];
    assert_eq!(word_at(&mut e, below(line, right_of_ink(line))), "بالعالم");
}

/// Past the end of a WRAPPED line, the target is that line's last word —
/// not the first word of the next line (the line's end offset is the next
/// line's start) — with the trailing space the line ends in (issue #598).
#[test]
fn double_click_past_a_wrapped_lines_end_selects_that_lines_last_word() {
    for unit in [
        "alpha beta gamma delta epsilon ",
        "Hello world مرحبا بالعالم ",
    ] {
        let text = unit.repeat(8);
        let mut e = engine_for(&text);
        let geom = lines(&e);
        assert!(geom.len() > 2, "precondition: the paragraph wraps");
        for (i, line) in geom.iter().enumerate() {
            let slice = &text[line.start_byte as usize..line.end_byte as usize];
            let trimmed = slice.trim_end();
            let word_start = trimmed
                .char_indices()
                .rev()
                .find(|(_, c)| c.is_whitespace())
                .map_or(0, |(i, c)| i + c.len_utf8());
            let expected = &slice[word_start..];
            assert!(
                expected.ends_with(' '),
                "{expected:?}: a wrapped line ends in a space"
            );
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
        baseline: 16.0,
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
    assert_eq!(
        line_edge_at(&one, 99.0),
        Some(LineEdge::Start),
        "right of RTL"
    );
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

/* ------------------------------------------------------------------ */
/* Issue #598 — the ONE word model under the pointer, and the drag     */
/* by units after a double / triple press (epic #555 row 22).          */

/// The middle of every caret CELL of a single-direction line — between
/// two logically adjacent caret stops, which are visually adjacent in a
/// run of one direction — with the byte the cell starts at. A cell is one
/// grapheme cluster (`grapheme_slots`): a letter with its harakat, one
/// tatweel.
fn cells(line: &LineGeom) -> Vec<(u32, f32)> {
    let mut bytes: Vec<u32> = line.slots.iter().map(|s| s.byte).collect();
    bytes.sort_unstable();
    bytes.dedup();
    bytes
        .windows(2)
        .map(|w| (w[0], (stop_x(line, w[0]) + stop_x(line, w[1])) / 2.0))
        .collect()
}

/// A double-click on every cell of `text` (one line) selects what
/// `expected` says for the text from the cell's first byte on.
fn assert_word_under_every_cell(e: &mut Engine, text: &str, expected: impl Fn(&str) -> String) {
    let geom = lines(e);
    assert_eq!(geom.len(), 1, "{text:?} lays out on one line");
    let line = &geom[0];
    let cells = cells(line);
    {
        use unicode_segmentation::UnicodeSegmentation;
        assert_eq!(
            cells.len(),
            text.graphemes(true).count(),
            "{text:?}: a cell per grapheme"
        );
    }
    for (byte, x) in cells {
        let rest = &text[byte as usize..];
        assert_eq!(
            word_at(e, on(line, x)),
            expected(rest),
            "{text:?}: the cell at byte {byte} ({rest:?})"
        );
    }
}

/// Memo §5 / §6: punctuation is a word of its own, a word takes its
/// trailing spaces, a double-click on a space selects the word it trails.
#[test]
fn punctuation_is_its_own_word_and_a_word_takes_its_trailing_space() {
    let text = "word, next one";
    let mut e = engine_for(text);
    assert_word_under_every_cell(&mut e, text, |rest| {
        if rest.len() > ", next one".len() {
            "word"
        } else if rest.len() > "next one".len() {
            ", "
        } else if rest.len() > "one".len() {
            "next "
        } else {
            "one"
        }
        .to_string()
    });
}

/// No Arabic-specific rule: in an RTL paragraph a word with a tatweel and
/// harakat is selected whole from ANY of its cells (every grapheme of it,
/// the tatweels and the marked letters included), with its trailing
/// space; the Arabic comma is punctuation, its own word.
#[test]
fn an_arabic_word_with_tatweel_and_harakat_is_selected_whole() {
    const TATWEEL_WORD: &str = "كتـــاب";
    const MARKED_WORD: &str = "جَدِيدٌ";
    let text = format!("{TATWEEL_WORD} {MARKED_WORD}، نص");
    let tail_of_marked = format!("{MARKED_WORD}، نص");
    let mut e = engine_for(&text);
    assert!(lines(&e)[0].rtl, "precondition: an RTL paragraph");
    assert_word_under_every_cell(&mut e, &text, |rest| {
        if rest.len() > tail_of_marked.len() {
            format!("{TATWEEL_WORD} ")
        } else if rest.len() > "، نص".len() {
            MARKED_WORD.to_string()
        } else if rest.len() > "نص".len() {
            "، ".to_string()
        } else {
            "نص".to_string()
        }
    });
}

/// `SelectUnitsInPage { from, to, unit }` and what it selected, as
/// `(selected text, anchor, caret)` (page 0: page-local = document px).
fn select_units(
    e: &mut Engine,
    from: BridgePoint,
    to: BridgePoint,
    unit: SelectionUnit,
) -> (String, u32, u32) {
    match apply(
        e,
        Command::SelectUnitsInPage {
            page: 0,
            from,
            to,
            unit,
        },
    ) {
        Event::SelectionChanged { range, .. } => {
            let sel = e.selection.clone().expect("selection");
            assert_eq!(
                (range.start.offset, range.end.offset),
                (
                    sel.anchor.offset.min(sel.caret.offset),
                    sel.anchor.offset.max(sel.caret.offset)
                )
            );
            let text = text_of(e);
            (
                text[range.start.offset as usize..range.end.offset as usize].to_string(),
                sel.anchor.offset,
                sel.caret.offset,
            )
        }
        other => panic!("SelectUnitsInPage: {other:?}"),
    }
}

/// Memo §6: the double press selects the word under it, and a drag after
/// it extends by WHOLE words — forward from the word's start, backward
/// from its END, so the double-clicked word always stays selected.
#[test]
fn a_drag_after_a_double_click_extends_by_whole_words() {
    let text = "one two three four five";
    let mut e = engine_for(text);
    let geom = lines(&e);
    let line = &geom[0];
    let start = |w: &str| text.find(w).expect("word") as u32;
    /* The middle of the second character of `w`. */
    let on_word = |w: &str| {
        let s = start(w);
        on(line, (stop_x(line, s + 1) + stop_x(line, s + 2)) / 2.0)
    };
    let press = on_word("three");
    let word = SelectionUnit::Word;
    /* The press itself (`from == to`): the word with its space. */
    assert_eq!(
        select_units(&mut e, press, press, word),
        ("three ".into(), start("three"), start("four"))
    );
    /* Forward into the middle of `four`: whole words, anchor at `three`. */
    assert_eq!(
        select_units(&mut e, press, on_word("four"), word),
        ("three four ".into(), start("three"), start("five"))
    );
    /* Back into `two`: the anchor flips to the END of `three `. */
    assert_eq!(
        select_units(&mut e, press, on_word("two"), word),
        ("two three ".into(), start("four"), start("two"))
    );
    /* Back past the start of the line: the first word whole. */
    assert_eq!(
        select_units(&mut e, press, on(line, left_of_ink(line)), word),
        ("one two three ".into(), start("four"), 0)
    );
    /* Past the end of the line: the line's last word. */
    assert_eq!(
        select_units(&mut e, press, on(line, right_of_ink(line)), word),
        ("three four five".into(), start("three"), text.len() as u32)
    );
    /* The space after `three` belongs to `three`: still just it. */
    let space = start("four") - 1;
    let on_space = on(line, (stop_x(line, space) + stop_x(line, space + 1)) / 2.0);
    assert_eq!(
        select_units(&mut e, press, on_space, word),
        ("three ".into(), start("three"), start("four"))
    );
}

/// The press of a double-click and `SelectWordAt` (the `dblclick`
/// fallback) resolve the same word: one resolution, two coordinate
/// conventions (page-local on page 0 == document px). A mixed line, so
/// the #553 logical rules hold for both.
#[test]
fn the_double_press_and_select_word_at_agree_everywhere() {
    let text = "Hello world, مرحبا بالعالم 3.14 isn't";
    let mut e = engine_for(text);
    let geom = lines(&e);
    let line = &geom[0];
    let mut stops: Vec<f32> = line.slots.iter().map(|s| s.x).collect();
    stops.sort_by(f32::total_cmp);
    let mut xs: Vec<f32> = stops.windows(2).map(|w| (w[0] + w[1]) / 2.0).collect();
    xs.extend([left_of_ink(line), right_of_ink(line)]);
    for x in xs {
        let at = on(line, x);
        let by_word_at = select_word(&mut e, at);
        let by_press = select_units(&mut e, at, at, SelectionUnit::Word);
        assert_eq!(by_press, by_word_at, "x = {x}");
    }
}

/// One paragraph per string.
fn doc_with_paragraphs(texts: &[&str]) -> DocumentTree {
    let mut doc = DocumentTree::new();
    doc.blocks.clear();
    for text in texts {
        doc.blocks
            .push_back(engine::Block::Paragraph(engine::Paragraph {
                text: (*text).to_string(),
                ..Default::default()
            }));
    }
    doc
}

/// Memo §6: a triple press selects the paragraph, a drag after it extends
/// by whole paragraphs, either way, keeping the first one.
#[test]
fn a_drag_after_a_triple_click_extends_by_whole_paragraphs() {
    let texts = ["alpha beta", "gamma delta", "epsilon zeta"];
    let mut e = engine_with(doc_with_paragraphs(&texts));
    let geom = lines(&e);
    assert_eq!(geom.len(), 3, "one line per paragraph");
    let mid = |l: &LineGeom| on(l, (ink(l).0 + ink(l).1) / 2.0);
    let sel = |e: &Engine| {
        let s = e.selection.clone().expect("selection");
        let idx = |p: &BridgeLogicalPos| match p.path.steps.first() {
            Some(BridgePathStep::Block { idx }) => *idx,
            other => panic!("{other:?}"),
        };
        (
            (idx(&s.anchor), s.anchor.offset),
            (idx(&s.caret), s.caret.offset),
        )
    };
    let press = mid(&geom[1]);
    let drag_to = |e: &mut Engine, to: BridgePoint| {
        apply(
            e,
            Command::SelectUnitsInPage {
                page: 0,
                from: press,
                to,
                unit: SelectionUnit::Paragraph,
            },
        );
        sel(e)
    };
    assert_eq!(drag_to(&mut e, press), ((1, 0), (1, 11)), "the press");
    assert_eq!(
        drag_to(&mut e, mid(&geom[2])),
        ((1, 0), (2, 12)),
        "down: whole paragraphs"
    );
    assert_eq!(
        drag_to(&mut e, mid(&geom[0])),
        ((1, 11), (0, 0)),
        "up: anchored at the END of the first paragraph"
    );
}

/// Typing over a double-clicked word replaces the word and keeps its
/// trailing space (the space belongs to the selection, not to the typed
/// text), so the replacement never glues onto the next word. Deleting a
/// double-clicked word takes its space with it.
#[test]
fn typing_over_a_double_clicked_word_keeps_its_trailing_space() {
    let text = "one two three";
    let mut e = engine_for(text);
    let geom = lines(&e);
    let line = &geom[0];
    let on_two = on(line, (stop_x(line, 5) + stop_x(line, 6)) / 2.0);
    assert_eq!(word_at(&mut e, on_two), "two ");
    apply(
        &mut e,
        Command::InsertText {
            at: None,
            text: "2".into(),
        },
    );
    assert_eq!(text_of(&e), "one 2 three");
    assert_eq!(caret_offset(&e), 5, "the caret after the typed text");
    apply(&mut e, Command::Undo);
    assert_eq!(text_of(&e), text, "one undo step");

    /* Deleting it removes the word with its space. */
    assert_eq!(word_at(&mut e, on_two), "two ");
    apply(
        &mut e,
        Command::DeleteAtCaret {
            forward: false,
            by_word: false,
        },
    );
    assert_eq!(text_of(&e), "one three");

    /* A comma's word `, ` keeps its space too. */
    let mut e = engine_for("word, next");
    let geom = lines(&e);
    let line = &geom[0];
    let on_comma = on(line, (stop_x(line, 4) + stop_x(line, 5)) / 2.0);
    assert_eq!(word_at(&mut e, on_comma), ", ");
    apply(
        &mut e,
        Command::InsertText {
            at: None,
            text: ";".into(),
        },
    );
    assert_eq!(text_of(&e), "word; next");
}
