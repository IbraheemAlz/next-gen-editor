//! Issue #295 — step 39: tracked formatting changes (`<w:rPrChange>`) on
//! runs the user splits, and package-unique annotation ids on both save
//! paths.

use super::{
    assert_document_xml_well_formed, build_styled_docx, extract_doc_xml, read_docx, write_docx,
};
use anyhow::{Context, Result, bail};
use engine::{BlockPath, DocumentTree, LogicalPos, RevisionKind, SpanStyle, UnderlineStyle};
use std::collections::HashSet;

const STYLES_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"/>"#;

/// A bold run whose tracked change made it bold (it was italic,
/// `w:id="8"`), and an unrelated insertion (`w:id="3"`).
const BODY: &str = concat!(
    r#"<w:p w:rsidR="00A1B2C3"><w:r w:rsidRPr="00D4E5F6"><w:rPr><w:b/><w:rPrChange w:id="8" w:author="A" w:date="2026-01-01T00:00:00Z"><w:rPr><w:i/></w:rPr></w:rPrChange></w:rPr><w:t>reviewed text</w:t></w:r></w:p>"#,
    r#"<w:p><w:ins w:id="3" w:author="A"><w:r><w:t>inserted</w:t></w:r></w:ins></w:p>"#,
);

fn document(body: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}<w:sectPr/></w:body></w:document>"#
    )
}

fn at(block: u32, offset: u32) -> LogicalPos {
    LogicalPos {
        path: BlockPath::top(block),
        offset,
    }
}

/// Every `w:id` of a tracked-change annotation element, document order.
fn annotation_ids(xml: &str) -> Vec<u32> {
    let mut out: Vec<(usize, u32)> = Vec::new();
    for tag in [
        "<w:ins ",
        "<w:del ",
        "<w:moveFrom ",
        "<w:moveTo ",
        "<w:rPrChange ",
        "<w:pPrChange ",
    ] {
        for (at, _) in xml.match_indices(tag) {
            let rest = &xml[at..];
            let open = &rest[..rest.find('>').unwrap_or(rest.len())];
            let id = open
                .split("w:id=\"")
                .nth(1)
                .and_then(|v| v.split('"').next())
                .and_then(|v| v.parse().ok());
            match id {
                Some(id) => out.push((at, id)),
                None => out.push((at, u32::MAX)),
            }
        }
    }
    out.sort_unstable();
    out.into_iter().map(|(_, id)| id).collect()
}

/// Save `doc` on both paths; every save well-formed with unique,
/// numeric annotation ids. Returns the `write_docx` document.xml.
fn save_both(step: &str, archive: &format_docx::DocxArchive, doc: &DocumentTree) -> Result<String> {
    let mut first = None;
    for (path, out) in [
        ("write_docx", write_docx(archive, doc).context("write")?),
        ("save_docx", format_docx::save_docx(doc).context("ui save")?),
    ] {
        assert_document_xml_well_formed(&out).with_context(|| format!("{step} {path}"))?;
        let xml = String::from_utf8(extract_doc_xml(&out)?).context("utf8")?;
        let ids = annotation_ids(&xml);
        let set: HashSet<u32> = ids.iter().copied().collect();
        if ids.contains(&u32::MAX) || set.len() != ids.len() {
            bail!("{step} {path}: annotation ids not unique / numeric: {ids:?}\n{xml}");
        }
        first.get_or_insert(xml);
    }
    Ok(first.unwrap_or_default())
}

fn format_change_ranges(doc: &DocumentTree, block: u32) -> Vec<(u32, u32)> {
    doc.nth_paragraph(block)
        .map(|p| {
            p.revisions
                .iter()
                .filter(|r| r.kind == RevisionKind::FormatChange)
                .map(|r| (r.start, r.end))
                .collect()
        })
        .unwrap_or_default()
}

/// Issue #295 — step 39.
///
/// a. A run's `<w:rPrChange>` reads as a FormatChange revision over the
///    run; an untouched save is byte-identical.
/// b. Formatting a sub-range, splitting the paragraph inside the run and
///    a tracked deletion inside it each save (both paths) with unique
///    annotation ids — the source id once, fresh ids above it.
/// c. Accept-all drops the change record from every piece; reject-all
///    restores the recorded formatting on every piece.
/// d. Engine-made revisions in two paragraphs save with distinct ids.
pub(crate) fn run_revision_ids_roundtrip() -> Result<()> {
    let xml = document(BODY);
    let archive = read_docx(&build_styled_docx(STYLES_XML, &xml)).context("read fixture")?;
    let doc = &archive.document;
    if format_change_ranges(doc, 0) != vec![(0, 13)] {
        bail!(
            "step 39a: format change read as {:?}",
            format_change_ranges(doc, 0)
        );
    }
    let untouched = write_docx(&archive, doc).context("untouched save")?;
    if extract_doc_xml(&untouched)? != xml.as_bytes() {
        bail!("step 39a: untouched rPrChange document drifted");
    }
    println!("[roundtrip] step 39a OK — rPrChange modeled, untouched save byte-identical");

    let underline = SpanStyle {
        underline: Some(UnderlineStyle::Single),
        ..SpanStyle::default()
    };
    let formatted = doc.apply_style(at(0, 4), at(0, 8), underline);
    let split = doc.split_paragraph(at(0, 6));
    let deleted = doc.tracked_delete_range(at(0, 2), at(0, 5), "B".into(), "2026-02-02".into());
    for (what, edited, pieces) in [
        ("sub-range format", &formatted, 3),
        ("paragraph split", &split, 2),
        ("tracked delete", &deleted, 3),
    ] {
        let got = save_both(&format!("step 39b {what}"), &archive, edited)?;
        let ids = annotation_ids(&got);
        if got.matches("<w:rPrChange ").count() != pieces
            || ids.iter().filter(|&&id| id == 8).count() != 1
            || !ids.contains(&3)
        {
            bail!("step 39b {what}: ids {ids:?}\n{got}");
        }
    }
    println!(
        "[roundtrip] step 39b OK — a split rPrChange run keeps its id once, fresh ids elsewhere"
    );

    for accept in [true, false] {
        let resolved = formatted.resolve_all_revisions(accept);
        let got = save_both(&format!("step 39c accept={accept}"), &archive, &resolved)?;
        if got.contains("<w:rPrChange") {
            bail!("step 39c (accept={accept}): a change record survived\n{got}");
        }
        let p = resolved.nth_paragraph(0).context("para")?;
        let ok = if accept {
            p.spans.iter().all(|s| s.style.bold == Some(true))
        } else {
            p.spans
                .iter()
                .all(|s| s.style.italic == Some(true) && s.style.bold.is_none())
        };
        if !ok {
            bail!("step 39c (accept={accept}): spans {:?}", p.spans);
        }
    }
    println!("[roundtrip] step 39c OK — accept / reject resolve every piece of the split run");

    let engine_made = DocumentTree::from_paragraphs(["one".into(), "two".into()])
        .tracked_insert_text(at(0, 3), "!", "A".into(), "d".into())
        .tracked_insert_text(at(1, 3), "?", "A".into(), "d".into());
    let out = format_docx::save_docx(&engine_made).context("save engine-made")?;
    assert_document_xml_well_formed(&out).context("step 39d")?;
    let got = String::from_utf8(extract_doc_xml(&out)?).context("utf8")?;
    let ids = annotation_ids(&got);
    if ids.len() != 2 || ids[0] == ids[1] {
        bail!("step 39d: engine-made revisions saved as {ids:?}\n{got}");
    }
    println!("[roundtrip] step 39d OK — engine-made revisions get distinct ids");
    Ok(())
}
