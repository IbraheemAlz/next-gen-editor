//! Issues #439 / #434 — malformed WordprocessingML parts: the reader
//! repairs what it can up front (`opc::well_formed`), reports it as
//! `DocxWarning::MalformedPart`, and read => write => read keeps the text
//! with a save that its own reader accepts as well-formed. The shapes are
//! the `docx_roundtrip` fuzz reproducers, minimized and spelled out so the
//! tests do not depend on the generator.

use crate::error::DocxWarning;
use crate::opc::archive::read_docx;
use crate::test_fixtures::package_with_document_xml_bytes;
use crate::writer::write_docx;

const ROOT: &[u8] = concat!(
    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
    "\n",
    r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" "#,
    r#"xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" "#,
    r#"xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006">"#,
)
.as_bytes();

const BEGIN: &[u8] = br#"<w:r><w:fldChar w:fldCharType="begin"/></w:r>"#;
const SEPARATE: &[u8] = br#"<w:r><w:fldChar w:fldCharType="separate"/></w:r>"#;

/// A whole `word/document.xml` around `body` (raw bytes: the shapes under
/// test are not always UTF-8).
fn document(body: &[u8]) -> Vec<u8> {
    [
        ROOT,
        b"<w:body>",
        body,
        b"<w:sectPr/></w:body></w:document>",
    ]
    .concat()
}

fn malformed_parts(warnings: &[DocxWarning]) -> Vec<(&str, bool)> {
    warnings
        .iter()
        .filter_map(|w| match w {
            DocxWarning::MalformedPart { part, repaired, .. } => Some((part.as_str(), *repaired)),
            _ => None,
        })
        .collect()
}

/// Read `xml`, save it with no edit, read the save back: the text survives,
/// the source is reported repaired, and the save needs no repair (the
/// writer's own output is well-formed — also by the save-side gate).
/// Returns the first read's plain text.
fn assert_repaired_round_trip(xml: &[u8]) -> String {
    let docx = package_with_document_xml_bytes(xml, &[]);
    let a = read_docx(&docx).expect("the malformed source opens");
    assert_eq!(
        malformed_parts(&a.warnings),
        vec![("word/document.xml", true)],
        "{:?}",
        a.warnings
    );
    let saved = write_docx(&a, &a.document).expect("write");
    crate::check_document_xml_well_formed(&saved).expect("the save is well-formed");
    let b = read_docx(&saved).expect("the writer's own output re-reads");
    assert!(
        malformed_parts(&b.warnings).is_empty(),
        "the save needed a repair: {:?}",
        b.warnings
    );
    let text = a.document.to_plain_text();
    assert_eq!(
        b.document.to_plain_text(),
        text,
        "read => write => read drifted"
    );
    text
}

/// Issue #439 — the nightly `docx_roundtrip` crash `f45b2694a2a6`: a
/// byte-flipped `<w:fld\xC3har>` (not UTF-8) in one run, then a field whose
/// result opens a second, never-closed field inside a paragraph-level
/// `mc:Choice`. The unclosed field hides the paragraph's tail; the capture
/// of that broken field code needed UTF-8, so it fell back and the save
/// dropped the field characters — the second read showed the hidden tail.
#[test]
fn issue_439_non_utf8_bytes_next_to_an_unclosed_field_keep_their_text() {
    let body = [
        br#"<w:p><w:r><w:fld"#.as_slice(),
        b"\xC3",
        br#"har w:fldCharType="end"/></w:r><mc:AlternateContent><mc:Choice Requires="wpg">"#,
        BEGIN,
        br#"<w:r><w:instrText>"</w:instrText></w:r>"#,
        SEPARATE,
        br#"<w:r><w:t>x</w:t></w:r>"#,
        BEGIN,
        br#"</mc:Choice></mc:AlternateContent><w:r><w:t>tail</w:t></w:r></w:p>"#,
    ]
    .concat();
    assert_eq!(assert_repaired_round_trip(&document(&body)), "x");
}

/// Issue #434 (sweep index 27778) — the same mechanism through a
/// paragraph's `<w:pPr>`: junk that is not UTF-8 and a stray field `begin`
/// run inside the properties. The pPr and the field used to be dropped by
/// the save, un-hiding the paragraph's text.
#[test]
fn non_utf8_junk_in_paragraph_properties_keeps_the_hidden_text_hidden() {
    let body = [
        br#"<w:p><w:pPr><w:jc w:val="start"/>x "#.as_slice(),
        b"\xC3(",
        br#" y"#,
        BEGIN,
        br#"</w:pPr><w:r><w:t>a</w:t></w:r></w:p><w:p><w:r><w:t>b</w:t></w:r></w:p>"#,
    ]
    .concat();
    assert_eq!(assert_repaired_round_trip(&document(&body)), "\nb");
}

/// Issue #439 — every byte sequence that is not UTF-8 reads as U+FFFD,
/// inside text too (the part used to be refused when the bad byte sat in a
/// `<w:t>`), and the repaired part is what the zero-edit save writes.
#[test]
fn non_utf8_text_reads_as_replacement_characters() {
    let body = b"<w:p><w:r><w:t>a\xFF\xFEb</w:t></w:r></w:p>";
    assert_eq!(
        assert_repaired_round_trip(&document(body)),
        "a\u{FFFD}\u{FFFD}b"
    );
}

/// Issue #439 — a sibling part (`styles.xml`) that is not UTF-8 is repaired
/// in place: its row in `other_entries` carries the repaired bytes, so the
/// passthrough writes a well-formed part.
#[test]
fn a_non_utf8_sibling_part_is_repaired_in_place() {
    let styles = [
        br#"<?xml version="1.0" encoding="UTF-8"?><w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:style w:type="paragraph" w:styleId="A"><w:name w:val="A"#.as_slice(),
        b"\xE9",
        br#""/></w:style></w:styles>"#,
    ]
    .concat();
    let xml = document(b"<w:p><w:r><w:t>a</w:t></w:r></w:p>");
    let docx = package_with_document_xml_bytes(&xml, &[("word/styles.xml", &styles)]);
    let a = read_docx(&docx).expect("read");
    assert_eq!(
        malformed_parts(&a.warnings),
        vec![("word/styles.xml", true)]
    );
    let part = a.part_by_name("word/styles.xml").expect("styles part");
    assert!(std::str::from_utf8(part).is_ok());
    assert!(String::from_utf8_lossy(part).contains("w:val=\"A\u{FFFD}\""));
    assert_eq!(
        a.document.styles.get("A").map(|s| s.name.as_str()),
        Some("A\u{FFFD}")
    );
}
