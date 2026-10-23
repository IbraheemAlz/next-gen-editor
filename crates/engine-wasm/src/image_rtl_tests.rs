//! Issue #554 - `ImageRect.rtl`: whether the picture's paragraph runs
//! right-to-left, so the shell can turn Left / Right on a selected picture
//! into the caret at its VISUAL left / right edge (in an RTL paragraph the
//! sentinel's byte offset `at` is the right edge and `at + 3` the left).
//! Same precedence as layout and the accessibility tree (`<w:bidi>` first,
//! then the document base direction).

use super::*;

fn blob() -> engine::ImageBlob {
    engine::ImageBlob {
        content_type: "image/png".to_string(),
        data: vec![0x89, b'P', b'N', b'G'],
    }
}

fn engine_with(doc: DocumentTree, base: ShapingDirection) -> Engine {
    let mut e = tests::test_engine_with_doc(doc);
    if let Some(cfg) = e.layout_cfg.as_mut() {
        cfg.base_direction = base;
    }
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

/// Two paragraphs, a picture at the start of each.
fn two_pictures(first: Option<engine::TextDirection>, second: Option<engine::TextDirection>) -> DocumentTree {
    let mut doc = DocumentTree::new();
    doc.blocks = vec![para("12345", first), para("67890", second)].into();
    for idx in 0..2 {
        doc = doc.insert_inline_image_at(
            engine::LogicalPos {
                path: EngineBlockPath::top(idx),
                offset: 0,
            },
            blob(),
            914_400,
            914_400,
        );
    }
    doc
}

fn rtl_flags(e: &Engine) -> Vec<bool> {
    let mut rects = e.image_geometry().expect("image geometry");
    rects.sort_by_key(|r| (r.rect.y * 10.0) as i64);
    rects.iter().map(|r| r.rtl).collect()
}

#[test]
fn a_picture_in_an_explicit_rtl_paragraph_reports_rtl() {
    use engine::TextDirection::{Ltr, Rtl};
    let e = engine_with(two_pictures(Some(Ltr), Some(Rtl)), ShapingDirection::Ltr);
    assert_eq!(rtl_flags(&e), vec![false, true]);
}

#[test]
fn a_paragraph_without_a_direction_takes_the_document_base() {
    let ltr = engine_with(two_pictures(None, None), ShapingDirection::Ltr);
    assert_eq!(rtl_flags(&ltr), vec![false, false]);
    let rtl = engine_with(two_pictures(None, None), ShapingDirection::Rtl);
    assert_eq!(rtl_flags(&rtl), vec![true, true]);
}

#[test]
fn an_explicit_ltr_paragraph_stays_ltr_in_an_rtl_document() {
    use engine::TextDirection::{Ltr, Rtl};
    let e = engine_with(two_pictures(Some(Ltr), Some(Rtl)), ShapingDirection::Rtl);
    assert_eq!(rtl_flags(&e), vec![false, true]);
}
