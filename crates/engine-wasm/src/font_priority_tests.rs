//! Issue #472 — `Command::SetFontPriority`: the shell-declared per-script
//! default faces reach layout, the read-backs and the substitution report.

use super::*;
use crate::font_substitution_tests::engine_with_editor_faces;
use bridge::FontPriority;

fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    use std::task::{Context, Poll, Waker};
    let mut cx = Context::from_waker(Waker::noop());
    let mut fut = Box::pin(fut);
    match fut.as_mut().poll(&mut cx) {
        Poll::Ready(v) => v,
        Poll::Pending => panic!("Engine::apply suspended in a native test"),
    }
}

fn set_priority(e: &mut Engine, entries: &[(bridge::Script, &[&str])]) -> Event {
    let priorities = entries
        .iter()
        .map(|(script, ids)| FontPriority {
            script: *script,
            ids: ids.iter().map(|s| (*s).to_string()).collect(),
        })
        .collect();
    let evt = block_on(e.apply(Command::SetFontPriority { priorities }));
    assert!(!matches!(evt, Event::Error { .. }), "{evt:?}");
    evt
}

/// What the interactive editor booted with before #472: the seed face
/// (`defaults[0]`) is Amiri, so the stack's root and — by id order — its
/// Latin default were both Amiri.
fn editor_engine(text: &str) -> Engine {
    let mut e = engine_with_editor_faces(DocumentTree::from_text(text));
    e.layout_cfg.as_mut().expect("cfg").font_id = "amiri".to_string();
    e
}

/// The face laid out for the first run of page 1's first paragraph.
fn first_run_font(e: &Engine) -> String {
    let (pages, _, _, _) = e.build_pages(1.0, false, None).expect("layout");
    let para = pages[0].blocks[0].as_paragraph().expect("paragraph");
    para.lines
        .iter()
        .flat_map(|l| &l.runs)
        .next()
        .expect("a run")
        .font
        .clone()
}

fn default_face(e: &Engine, script: text_pipeline::Script) -> String {
    e.default_face_for(&std::cell::OnceCell::new(), script, false, false)
}

#[test]
fn without_a_priority_latin_falls_to_the_first_covering_id() {
    /* The bug: `amiri` sorts before `liberation` and covers Latin. */
    let e = editor_engine("Hello world");
    assert_eq!(first_run_font(&e), "amiri");
    assert_eq!(default_face(&e, text_pipeline::Script::Latin), "amiri");
}

#[test]
fn a_declared_latin_default_reaches_layout_and_the_read_back() {
    let mut e = editor_engine("Hello world");
    let evt = set_priority(
        &mut e,
        &[
            (bridge::Script::Latin, &["liberation"]),
            (bridge::Script::Arabic, &["amiri", "noto-naskh"]),
        ],
    );
    let Event::FontPriorityApplied { priorities } = evt else {
        panic!("expected FontPriorityApplied, got {evt:?}");
    };
    let got: Vec<(String, Vec<String>)> = priorities
        .into_iter()
        .map(|p| (format!("{:?}", p.script), p.ids))
        .collect();
    assert_eq!(
        got,
        [
            ("Latin".to_string(), vec!["liberation".to_string()]),
            (
                "Arabic".to_string(),
                vec!["amiri".to_string(), "noto-naskh".to_string()]
            ),
        ]
    );
    assert_eq!(first_run_font(&e), "liberation");
    assert_eq!(default_face(&e, text_pipeline::Script::Latin), "liberation");
    assert_eq!(default_face(&e, text_pipeline::Script::Arabic), "amiri");
}

#[test]
fn arabic_text_keeps_amiri_when_it_is_the_declared_default() {
    let mut e = editor_engine("\u{645}\u{631}\u{62d}\u{628}\u{627}");
    let before = first_run_font(&e);
    set_priority(
        &mut e,
        &[
            (bridge::Script::Latin, &["liberation"]),
            (bridge::Script::Arabic, &["amiri", "noto-naskh"]),
        ],
    );
    assert_eq!(before, "amiri");
    assert_eq!(
        first_run_font(&e),
        before,
        "the Arabic default does not move"
    );
}

#[test]
fn the_effective_reply_drops_unloaded_and_non_covering_ids() {
    let mut e = editor_engine("x");
    let evt = set_priority(
        &mut e,
        &[
            (bridge::Script::Latin, &["not-loaded", "liberation"]),
            /* Liberation Sans has no Arabic: nothing effective. */
            (bridge::Script::Arabic, &["liberation"]),
        ],
    );
    let Event::FontPriorityApplied { priorities } = evt else {
        panic!("expected FontPriorityApplied, got {evt:?}");
    };
    assert_eq!(priorities.len(), 1);
    assert_eq!(priorities[0].ids, ["liberation"]);
}

#[test]
fn each_call_replaces_the_table_and_recovery_clears_it() {
    let mut e = editor_engine("Hello");
    set_priority(&mut e, &[(bridge::Script::Latin, &["liberation"])]);
    assert_eq!(first_run_font(&e), "liberation");
    set_priority(&mut e, &[]);
    assert_eq!(
        first_run_font(&e),
        "amiri",
        "an empty table declares nothing"
    );
    set_priority(&mut e, &[(bridge::Script::Latin, &["liberation"])]);
    e.reset_session_state();
    assert!(e.font_priority.is_empty(), "recovery wipes the font setup");
}

#[test]
fn the_priority_is_not_document_state() {
    /* Font setup like `LoadFont`: no undo step, no document change, no
    selection move (the worker's clean-state fold reads `mutates_doc`). */
    let mut e = editor_engine("Hello");
    let depth = e.undo.depth();
    set_priority(&mut e, &[(bridge::Script::Latin, &["liberation"])]);
    assert_eq!(e.undo.depth(), depth);
    let meta = bridge::CommandKind::SetFontPriority.meta();
    assert!(!meta.mutates_doc && !meta.moves_selection);
}
