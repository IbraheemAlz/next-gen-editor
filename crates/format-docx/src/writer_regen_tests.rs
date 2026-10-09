//! Issue #384 — every paragraph regenerates byte-identically with no edit
//! (`regen_check`), and an edit to one rewrites none of its source bytes:
//! markers keep their slot among the wrapper boundaries at their offset
//! (`<w:proofErr/>` inside a link's end, pretty-print whitespace inside an
//! `<w:ins>`), a `_Toc*` bookmark keeps its source position, a TOC keeps
//! its source prologue / end run, an in-paragraph `<w:smartTag>` /
//! `<w:customXml>` keeps its wrapper, a field inside a deletion keeps its
//! `<w:delInstrText>`, a run keeps the whitespace between its children,
//! and an empty source paragraph stays `<w:p …/>`.

use super::regen_check::regen_check;
use super::tests::{build_docx_with_styles, document_xml_of};
use super::*;
use crate::opc::archive::read_docx;
use engine::{BlockPath, LogicalPos};

const STYLES_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"/>"#;

fn document(body: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml" xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006" mc:Ignorable="w14"><w:body>{body}<w:sectPr/></w:body></w:document>"#
    )
}

fn open(body: &str) -> (String, DocxArchive) {
    let xml = document(body);
    let archive = read_docx(&build_docx_with_styles(STYLES_XML, &xml)).expect("read fixture");
    (xml, archive)
}

fn save(archive: &DocxArchive, doc: &engine::DocumentTree) -> String {
    let bytes = write_docx(archive, doc).expect("write");
    crate::check_document_xml_well_formed(&bytes).expect("well-formed");
    document_xml_of(&bytes)
}

/// Bytes of `orig` an edited `out` respelled (the span between their
/// common prefix and suffix, on `orig`'s side).
fn rewritten(orig: &str, out: &str) -> usize {
    let (a, b) = (orig.as_bytes(), out.as_bytes());
    let prefix = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    let max = a.len().min(b.len()) - prefix;
    let suffix = a
        .iter()
        .rev()
        .zip(b.iter().rev())
        .take(max)
        .take_while(|(x, y)| x == y)
        .count();
    a.len() - prefix - suffix
}

use crate::test_fixtures::{
    REGEN_PROOF_ERR as PROOF_ERR, REGEN_TOC_FIELD as TOC_FIELD, REGEN_TOC_HEADING as TOC_HEADING,
    regen_classes_body,
};

fn all_classes() -> String {
    regen_classes_body()
}

/// The probe regenerates every paragraph of the fixture and finds each
/// byte-identical to its source.
#[test]
fn every_class_regenerates_byte_identically() {
    let (xml, archive) = open(&all_classes());
    assert_eq!(save(&archive, &archive.document), xml, "zero-edit identity");
    let report = regen_check(&archive, &archive.document).expect("regen check");
    assert_eq!(report.checked, 10, "{report:#?}");
    for m in &report.mismatches {
        eprintln!("SOURCE: {}\nREGEN:  {}", m.source, m.regenerated);
    }
    assert!(
        report.mismatches.is_empty(),
        "{} mismatches",
        report.mismatches.len()
    );
}

/// The probe is off for an ordinary save, and finds what does differ.
#[test]
fn regen_check_reports_a_mismatch() {
    /* Two `<w:t>` in one run: the model joins them (a known gap). */
    let (_, archive) = open(r#"<w:p><w:r><w:t>one</w:t><w:t>two</w:t></w:r></w:p>"#);
    let report = regen_check(&archive, &archive.document).expect("regen check");
    assert_eq!(report.checked, 1);
    assert_eq!(report.nonempty_checked, 1);
    assert_eq!(report.mismatches.len(), 1);
    assert!(report.mismatches[0].regenerated.contains("onetwo"));
    assert!(!regen_check::active());
}

/// One character typed into each paragraph is a pure insertion: no byte
/// of the source is respelled.
#[test]
fn an_edit_in_each_class_is_a_pure_insertion() {
    let (xml, archive) = open(&all_classes());
    let doc = &archive.document;
    for block in 0..doc.blocks.len() as u32 {
        let Some(engine::Block::Paragraph(p)) = doc.blocks.get(block as usize) else {
            continue;
        };
        if p.text.is_empty() {
            continue;
        }
        let at = p.text.char_indices().nth(1).map_or(0, |(i, _)| i) as u32;
        let edited = doc.insert_text(LogicalPos::new(BlockPath::top(block), at), "X");
        let out = save(&archive, &edited);
        assert_eq!(
            rewritten(&xml, &out),
            0,
            "paragraph {block} ({:?}) rewrote source bytes:\n{out}",
            p.text
        );
        assert_eq!(out.len(), xml.len() + 1, "paragraph {block}: only the X");
    }
}

/// A marker's slot is clamped to the boundaries an edit leaves at its
/// offset: typing at a link's end (outside the link) moves the
/// `spellEnd` past the new text, never across `</w:hyperlink>`.
#[test]
fn a_moved_marker_never_crosses_a_wrapper() {
    let (_, archive) = open(PROOF_ERR);
    let p = archive.document.nth_paragraph(0).unwrap();
    let end = p.hyperlinks[0].end;
    let edited = archive
        .document
        .insert_text(LogicalPos::new(BlockPath::top(0), end), "Z");
    let out = save(&archive, &edited);
    assert_eq!(out.matches("<w:proofErr").count(), 4, "{out}");
    let back = read_docx(&write_docx(&archive, &edited).unwrap()).unwrap();
    let p = back.document.nth_paragraph(0).unwrap();
    assert_eq!(p.text, "see TilakaZ by jharrop");
    assert_eq!(p.hyperlinks.len(), 1);
    assert_eq!(
        &p.text[p.hyperlinks[0].start as usize..p.hyperlinks[0].end as usize],
        "Tilaka"
    );
}

/// A `_Toc*` bookmark keeps its source position; a split leaves its end
/// on the left half (closed at the paragraph end there), never twice.
#[test]
fn toc_bookmark_ends_stay_balanced_across_a_split() {
    let (_, archive) = open(TOC_HEADING);
    let p = archive.document.nth_paragraph(0).unwrap();
    assert_eq!(p.bookmarks.len(), 1);
    let split = archive
        .document
        .split_paragraph(LogicalPos::new(BlockPath::top(0), 3));
    let out = save(&archive, &split);
    assert_eq!(out.matches("<w:bookmarkStart").count(), 1, "{out}");
    assert_eq!(out.matches("<w:bookmarkEnd").count(), 1, "{out}");
    let start = out.find("<w:bookmarkStart").unwrap();
    let end = out.find("<w:bookmarkEnd").unwrap();
    let second_p = out[start..].find("<w:p ").map(|i| start + i).unwrap();
    assert!(
        start < end && end < second_p,
        "end stays in the left half: {out}"
    );
}

/// A TOC's head keeps its verbatim prologue (untrimmed instruction, run
/// properties) and its tail its end run when an entry is edited.
#[test]
fn toc_prologue_and_end_run_survive_an_edit() {
    let (xml, archive) = open(TOC_FIELD);
    let p0 = archive.document.nth_paragraph(0).unwrap();
    assert!(
        p0.fields
            .iter()
            .any(|f| f.span == Some(engine::FieldSpan::Head) && f.source.is_some())
    );
    let edited = archive
        .document
        .insert_text(LogicalPos::new(BlockPath::top(0), 2), "Q");
    let out = save(&archive, &edited);
    assert_eq!(rewritten(&xml, &out), 0, "{out}");
    assert!(
        out.contains(r#"<w:instrText xml:space="preserve"> TOC \o "1-3" \h \z \u </w:instrText>"#)
    );
}
