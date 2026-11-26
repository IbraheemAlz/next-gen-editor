//! Epic #555 — the TEXT rows of the cursor-behaviour matrix, driven
//! through real `Command`s on a laid-out engine (the clean-room cursor memo
//! `plans/cleanroom/memos/onlyoffice-cursor-memo.md`). The real-UI twin is
//! `ts/e2e/text-cursor-matrix.spec.ts`.
//!
//! * Row 2 (memo §1, §3) — the caret never rests inside a grapheme cluster
//!   and Left / Right step one cluster per press, ligatures included
//!   (`grapheme_slots`).
//! * Row 20 (memo §5) — Ctrl+Left / Ctrl+Right stop at three classes:
//!   text, punctuation, separators (`engine::words`, issue #598).
//! * Row 37 (memo §10) — an empty IME commit leaves the document and the
//!   selection as they were.

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

/// An engine over `text` with the shell's Arabic default (Amiri, whose
/// ligatures are the cluster-wider-than-a-grapheme case) beside the Latin
/// face.
fn engine_for(text: &str) -> Engine {
    let mut e = tests::test_engine_with_doc(DocumentTree::from_text(text));
    let bytes = include_bytes!("../../../ts/fonts/Amiri-Regular.ttf").to_vec();
    let font = LoadedFont::parse("test-arabic".to_string(), bytes).expect("amiri");
    e.fonts.insert("test-arabic".to_string(), Arc::new(font));
    e
}

fn caret_at(e: &mut Engine, offset: u32) {
    let p = bpos_top(0, offset);
    apply(
        e,
        Command::SetSelection {
            range: BridgeLogicalRange {
                start: p.clone(),
                end: p.clone(),
            },
            caret: p,
        },
    );
}

/// The collapsed caret after `cmd`.
fn caret_after(e: &mut Engine, cmd: Command) -> u32 {
    match apply(e, cmd) {
        Event::SelectionChanged { range, .. } => {
            assert_eq!(range.start, range.end, "a collapsed caret");
            range.start.offset
        }
        other => panic!("{other:?}"),
    }
}

fn press(e: &mut Engine, direction: MoveDirection) -> u32 {
    caret_after(
        e,
        Command::MoveCaret {
            direction,
            extend: false,
        },
    )
}

fn boundaries(text: &str) -> Vec<u32> {
    grapheme_slots::grapheme_boundaries(text)
}

/* ------------------------------------------------------------------ */
/* Row 2                                                               */

/// `كَتَبَّ`: three clusters (each letter with its harakat). Amiri shapes
/// `تَبَّ` as ONE ligature glyph, so the line used to offer caret stops at
/// 0, 4 and the end only — Left from 4 jumped over two clusters — and the
/// fatha glyph added a stop for the cluster byte at the far edge of its
/// base. Every stop is now a grapheme boundary, every boundary a stop.
#[test]
fn every_caret_stop_is_a_grapheme_boundary_and_a_ligature_is_split() {
    let text = "كَتَبَّ";
    let mut e = engine_for(text);
    let geom = e.document_geometry().expect("geometry");
    assert_eq!(geom.len(), 1);
    let line = &geom[0];
    assert!(line.rtl, "precondition: an RTL paragraph");
    let bounds = boundaries(text);
    assert_eq!(bounds, vec![0, 4, 8, 14]);
    let mut bytes: Vec<u32> = line.slots.iter().map(|s| s.byte).collect();
    bytes.sort_unstable();
    bytes.dedup();
    assert_eq!(bytes, bounds, "one stop per cluster boundary");
    /* Right-to-left: each later boundary sits further left. */
    let x = |b: u32| line.slots.iter().find(|s| s.byte == b).expect("stop").x;
    assert!(x(0) > x(4) && x(4) > x(8) && x(8) > x(14), "visual order");

    /* Left (logical forward in an RTL paragraph): one cluster per press. */
    caret_at(&mut e, 0);
    let walked: Vec<u32> = (0..3).map(|_| press(&mut e, MoveDirection::Left)).collect();
    assert_eq!(walked, vec![4, 8, 14]);
    let back: Vec<u32> = (0..3)
        .map(|_| press(&mut e, MoveDirection::Right))
        .collect();
    assert_eq!(back, vec![8, 4, 0]);
}

/// Decomposed accents in an LTR paragraph: `e` + U+0301 is one step, and
/// no stop sits between a base and its mark.
#[test]
fn a_base_and_its_combining_mark_are_one_caret_step() {
    let text = "e\u{301}te\u{301}";
    let mut e = engine_for(text);
    let bounds = boundaries(text);
    assert_eq!(bounds, vec![0, 3, 4, 7]);
    for line in e.document_geometry().expect("geometry") {
        for s in &line.slots {
            assert!(
                bounds.contains(&s.byte),
                "stop at {} is inside a cluster",
                s.byte
            );
        }
    }
    caret_at(&mut e, 0);
    let walked: Vec<u32> = (0..3)
        .map(|_| press(&mut e, MoveDirection::Right))
        .collect();
    assert_eq!(walked, vec![3, 4, 7]);
}

/// A click anywhere along the line (and past its ink) resolves to a
/// cluster boundary — never between a letter and its harakat, never inside
/// the ligature's middle cluster boundary's neighbours.
#[test]
fn a_click_never_lands_inside_a_cluster() {
    for text in [
        "كَتَبَّ",
        "e\u{301}te\u{301}",
        "a\u{1F469}\u{200D}\u{1F469}\u{200D}\u{1F467}b",
    ] {
        let mut e = engine_for(text);
        let bounds = boundaries(text);
        let geom = e.document_geometry().expect("geometry");
        let line = &geom[0];
        let (lo, hi) = line
            .slots
            .iter()
            .fold((f32::INFINITY, f32::NEG_INFINITY), |(a, b), s| {
                (a.min(s.x), b.max(s.x))
            });
        let y = line.y_top + line.height / 2.0;
        let mut seen = std::collections::BTreeSet::new();
        let mut x = lo - 10.0;
        while x <= hi + 10.0 {
            let pos = match apply(
                &mut e,
                Command::HitTest {
                    at: BridgePoint { x, y },
                },
            ) {
                Event::HitResult { pos } => pos,
                other => panic!("{other:?}"),
            };
            assert!(
                bounds.contains(&pos.offset),
                "{text:?}: click at x={x} landed at {}",
                pos.offset
            );
            seen.insert(pos.offset);
            x += 1.0;
        }
        assert_eq!(
            seen.into_iter().collect::<Vec<_>>(),
            bounds,
            "{text:?}: every boundary is reachable by a click"
        );
    }
}

/* ------------------------------------------------------------------ */
/* Row 20                                                              */

fn forward_stops(text: &str) -> Vec<usize> {
    let mut out = Vec::new();
    let mut off = 0;
    while off < text.len() {
        off = word_stop_forward(text, off);
        out.push(off);
    }
    out
}

fn backward_stops(text: &str) -> Vec<usize> {
    let mut out = Vec::new();
    let mut off = text.len();
    while off > 0 {
        off = word_stop_backward(text, off);
        out.push(off);
    }
    out
}

/// Punctuation is a word stop of its own; separators are skipped after a
/// word (Ctrl+Right) and before one (Ctrl+Left).
#[test]
fn punctuation_is_its_own_word_stop() {
    assert_eq!(forward_stops("word, next"), vec![4, 6, 10]);
    assert_eq!(backward_stops("word, next"), vec![6, 4, 0]);
    /* A run of punctuation is one stop. */
    assert_eq!(forward_stops("wait... go"), vec![4, 8, 10]);
    assert_eq!(backward_stops("wait... go"), vec![8, 4, 0]);
    /* From inside a word: its end-side neighbours. */
    assert_eq!(word_stop_forward("word, next", 2), 4);
    assert_eq!(word_stop_backward("word, next", 8), 6);
    /* From inside the separators. */
    assert_eq!(word_stop_forward("a   b", 2), 4);
    assert_eq!(word_stop_backward("a   b", 3), 0);
}

/// UAX #29 keeps a contraction and a decimal number whole (the class
/// model works on its segments), and an inline object is a word by itself.
#[test]
fn contractions_numbers_and_objects() {
    assert_eq!(forward_stops("isn't done"), vec![6, 10]);
    assert_eq!(forward_stops("3.14 pi"), vec![5, 7]);
    let obj = "a\u{FFFC}\u{FFFC}b";
    assert_eq!(forward_stops(obj), vec![1, 4, 7, 8]);
    assert_eq!(backward_stops(obj), vec![7, 4, 1, 0]);
}

/// No Arabic-specific rule: a tatweel and harakat are part of their word,
/// the Arabic comma is punctuation.
#[test]
fn arabic_tatweel_and_harakat_stay_inside_their_word() {
    let w1 = "كتـــاب";
    let w2 = "جدّيد";
    let text = format!("go {w1}، {w2} end");
    let b = |s: &str| s.len();
    assert_eq!(
        forward_stops(&text),
        vec![
            b("go "),
            b(&format!("go {w1}")),
            b(&format!("go {w1}، ")),
            b(&format!("go {w1}، {w2} ")),
            text.len(),
        ]
    );
    assert_eq!(
        backward_stops(&text),
        vec![
            b(&format!("go {w1}، {w2} ")),
            b(&format!("go {w1}، ")),
            b(&format!("go {w1}")),
            b("go "),
            0,
        ]
    );
}

/// Through the command: Ctrl+Right in an LTR paragraph, flipped in an RTL
/// one (Ctrl+Left walks logically forward there).
#[test]
fn word_moves_through_the_command_follow_the_classes_and_flip_in_rtl() {
    let mut e = engine_for("word, next");
    caret_at(&mut e, 0);
    let walked: Vec<u32> = (0..3)
        .map(|_| press(&mut e, MoveDirection::WordRight))
        .collect();
    assert_eq!(walked, vec![4, 6, 10]);

    let rtl = "كتـــاب جدّيد";
    let mut e = engine_for(rtl);
    caret_at(&mut e, 0);
    let mid = "كتـــاب ".len() as u32;
    assert_eq!(press(&mut e, MoveDirection::WordLeft), mid);
    assert_eq!(press(&mut e, MoveDirection::WordLeft), rtl.len() as u32);
    assert_eq!(press(&mut e, MoveDirection::WordRight), mid);
}

/// Ctrl+Backspace / Ctrl+Delete reach exactly as far as the word moves.
#[test]
fn word_deletion_uses_the_same_stops() {
    let mut e = engine_for("word, next");
    caret_at(&mut e, 0);
    apply(
        &mut e,
        Command::DeleteAtCaret {
            forward: true,
            by_word: true,
        },
    );
    assert_eq!(e.undo.current().nth_paragraph(0).unwrap().text, ", next");
    let mut e = engine_for("word, next");
    caret_at(&mut e, 6);
    apply(
        &mut e,
        Command::DeleteAtCaret {
            forward: false,
            by_word: true,
        },
    );
    assert_eq!(e.undo.current().nth_paragraph(0).unwrap().text, "wordnext");
}

/* ------------------------------------------------------------------ */
/* Row 37                                                              */

/// An empty commit — the composed text emptied before the end, which is
/// what the shell now sends for a cancelled composition — leaves the
/// document and the selection exactly as they were before it began.
#[test]
fn an_empty_ime_commit_restores_the_text_and_the_selection() {
    let mut e = engine_for("Hello world!");
    let (a, b) = (bpos_top(0, 6), bpos_top(0, 11));
    apply(
        &mut e,
        Command::SetSelection {
            range: BridgeLogicalRange {
                start: a.clone(),
                end: b.clone(),
            },
            caret: b.clone(),
        },
    );
    let depth = e.undo.depth();
    apply(&mut e, Command::BeginComposition { at: None });
    apply(
        &mut e,
        Command::UpdateComposition {
            text: "に".into(),
            target_range: None,
        },
    );
    apply(
        &mut e,
        Command::UpdateComposition {
            text: String::new(),
            target_range: None,
        },
    );
    match apply(&mut e, Command::EndComposition { commit: true }) {
        Event::SelectionChanged { range, .. } => {
            assert_eq!((range.start, range.end), (a, b), "the selection survives");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        e.undo.current().nth_paragraph(0).unwrap().text,
        "Hello world!"
    );
    assert_eq!(e.undo.depth(), depth, "no undo step");
}
