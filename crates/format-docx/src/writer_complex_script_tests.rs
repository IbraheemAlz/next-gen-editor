//! Issues #359 / #104 / #249 — complex-script run properties through a
//! read → edit → write cycle: each twin (`<w:szCs>`, `<w:bCs>`, `<w:iCs>`,
//! `<w:rFonts w:cs>`) is read into its own slot and written from it, the
//! run's `<w:rStyle>` survives a regeneration, and a regenerated run never
//! drops what its source said nor synthesizes what it did not.

use super::tests::{build_docx_with_styles, document_xml_of};
use super::*;
use crate::opc::archive::read_docx;
use engine::{BlockPath, LogicalPos};

const STYLES_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:style w:type="character" w:styleId="Emph"><w:name w:val="Emphasis"/><w:rPr><w:color w:val="1F4E79"/></w:rPr></w:style></w:styles>"#;

fn document(body: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}<w:sectPr/></w:body></w:document>"#
    )
}

fn open(body: &str) -> (String, DocxArchive) {
    let xml = document(body);
    let archive = read_docx(&build_docx_with_styles(STYLES_XML, &xml)).expect("read fixture");
    (xml, archive)
}

fn at(block: u32, offset: usize) -> LogicalPos {
    LogicalPos {
        path: BlockPath::top(block),
        offset: offset as u32,
    }
}

fn bold(on: bool) -> SpanStyle {
    SpanStyle {
        bold: Some(on),
        ..SpanStyle::default()
    }
}

const ARABIC: &str = "نص عربي";

/// Issue #104 acceptance — bolding an Arabic run through the UI (both
/// slots, `with_cs_twins`) writes `<w:b/>` AND `<w:bCs/>`, so Word shows
/// the Arabic bold; un-bolding clears both; each re-reads into its slot.
#[test]
fn arabic_bold_toggle_round_trips_b_and_bcs() {
    let body = format!(
        r#"<w:p><w:pPr><w:bidi/></w:pPr><w:r><w:rPr><w:rtl/></w:rPr><w:t>{ARABIC}</w:t></w:r></w:p>"#
    );
    let (_, archive) = open(&body);
    let len = ARABIC.len();
    let bolded = archive
        .document
        .apply_style(at(0, 0), at(0, len), bold(true).with_cs_twins());
    let xml = document_xml_of(&write_docx(&archive, &bolded).expect("write"));
    assert!(
        xml.contains("<w:rPr><w:b/><w:bCs/><w:rtl/></w:rPr>"),
        "{xml}"
    );
    let back = read_docx(&write_docx(&archive, &bolded).unwrap()).unwrap();
    let s = back.document.nth_paragraph(0).unwrap().style_at(0);
    assert_eq!((s.bold, s.bold_cs), (Some(true), Some(true)));

    let plain = bolded.apply_style(at(0, 0), at(0, len), bold(false).with_cs_twins());
    let xml = document_xml_of(&write_docx(&archive, &plain).expect("write"));
    assert!(
        !xml.contains("<w:b/>") && !xml.contains("<w:bCs/>"),
        "{xml}"
    );
}

/// The twins read into their own slots: `<w:bCs/>` alone bolds only
/// complex-script text (rtl.docx's shape), `<w:iCs w:val="0"/>` is an
/// explicit "not italic" for it; neither rides the grab bag any more.
#[test]
fn bcs_and_ics_read_into_their_own_slots() {
    let body = format!(
        r#"<w:p><w:r><w:rPr><w:bCs/><w:i/><w:iCs w:val="0"/></w:rPr><w:t>{ARABIC}</w:t></w:r></w:p>"#
    );
    let (xml, archive) = open(&body);
    let s = archive.document.nth_paragraph(0).unwrap().style_at(0);
    assert_eq!((s.bold, s.bold_cs), (None, Some(true)));
    assert_eq!((s.italic, s.italic_cs), (Some(true), Some(false)));
    assert!(s.grab_bag.is_none(), "modeled twins left the bag");
    /* Zero-edit: verified reuse keeps the source bytes. */
    let resaved = document_xml_of(&write_docx(&archive, &archive.document).unwrap());
    assert_eq!(resaved, xml);
}

/// Issue #104 acceptance — `<w:rStyle>` survives a regeneration: an edit
/// that changes the run's formatting writes the character-style reference
/// back (its folded properties follow as direct formatting, as before).
#[test]
fn rstyle_survives_an_edit() {
    let body =
        r#"<w:p><w:r><w:rPr><w:rStyle w:val="Emph"/></w:rPr><w:t>emphasis here</w:t></w:r></w:p>"#;
    let (_, archive) = open(body);
    let s = archive.document.nth_paragraph(0).unwrap().style_at(0);
    assert_eq!(s.char_style.as_deref(), Some("Emph"));
    assert_eq!(s.color, Some([0x1F, 0x4E, 0x79, 255]), "style folded in");
    let edited = archive
        .document
        .apply_style(at(0, 0), at(0, 8), bold(true).with_cs_twins());
    let xml = document_xml_of(&write_docx(&archive, &edited).expect("write"));
    assert_eq!(
        xml.matches(r#"<w:rStyle w:val="Emph"/>"#).count(),
        2,
        "both the bolded and the untouched piece keep the style: {xml}"
    );
    let back = read_docx(&write_docx(&archive, &edited).unwrap()).unwrap();
    let p = back.document.nth_paragraph(0).unwrap();
    assert_eq!(p.style_at(0).char_style.as_deref(), Some("Emph"));
    assert_eq!(p.style_at(10).char_style.as_deref(), Some("Emph"));
}

/// A grab bag captured before `<w:bCs>` was modeled (an older build's
/// crash-recovery snapshot) never writes a second `<w:bCs>` next to the
/// modeled one.
#[test]
fn a_stale_bag_twin_never_duplicates_the_modeled_child() {
    let mut style = bold(true).with_cs_twins();
    engine::GrabBag::push_into(&mut style.grab_bag, b"<w:bCs/>".to_vec());
    engine::GrabBag::push_into(&mut style.grab_bag, br#"<w:lang w:bidi="ar-SA"/>"#.to_vec());
    let mut out = String::new();
    emit_rpr(&style, &mut out);
    assert_eq!(
        out,
        r#"<w:rPr><w:b/><w:bCs/><w:lang w:bidi="ar-SA"/></w:rPr>"#
    );
}
