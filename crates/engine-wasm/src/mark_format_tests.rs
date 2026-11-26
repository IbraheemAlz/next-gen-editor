//! Issue #489 — formatting a whole paragraph reaches its paragraph MARK.
//!
//! Word's pilcrow is part of a selection that runs past the paragraph's
//! last character, and of Select All, a triple-click paragraph selection
//! and a cell selection; character formatting then applies to the mark
//! too. The mark's face sizes the paragraph's LAST line (issues #463 /
//! #475, `mark_line_metrics`), so "select all -> 8 pt" used to leave
//! every last line at the old pitch. A selection that stops at the last
//! character does not reach the mark.
//!
//! The tests drive real `Command`s through `Engine::apply` (the selection
//! commands included — the engine, not the range, knows a Select All) and
//! read the laid-out line heights.

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

const TEXTS: [&str; 3] = ["alpha", "beta", "gamma"];

/// `["alpha", "beta", "gamma"]`, unformatted (the layout default is the
/// config's 16 px).
fn three() -> DocumentTree {
    let mut doc = DocumentTree::new();
    doc.blocks.clear();
    for t in TEXTS {
        doc.blocks
            .push_back(engine::Block::Paragraph(engine::Paragraph {
                text: t.into(),
                ..Default::default()
            }));
    }
    doc
}

/// Liberation Sans (the test face): single line = 2355 / 2048 em (win
/// ascent 1854 + external leading 67 + win descent 434).
fn single(px: f32) -> f32 {
    px * 2355.0 / 2048.0
}

const DEFAULT: f32 = 16.0;
const SMALL: f32 = 8.0;

fn size_patch(size: f32) -> TextAttrsPatch {
    TextAttrsPatch {
        bold: None,
        italic: None,
        underline: None,
        strike: None,
        font_family: None,
        font_size: Some(size),
        color: None,
        bg_color: None,
        script: None,
        language: None,
        caps: None,
        small_caps: None,
        font_slot: None,
    }
}

/// `ApplyFormatting` over the LIVE selection (`range: None`), as the
/// toolbar sends it.
fn format_selection(e: &mut Engine, attrs: TextAttrsPatch) {
    apply(e, Command::ApplyFormatting { range: None, attrs });
}

fn select(e: &mut Engine, start: BridgeLogicalPos, end: BridgeLogicalPos) {
    apply(
        e,
        Command::SetSelection {
            range: BridgeLogicalRange {
                start,
                end: end.clone(),
            },
            caret: end,
        },
    );
}

fn end_of(block: u32) -> BridgeLogicalPos {
    bpos_top(block, TEXTS[block as usize].len() as u32)
}

/// The height of every body paragraph's LAST line (each fixture paragraph
/// is one line, so this is its only one).
fn line_heights(e: &Engine) -> Vec<f32> {
    let (pages, _, _, info) = e.build_pages(1.0, false, None).expect("layout");
    assert!(info.degradations.is_empty(), "{:?}", info.degradations);
    pages[0]
        .blocks
        .iter()
        .map(|b| {
            let p = b.as_paragraph().expect("paragraph");
            assert_eq!(p.lines.len(), 1, "one line per fixture paragraph");
            p.lines[0].height
        })
        .collect()
}

fn mark_sizes(e: &Engine) -> Vec<Option<f32>> {
    (0..TEXTS.len() as u32)
        .map(|i| {
            e.undo
                .current()
                .nth_paragraph(i)
                .expect("paragraph")
                .mark_style
                .as_deref()
                .and_then(|m| m.font_size)
        })
        .collect()
}

fn text_sizes(e: &Engine) -> Vec<Option<f32>> {
    (0..TEXTS.len() as u32)
        .map(|i| {
            e.undo
                .current()
                .nth_paragraph(i)
                .expect("paragraph")
                .style_at(0)
                .font_size
        })
        .collect()
}

fn assert_heights(got: &[f32], want: &[f32]) {
    assert_eq!(got.len(), want.len());
    for (i, (g, w)) in got.iter().zip(want).enumerate() {
        assert!((g - w).abs() < 1e-3, "line {i}: {g} vs {w} ({got:?})");
    }
}

/// The geometry of the same document AUTHORED at `size`: every run and
/// every mark (what Word writes for a document formatted as a whole).
fn authored_fingerprint(size: f32) -> u64 {
    let sized = engine::SpanStyle {
        font_size: Some(size),
        ..Default::default()
    };
    let mut doc = three();
    for b in doc.blocks.iter_mut() {
        if let engine::Block::Paragraph(p) = b {
            p.spans = vec![engine::StyleRun {
                start: 0,
                end: p.text.len() as u32,
                style: sized.clone(),
            }];
            p.mark_style = Some(Box::new(sized.clone()));
        }
    }
    let e = tests::test_engine_with_doc(doc);
    let (pages, ..) = e.build_pages(1.0, false, None).expect("layout");
    layout::geometry_fingerprint(&pages)
}

fn fingerprint(e: &Engine) -> u64 {
    let (pages, ..) = e.build_pages(1.0, false, None).expect("layout");
    layout::geometry_fingerprint(&pages)
}

/// Select All -> 8: every mark (the last one included) takes 8, so every
/// line shrinks to an 8 px line — the geometry of the document authored
/// at 8 throughout. Before #489 each (single-line) paragraph kept the
/// 16 px pitch of its unformatted mark.
#[test]
fn select_all_formats_every_mark_and_every_line_shrinks() {
    let mut e = tests::test_engine_with_doc(three());
    assert_heights(&line_heights(&e), &[single(DEFAULT); 3]);
    apply(&mut e, Command::SelectAll);
    format_selection(&mut e, size_patch(SMALL));
    assert_eq!(text_sizes(&e), vec![Some(SMALL); 3]);
    assert_eq!(mark_sizes(&e), vec![Some(SMALL); 3]);
    assert_heights(&line_heights(&e), &[single(SMALL); 3]);
    assert_eq!(
        fingerprint(&e),
        authored_fingerprint(SMALL),
        "select all -> 8 must lay out like a document authored at 8"
    );
}

/// The selection runs past the paragraph end into the next paragraph:
/// the crossed mark takes the formatting, the paragraph the selection
/// stops in keeps its mark.
#[test]
fn a_selection_into_the_next_paragraph_formats_the_crossed_mark() {
    let mut e = tests::test_engine_with_doc(three());
    select(&mut e, bpos_top(0, 0), bpos_top(1, 0));
    format_selection(&mut e, size_patch(SMALL));
    assert_eq!(mark_sizes(&e), vec![Some(SMALL), None, None]);
    assert_heights(
        &line_heights(&e),
        &[single(SMALL), single(DEFAULT), single(DEFAULT)],
    );
}

/// A selection that stops at the last character — of one paragraph, or
/// the last paragraph of a longer one — does NOT reach that mark: the
/// text is 8, the line keeps the mark's 16 px pitch (Word).
#[test]
fn a_selection_that_stops_at_the_last_character_leaves_the_mark() {
    let mut e = tests::test_engine_with_doc(three());
    select(&mut e, bpos_top(1, 0), end_of(1));
    format_selection(&mut e, size_patch(SMALL));
    assert_eq!(text_sizes(&e), vec![None, Some(SMALL), None]);
    assert_eq!(mark_sizes(&e), vec![None, None, None]);
    assert_heights(&line_heights(&e), &[single(DEFAULT); 3]);

    let mut e = tests::test_engine_with_doc(three());
    select(&mut e, bpos_top(0, 0), end_of(2));
    format_selection(&mut e, size_patch(SMALL));
    assert_eq!(text_sizes(&e), vec![Some(SMALL); 3]);
    assert_eq!(mark_sizes(&e), vec![Some(SMALL), Some(SMALL), None]);
    assert_heights(
        &line_heights(&e),
        &[single(SMALL), single(SMALL), single(DEFAULT)],
    );
}

/// A triple-click selects the paragraph WITH its mark.
#[test]
fn a_triple_click_paragraph_selection_formats_the_mark() {
    let mut e = tests::test_engine_with_doc(three());
    let geom = e.document_geometry().expect("geometry");
    let line = geom
        .iter()
        .find(|l| l.path == bpos_top(1, 0).path)
        .expect("paragraph 1's line");
    let at = BridgePoint {
        x: line.hit_left + 2.0,
        y: line.y_top + line.height / 2.0,
    };
    match apply(&mut e, Command::SelectParagraphAt { at }) {
        Event::SelectionChanged { range, .. } => {
            assert_eq!((range.start, range.end), (bpos_top(1, 0), end_of(1)));
        }
        other => panic!("SelectParagraphAt: {other:?}"),
    }
    format_selection(&mut e, size_patch(SMALL));
    assert_eq!(mark_sizes(&e), vec![None, Some(SMALL), None]);
    assert_heights(
        &line_heights(&e),
        &[single(DEFAULT), single(SMALL), single(DEFAULT)],
    );
}

/// "This selection includes its mark" lives exactly as long as the
/// selection: it survives formatting commands that keep the selection
/// (bold, then a size, both reach the mark), and lapses once another
/// selection is made — re-selecting the same range by hand afterwards
/// stops at the last character like any other selection.
#[test]
fn the_mark_lapses_with_the_selection_that_included_it() {
    let mut e = tests::test_engine_with_doc(three());
    apply(&mut e, Command::SelectAll);
    format_selection(
        &mut e,
        TextAttrsPatch {
            bold: Some(true),
            ..size_patch(SMALL)
        },
    );
    format_selection(&mut e, size_patch(10.0));
    let last = e.undo.current().nth_paragraph(2).expect("last").clone();
    let mark = last.mark_style.as_deref().expect("mark formatted");
    assert_eq!((mark.bold, mark.font_size), (Some(true), Some(10.0)));

    let mut e = tests::test_engine_with_doc(three());
    apply(&mut e, Command::SelectAll);
    select(&mut e, bpos_top(0, 0), bpos_top(0, 1));
    select(&mut e, bpos_top(0, 0), end_of(2));
    format_selection(&mut e, size_patch(SMALL));
    assert_eq!(mark_sizes(&e), vec![Some(SMALL), Some(SMALL), None]);
}

/// Crash recovery: a snapshot taken between Select All and the format
/// (the worker's idle snapshot) carries "this selection includes its mark",
/// so the replayed `ApplyFormatting` reaches the marks exactly as the live
/// one did. A snapshot of any other selection carries nothing (its bytes
/// are what they were before #489).
#[test]
fn a_recovery_replays_the_format_onto_the_marks() {
    let mut live = tests::test_engine_with_doc(three());
    apply(&mut live, Command::SelectAll);
    let snapshot = live.snapshot_bytes().expect("snapshot");
    let tail = Command::ApplyFormatting {
        range: None,
        attrs: size_patch(SMALL),
    };
    apply(&mut live, tail.clone());
    assert_eq!(mark_sizes(&live), vec![Some(SMALL); 3]);

    let mut recovered = tests::test_engine_with_doc(DocumentTree::from_text(""));
    let evt = apply(
        &mut recovered,
        Command::Recover {
            snapshot,
            log_tail: vec![tail],
            renderer_downgrade: None,
            package: None,
        },
    );
    assert!(matches!(evt, Event::Recovered { .. }), "{evt:?}");
    assert_eq!(mark_sizes(&recovered), mark_sizes(&live));

    let mut plain = tests::test_engine_with_doc(three());
    select(&mut plain, bpos_top(0, 0), end_of(2));
    assert!(plain.capture_snapshot().mark_selection.is_none());
}

/// Review mode resolves the same targets: the marks take the size (the
/// lines shrink) and the text records its format change. (The mark's own
/// `<w:rPrChange>` is issue #372.)
#[test]
fn tracked_formatting_reaches_the_same_marks() {
    let mut e = tests::test_engine_with_doc(three());
    e.tracking_changes = true;
    apply(&mut e, Command::SelectAll);
    format_selection(&mut e, size_patch(SMALL));
    assert_eq!(mark_sizes(&e), vec![Some(SMALL); 3]);
    assert_heights(&line_heights(&e), &[single(SMALL); 3]);
    for i in 0..3 {
        let p = e.undo.current().nth_paragraph(i).expect("paragraph");
        assert!(
            p.revisions
                .iter()
                .any(|r| r.kind == engine::RevisionKind::FormatChange),
            "paragraph {i}: {:?}",
            p.revisions
        );
        assert!(p.mark_revisions.is_empty(), "no mark revision is invented");
    }
}

/// Select All -> 8, `SaveDocx`, reopen: every mark re-reads 8 (the writer
/// emits it in `<w:pPr><w:rPr>`) and the reopened document lays out
/// identically.
#[test]
fn the_formatted_marks_round_trip_through_a_save() {
    let mut e = tests::test_engine_with_doc(three());
    apply(&mut e, Command::SelectAll);
    format_selection(&mut e, size_patch(SMALL));
    let typed = fingerprint(&e);
    let Event::DocumentSaved { bytes, .. } = apply(&mut e, Command::SaveDocx) else {
        panic!("SaveDocx did not answer DocumentSaved");
    };
    let xml = {
        let archive = format_docx::read_docx(&bytes).expect("re-read");
        (0..3)
            .map(|i| {
                archive
                    .document
                    .nth_paragraph(i)
                    .and_then(|p| p.mark_style.as_deref())
                    .and_then(|m| m.font_size)
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(xml, vec![Some(SMALL); 3], "the marks did not re-read 8");
    let mut reopened = tests::test_engine_with_doc(DocumentTree::from_text("seed"));
    let evt = apply(
        &mut reopened,
        Command::OpenDocument {
            bytes,
            format: DocFormat::Docx,
            name: None,
            defaults: None,
            limits: None,
            password: None,
        },
    );
    assert!(matches!(evt, Event::DocumentLoaded { .. }), "{evt:?}");
    assert_eq!(mark_sizes(&reopened), vec![Some(SMALL); 3]);
    assert_heights(&line_heights(&reopened), &[single(SMALL); 3]);
    assert_eq!(
        fingerprint(&reopened),
        typed,
        "save + reopen moved the geometry"
    );
}
