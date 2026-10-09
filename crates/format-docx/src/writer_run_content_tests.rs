//! Issue #335 — run-content elements with no text of their own: the
//! reader maps `<w:softHyphen/>` / `<w:noBreakHyphen/>` onto U+00AD /
//! U+2011 in the paragraph text, an untouched paragraph saves
//! byte-identical, and a regenerated paragraph re-emits the ELEMENTS
//! (never the raw characters, which Word treats differently).

use super::tests::{build_docx_with_styles, document_xml_of};
use super::*;
use crate::opc::archive::read_docx;
use engine::run_content::{NON_BREAKING_HYPHEN, SOFT_HYPHEN};
use engine::{BlockPath, LogicalPos};

const STYLES_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:docDefaults><w:rPrDefault><w:rPr><w:sz w:val="22"/></w:rPr></w:rPrDefault></w:docDefaults></w:styles>"#;

fn document(body: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}<w:sectPr/></w:body></w:document>"#
    )
}

fn at(offset: usize) -> LogicalPos {
    LogicalPos::new(BlockPath::top(0), offset as u32)
}

/// Word's shape: the soft hyphen inside the run that carries the word,
/// the non-breaking hyphen in a run of its own between two runs.
const HYPHEN_BODY: &str = concat!(
    r#"<w:p><w:r w:rsidR="00A1"><w:t>Extra</w:t><w:softHyphen/>"#,
    r#"<w:t>ordinary e</w:t></w:r><w:r><w:noBreakHyphen/></w:r>"#,
    r#"<w:r><w:t>mail</w:t></w:r></w:p>"#,
);

fn open(body: &str) -> (String, DocxArchive) {
    let xml = document(body);
    let archive = read_docx(&build_docx_with_styles(STYLES_XML, &xml)).expect("read");
    (xml, archive)
}

#[test]
fn the_reader_maps_both_hyphen_elements_into_the_text() {
    let (_, archive) = open(HYPHEN_BODY);
    let p = archive.document.nth_paragraph(0).unwrap();
    assert_eq!(
        p.text,
        format!("Extra{SOFT_HYPHEN}ordinary e{NON_BREAKING_HYPHEN}mail")
    );
}

#[test]
fn a_zero_edit_save_is_byte_identical() {
    let (xml, archive) = open(HYPHEN_BODY);
    let out = document_xml_of(&write_docx(&archive, &archive.document).expect("write"));
    assert_eq!(out, xml);
}

/// Typing at the end regenerates the paragraph: both elements come back
/// as elements in their source runs, the raw characters never reach a
/// `<w:t>`, and the source bytes survive as a pure insertion.
#[test]
fn a_regenerated_paragraph_re_emits_the_elements() {
    let (xml, archive) = open(HYPHEN_BODY);
    let len = archive.document.nth_paragraph(0).unwrap().text.len();
    let edited = archive.document.insert_text(at(len), " today");
    let out = document_xml_of(&write_docx(&archive, &edited).expect("write"));
    assert!(!out.contains(SOFT_HYPHEN), "raw U+00AD written:\n{out}");
    assert!(
        !out.contains(NON_BREAKING_HYPHEN),
        "raw U+2011 written:\n{out}"
    );
    let want = concat!(
        r#"<w:r w:rsidR="00A1"><w:t>Extra</w:t><w:softHyphen/>"#,
        r#"<w:t>ordinary e</w:t></w:r><w:r><w:noBreakHyphen/></w:r>"#,
        r#"<w:r><w:t>mail today</w:t></w:r>"#,
    );
    assert!(out.contains(want), "elements in their source runs:\n{out}");
    let back = read_docx(&write_docx(&archive, &edited).expect("write")).expect("reread");
    assert_eq!(
        back.document.nth_paragraph(0).unwrap().text,
        edited.nth_paragraph(0).unwrap().text
    );
    assert_ne!(out, xml);
}

/// A run holding nothing but a soft hyphen is a text run now (it used to
/// be kept as an opaque marker): typing elsewhere still re-emits it.
#[test]
fn a_run_of_only_a_soft_hyphen_regenerates_as_the_element() {
    let (_, archive) = open(
        r#"<w:p><w:r><w:t>co</w:t></w:r><w:r><w:softHyphen/></w:r><w:r><w:t>operate</w:t></w:r></w:p>"#,
    );
    let p = archive.document.nth_paragraph(0).unwrap();
    assert_eq!(p.text, format!("co{SOFT_HYPHEN}operate"));
    let edited = archive.document.insert_text(at(0), "Re-");
    let out = document_xml_of(&write_docx(&archive, &edited).expect("write"));
    assert!(
        out.contains("<w:r><w:softHyphen/></w:r>"),
        "the element's own run:\n{out}"
    );
}

/// Engine-authored text (a paste, the minimal writer) spells the
/// characters as the elements too.
#[test]
fn engine_authored_hyphen_characters_write_as_elements() {
    let doc = engine::DocumentTree::from_text(&format!("a{SOFT_HYPHEN}b{NON_BREAKING_HYPHEN}c"));
    let out = document_xml_of(&build_minimal_docx(&doc).expect("write"));
    assert!(out.contains("<w:softHyphen/>"), "{out}");
    assert!(out.contains("<w:noBreakHyphen/>"), "{out}");
    assert!(!out.contains(SOFT_HYPHEN) && !out.contains(NON_BREAKING_HYPHEN));
    let back = read_docx(&build_minimal_docx(&doc).expect("write")).expect("reread");
    assert_eq!(
        back.document.nth_paragraph(0).unwrap().text,
        doc.nth_paragraph(0).unwrap().text
    );
}

/// Issue #350 × #335 — inside a field's instruction a hyphen element is
/// code, never visible text.
#[test]
fn a_hyphen_element_in_field_code_stays_hidden() {
    let (_, archive) = open(concat!(
        r#"<w:p><w:r><w:t>a</w:t></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r>"#,
        r#"<w:r><w:instrText xml:space="preserve"> MERGEFIELD x</w:instrText><w:softHyphen/>"#,
        r#"<w:noBreakHyphen/></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r>"#,
        r#"<w:r><w:t>v</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p>"#,
    ));
    assert_eq!(archive.document.nth_paragraph(0).unwrap().text, "av");
}
