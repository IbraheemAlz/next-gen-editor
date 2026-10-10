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

/* ------------------------------------------------------------------ */
/* Issue #434 — the lexical repairs and the structural refusals         */
/* ------------------------------------------------------------------ */

/// The repaired source's `word/document.xml`, as the zero-edit save writes
/// it (the part is regenerate-only).
fn saved_document_xml(xml: &[u8]) -> String {
    use std::io::Read;
    let a = read_docx(&package_with_document_xml_bytes(xml, &[])).expect("read");
    let saved = write_docx(&a, &a.document).expect("write");
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(saved)).expect("zip");
    let mut s = String::new();
    z.by_name("word/document.xml")
        .expect("main part")
        .read_to_string(&mut s)
        .expect("utf-8");
    s
}

/// `ROOT` split around its newline: the XML declaration, then the root
/// start tag.
fn prolog_and_root() -> (&'static [u8], &'static [u8]) {
    let nl = ROOT.iter().position(|&b| b == b'\n').expect("declaration");
    (&ROOT[..nl], &ROOT[nl + 1..])
}

/// Issue #434 (sweep index 37398) — a raw `&` in a field's hidden code
/// inside a paragraph-level `mc:Fallback`: quick-xml never decodes it, so
/// the part opened, but an edit regenerates the paragraph and the writer
/// copied the bytes into a `<w:t>` — a save its own reader refused
/// ("Cannot find ';' after '&'"). Escaped up front, every save re-reads.
#[test]
fn a_raw_ampersand_in_hidden_field_code_no_longer_breaks_an_edited_save() {
    let body = [
        br#"<w:p><mc:AlternateContent><mc:Fallback>"#.as_slice(),
        br#"<w:r><w:fldChar w:fldCharType="end"/></w:r>"#,
        BEGIN,
        br#"<w:r><w:instrText> MERGEFIELD Name </w:instrText></w:r>"#,
        br#"<w:r><w:fldChar w:fldCharType="bogus"/></w:r><w:r><w:t>a & b</w:t></w:r>"#,
        BEGIN,
        br#"</mc:Fallback></mc:AlternateContent></w:p>"#,
    ]
    .concat();
    let xml = document(&body);
    assert_repaired_round_trip(&xml);
    let a = read_docx(&package_with_document_xml_bytes(&xml, &[])).expect("read");
    let edited = a.document.insert_text(a.document.end_of_document(), "Z");
    let saved = write_docx(&a, &edited).expect("write");
    crate::check_document_xml_well_formed(&saved).expect("the edited save is well-formed");
    let b = read_docx(&saved).expect("the edited save re-reads");
    assert!(malformed_parts(&b.warnings).is_empty(), "{:?}", b.warnings);
}

/// Issue #434 — raw `&` / undefined entities / `<` in the verbatim spans
/// the reader never decodes (paragraph-property junk, an unmodeled run
/// property's text and attribute, an unselected `mc:Choice`) are escaped
/// up front: the text is unchanged and the zero-edit save is well-formed.
#[test]
fn raw_ampersands_in_verbatim_spans_are_escaped() {
    let body = concat!(
        r#"<w:p><w:pPr>x & y</w:pPr><w:r><w:rPr><w:foo w:val="1 & 2 < 3">a & b</w:foo></w:rPr><w:t>t</w:t></w:r>"#,
        r#"<w:r><mc:AlternateContent><mc:Choice Requires="w14"><w:t>&bogus; & more</w:t></mc:Choice>"#,
        r#"<mc:Fallback><w:t>fb</w:t></mc:Fallback></mc:AlternateContent></w:r></w:p>"#,
    );
    let xml = document(body.as_bytes());
    assert_eq!(assert_repaired_round_trip(&xml), "t[image]");
    let saved = saved_document_xml(&xml);
    for spelled in [
        "<w:pPr>x &amp; y</w:pPr>",
        r#"w:val="1 &amp; 2 &lt; 3">a &amp; b</w:foo>"#,
        "<w:t>&amp;bogus; &amp; more</w:t>",
    ] {
        assert!(saved.contains(spelled), "{spelled} not in {saved}");
    }
}

/// Issue #434 — characters XML 1.0 excludes, raw or referenced (`&#0;`
/// used to refuse the part, a raw control character used to be written
/// back raw), read as U+FFFD.
#[test]
fn excluded_characters_read_as_replacement_characters() {
    let body = b"<w:p><w:r><w:t>a&#0;b\x01c&#xFFFE;d</w:t></w:r></w:p>";
    assert_eq!(
        assert_repaired_round_trip(&document(body)),
        "a\u{FFFD}b\u{FFFD}c\u{FFFD}d"
    );
}

/// Issue #434 (sweep index 39869) — a byte-flipped `w:fldCharType` value
/// that is not UTF-8 ends a field that never began, beside a content
/// control whose only content is an unclosed `begin`, inside a
/// paragraph-level `mc:Choice`: the same capture fallback as #439.
#[test]
fn a_non_utf8_field_character_type_keeps_the_text() {
    let body = [
        br#"<w:p><mc:AlternateContent><mc:Choice Requires="w14"><w:sdt><w:sdtContent>"#.as_slice(),
        br#"<w:r><w:t>x</w:t></w:r><w:r><w:fldChar w:fldCharType="e"#,
        b"\xC3",
        br#"d"/></w:r></w:sdtContent></w:sdt><w:sdt><w:sdtContent>"#,
        BEGIN,
        br#"</w:sdtContent></w:sdt></mc:Choice><mc:Fallback><w:r><w:t>fb</w:t></w:r></mc:Fallback>"#,
        br#"</mc:AlternateContent><w:r><w:t>tail</w:t></w:r></w:p><w:p><w:r><w:t>next</w:t></w:r></w:p>"#,
    ]
    .concat();
    assert_repaired_round_trip(&document(&body));
}

/// Issue #434 — a main part that simply stops (truncated between two
/// tags) is closed: the text up to the cut survives and the save is
/// well-formed. Cut inside a tag there is no faithful repair: the open is
/// refused with a typed error, as before.
#[test]
fn a_truncated_main_part_is_closed_and_a_cut_tag_refuses() {
    let full = document(br#"<w:p><w:r><w:t>first</w:t></w:r></w:p><w:p><w:r><w:t>sec"#);
    let cut = &full[..full.len() - "<w:sectPr/></w:body></w:document>".len()];
    assert_eq!(assert_repaired_round_trip(cut), "first\nsec");
    let docx = package_with_document_xml_bytes(&cut[..cut.len() - 9], &[]);
    let err = read_docx(&docx).expect_err("cut inside `<w:t>`");
    assert!(matches!(err, crate::DocxError::Xml(_)), "{err:?}");
}

/// Issue #434 (the #439 package's prolog) — junk text between the XML
/// declaration and the root, and after the root, is dropped.
#[test]
fn junk_outside_the_root_is_dropped() {
    let (decl, root) = prolog_and_root();
    let xml = [
        decl,
        b"PK\x03\x04 junk\n",
        root,
        b"<w:body><w:p><w:r><w:t>a</w:t></w:r></w:p><w:sectPr/></w:body></w:document>&#0;tail",
    ]
    .concat();
    assert_eq!(assert_repaired_round_trip(&xml), "a");
    let saved = saved_document_xml(&xml);
    assert!(
        saved.starts_with(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document"#),
        "{saved}"
    );
    assert!(saved.ends_with("</w:document>"), "{saved}");
}

/// Issue #434 — entities a DOCTYPE declares are never expanded (an OPC part
/// may not carry a DTD; expanding one is the XXE attack): the reference
/// reads as its literal text, where the part used to be refused outright
/// ("unrecognized entity").
#[test]
fn entities_a_dtd_declares_are_never_expanded() {
    let (decl, root) = prolog_and_root();
    let xml = [
        decl,
        b"\n",
        br#"<!DOCTYPE w:document [<!ENTITY e "EXPANDED">]>"#,
        root,
        b"<w:body><w:p><w:r><w:t>&e;</w:t></w:r></w:p><w:sectPr/></w:body></w:document>",
    ]
    .concat();
    assert_eq!(assert_repaired_round_trip(&xml), "&e;");
}

/// Issue #434 — an end tag that does not close the open element has no
/// faithful repair (an unclosed tag inside a preserved span: where did the
/// producer mean it to end?): the main part is refused with a typed error
/// — never a panic, never a guess.
#[test]
fn an_unclosed_tag_inside_a_preserved_span_refuses_typed() {
    for body in [
        &br#"<w:p><w:pPr><w:foo></w:pPr><w:r><w:t>a</w:t></w:r></w:p>"#[..],
        br#"<w:p><w:r><mc:AlternateContent><mc:Choice Requires="w14"><w:t>a</mc:Choice></mc:AlternateContent></w:r></w:p>"#,
        br#"<w:p><w:r><w:t>a</w:t></w:r></w:p></w:p>"#,
    ] {
        let docx = package_with_document_xml_bytes(&document(body), &[]);
        let err = read_docx(&docx).expect_err("structurally broken");
        assert!(matches!(err, crate::DocxError::Xml(_)), "{err:?}");
    }
}

/// Issue #434 — a structurally broken sibling is reported and left exactly
/// as it is: the document still opens (default styles), and the part
/// passes through byte-identical, as before.
#[test]
fn a_structurally_broken_sibling_is_reported_and_kept() {
    let styles: &[u8] = br#"<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:style></w:styles>"#;
    let xml = document(b"<w:p><w:r><w:t>a</w:t></w:r></w:p>");
    let docx = package_with_document_xml_bytes(&xml, &[("word/styles.xml", styles)]);
    let a = read_docx(&docx).expect("the document opens");
    assert_eq!(
        malformed_parts(&a.warnings),
        vec![("word/styles.xml", false)]
    );
    assert_eq!(a.part_by_name("word/styles.xml"), Some(styles));
    assert_eq!(a.document.to_plain_text(), "a");
}

/* ------------------------------------------------------------------ */
/* Issue #435 — prefixes no `xmlns:` declares                           */
/* ------------------------------------------------------------------ */

/// A `word/document.xml` whose root binds `w` only, around `body`.
fn w_only_document(body: &str) -> Vec<u8> {
    format!(
        concat!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
            r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">"#,
            "<w:body>{}<w:sectPr/></w:body></w:document>"
        ),
        body
    )
    .into_bytes()
}

/// Issue #435 — a drawing whose `wp` / `a` / `pic` prefixes the root never
/// declares: the first read skipped it (unknown binding), the writer then
/// bound the conventional URIs on save, and the second read showed a
/// picture the first had not. The prefixes are now bound on the root up
/// front, so every read sees the picture.
#[test]
fn an_undeclared_drawing_prefix_is_bound_before_the_first_read() {
    let xml = w_only_document(concat!(
        r#"<w:p><w:r><w:drawing><wp:inline><wp:extent cx="914400" cy="914400"/>"#,
        r#"<a:graphic><a:graphicData><pic:pic/></a:graphicData></a:graphic></wp:inline></w:drawing></w:r>"#,
        r#"<w:r><w:t>x</w:t></w:r></w:p>"#
    ));
    assert_eq!(assert_repaired_round_trip(&xml), "[image]x");
    let saved = saved_document_xml(&xml);
    for decl in [
        r#"xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing""#,
        r#"xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main""#,
        r#"xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture""#,
    ] {
        assert!(
            saved.contains(decl),
            "{decl} not on the saved root: {saved}"
        );
    }
}

/// Issue #435 — `mc:AlternateContent`, a `w14:` attribute and an unknown
/// `foo:` element, none of them declared: bound on the root (`mc` / `w14`
/// to their URIs, `foo` to a placeholder URN that names no namespace), so
/// the zero-edit save is namespace-well-formed (it used to be refused by
/// the save-side gate) and reads back the same text.
#[test]
fn undeclared_prefixes_save_namespace_well_formed() {
    let xml = w_only_document(concat!(
        r#"<w:p w14:paraId="1A2B3C4D"><mc:AlternateContent><mc:Choice Requires="w14"><w:r><w:t>choice</w:t></w:r></mc:Choice>"#,
        r#"<mc:Fallback><w:r><w:t>fallback</w:t></w:r></mc:Fallback></mc:AlternateContent>"#,
        r#"<foo:bar><w:r><w:t>!</w:t></w:r></foo:bar></w:p>"#
    ));
    assert_eq!(assert_repaired_round_trip(&xml), "choice!");
    let saved = saved_document_xml(&xml);
    for decl in [
        r#"xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml""#,
        r#"xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006""#,
        r#"xmlns:foo="urn:x-nge-undeclared:foo""#,
    ] {
        assert!(
            saved.contains(decl),
            "{decl} not on the saved root: {saved}"
        );
    }
}

/// Issue #435 (a 50k-sweep finding) — markup spliced in front of the real
/// root wraps it (`<w:t><w:document xmlns:…>`): the reader walks it
/// leniently, but the writer's synthesized root re-declares only the
/// first element's bindings, so the wrapped root's declarations cannot
/// survive a save. Reported beyond repair; the document still opens.
#[test]
fn a_wrapped_main_root_is_reported_beyond_repair() {
    let (decl, root) = prolog_and_root();
    let xml = [
        decl,
        b"<w:t>",
        root,
        b"<w:body><w:p><w:r><w:t>a</w:t></w:r></w:p><w:sectPr/></w:body></w:document></w:t>",
    ]
    .concat();
    let a = read_docx(&package_with_document_xml_bytes(&xml, &[])).expect("opens");
    assert!(
        a.warnings.iter().any(|w| matches!(
            w,
            DocxWarning::MalformedPart { part, detail, repaired: false }
                if part == "word/document.xml" && detail.contains("`w:t`")
        )),
        "{:?}",
        a.warnings
    );
}

/// Issue #435 — the same in a sibling part: an undeclared `w14:` attribute
/// in `styles.xml` is bound on the part's root, in place.
#[test]
fn an_undeclared_prefix_in_a_sibling_is_bound_on_its_root() {
    let styles: &[u8] = br#"<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:style w:type="paragraph" w:styleId="A" w14:x="1"><w:name w:val="A"/></w:style></w:styles>"#;
    let xml = document(b"<w:p><w:r><w:t>a</w:t></w:r></w:p>");
    let docx = package_with_document_xml_bytes(&xml, &[("word/styles.xml", styles)]);
    let a = read_docx(&docx).expect("read");
    assert_eq!(
        malformed_parts(&a.warnings),
        vec![("word/styles.xml", true)]
    );
    let part = String::from_utf8_lossy(a.part_by_name("word/styles.xml").expect("styles"));
    assert!(
        part.starts_with(r#"<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml">"#),
        "{part}"
    );
    let saved = write_docx(&a, &a.document).expect("write");
    crate::check_part_xml_well_formed(&saved, "word/styles.xml").expect("well-formed styles");
}
