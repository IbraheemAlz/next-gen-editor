//! Issue #558 — the DEFAULT CHARACTER style (`DefaultParagraphFont`, here
//! Liberation Serif, bold, 16 pt) formats every run that names no
//! character style: span materialization, the paragraph mark (an empty
//! paragraph's line) and the caret / toolbar read-back all go through the
//! one fold (`engine::StyleCascade::text_style`), between the paragraph
//! style's `<w:rPr>` and the run's own formatting
//! (`format_docx::test_fixtures::default_char_style_docx`).

use super::*;
use format_docx::test_fixtures::{DEFAULT_CHAR_TEXTS, default_char_style_docx};

fn fixture() -> DocumentTree {
    format_docx::read_docx(&default_char_style_docx())
        .expect("read")
        .document
}

/// `(bold, italic, px size)` of every layout span of paragraph `i`.
fn spans(doc: &DocumentTree, i: u32) -> Vec<(bool, bool, f32)> {
    let sctx = StyleContext::of(doc);
    let para = doc.nth_paragraph(i).expect("paragraph");
    build_style_spans(para, sctx, 12.0, [0, 0, 0, 255], 1.0)
        .iter()
        .map(|s| (s.bold, s.italic, s.px_size))
        .collect()
}

#[test]
fn runs_naming_no_character_style_lay_out_in_the_default_one() {
    let doc = fixture();
    assert_eq!(doc.nth_paragraph(5).unwrap().text, DEFAULT_CHAR_TEXTS[5]);
    assert_eq!(spans(&doc, 0), [(true, false, 16.0)], "the plain run");
    assert_eq!(
        spans(&doc, 1),
        [(false, true, 11.0)],
        "Quiet is not based on it: the docDefaults' 11 pt, not bold"
    );
    assert_eq!(spans(&doc, 2), [(true, false, 16.0)], "Loud is based on it");
    assert_eq!(
        spans(&doc, 3),
        [(false, false, 16.0)],
        "the direct b=0 wins"
    );
    assert_eq!(
        spans(&doc, 5),
        [(true, true, 16.0)],
        "above the Aside paragraph style's italic runs"
    );

    /* Without the default character style the plain run is the
    docDefaults' 11 pt regular text (what the reader gave it before). */
    let mut without = doc.clone();
    without.default_char_style = None;
    assert_eq!(spans(&without, 0), [(false, false, 11.0)]);
    assert_eq!(spans(&without, 1), spans(&doc, 1), "Quiet never took it");
}

#[test]
fn the_caret_reads_back_the_default_character_style() {
    let engine = crate::tests::test_engine_with_doc(fixture());
    let at = |i: u32| BridgeLogicalPos {
        path: BridgeBlockPath::top(i),
        offset: 3,
    };
    let read = |i: u32, collapsed: bool| {
        let c = engine.caret_style(&at(i), collapsed);
        let a = engine.text_attrs_of(&c);
        (a.bold, a.italic, a.font_size)
    };
    assert_eq!(
        read(0, true),
        (true, false, 16.0),
        "the toolbar shows bold 16"
    );
    assert_eq!(read(0, false), (true, false, 16.0));
    assert_eq!(read(1, true), (false, true, 11.0));
    assert_eq!(read(3, true), (false, false, 16.0));
    /* The empty paragraph types in it too (its mark is a run). */
    let empty = engine.caret_style(
        &BridgeLogicalPos {
            path: BridgeBlockPath::top(4),
            offset: 0,
        },
        true,
    );
    assert_eq!(engine.text_attrs_of(&empty).font_size, 16.0);
}

/// The fixture's geometry, the empty paragraph's 16 pt line included.
#[test]
fn the_default_character_style_moves_the_layout() {
    let layout = |doc: DocumentTree| {
        let engine = crate::tests::test_engine_with_doc(doc);
        let (pages, _, _, info) = engine.build_pages(1.0, false, None).expect("layout");
        assert!(info.degradations.is_empty(), "{:?}", info.degradations);
        let heights: Vec<f32> = pages[0]
            .blocks
            .iter()
            .filter_map(|b| match b {
                LayoutBlock::Paragraph(p) => Some(p.size.height),
                LayoutBlock::Table(_) => None,
            })
            .collect();
        (layout::geometry_fingerprint(&pages), heights)
    };
    let (fingerprint, heights) = layout(fixture());
    assert_eq!(format!("{fingerprint:#018x}"), PINNED_FINGERPRINT);
    let mut without = fixture();
    without.default_char_style = None;
    let (plain_fingerprint, plain_heights) = layout(without);
    assert_ne!(plain_fingerprint, fingerprint);
    assert_eq!(heights.len(), 6);
    /* The plain, direct, empty and Aside lines are 16 pt now; Quiet's
    TEXT stays 11 pt (see the span test) but its paragraph MARK names no
    character style, and the mark sits on the last line (issue #463). */
    for i in [0, 1, 3, 4, 5] {
        assert!(
            heights[i] > plain_heights[i],
            "paragraph {i}: a 16 pt line is taller than an 11 pt one: {heights:?} vs {plain_heights:?}"
        );
    }
    assert_eq!(
        heights[2], plain_heights[2],
        "Loud's own span already carries the chain it is based on"
    );
}

/// Issue #558 — the six fixture paragraphs in the default character style.
const PINNED_FINGERPRINT: &str = "0xe6e791a07556270b";
