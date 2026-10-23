//! Issue #556 — the DEFAULT paragraph style (`Normal`, `w:default="1"`)
//! formats every paragraph that names no style (or an undefined one):
//! its `<w:bidi/>` makes an EMPTY paragraph RTL through the one direction
//! resolution (`paragraph_base_direction` — layout, caret read-back and
//! the accessibility mirror alike), the document default
//! (`document_base_direction`) follows it, and its spacing and run size
//! move the layout (`format_docx::test_fixtures::default_style_rtl_docx`).

use super::*;
use format_docx::test_fixtures::{DEFAULT_STYLE_TEXTS, default_style_rtl_docx};

fn top(idx: u32) -> BridgeBlockPath {
    BridgeBlockPath {
        steps: vec![BridgePathStep::Block { idx }],
    }
}

fn fixture() -> DocumentTree {
    format_docx::read_docx(&default_style_rtl_docx())
        .expect("read")
        .document
}

/// Paragraph 1 is empty: RTL only through `Normal`. 4 is `Plain` (not
/// based on `Normal`, Latin text), 5 a direct `<w:bidi w:val="0"/>`.
const EXPECTED: [ShapingDirection; 6] = [
    ShapingDirection::Rtl,
    ShapingDirection::Rtl,
    ShapingDirection::Rtl,
    ShapingDirection::Rtl,
    ShapingDirection::Ltr,
    ShapingDirection::Ltr,
];

#[test]
fn the_empty_unstyled_paragraph_resolves_rtl_through_normal() {
    let doc = fixture();
    assert_eq!(doc.nth_paragraph(1).unwrap().text, DEFAULT_STYLE_TEXTS[1]);
    let dirs: Vec<ShapingDirection> = (0..6)
        .map(|i| paragraph_base_direction(doc.nth_paragraph(i).unwrap(), ShapingDirection::Ltr))
        .collect();
    assert_eq!(dirs, EXPECTED, "against the editor's LTR boot base");
    assert_eq!(
        document_base_direction(&doc, ShapingDirection::Ltr),
        ShapingDirection::Rtl,
        "the document default: docDefaults + Normal"
    );

    let engine = crate::tests::test_engine_with_doc(doc);
    let read_back: Vec<ShapingDirection> = (0..6)
        .map(|i| engine.paragraph_direction_at(&top(i)))
        .collect();
    assert_eq!(read_back, EXPECTED, "the caret's read-back");
    let mirrored: Vec<Direction> = engine
        .build_a11y_nodes()
        .iter()
        .filter_map(|n| match n {
            A11yNode::Paragraph(p) => Some(p.resolved_direction),
            _ => None,
        })
        .collect();
    let expected_mirror: Vec<Direction> = EXPECTED
        .iter()
        .map(|d| match d {
            ShapingDirection::Rtl => Direction::Rtl,
            ShapingDirection::Ltr => Direction::Ltr,
        })
        .collect();
    assert_eq!(mirrored, expected_mirror, "the accessibility mirror");
}

/// Without the default claim the same document is what the reader
/// produced before #556: the empty paragraph falls to the LTR base.
#[test]
fn without_the_default_claim_the_empty_paragraph_is_ltr() {
    let mut doc = fixture();
    let mut styles = doc.styles.clone();
    styles.get_mut("Normal").expect("Normal").is_default = false;
    doc.styles = styles;
    assert_eq!(
        document_base_direction(&doc, ShapingDirection::Ltr),
        ShapingDirection::Ltr
    );
    let mut empty = doc.nth_paragraph(1).unwrap().clone();
    doc.style_cascade()
        .recompute(&mut empty, &doc.style_defaults);
    assert_eq!(
        paragraph_base_direction(&empty, ShapingDirection::Ltr),
        ShapingDirection::Ltr
    );
}

/// `Normal`'s spacing (6 pt before / 12 pt after, 1.5 lines) and run size
/// (14 pt) lay the unstyled paragraphs out; the geometry is pinned.
#[test]
fn normal_formats_the_layout_of_unstyled_paragraphs() {
    let engine = crate::tests::test_engine_with_doc(fixture());
    let (pages, _, _, info) = engine.build_pages(1.0, false, None).expect("layout");
    assert!(info.degradations.is_empty(), "{:?}", info.degradations);
    assert_eq!(
        format!("{:#018x}", layout::geometry_fingerprint(&pages)),
        PINNED_FINGERPRINT,
    );
    let boxes: Vec<(f32, f32)> = pages[0]
        .blocks
        .iter()
        .filter_map(|b| match b {
            LayoutBlock::Paragraph(p) => Some((p.origin.y, p.size.height)),
            LayoutBlock::Table(_) => None,
        })
        .collect();
    assert_eq!(boxes.len(), 6);
    /* Paragraphs 2 → 3 (both Normal) are separated by at least Normal's
    12 pt after-spacing (the docDefaults' 8 pt before #556), whatever
    the paragraph box includes. */
    let gap = boxes[3].0 - (boxes[2].0 + boxes[2].1);
    let gap_plain = boxes[5].0 - (boxes[4].0 + boxes[4].1);
    assert!(
        gap > gap_plain,
        "Normal's 12 pt after > Plain's 8 pt docDefaults after: {boxes:?}"
    );
}

/// Issue #556 — the six fixture paragraphs under `Normal`'s formatting.
const PINNED_FINGERPRINT: &str = "0x0099cc2dce124f3f";
