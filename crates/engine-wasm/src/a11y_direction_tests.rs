//! Issue #195 — every accessibility paragraph carries its OWN resolved base
//! direction (`A11yParagraph.resolved_direction`), from the same precedence
//! layout uses (`resolve_base_direction`): explicit `<w:bidi>` / paragraph
//! direction first, then UAX #9 first-strong auto-direction, then the
//! document base direction. The mirror writes `dir` from it per `<p>`, so
//! an RTL paragraph in an LTR document (and vice versa) reads right.

use super::*;
use bridge::HeaderFooterArea;

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

fn engine_with(doc: DocumentTree, base: ShapingDirection) -> Engine {
    let mut e = assemble_engine(None, None);
    let bytes = include_bytes!("../../../ts/fonts/LiberationSans-Regular.ttf").to_vec();
    let font = LoadedFont::parse("test-latin".to_string(), bytes).expect("parse test font");
    e.fonts.insert("test-latin".to_string(), Arc::new(font));
    e.layout_cfg = Some(RenderConfig {
        font_id: "test-latin".to_string(),
        base_direction: base,
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

fn para(text: &str, explicit: Option<engine::TextDirection>) -> engine::Block {
    let mut p = engine::Paragraph {
        text: text.to_owned(),
        ..Default::default()
    };
    p.props.direction = explicit;
    engine::Block::Paragraph(p)
}

fn doc_of(blocks: Vec<engine::Block>) -> DocumentTree {
    let mut d = DocumentTree::new();
    d.blocks = blocks.into_iter().collect();
    d
}

const ARABIC: &str = "مرحبا بالعالم";

/// The mixed-direction fixture: LTR Latin, auto-RTL Arabic, explicit-RTL
/// Latin, explicit-LTR Arabic, neutral digits (→ document base).
fn mixed_doc() -> DocumentTree {
    use engine::TextDirection::{Ltr, Rtl};
    doc_of(vec![
        para("Hello world", None),
        para(ARABIC, None),
        para("Explicit right to left", Some(Rtl)),
        para(ARABIC, Some(Ltr)),
        para("12345", None),
    ])
}

fn paragraph_dirs(nodes: &[A11yNode]) -> Vec<(Direction, Direction)> {
    nodes
        .iter()
        .filter_map(|n| match n {
            A11yNode::Paragraph(p) => Some((p.direction, p.resolved_direction)),
            _ => None,
        })
        .collect()
}

#[test]
fn each_paragraph_carries_its_resolved_direction_in_an_ltr_document() {
    let e = engine_with(mixed_doc(), ShapingDirection::Ltr);
    let dirs = paragraph_dirs(&e.build_a11y_nodes());
    let resolved: Vec<Direction> = dirs.iter().map(|d| d.1).collect();
    assert_eq!(
        resolved,
        vec![
            Direction::Ltr, // Latin, auto
            Direction::Rtl, // Arabic, auto (first strong)
            Direction::Rtl, // explicit bidi wins over Latin text
            Direction::Ltr, // explicit LTR wins over Arabic text
            Direction::Ltr, // neutral → document base
        ]
    );
    /* `direction` keeps its meaning: the document base direction. */
    assert!(dirs.iter().all(|d| d.0 == Direction::Ltr));
}

#[test]
fn neutral_paragraphs_follow_an_rtl_document_base() {
    let e = engine_with(mixed_doc(), ShapingDirection::Rtl);
    let resolved: Vec<Direction> = paragraph_dirs(&e.build_a11y_nodes())
        .iter()
        .map(|d| d.1)
        .collect();
    assert_eq!(
        resolved,
        vec![
            Direction::Ltr, // Latin first-strong beats the RTL base
            Direction::Rtl,
            Direction::Rtl,
            Direction::Ltr,
            Direction::Rtl, // neutral → document base
        ]
    );
}

/// The a11y resolution is the layout resolution — one function, no drift.
#[test]
fn a11y_direction_matches_the_layout_resolution() {
    let e = engine_with(mixed_doc(), ShapingDirection::Ltr);
    let cfg = e.layout_cfg.clone().unwrap();
    let doc = e.undo.current().clone();
    let nodes = e.build_a11y_nodes();
    for (block, node) in doc.blocks.iter().zip(nodes.iter()) {
        let (engine::Block::Paragraph(p), A11yNode::Paragraph(a)) = (block, node) else {
            panic!("fixture is paragraphs only");
        };
        let layout = match resolve_base_direction(p, &cfg) {
            ShapingDirection::Rtl => Direction::Rtl,
            ShapingDirection::Ltr => Direction::Ltr,
        };
        assert_eq!(a.resolved_direction, layout, "{:?}", p.text);
    }
}

/// Flipping one paragraph's direction is a fine-grained `Update` of that
/// paragraph alone — the resolved direction is part of the node.
#[test]
fn a_direction_flip_patches_only_that_paragraph() {
    let mut e = engine_with(mixed_doc(), ShapingDirection::Ltr);
    let _ = e.build_a11y_delta();
    apply(
        &mut e,
        Command::SetParagraphDirection {
            range: BridgeLogicalRange {
                start: bpos_top(0, 0),
                end: bpos_top(0, 0),
            },
            direction: Direction::Rtl,
        },
    );
    let patches = e.build_a11y_delta();
    let [A11yPatch::Update { index: 0, node }] = patches.as_slice() else {
        panic!("{patches:?}");
    };
    let A11yNode::Paragraph(p) = node else {
        panic!("{node:?}");
    };
    assert_eq!(p.resolved_direction, Direction::Rtl);
}

/// Region paragraphs (text box, header) resolve on their own too — they
/// inherit nothing from the paragraph that anchors / references them.
#[test]
fn region_paragraphs_resolve_independently() {
    let mut e = engine_with(
        doc_of(vec![para("Latin anchor", None)]),
        ShapingDirection::Ltr,
    );
    let evt = apply(
        &mut e,
        Command::InsertTextBox {
            at: bpos_top(0, 5),
            width_emu: 914_400,
            height_emu: 457_200,
        },
    );
    assert!(!matches!(evt, Event::Error { .. }), "{evt:?}");
    let evt = apply(
        &mut e,
        Command::InsertText {
            at: None,
            text: ARABIC.into(),
        },
    );
    assert!(!matches!(evt, Event::Error { .. }), "{evt:?}");
    apply(&mut e, Command::ExitHeaderFooter);
    apply(
        &mut e,
        Command::EnterHeaderFooter {
            page: 0,
            area: HeaderFooterArea::Header,
        },
    );
    let evt = apply(
        &mut e,
        Command::InsertText {
            at: None,
            text: ARABIC.into(),
        },
    );
    assert!(!matches!(evt, Event::Error { .. }), "{evt:?}");
    apply(&mut e, Command::ExitHeaderFooter);

    let nodes = e.build_a11y_nodes();
    let A11yNode::Paragraph(anchor) = &nodes[0] else {
        panic!("{:?}", nodes[0]);
    };
    assert_eq!(anchor.resolved_direction, Direction::Ltr);
    let text_box = nodes
        .iter()
        .find_map(|n| match n {
            A11yNode::TextBox(b) => Some(b),
            _ => None,
        })
        .expect("text box region");
    assert_eq!(
        paragraph_dirs(&text_box.nodes)
            .iter()
            .map(|d| d.1)
            .collect::<Vec<_>>(),
        vec![Direction::Rtl]
    );
    let header = nodes
        .iter()
        .find_map(|n| match n {
            A11yNode::Story(s) if s.header => Some(s),
            _ => None,
        })
        .expect("header region");
    assert!(
        paragraph_dirs(&header.nodes)
            .iter()
            .any(|d| d.1 == Direction::Rtl),
        "{header:?}"
    );
}

/* ---- Issue #202 — direction inherited from a paragraph style ---- */

/// A `.docx` whose `RtlBase` style sets `<w:bidi/>` and whose `RtlBody`
/// inherits it via `basedOn`. Paragraphs: (0) RtlBody, Latin-first
/// text; (1) RtlBody with a direct `<w:bidi w:val="0"/>` over Arabic-
/// first text; (2) unstyled Latin; (3) RtlBody, digits first.
fn style_bidi_docx() -> Vec<u8> {
    use engine::TextDirection::{Ltr, Rtl};
    let mut d = doc_of(vec![
        para("Word مرحبا", None),
        para("مرحبا Word", None),
        para("plain", None),
        para("2026 Word", None),
    ]);
    d.styles.insert(
        "RtlBase".into(),
        engine::ParagraphStyle {
            id: "RtlBase".into(),
            name: "RTL Base".into(),
            para: engine::ParaProperties {
                direction: Some(Rtl),
                ..Default::default()
            },
            ..Default::default()
        },
    );
    d.styles.insert(
        "RtlBody".into(),
        engine::ParagraphStyle {
            id: "RtlBody".into(),
            name: "RTL Body".into(),
            based_on: Some("RtlBase".into()),
            ..Default::default()
        },
    );
    d.styles_dirty = true;
    let styles = d.styles.clone();
    let defaults = d.style_defaults.clone();
    let mut blocks: Vec<engine::Block> = d.blocks.iter().cloned().collect();
    for (i, b) in blocks.iter_mut().enumerate() {
        let engine::Block::Paragraph(p) = b else {
            unreachable!()
        };
        if i != 2 {
            p.style_id = Some("RtlBody".into());
        }
        if i == 1 {
            p.direct_overrides.direction = Some(Ltr);
        }
        engine::recompute_paragraph_props(p, &styles, &defaults);
    }
    d.blocks = blocks.into_iter().collect();
    let bytes = format_docx::build_minimal_docx(&d).expect("build");
    /* The fixture itself must carry the direction on the style only. */
    let reread = format_docx::read_docx(&bytes).expect("reread");
    let p0 = reread.document.nth_paragraph(0).unwrap();
    assert_eq!(p0.style_id.as_deref(), Some("RtlBody"));
    assert_eq!(p0.direct_overrides.direction, None, "bidi is style-only");
    bytes
}

fn open_style_bidi(base: ShapingDirection) -> Engine {
    let mut e = engine_with(doc_of(vec![para("", None)]), base);
    let evt = apply(
        &mut e,
        Command::OpenDocument {
            bytes: style_bidi_docx(),
            format: DocFormat::Docx,
            name: None,
        },
    );
    assert!(matches!(evt, Event::DocumentLoaded { .. }), "{evt:?}");
    e
}

fn layout_dirs(e: &Engine) -> Vec<ShapingDirection> {
    let (pages, _, _, _) = e.build_pages(1.0, false, None).expect("layout");
    pages
        .iter()
        .flat_map(|p| p.blocks.iter())
        .filter_map(|b| b.as_paragraph().map(|p| p.direction))
        .collect()
}

#[test]
fn style_inherited_bidi_drives_layout_a11y_and_caret() {
    for base in [ShapingDirection::Ltr, ShapingDirection::Rtl] {
        let e = open_style_bidi(base);
        let expected = vec![
            ShapingDirection::Rtl, // style RTL beats the Latin first-strong
            ShapingDirection::Ltr, // direct w:val="0" beats the style
            ShapingDirection::Ltr, // unstyled Latin → first strong
            ShapingDirection::Rtl, // style RTL, digits first
        ];
        assert_eq!(layout_dirs(&e), expected, "layout, base {base:?}");
        let a11y: Vec<ShapingDirection> = paragraph_dirs(&e.build_a11y_nodes())
            .iter()
            .map(|d| match d.1 {
                Direction::Rtl => ShapingDirection::Rtl,
                Direction::Ltr => ShapingDirection::Ltr,
            })
            .collect();
        assert_eq!(a11y, expected, "a11y, base {base:?}");
        for (i, want) in expected.iter().enumerate() {
            let path = bpos_top(i as u32, 0).path;
            assert_eq!(e.paragraph_direction_at(&path), *want, "caret {i}");
        }
    }
}

/// Typing into / splitting a style-RTL paragraph keeps it RTL (the tail
/// inherits the style), and applying the style to a Latin paragraph
/// flips it RTL.
#[test]
fn style_inherited_bidi_survives_edits_and_apply_style() {
    let mut e = open_style_bidi(ShapingDirection::Ltr);
    let evt = apply(
        &mut e,
        Command::InsertText {
            at: Some(bpos_top(0, 0)),
            text: "A ".into(),
        },
    );
    assert!(!matches!(evt, Event::Error { .. }), "{evt:?}");
    let evt = apply(
        &mut e,
        Command::SplitParagraph {
            at: Some(bpos_top(0, 2)),
        },
    );
    assert!(!matches!(evt, Event::Error { .. }), "{evt:?}");
    let evt = apply(
        &mut e,
        Command::ApplyStyle {
            range: BridgeLogicalRange {
                start: bpos_top(3, 0),
                end: bpos_top(3, 0),
            },
            style_id: Some("RtlBody".into()),
        },
    );
    assert!(!matches!(evt, Event::Error { .. }), "{evt:?}");
    let dirs = layout_dirs(&e);
    assert_eq!(dirs[0], ShapingDirection::Rtl, "{dirs:?}");
    assert_eq!(dirs[1], ShapingDirection::Rtl, "split tail {dirs:?}");
    assert_eq!(dirs[3], ShapingDirection::Rtl, "restyled Latin {dirs:?}");
}
