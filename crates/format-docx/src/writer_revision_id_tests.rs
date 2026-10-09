//! Issue #295 — a run carrying a tracked formatting change
//! (`<w:rPrChange w:id=…>`) that is split in two — sub-range formatting,
//! a paragraph split, a writer cut at a tracked deletion — saves with the
//! source id on one half and a fresh, package-unique id on the other;
//! engine-made revisions no longer share the per-paragraph `1, 2, …`
//! fallback; and the modeled `FormatChange` revision accepts / rejects
//! both halves.

use super::tests::{build_docx_with_styles, document_xml_of};
use super::*;
use crate::opc::archive::read_docx;
use engine::{BlockPath, LogicalPos};

const STYLES_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"/>"#;

fn document(body: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}<w:sectPr/></w:body></w:document>"#
    )
}

/// "abcdef" bold, a tracked change that made it bold (it was italic):
/// `w:id="8"`; an unrelated insertion `w:id="3"` in the next paragraph.
const BODY: &str = concat!(
    r#"<w:p><w:r><w:rPr><w:b/><w:rPrChange w:id="8" w:author="A" w:date="2026-01-01T00:00:00Z"><w:rPr><w:i/></w:rPr></w:rPrChange></w:rPr><w:t>abcdef</w:t></w:r></w:p>"#,
    r#"<w:p><w:ins w:id="3" w:author="A"><w:r><w:t>new</w:t></w:r></w:ins></w:p>"#,
);

fn open() -> (String, DocxArchive) {
    let xml = document(BODY);
    let archive = read_docx(&build_docx_with_styles(STYLES_XML, &xml)).expect("read fixture");
    (xml, archive)
}

fn at(block: u32, offset: u32) -> LogicalPos {
    LogicalPos {
        path: BlockPath::top(block),
        offset,
    }
}

fn save(archive: &DocxArchive, doc: &engine::DocumentTree) -> String {
    let bytes = write_docx(archive, doc).expect("write");
    crate::check_document_xml_well_formed(&bytes).expect("well-formed");
    document_xml_of(&bytes)
}

/// Every `w:id` of an annotation element (`<w:ins` / `<w:del` /
/// `<w:rPrChange` …), in document order.
fn ids(xml: &str) -> Vec<u32> {
    let mut out: Vec<(usize, u32)> = Vec::new();
    for tag in [
        "<w:ins ",
        "<w:del ",
        "<w:rPrChange ",
        "<w:moveFrom ",
        "<w:moveTo ",
    ] {
        for (at, _) in xml.match_indices(tag) {
            let rest = &xml[at..];
            let open = &rest[..rest.find('>').unwrap()];
            let v = open.split("w:id=\"").nth(1).expect("w:id");
            let id: u32 = v[..v.find('"').unwrap()].parse().expect("numeric id");
            out.push((at, id));
        }
    }
    out.sort_unstable();
    out.into_iter().map(|(_, id)| id).collect()
}

fn unique(xml: &str) -> bool {
    let all = ids(xml);
    let set: std::collections::HashSet<u32> = all.iter().copied().collect();
    set.len() == all.len()
}

fn rpr_change_count(xml: &str) -> usize {
    xml.matches("<w:rPrChange ").count()
}

fn format_changes(doc: &engine::DocumentTree, block: u32) -> Vec<(u32, u32, Option<u32>)> {
    doc.nth_paragraph(block)
        .map(|p| {
            p.revisions
                .iter()
                .filter(|r| r.kind == engine::RevisionKind::FormatChange)
                .map(|r| (r.start, r.end, r.id))
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn the_reader_models_the_change_and_a_zero_edit_save_is_untouched() {
    let (xml, archive) = open();
    let doc = &archive.document;
    assert_eq!(format_changes(doc, 0), vec![(0, 6, Some(8))]);
    let r = &doc.nth_paragraph(0).unwrap().revisions[0];
    let prev = r.prev_attrs.as_ref().expect("recorded formatting");
    assert_eq!((prev.bold, prev.italic), (None, Some(true)));
    assert_eq!(save(&archive, doc), xml);
}

/// Sub-range formatting splits the run in three: one piece keeps `8`,
/// the others get fresh ids above every id in the document.
#[test]
fn formatting_a_sub_range_keeps_the_id_once() {
    let (_, archive) = open();
    let underline = engine::SpanStyle {
        underline: Some(engine::UnderlineStyle::Single),
        ..engine::SpanStyle::default()
    };
    let edited = archive.document.apply_style(at(0, 2), at(0, 4), underline);
    let xml = save(&archive, &edited);
    assert_eq!(rpr_change_count(&xml), 3, "{xml}");
    assert!(unique(&xml), "duplicate w:id in {xml}");
    let all = ids(&xml);
    assert_eq!(all.iter().filter(|&&id| id == 8).count(), 1);
    assert!(all.contains(&3), "the unrelated insertion keeps its id");
    assert!(all.iter().all(|&id| id == 3 || id == 8 || id > 8));
    /* The first piece — the one the source run still starts with —
    keeps the source id. */
    assert_eq!(all[0], 8, "{xml}");
    /* It re-reads as three changes with three ids. */
    let reread = read_docx(&write_docx(&archive, &edited).unwrap()).unwrap();
    let fc = format_changes(&reread.document, 0);
    assert_eq!(fc.len(), 3);
    let set: std::collections::HashSet<_> = fc.iter().map(|(.., id)| *id).collect();
    assert_eq!(set.len(), 3);
}

/// A paragraph split puts a piece of the run in each paragraph; a
/// tracked deletion in the middle makes the writer cut the run around a
/// `<w:del>`. Either way: no repeated id.
#[test]
fn a_paragraph_split_and_a_writer_cut_never_repeat_an_id() {
    let (_, archive) = open();
    let split = archive.document.split_paragraph(at(0, 3));
    let xml = save(&archive, &split);
    assert_eq!(rpr_change_count(&xml), 2, "{xml}");
    assert!(unique(&xml), "duplicate w:id in {xml}");

    let cut =
        archive
            .document
            .tracked_delete_range(at(0, 2), at(0, 4), "B".into(), "2026-02-02".into());
    let xml = save(&archive, &cut);
    assert_eq!(rpr_change_count(&xml), 3, "{xml}");
    assert!(xml.contains("<w:del "), "{xml}");
    assert!(unique(&xml), "duplicate w:id in {xml}");
}

/// The modeled change covers both halves: accept drops the record from
/// both (the new formatting stays), reject restores the recorded
/// formatting on both.
#[test]
fn accept_and_reject_resolve_both_halves() {
    let (_, archive) = open();
    let underline = engine::SpanStyle {
        underline: Some(engine::UnderlineStyle::Single),
        ..engine::SpanStyle::default()
    };
    let edited = archive.document.apply_style(at(0, 2), at(0, 4), underline);
    let accepted = edited.resolve_all_revisions(true);
    let xml = save(&archive, &accepted);
    assert_eq!(rpr_change_count(&xml), 0, "{xml}");
    let p = accepted.nth_paragraph(0).unwrap();
    assert!(p.spans.iter().all(|s| s.style.bold == Some(true)));

    let rejected = edited.resolve_all_revisions(false);
    let xml = save(&archive, &rejected);
    assert_eq!(rpr_change_count(&xml), 0, "{xml}");
    let p = rejected.nth_paragraph(0).unwrap();
    assert!(
        p.spans
            .iter()
            .all(|s| s.style.italic == Some(true) && s.style.bold.is_none()),
        "{:?}",
        p.spans
    );

    /* Saved and re-read, the halves are two changes with two ids; the
    single accept resolves exactly the addressed one. */
    let split = archive.document.split_paragraph(at(0, 3));
    let reread = read_docx(&write_docx(&archive, &split).unwrap()).unwrap();
    let doc = &reread.document;
    let (a, b) = (format_changes(doc, 0), format_changes(doc, 1));
    assert_eq!((a.len(), b.len()), (1, 1));
    assert_ne!(a[0].2, b[0].2);
    let entry = doc
        .revision_entries()
        .into_iter()
        .find(|e| e.at.path == BlockPath::top(1))
        .unwrap();
    let one = doc.resolve_revision(&entry.at, true).unwrap();
    let xml = save(&reread, &one);
    assert_eq!(rpr_change_count(&xml), 1, "{xml}");
}

/// Engine-made revisions (no source id) in two paragraphs used to be
/// written `w:id="1"` twice; a paragraph-mark revision without an id
/// used to be written `w:id="0"`.
#[test]
fn engine_made_revisions_get_distinct_ids() {
    let doc = engine::DocumentTree::from_paragraphs(["one".into(), "two".into()]);
    let doc = doc
        .tracked_insert_text(at(0, 3), "!", "A".into(), "d".into())
        .tracked_insert_text(at(1, 3), "?", "A".into(), "d".into());
    let mut doc = doc;
    let mut blocks = doc.blocks.clone();
    if let engine::Block::Paragraph(p) = &mut blocks[0] {
        p.mark_revisions = vec![engine::Revision {
            start: 0,
            end: 0,
            kind: engine::RevisionKind::Insert,
            author: "A".into(),
            date: "d".into(),
            id: None,
            prev_attrs: None,
            move_name: None,
        }];
    }
    doc.blocks = blocks;
    let xml = build_document_xml(&doc, &HashMap::new());
    let all = ids(&xml);
    assert_eq!(all.len(), 3, "{xml}");
    assert!(unique(&xml), "{xml}");
    assert!(!all.contains(&0), "{xml}");
}

/// A regenerated copy of an id that an untouched paragraph spells
/// literally is renumbered; the literal stays.
#[test]
fn a_literal_id_elsewhere_wins() {
    let body = concat!(
        r#"<w:p><w:ins w:id="5" w:author="A"><w:r><w:t>kept</w:t></w:r></w:ins></w:p>"#,
        r#"<w:p><w:ins w:id="5" w:author="A"><w:r><w:t>edited</w:t></w:r></w:ins></w:p>"#,
    );
    let xml = document(body);
    let archive = read_docx(&build_docx_with_styles(STYLES_XML, &xml)).unwrap();
    /* Untouched: the source's own duplicate is not ours to rewrite. */
    assert_eq!(save(&archive, &archive.document), xml);
    let edited = archive.document.insert_text(at(1, 0), "X");
    let out = save(&archive, &edited);
    assert!(unique(&out), "{out}");
    assert!(
        out.contains(r#"<w:p><w:ins w:id="5" w:author="A"><w:r><w:t>kept</w:t>"#),
        "{out}"
    );
}

/* ---- the token resolver itself ---- */

fn resolve(parts: &[&str], reserved: &[u32]) -> Vec<String> {
    let mut bytes: Vec<Vec<u8>> = parts.iter().map(|p| p.as_bytes().to_vec()).collect();
    let mut refs: Vec<&mut Vec<u8>> = bytes.iter_mut().collect();
    revision_ids::finalize(&mut refs, &reserved.iter().copied().collect());
    bytes
        .into_iter()
        .map(|b| String::from_utf8(b).unwrap())
        .collect()
}

fn tok(id: Option<u32>) -> String {
    revision_ids::token(id)
}

#[test]
fn tokens_resolve_across_parts_above_every_literal() {
    let body = format!(
        r#"<w:ins w:id="{}"/><w:ins w:id="{}"/><w:del w:id="12"/><w:ins w:id="{}"/><w:moveFromRangeStart w:id="20" w:name="m"/>"#,
        tok(Some(4)),
        tok(Some(4)),
        tok(Some(12)),
    );
    let header = format!(
        r#"<w:ins w:id="{}"/><w:ins w:id="{}"/><w:ins w:id="{}"/>"#,
        tok(None),
        tok(Some(12)),
        tok(Some(4)),
    );
    let out = resolve(&[&body, &header], &[30]);
    /* Body: 4 kept once; its copy and the KEEP 12 clashing with the
    body's own literal 12 are minted above every id of the package (the
    literal 20, the reserved 30): 31, 32. */
    assert_eq!(
        out[0],
        r#"<w:ins w:id="4"/><w:ins w:id="31"/><w:del w:id="12"/><w:ins w:id="32"/><w:moveFromRangeStart w:id="20" w:name="m"/>"#
    );
    /* Header: the fresh one gets 33; its KEEP 12 stays — the header
    spells no 12 and no KEEP claimed it (a source id two parts already
    shared is not ours to rewrite) — while its KEEP 4, already claimed by
    the body in this save, is minted: 34. */
    assert_eq!(
        out[1],
        r#"<w:ins w:id="33"/><w:ins w:id="12"/><w:ins w:id="34"/>"#
    );
}

#[test]
fn a_part_without_tokens_is_untouched_and_bookmark_ids_are_not_annotations() {
    /* Bookmark / comment ids live in their own id spaces: a KEEP 7 next
    to `<w:bookmarkStart w:id="7">` keeps 7. */
    let body = format!(
        r#"<w:bookmarkStart w:id="7" w:name="b"/><w:rPrChange w:id='{}' w:author="A"/>"#,
        tok(Some(7))
    );
    assert_eq!(
        resolve(&[&body], &[]),
        vec![r#"<w:bookmarkStart w:id="7" w:name="b"/><w:rPrChange w:id='7' w:author="A"/>"#]
    );
    let plain = r#"<w:ins w:id="1"/><w:ins w:id="1"/>"#;
    assert_eq!(resolve(&[plain], &[]), vec![plain.to_string()]);
}

#[test]
fn tokenize_rewrites_literal_annotation_ids_only() {
    let mut xml = String::from(
        r#"<w:p><w:pPr><w:pPrChange w:id="2"/></w:pPr><w:moveToRangeStart w:id="5" w:name="m"/><w:r><w:rPr><w:rPrChange w:id="9" w:author="A"><w:rPr><w:i/></w:rPr></w:rPrChange></w:rPr></w:r><w:bookmarkStart w:id="3" w:name="x"/></w:p>"#,
    );
    revision_ids::tokenize_annotations(&mut xml, 0);
    assert!(xml.contains(&format!(r#"<w:pPrChange w:id="{}"/>"#, tok(Some(2)))));
    assert!(xml.contains(&format!(r#"<w:rPrChange w:id="{}""#, tok(Some(9)))));
    assert!(
        xml.contains(r#"<w:moveToRangeStart w:id="5""#),
        "range markers stay"
    );
    assert!(xml.contains(r#"<w:bookmarkStart w:id="3""#));
}
