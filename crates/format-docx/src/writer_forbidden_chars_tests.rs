//! Issue #467 — the writer's last-line defence: a character XML 1.0 cannot
//! carry that reaches the model anyway (the input boundary,
//! `engine::text_input`, keeps them out, but an old snapshot or an
//! unclassified command may not) is written as U+FFFD, never raw, and the
//! save report says how many.

use super::tests::{build_docx_with_styles, document_xml_of};
use super::*;
use crate::opc::archive::read_docx;
use engine::{BlockPath, LogicalPos};

const DOC: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>seed</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#;

const STYLES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"/>"#;

fn forbidden_count(notes: &[WriteNote]) -> u32 {
    notes
        .iter()
        .map(|n| match n {
            WriteNote::ForbiddenCharactersReplaced { count } => *count,
            _ => 0,
        })
        .sum()
}

fn has_raw_forbidden(xml: &str) -> bool {
    xml.chars().any(|c| !engine::text_input::is_xml_char(c))
}

#[test]
fn a_forbidden_character_in_text_is_written_as_fffd_with_a_note() {
    let doc = DocumentTree::from_text("a\u{1}b\u{FFFE}c\u{B}d");
    let (bytes, notes) = save_docx_with_notes(&doc).expect("a repaired save succeeds");
    let xml = document_xml_of(&bytes);
    assert!(xml.contains("a\u{FFFD}b\u{FFFD}c\u{FFFD}d"), "{xml}");
    assert!(!has_raw_forbidden(&xml));
    crate::check_part_xml_well_formed(&bytes, "word/document.xml").expect("well-formed");
    assert_eq!(forbidden_count(&notes), 3, "{notes:?}");
}

#[test]
fn a_clean_document_reports_nothing() {
    let doc = DocumentTree::from_text("plain\ttext\r\nwith \u{2028} and \u{7F}");
    let (_, notes) = save_docx_with_notes(&doc).expect("save");
    assert_eq!(forbidden_count(&notes), 0, "{notes:?}");
    assert!(
        !notes
            .iter()
            .any(|n| matches!(n, WriteNote::ForbiddenCharactersReplaced { .. }))
    );
}

#[test]
fn the_counter_does_not_leak_into_the_next_save() {
    let dirty = DocumentTree::from_text("x\u{1}");
    let (_, notes) = save_docx_with_notes(&dirty).expect("save");
    assert_eq!(forbidden_count(&notes), 1);
    let clean = DocumentTree::from_text("x");
    let (_, notes) = save_docx_with_notes(&clean).expect("save");
    assert_eq!(forbidden_count(&notes), 0, "{notes:?}");
}

/// The package path (`write_docx` against a retained source) repairs the
/// same way, and the untouched source bytes stay verbatim.
#[test]
fn an_edit_with_a_forbidden_character_saves_through_the_package_writer() {
    let archive = read_docx(&build_docx_with_styles(STYLES, DOC)).expect("read");
    let edited = archive
        .document
        .insert_text(LogicalPos::new(BlockPath::top(0), 4), "\u{1}\u{FFFF}!");
    let (bytes, notes) = write_docx_with_notes(&archive, &edited).expect("write");
    let xml = document_xml_of(&bytes);
    assert!(xml.contains('\u{FFFD}'), "{xml}");
    assert!(!has_raw_forbidden(&xml), "{xml}");
    assert!(xml.contains("seed"), "{xml}");
    assert_eq!(forbidden_count(&notes), 2, "{notes:?}");
}

/// Attribute values go through the same defence (a tracked change's
/// author, a bookmark name).
#[test]
fn attribute_values_are_repaired_too() {
    let mut out = String::new();
    push_escaped_attr("A\u{2}\"&<>\u{FFFE}", &mut out);
    assert_eq!(out, "A\u{FFFD}&quot;&amp;&lt;&gt;\u{FFFD}");
    let mut out = String::new();
    push_escaped("A\u{2}&<>\"\u{FFFF}\t\n\r", &mut out);
    assert_eq!(out, "A\u{FFFD}&amp;&lt;&gt;\"\u{FFFD}\t\n\r");
}

/// A comment's body and author reach `comments.xml` through the shared
/// escape helpers, not a private copy.
#[test]
fn comment_text_and_author_are_repaired() {
    let doc = DocumentTree::from_text("seed");
    let (doc, _) = doc.insert_comment(
        LogicalPos::new(BlockPath::top(0), 0),
        LogicalPos::new(BlockPath::top(0), 4),
        "note\u{1}!".into(),
        "Ann\u{2}".into(),
        "2026-01-01T00:00:00Z".into(),
    );
    let (bytes, notes) = save_docx_with_notes(&doc).expect("save");
    let mut z = zip::ZipArchive::new(Cursor::new(&bytes[..])).expect("zip");
    let mut comments = String::new();
    std::io::Read::read_to_string(
        &mut z.by_name("word/comments.xml").expect("comments part"),
        &mut comments,
    )
    .expect("utf-8");
    assert!(comments.contains("note\u{FFFD}!"), "{comments}");
    assert!(comments.contains("w:author=\"Ann\u{FFFD}\""), "{comments}");
    assert!(!has_raw_forbidden(&comments));
    assert_eq!(forbidden_count(&notes), 2, "{notes:?}");
}
