//! Issue #575 — a legacy `.doc`'s style sheet and font table open through
//! the real dispatcher (`OpenDocument`) into the same structures the
//! `.docx` reader fills, so the ONE direction resolution
//! (`paragraph_base_direction` — layout, caret read-back, accessibility
//! mirror), the font-derived line pitch (#475), the font read-back
//! (`SelectionChanged.resolved_font_latin` through the substitution
//! table) and `ModifyStyle` work on it unchanged. The `.doc` is built in
//! memory (`format_doc::test_builder::styled_sample`): Normal is
//! right-to-left 12 pt Arial with 8 pt after, Heading 1 bold 16 pt, a
//! "Quote Char" character style, a paragraph made left-to-right directly.

use super::*;
use bridge::FontSource;
use format_doc::test_builder::{SAMPLE_TEXTS, build, styled_sample};

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

/// The test engine with the styled `.doc` opened through `OpenDocument`.
fn opened() -> Engine {
    let mut e = tests::test_engine_with_doc(DocumentTree::from_text(""));
    let evt = apply(
        &mut e,
        Command::OpenDocument {
            bytes: build(&styled_sample()),
            format: bridge::DocFormat::Docx,
            name: Some("styled.doc".into()),
            defaults: None,
            limits: None,
            password: None,
        },
    );
    let Event::DocumentLoaded { warnings, .. } = evt else {
        panic!("expected DocumentLoaded, got {evt:?}");
    };
    assert!(
        warnings.is_empty(),
        "styles are imported, nothing to report: {warnings:?}"
    );
    e
}

fn top(idx: u32) -> BridgeBlockPath {
    BridgeBlockPath {
        steps: vec![BridgePathStep::Block { idx }],
    }
}

/// Normal is right-to-left: the three paragraphs that do not say
/// otherwise — a Latin one among them — are RTL everywhere the editor
/// asks; the fourth is LTR by its own direct formatting.
const EXPECTED: [ShapingDirection; 4] = [
    ShapingDirection::Rtl,
    ShapingDirection::Rtl,
    ShapingDirection::Rtl,
    ShapingDirection::Ltr,
];

#[test]
fn normal_makes_the_unformatted_paragraphs_right_to_left() {
    let e = opened();
    let doc = e.undo.current();
    let dirs: Vec<ShapingDirection> = (0..4)
        .map(|i| paragraph_base_direction(doc.nth_paragraph(i).unwrap(), ShapingDirection::Ltr))
        .collect();
    assert_eq!(dirs, EXPECTED, "against the editor's LTR boot base");
    assert_eq!(
        document_base_direction(doc, ShapingDirection::Ltr),
        ShapingDirection::Rtl,
        "the document default: Normal"
    );
    let read_back: Vec<ShapingDirection> =
        (0..4).map(|i| e.paragraph_direction_at(&top(i))).collect();
    assert_eq!(read_back, EXPECTED, "the caret's read-back");
    let mirrored: Vec<Direction> = e
        .build_a11y_nodes()
        .iter()
        .filter_map(|n| match n {
            A11yNode::Paragraph(p) => Some(p.resolved_direction),
            _ => None,
        })
        .collect();
    assert_eq!(
        mirrored,
        vec![
            Direction::Rtl,
            Direction::Rtl,
            Direction::Rtl,
            Direction::Ltr
        ],
        "the accessibility mirror"
    );
    /* The character style rides its run (the mirror's italic). */
    let runs: Vec<(String, bool)> = e
        .build_a11y_nodes()
        .iter()
        .filter_map(|n| match n {
            A11yNode::Paragraph(p) if p.runs.iter().any(|r| r.text == "quoted") => Some(
                p.runs
                    .iter()
                    .map(|r| (r.text.clone(), r.italic))
                    .collect::<Vec<_>>(),
            ),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(
        runs,
        vec![
            ("Before ".to_string(), false),
            ("quoted".to_string(), true),
            (" after".to_string(), false)
        ]
    );
}

/// The styles lay the document out: every paragraph box is right-to-left
/// as resolved, the bold 16 pt heading's line is taller than Normal's
/// 12 pt one (font-derived pitch, #475), and the geometry is pinned.
#[test]
fn the_styles_lay_the_document_out() {
    let e = opened();
    let (pages, _, _, info) = e.build_pages(1.0, false, None).expect("layout");
    assert!(info.degradations.is_empty(), "{:?}", info.degradations);
    let boxes: Vec<&layout::ParagraphBox> = pages[0]
        .blocks
        .iter()
        .filter_map(|b| match b {
            LayoutBlock::Paragraph(p) => Some(p),
            LayoutBlock::Table(_) => None,
        })
        .collect();
    assert_eq!(boxes.len(), 4);
    assert_eq!(
        boxes.iter().map(|b| b.direction).collect::<Vec<_>>(),
        EXPECTED.to_vec()
    );
    let line = |i: usize| boxes[i].lines[0].height;
    assert!(
        line(1) > line(0),
        "heading 1 (16 pt) {} > Normal (12 pt) {}",
        line(1),
        line(0)
    );
    assert_eq!(
        format!("{:#018x}", layout::geometry_fingerprint(&pages)),
        PINNED_FINGERPRINT,
    );
}

/// The styled sample under its styles (issue #575).
const PINNED_FINGERPRINT: &str = "0xf5ed85d24d40ec56";

/// The caret in a Normal paragraph reads back Normal's Arial — served by
/// its metric clone (the test engine's Liberation Sans), exactly as a
/// `.docx` run naming Arial would.
#[test]
fn the_style_font_reads_back_through_the_substitution_table() {
    let mut e = opened();
    let at = bpos_top(0, 3);
    let evt = apply(
        &mut e,
        Command::SetSelection {
            range: BridgeLogicalRange {
                start: at.clone(),
                end: at.clone(),
            },
            caret: at,
        },
    );
    let Event::SelectionChanged {
        resolved_font_latin,
        resolved_font_cs,
        font_source,
        ..
    } = evt
    else {
        panic!("expected SelectionChanged, got {evt:?}");
    };
    assert_eq!(
        (resolved_font_latin.as_str(), resolved_font_cs.as_str()),
        ("Arial", "Simplified Arabic")
    );
    assert_eq!(font_source.latin, FontSource::Substituted);
    assert_eq!(font_source.complex_script, FontSource::Style);
    assert!(
        e.font_substitutions()
            .iter()
            .any(|s| s.family == "Arial" && s.slot == bridge::FontSlot::Latin)
    );
}

/// `ModifyStyle` on an imported style re-cascades its paragraphs, and a
/// save writes a `.docx` whose styles and paragraph styles read back.
#[test]
fn an_imported_style_is_modified_and_saved() {
    let mut e = opened();
    apply(
        &mut e,
        Command::ModifyStyle {
            style_id: "Heading1".into(),
            properties: bridge::BridgeStyleProperties {
                para_props: Some(bridge::BridgeParaPropertiesPatch {
                    alignment: Some(bridge::Alignment::Center),
                    ..Default::default()
                }),
                ..Default::default()
            },
        },
    );
    let doc = e.undo.current();
    assert_eq!(
        doc.nth_paragraph(1).unwrap().props.alignment,
        Some(engine::Alignment::Center)
    );
    assert_eq!(doc.nth_paragraph(0).unwrap().props.alignment, None);

    let Event::DocumentSaved { bytes, .. } = apply(
        &mut e,
        Command::SaveDocument {
            format: DocFormat::Docx,
        },
    ) else {
        panic!("the save answers DocumentSaved");
    };
    let saved = format_docx::read_docx(&bytes)
        .expect("the saved .docx reads")
        .document;
    let texts: Vec<String> = (0..4)
        .map(|i| saved.nth_paragraph(i).unwrap().text.clone())
        .collect();
    assert_eq!(texts, SAMPLE_TEXTS.to_vec());
    assert!(saved.styles["Normal"].is_default);
    assert_eq!(saved.styles["Heading1"].based_on.as_deref(), Some("Normal"));
    let h = saved.nth_paragraph(1).unwrap();
    assert_eq!(h.style_id.as_deref(), Some("Heading1"));
    assert_eq!(h.props.alignment, Some(engine::Alignment::Center));
    assert_eq!(
        saved.nth_paragraph(0).unwrap().props.direction,
        Some(engine::TextDirection::Rtl),
        "Normal's bidi survives the save"
    );
}
