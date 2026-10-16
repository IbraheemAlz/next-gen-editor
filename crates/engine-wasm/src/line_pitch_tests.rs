//! Issues #475 / #463 — ONE line-pitch model for every document.
//!
//! Every document — read from a Word package or built in the editor (the
//! seeded page, `.txt` / `.html` opens, the `RenderPage` harness) — is laid
//! out with Word's font-derived pitch (`layout::FontLinePitch`,
//! `text_pipeline::LineMetrics`, Windows Word's rule): a line is as tall as
//! the faces actually on it say, times the `auto` multiple, floored by
//! `atLeast` only. `exact` stays absolute.

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

/// A `TextAttrsPatch` that sets only the font size (pt). Only the
/// `fuzz-native` test below drives it (issue #501 item 4: dead code, hence a
/// warning, without the feature).
#[cfg(feature = "fuzz-native")]
fn size_patch(pt: f32) -> TextAttrsPatch {
    TextAttrsPatch {
        bold: None,
        italic: None,
        underline: None,
        strike: None,
        font_family: None,
        font_size: Some(pt),
        color: None,
        bg_color: None,
        script: None,
        language: None,
        caps: None,
        small_caps: None,
        font_slot: None,
    }
}

/// Issue #475 acceptance — type a document in the editor, `SaveDocx` it
/// and re-open the package: the geometry is identical, bit for bit
/// (`layout::geometry_fingerprint`). Before #475 the re-opened document
/// switched to Word's font pitch (its envelope was now captured) while the
/// typed one kept the configured 26 px pitch, so page breaks and the caret
/// moved on reopen.
#[cfg(feature = "fuzz-native")]
#[test]
fn a_typed_document_reopens_from_its_docx_with_identical_geometry() {
    let mut engine = Engine::new_headless(DocumentTree::from_text(""));
    let ok = |engine: &mut Engine, cmd: Command| {
        let evt = engine.apply_sync(cmd);
        assert!(!matches!(evt, Event::Error { .. }), "{evt:?}");
    };
    /* A few dozen paragraphs so the document breaks across pages: plain
    lines, a long wrapping one, a soft break, an empty paragraph, a run
    set smaller and one larger than the default, double spacing. */
    for i in 0..40 {
        let text = match i % 5 {
            0 => format!("Paragraph {i} with a few words of body text."),
            1 => format!(
                "Paragraph {i} is long enough to wrap: {}",
                "the quick brown fox jumps over the lazy dog ".repeat(4)
            ),
            2 => format!("Paragraph {i} first line\u{2028}and its second line"),
            3 => String::new(),
            _ => format!("Paragraph {i} closes the group."),
        };
        if !text.is_empty() {
            ok(
                &mut engine,
                Command::InsertText {
                    at: None,
                    text: text.clone(),
                },
            );
        }
        let para = engine.undo.current().blocks.len() as u32 - 1;
        if i % 10 == 4 {
            ok(
                &mut engine,
                Command::ApplyFormatting {
                    range: Some(BridgeLogicalRange {
                        start: bpos_top(para, 0),
                        end: bpos_top(para, 9),
                    }),
                    attrs: size_patch(8.0),
                },
            );
        }
        if i % 10 == 9 {
            ok(
                &mut engine,
                Command::ApplyFormatting {
                    range: Some(BridgeLogicalRange {
                        start: bpos_top(para, 0),
                        end: bpos_top(para, text.len() as u32),
                    }),
                    attrs: size_patch(20.0),
                },
            );
        }
        if i % 10 == 0 {
            ok(
                &mut engine,
                Command::SetLineSpacing {
                    range: BridgeLogicalRange {
                        start: bpos_top(para, 0),
                        end: bpos_top(para, 0),
                    },
                    multiplier: 2.0,
                },
            );
        }
        let end = bpos_top(para, text.len() as u32);
        ok(
            &mut engine,
            Command::SetSelection {
                range: BridgeLogicalRange {
                    start: end.clone(),
                    end: end.clone(),
                },
                caret: end.clone(),
            },
        );
        ok(&mut engine, Command::SplitParagraph { at: None });
    }
    let (typed, ..) = engine.build_pages(1.0, false, None).expect("layout");
    assert!(typed.len() > 1, "the fixture must break across pages");
    assert!(
        !engine.undo.current().document_envelope.is_captured(),
        "a typed document has no Word envelope"
    );

    let Event::DocumentSaved { bytes, .. } = engine.apply_sync(Command::SaveDocx) else {
        panic!("SaveDocx did not answer DocumentSaved");
    };
    let mut reopened = Engine::new_headless(DocumentTree::from_text("seed"));
    let evt = reopened.apply_sync(Command::OpenDocument {
        bytes,
        format: DocFormat::Docx,
        name: None,
        defaults: None,
        limits: None,
        password: None,
    });
    assert!(
        matches!(evt, Event::DocumentLoaded { .. }),
        "expected DocumentLoaded, got {evt:?}"
    );
    assert!(reopened.undo.current().document_envelope.is_captured());
    let (back, ..) = reopened.build_pages(1.0, false, None).expect("layout");
    assert_eq!(back.len(), typed.len(), "page count moved on reopen");
    assert_eq!(
        layout::geometry_fingerprint(&back),
        layout::geometry_fingerprint(&typed),
        "type -> SaveDocx -> reopen changed the geometry"
    );
}

/* ================================================================
Issue #463 — a line is as tall as the faces ON it (plus the paragraph
mark on the last line), never floored by the document default.
================================================================ */

/// Liberation Sans (the test face): single line = 2355 / 2048 em (win
/// ascent 1854 + external leading 67 + win descent 434).
fn single(pt: f32) -> f32 {
    pt * 2355.0 / 2048.0
}

fn sized(pt: f32) -> SpanStyle {
    SpanStyle {
        font_size: Some(pt),
        ..Default::default()
    }
}

/// A paragraph set entirely in `pt` — its text AND its mark, as Word
/// writes a paragraph formatted as a whole.
fn para_in(text: &str, pt: f32) -> engine::Paragraph {
    engine::Paragraph {
        text: text.into(),
        spans: vec![engine::StyleRun {
            start: 0,
            end: text.len() as u32,
            style: sized(pt),
        }],
        mark_style: Some(Box::new(sized(pt))),
        ..Default::default()
    }
}

/// A document whose default run (`docDefaults`) is 11 pt.
fn doc_11pt(paras: Vec<engine::Paragraph>) -> DocumentTree {
    let mut doc = DocumentTree::from_blocks(paras.into_iter().map(engine::Block::Paragraph));
    doc.style_run_defaults = sized(11.0);
    doc
}

fn page_one(e: &Engine) -> Vec<ParagraphBox> {
    let (pages, _, _, info) = e.build_pages(1.0, false, None).expect("layout");
    assert!(info.degradations.is_empty(), "{:?}", info.degradations);
    pages[0]
        .blocks
        .iter()
        .map(|b| b.as_paragraph().expect("paragraph").clone())
        .collect()
}

fn close(got: f32, want: f32) -> bool {
    (got - want).abs() < 1e-3
}

/// 8 / 10 / 14 pt paragraphs in a default-11 pt document: each line is
/// Word's single line of ITS size — the 8 pt one is no longer floored at
/// the 11 pt default (before #463 / #475 it was the configured 26 px).
/// Geometry pinned.
#[test]
fn small_and_large_paragraphs_take_their_own_line_height() {
    let doc = doc_11pt(vec![
        engine::Paragraph {
            text: "default".into(),
            ..Default::default()
        },
        para_in("eight", 8.0),
        para_in("ten", 10.0),
        para_in("fourteen", 14.0),
    ]);
    let e = tests::test_engine_with_doc(doc);
    let boxes = page_one(&e);
    for (b, pt) in boxes.iter().zip([11.0, 8.0, 10.0, 14.0]) {
        assert_eq!(b.lines.len(), 1);
        assert!(
            close(b.lines[0].height, single(pt)),
            "{pt} pt line: {} vs {}",
            b.lines[0].height,
            single(pt)
        );
    }
    let (pages, ..) = e.build_pages(1.0, false, None).expect("layout");
    let fp = layout::geometry_fingerprint(&pages);
    eprintln!("LINE PITCH FIXTURE FINGERPRINT 8/10/14 = {fp:#x}");
    assert_eq!(fp, PINNED_SMALL_AND_LARGE, "8/10/14 pt fixture moved");
}

/// Recorded on the #463 change via `--nocapture`.
const PINNED_SMALL_AND_LARGE: u64 = 0xe222ae71844870dc;

/// Word sizes the LAST line by the paragraph mark too: 8 pt text over
/// several lines with the default 11 pt mark keeps 8 pt lines except the
/// last, which the 11 pt pilcrow raises; a 20 pt mark raises it further.
#[test]
fn the_mark_sizes_the_last_line_only() {
    let text = "small text that wraps over several lines of the page because it \
                goes on and on and on for quite a while longer than one line can hold \
                at this width, so the paragraph has at least two lines";
    let run_only = |mark: Option<SpanStyle>| engine::Paragraph {
        text: text.into(),
        spans: vec![engine::StyleRun {
            start: 0,
            end: text.len() as u32,
            style: sized(8.0),
        }],
        mark_style: mark.map(Box::new),
        ..Default::default()
    };
    let doc = doc_11pt(vec![run_only(None), run_only(Some(sized(20.0)))]);
    let boxes = page_one(&tests::test_engine_with_doc(doc));
    for (b, mark_pt) in boxes.iter().zip([11.0, 20.0]) {
        let n = b.lines.len();
        assert!(n >= 2, "{n} lines");
        for l in &b.lines[..n - 1] {
            assert!(close(l.height, single(8.0)), "{}", l.height);
        }
        assert!(
            close(b.lines[n - 1].height, single(mark_pt)),
            "last line {} vs {}",
            b.lines[n - 1].height,
            single(mark_pt)
        );
        /* The extra space goes above the text: the baseline sits one
        (mark-size) descent above the bottom. */
        let descent = mark_pt * 434.0 / 2048.0;
        let last = &b.lines[n - 1];
        assert!(close(last.height - last.baseline, descent), "{last:?}");
    }
}

/// The #370 follow-up: an empty paragraph with an 8 pt mark is an 8 pt
/// line, and typing one character into it does NOT jump the line back
/// to the 11 pt default — the typed character takes the mark's
/// formatting and the mark keeps sizing the (now last) line.
#[test]
fn typing_into_a_small_mark_empty_paragraph_keeps_its_height() {
    let doc = doc_11pt(vec![
        engine::Paragraph {
            text: "Above".into(),
            ..Default::default()
        },
        engine::Paragraph {
            mark_style: Some(Box::new(sized(8.0))),
            ..Default::default()
        },
        engine::Paragraph {
            text: "Below".into(),
            ..Default::default()
        },
    ]);
    let mut e = tests::test_engine_with_doc(doc);
    let empty = page_one(&e)[1].lines[0].height;
    assert!(close(empty, single(8.0)), "{empty}");
    let at = bpos_top(1, 0);
    let evt = block_on(e.apply(Command::SetSelection {
        range: BridgeLogicalRange {
            start: at.clone(),
            end: at.clone(),
        },
        caret: at,
    }));
    assert!(!matches!(evt, Event::Error { .. }), "{evt:?}");
    let evt = block_on(e.apply(Command::InsertText {
        at: None,
        text: "x".into(),
    }));
    assert!(!matches!(evt, Event::Error { .. }), "{evt:?}");
    assert_eq!(e.undo.current().paragraph_text(1), Some("x"));
    let typed = page_one(&e)[1].lines[0].height;
    assert!(
        (typed - empty).abs() < 1e-4,
        "typing jumped the line: {empty} -> {typed}"
    );
}
