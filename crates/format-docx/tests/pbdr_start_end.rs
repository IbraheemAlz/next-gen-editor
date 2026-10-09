//! Issue #352 — `<w:pBdr>` `<w:start>` / `<w:end>` (ISO 29500 logical
//! edges) map to the physical side by the paragraph's direction, and the
//! writer re-emits the spelling the source used.

use engine::{BorderSpelling, DocumentTree, TextDirection};
use format_docx::test_fixtures::paragraph_start_end_borders_docx;
use format_docx::{build_minimal_docx, read_docx, write_docx};
use std::io::Read;

fn part(docx: &[u8], name: &str) -> String {
    let mut a = zip::ZipArchive::new(std::io::Cursor::new(docx)).expect("zip");
    let mut s = String::new();
    a.by_name(name)
        .expect("entry")
        .read_to_string(&mut s)
        .expect("utf8");
    s
}

/// `(left stroke present, right stroke present, spelling)` of paragraph `n`.
fn edges(doc: &DocumentTree, n: u32) -> (bool, bool, BorderSpelling) {
    let p = doc.nth_paragraph(n).expect("paragraph");
    let b = p.props.borders.clone().expect("borders");
    (b.left.is_some(), b.right.is_some(), p.props.border_spelling)
}

#[test]
fn start_and_end_map_to_the_physical_side_by_direction() {
    let doc = read_docx(&paragraph_start_end_borders_docx())
        .expect("read")
        .document;
    let both = |s, e| BorderSpelling { start: s, end: e };
    /* LTR start → left. */
    assert_eq!(edges(&doc, 0), (true, false, both(true, false)));
    /* RTL start → right. */
    assert_eq!(
        doc.nth_paragraph(1).unwrap().props.direction,
        Some(TextDirection::Rtl)
    );
    assert_eq!(edges(&doc, 1), (false, true, both(true, false)));
    /* RTL end → left. */
    assert_eq!(edges(&doc, 2), (true, false, both(false, true)));
    /* Physical `w:left` is untouched and carries no logical spelling. */
    assert_eq!(edges(&doc, 3), (true, false, both(false, false)));
    /* The stroke itself came through: 24 eighths of a point, red. */
    let b = doc
        .nth_paragraph(1)
        .unwrap()
        .props
        .borders
        .clone()
        .unwrap()
        .right
        .unwrap();
    assert_eq!(b.size_eighth_pt, 24);
    assert_eq!(b.color, Some([255, 0, 0, 255]));
}

#[test]
fn zero_edit_save_is_byte_identical() {
    let src = paragraph_start_end_borders_docx();
    let archive = read_docx(&src).expect("read");
    let saved = write_docx(&archive, &archive.document).expect("write");
    assert_eq!(
        part(&saved, "word/document.xml"),
        part(&src, "word/document.xml")
    );
}

#[test]
fn a_regenerated_paragraph_keeps_the_source_spelling() {
    let doc = read_docx(&paragraph_start_end_borders_docx())
        .expect("read")
        .document;
    /* `build_minimal_docx` regenerates every pPr from the model. */
    let regenerated = build_minimal_docx(&doc).expect("minimal");
    let xml = part(&regenerated, "word/document.xml");
    let ppr = |i: usize| -> String {
        xml.split("<w:pPr>")
            .nth(i + 1)
            .and_then(|s| s.split("</w:pPr>").next())
            .unwrap_or_default()
            .to_string()
    };
    assert!(ppr(0).contains("<w:start "), "{}", ppr(0));
    assert!(!ppr(0).contains("<w:left"), "{}", ppr(0));
    /* RTL start sits in the right slot but keeps the logical name. */
    assert!(ppr(1).contains("<w:start "), "{}", ppr(1));
    assert!(!ppr(1).contains("<w:right"), "{}", ppr(1));
    assert!(ppr(2).contains("<w:end "), "{}", ppr(2));
    assert!(!ppr(2).contains("<w:left"), "{}", ppr(2));
    /* The legacy physical spelling stays physical. */
    assert!(ppr(3).contains("<w:left "), "{}", ppr(3));
    assert!(!ppr(3).contains("<w:start"), "{}", ppr(3));

    /* …and the regenerated file reads back to the same model. */
    let back = read_docx(&regenerated).expect("re-read").document;
    for n in 0..4 {
        assert_eq!(edges(&back, n), edges(&doc, n), "paragraph {n}");
    }
}

#[test]
fn schema_order_is_by_written_name_not_slot() {
    /* An RTL paragraph with BOTH a start and a bottom edge: `start` (the
    right slot) must still precede `bottom` in the CT_PBdr sequence. */
    let mut doc = read_docx(&paragraph_start_end_borders_docx())
        .expect("read")
        .document;
    let mut p = doc.nth_paragraph(1).unwrap().clone();
    let b = p.props.borders.as_mut().unwrap();
    b.bottom = b.right.clone();
    p.dirty = true;
    doc.blocks.set(1, engine::Block::Paragraph(p));
    let xml = part(
        &build_minimal_docx(&doc).expect("minimal"),
        "word/document.xml",
    );
    let start = xml.find("<w:start ").expect("start edge");
    let bottom = xml.find("<w:bottom ").expect("bottom edge");
    assert!(start < bottom, "{xml}");
}
