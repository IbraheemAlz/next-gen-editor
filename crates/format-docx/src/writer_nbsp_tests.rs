//! Issue #599 — U+00A0 NO-BREAK SPACE through the `.docx` writer and reader.
//! Pasted / opened HTML now decodes `&nbsp;` to it, so a document holding
//! one must save and reopen with every one of them: Word writes it as the
//! literal character inside `<w:t>` (it is a legal XML character and is not
//! XML white space), and so does this writer.

use super::tests::{build_docx_with_styles, document_xml_of};
use super::*;
use crate::opc::archive::read_docx;
use engine::{BlockPath, LogicalPos};

const NBSP: &str = "\u{a0}";

const STYLES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"/>"#;

fn doc_with_text(w_t: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>{w_t}</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#
    )
}

/// A document the editor built (no source package) writes every no-break
/// space as the literal character inside a `preserve` text element, with
/// nothing replaced and nothing reported, and reads back identically.
#[test]
fn a_fresh_save_writes_no_break_spaces_literally() {
    let text = format!("a{NBSP}{NBSP}{NBSP}b {NBSP}c{NBSP}");
    let doc = DocumentTree::from_text(&text);
    let (bytes, notes) = save_docx_with_notes(&doc).expect("save");
    assert!(notes.is_empty(), "{notes:?}");
    let xml = document_xml_of(&bytes);
    assert!(
        xml.contains(&format!("<w:t xml:space=\"preserve\">{text}</w:t>")),
        "{xml}"
    );
    assert!(!xml.contains("&#160;") && !xml.contains("&#xA0;"), "{xml}");
    crate::check_part_xml_well_formed(&bytes, "word/document.xml").expect("well-formed");
    let back = read_docx(&bytes).expect("read");
    assert_eq!(back.document.to_plain_text(), text);
}

/// A no-break space alone in a run, at either edge of one, in a numeric
/// character reference, and a run of them: read exactly, kept by a zero-edit
/// save (byte-identical `document.xml`), kept by an edit's regenerated
/// paragraph, and written back as the same character.
#[test]
fn a_source_with_no_break_spaces_round_trips() {
    let source = doc_with_text(&format!("{NBSP}x&#160;&#xA0;y{NBSP}"));
    let archive = read_docx(&build_docx_with_styles(STYLES, &source)).expect("read");
    let text = format!("{NBSP}x{NBSP}{NBSP}y{NBSP}");
    assert_eq!(archive.document.to_plain_text(), text);

    /* Zero edit: the part is replayed verbatim. */
    let (bytes, _) = write_docx_with_notes(&archive, &archive.document).expect("write");
    assert_eq!(document_xml_of(&bytes), source);

    /* An edit regenerates the paragraph: every no-break space survives. */
    let edited = archive
        .document
        .insert_text(LogicalPos::new(BlockPath::top(0), 0), "!");
    let (bytes, notes) = write_docx_with_notes(&archive, &edited).expect("write");
    assert!(notes.is_empty(), "{notes:?}");
    let xml = document_xml_of(&bytes);
    assert!(xml.contains('\u{a0}'), "{xml}");
    assert!(
        !xml.contains("&#160;") || source.contains("&#160;"),
        "{xml}"
    );
    let back = read_docx(&bytes).expect("re-read");
    assert_eq!(back.document.to_plain_text(), format!("!{text}"));
}

/// A paragraph holding only a no-break space (Word's blank line pasted from
/// HTML) is a paragraph with that text, not an empty one, after a save.
#[test]
fn a_paragraph_of_only_a_no_break_space_survives_a_save() {
    let doc = DocumentTree::from_text(&format!("one\n{NBSP}\ntwo"));
    let (bytes, _) = save_docx_with_notes(&doc).expect("save");
    let back = read_docx(&bytes).expect("read");
    assert_eq!(back.document.to_plain_text(), format!("one\n{NBSP}\ntwo"));
}
