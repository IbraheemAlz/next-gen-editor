//! Positioned verbatim spans in `Paragraph::source_markup` — the Tier-3
//! ("never lose content") regeneration contracts of issues #244 (legacy
//! form fields), #245 (run-level content controls) and #246 (simple
//! fields): a regenerated paragraph keeps the unmodeled markup whole, at
//! its text offset, through edits before, after and inside it.

use super::tests::{build_docx_with_styles, document_xml_of};
use super::*;
use crate::opc::archive::read_docx;
use engine::{BlockPath, LogicalPos, PathStep};

const STYLES_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"/>"#;

fn document(body: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml" xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006" mc:Ignorable="w14"><w:body>{body}<w:sectPr/></w:body></w:document>"#
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

fn save(archive: &DocxArchive, doc: &engine::DocumentTree) -> String {
    let bytes = write_docx(archive, doc).expect("write");
    crate::check_document_xml_well_formed(&bytes).expect("well-formed");
    document_xml_of(&bytes)
}

/* ============================== issue #244 — legacy form fields ==== */

/// `checkboxes.docx`'s shape: a `FORMCHECKBOX` whose begin `fldChar`
/// carries `<w:ffData>`, the form field's name bookmark inside the field,
/// a text-less rPr-only run between the instruction and `separate`, run
/// rsids everywhere, the bookmark end after the field.
const CHECKBOX: &str = concat!(
    r#"<w:r w:rsidR="00414CBC" w:rsidRPr="00045869"><w:rPr><w:lang w:val="de-DE"/></w:rPr><w:fldChar w:fldCharType="begin"><w:ffData><w:name w:val="Check1"/><w:enabled/><w:calcOnExit w:val="0"/><w:checkBox><w:sizeAuto/><w:default w:val="0"/></w:checkBox></w:ffData></w:fldChar></w:r>"#,
    r#"<w:bookmarkStart w:id="0" w:name="Check1"/>"#,
    r#"<w:r w:rsidRPr="00D051C0"><w:instrText xml:space="preserve"> FORMCHECKBOX </w:instrText></w:r>"#,
    r#"<w:r w:rsidR="00414CBC"><w:rPr><w:lang w:val="de-DE"/></w:rPr></w:r>"#,
    r#"<w:r w:rsidR="00414CBC"><w:rPr><w:lang w:val="de-DE"/></w:rPr><w:fldChar w:fldCharType="separate"/></w:r>"#,
    r#"<w:r w:rsidR="00414CBC" w:rsidRPr="00045869"><w:rPr><w:lang w:val="de-DE"/></w:rPr><w:fldChar w:fldCharType="end"/></w:r>"#,
);

/// A `FORMDROPDOWN` without a `separate` (Word omits it for an empty
/// result).
const DROPDOWN: &str = concat!(
    r#"<w:r><w:fldChar w:fldCharType="begin"><w:ffData><w:name w:val="Drop1"/><w:enabled/><w:ddList><w:listEntry w:val="one"/><w:listEntry w:val="two"/></w:ddList></w:ffData></w:fldChar></w:r>"#,
    r#"<w:r><w:instrText xml:space="preserve"> FORMDROPDOWN </w:instrText></w:r>"#,
    r#"<w:r><w:fldChar w:fldCharType="end"/></w:r>"#,
);

fn checkbox_paragraph() -> String {
    format!(
        r#"<w:p w:rsidR="00D8217F" w:rsidRDefault="00D051C0"><w:r><w:t xml:space="preserve">unchecked: </w:t></w:r>{CHECKBOX}<w:bookmarkEnd w:id="0"/><w:r w:rsidRPr="00D051C0"><w:t xml:space="preserve"> after</w:t></w:r>{DROPDOWN}</w:p>"#
    )
}

/// The field is ONE content marker at its offset; the markers inside it
/// (the name bookmark, the text-less run) are part of its bytes, not
/// separate markers; nothing reaches the field model.
#[test]
fn form_field_reads_as_one_content_span() {
    let (_, archive) = open(&checkbox_paragraph());
    let p = archive.document.nth_paragraph(0).unwrap();
    assert_eq!(p.text, "unchecked:  after");
    assert!(p.fields.is_empty(), "{:?}", p.fields);
    let m = p.source_markup.as_deref().unwrap();
    let content: Vec<_> = m
        .markers
        .iter()
        .filter(|mk| mk.role == engine::MarkerRole::Content)
        .collect();
    assert_eq!(content.len(), 2, "{:?}", m.markers);
    assert_eq!(content[0].at, "unchecked: ".len() as u32);
    assert_eq!(content[0].xml, CHECKBOX.as_bytes());
    assert_eq!(content[1].at, p.text.len() as u32);
    assert_eq!(content[1].xml, DROPDOWN.as_bytes());
    assert!(
        !m.markers
            .iter()
            .any(|mk| mk.xml.starts_with(br#"<w:bookmarkStart w:id="0""#)),
        "the name bookmark rides inside the span, never twice"
    );
}

/// Edits before, at and after the field regenerate the paragraph as
/// exactly source + the inserted bytes: the `fldChar` / `ffData` bytes
/// survive untouched.
#[test]
fn form_field_survives_edits_around_it() {
    let (xml, archive) = open(&checkbox_paragraph());
    assert_eq!(save(&archive, &archive.document), xml, "zero-edit");

    let edited = archive.document.insert_text(at(0, "unch".len()), "INS");
    assert_eq!(
        save(&archive, &edited),
        xml.replacen("unchecked", "unchINSecked", 1)
    );
    /* At the field's offset: the text continues the run before it. */
    let edited = archive
        .document
        .insert_text(at(0, "unchecked: ".len()), "X");
    assert_eq!(
        save(&archive, &edited),
        xml.replacen("unchecked: <", "unchecked: X<", 1)
    );
    let edited = archive
        .document
        .insert_text(at(0, "unchecked:  aft".len()), "Y");
    assert_eq!(
        save(&archive, &edited),
        xml.replacen(" after", " aftYer", 1)
    );
    /* Deleting the text on both sides keeps both fields. */
    let len = archive.document.nth_paragraph(0).unwrap().text.len();
    let edited = archive.document.delete_range(at(0, 0), at(0, len));
    let out = save(&archive, &edited);
    assert!(out.contains(CHECKBOX) && out.contains(DROPDOWN), "{out}");
    let back = read_docx(&write_docx(&archive, &edited).unwrap()).unwrap();
    assert_eq!(back.document.paragraph_text(0), Some(""));
}

/// A form field inside a table cell survives a cell edit (cells parse
/// through the body run parser).
#[test]
fn form_field_in_a_table_cell_survives_a_cell_edit() {
    let body = format!(
        r#"<w:tbl><w:tblPr><w:tblW w:w="0" w:type="auto"/></w:tblPr><w:tblGrid><w:gridCol w:w="4000"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:tcW w:w="4000" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>cell</w:t></w:r>{CHECKBOX}</w:p></w:tc></w:tr></w:tbl><w:p/>"#
    );
    let (_, archive) = open(&body);
    let cell = LogicalPos {
        path: BlockPath::top(0)
            .push(PathStep::Cell { row: 0, col: 0 })
            .push(PathStep::Block(0)),
        offset: 0,
    };
    let edited = archive.document.insert_text(cell, "Z");
    let out = save(&archive, &edited);
    assert!(out.contains(CHECKBOX), "{out}");
    assert!(out.contains(">Zcell<"), "{out}");
}

/// Stale offsets (an edit path that does not remap the markup) never drop
/// a form field: it is written at its offset clamped to the text, and
/// the write records a note.
#[test]
fn stale_markup_keeps_the_form_field_and_notes_it() {
    let (_, archive) = open(&checkbox_paragraph());
    let mut doc = archive.document.clone();
    let Some(engine::Block::Paragraph(mut p)) = doc.blocks.get(0).cloned() else {
        panic!("paragraph");
    };
    p.text = "rewritten".into();
    p.spans.clear();
    p.dirty = true;
    p.source_xml = None;
    doc.blocks.set(0, engine::Block::Paragraph(p));
    let (bytes, notes) = write_docx_with_notes(&archive, &doc).expect("write");
    crate::check_document_xml_well_formed(&bytes).expect("well-formed");
    let out = document_xml_of(&bytes);
    assert!(out.contains(CHECKBOX) && out.contains(DROPDOWN), "{out}");
    assert!(
        !out.contains(r#"<w:bookmarkEnd w:id="0"/>"#),
        "verbatim markers stay dropped when stale"
    );
    assert_eq!(notes, vec![WriteNote::StaleMarkupClamped { markers: 2 }]);
    /* The plain write path records nothing and leaks nothing. */
    assert_eq!(write_docx(&archive, &doc).unwrap(), bytes);
}

/// A field with a result is still modeled (not swallowed as a span); a
/// zero-result field NESTED in another field's instruction is left to
/// the enclosing field.
#[test]
fn only_unmodeled_zero_result_fields_become_spans() {
    let body = concat!(
        r#"<w:p><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText> PAGE </w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>3</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p>"#,
        r#"<w:p><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText> IF </w:instrText></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText> MERGEFIELD x </w:instrText></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:instrText> = 1 </w:instrText></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p>"#,
    );
    let (_, archive) = open(body);
    let p0 = archive.document.nth_paragraph(0).unwrap();
    assert_eq!(p0.fields.len(), 1);
    let m0 = p0.source_markup.as_deref().unwrap();
    assert!(m0.markers.iter().all(|mk| mk.role.is_verbatim()));
    /* The outer IF has no result either: kept whole, the inner MERGEFIELD
    inside its bytes (one span, not two). */
    let p1 = archive.document.nth_paragraph(1).unwrap();
    let m1 = p1.source_markup.as_deref().unwrap();
    let spans: Vec<_> = m1
        .markers
        .iter()
        .filter(|mk| mk.role.must_survive())
        .collect();
    assert_eq!(spans.len(), 1, "{:?}", m1.markers);
    assert!(spans[0].xml.windows(10).any(|w| w == b"MERGEFIELD"));
    assert!(
        spans[0]
            .xml
            .ends_with(br#"<w:fldChar w:fldCharType="end"/></w:r>"#)
    );
}
