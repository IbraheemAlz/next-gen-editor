//! Issues #475 / #463 — ONE line-pitch model for every document.
//!
//! Every document — read from a Word package or built in the editor (the
//! seeded page, `.txt` / `.html` opens, the `RenderPage` harness) — is laid
//! out with Word's font-derived pitch (`layout::FontLinePitch`,
//! `text_pipeline::LineMetrics`, Windows Word's rule): a line is as tall as
//! the faces actually on it say, times the `auto` multiple, floored by
//! `atLeast` only. `exact` stays absolute.

use super::*;

/// A `TextAttrsPatch` that sets only the font size (pt).
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
