//! Tracked changes beyond one table level (issue #440) — read → list →
//! accept / reject → save → re-read for a revision inside a table nested
//! in a table cell.

use super::{assert_document_xml_well_formed, build_styled_docx, extract_doc_xml, read_docx};
use anyhow::{Context, Result, bail};
use engine::{DocumentTree, PathStep};

const STYLES_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"/>"#;

fn document(body: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}<w:sectPr/></w:body></w:document>"#
    )
}

/// A one-row, one-cell table around `cell` (a cell's block content).
fn table(tr_pr: &str, cell: &str) -> String {
    format!(
        r#"<w:tbl><w:tblPr><w:tblW w:w="0" w:type="auto"/></w:tblPr><w:tblGrid><w:gridCol w:w="4000"/></w:tblGrid><w:tr>{tr_pr}<w:tc><w:tcPr><w:tcW w:w="4000" w:type="dxa"/></w:tcPr>{cell}</w:tc></w:tr></w:tbl>"#
    )
}

/// Every paragraph text with its table depth, depth-first.
fn texts(doc: &DocumentTree) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    engine::for_each_paragraph_nested(&doc.blocks, &mut |path, p| {
        let depth = path
            .steps
            .iter()
            .filter(|s| matches!(s, PathStep::Cell { .. }))
            .count();
        out.push((depth, p.text.clone()));
    });
    out
}

/// Issue #440 — step 61: `intro` / table ▸ cell ▸ table (its row a tracked
/// row insertion, its paragraph `inner <w:ins>NEW</w:ins>`) / `outro`.
///
/// a. Both changes are listed (`revision_entries`) with depth-2 paths;
///    the zero-edit save is byte-identical.
/// b. Accept-all leaves no revision; both save paths write no `<w:ins`,
///    keep `NEW`, and re-read clean.
/// c. Reject-all removes the inserted row — the nested table with it —
///    and saves clean.
pub(crate) fn run_nested_table_revisions_roundtrip() -> Result<()> {
    let ins = r#"w:id="41" w:author="Ann" w:date="2026-01-01T00:00:00Z""#;
    let inner = table(
        &format!(r#"<w:trPr><w:ins {ins}/></w:trPr>"#),
        r#"<w:p><w:r><w:t xml:space="preserve">inner </w:t></w:r><w:ins w:id="42" w:author="Ann" w:date="2026-01-01T00:00:00Z"><w:r><w:t>NEW</w:t></w:r></w:ins></w:p>"#,
    );
    let outer = table(
        "",
        &format!(r#"<w:p><w:r><w:t>outer</w:t></w:r></w:p>{inner}<w:p/>"#),
    );
    let body = format!(
        r#"<w:p><w:r><w:t>intro</w:t></w:r></w:p>{outer}<w:p><w:r><w:t>outro</w:t></w:r></w:p>"#
    );
    let bytes = build_styled_docx(STYLES_XML, &document(&body));
    let source = extract_doc_xml(&bytes)?;
    let archive = read_docx(&bytes).context("step 61: read")?;
    let doc = &archive.document;
    let entries = doc.revision_entries();
    let depths: Vec<usize> = entries
        .iter()
        .map(|e| {
            e.at.path
                .steps
                .iter()
                .filter(|s| matches!(s, PathStep::Cell { .. }))
                .count()
                + usize::from(matches!(e.at.slot, engine::RevisionSlot::Row { .. }))
        })
        .collect();
    if depths != vec![2, 2] {
        bail!(
            "step 61a: listed {:?}",
            entries.iter().map(|e| &e.at).collect::<Vec<_>>()
        );
    }
    for (path, out) in [
        (
            "write_docx",
            format_docx::write_docx(&archive, doc).context("write")?,
        ),
        ("save_docx", format_docx::save_docx(doc).context("ui save")?),
    ] {
        if extract_doc_xml(&out)? != source {
            bail!("step 61a: the untouched {path} save drifted");
        }
    }
    println!(
        "[roundtrip] step 61a OK — a nested table's text + row revisions are listed at depth 2; zero-edit save byte-identical"
    );

    for (accept, want_new, want_inner) in [(true, true, true), (false, false, false)] {
        let resolved = doc.resolve_all_revisions(accept);
        let step = if accept { "61b" } else { "61c" };
        let check = |d: &DocumentTree| {
            let t = texts(d);
            !d.has_revisions()
                && t.iter().any(|(_, s)| s.contains("NEW")) == want_new
                && t.iter().any(|(depth, _)| *depth == 2) == want_inner
                && t.iter().any(|(depth, s)| *depth == 1 && s == "outer")
        };
        if !check(&resolved) {
            bail!("step {step}: resolved to {:?}", texts(&resolved));
        }
        for (path, out) in [
            (
                "write_docx",
                format_docx::write_docx(&archive, &resolved).context("write")?,
            ),
            (
                "save_docx",
                format_docx::save_docx(&resolved).context("ui save")?,
            ),
        ] {
            assert_document_xml_well_formed(&out).with_context(|| format!("step {step} {path}"))?;
            let got = String::from_utf8(extract_doc_xml(&out)?).context("utf8")?;
            let reread = read_docx(&out).with_context(|| format!("step {step} {path} re-read"))?;
            if got.contains("<w:ins ") || got.contains("<w:del ") || !check(&reread.document) {
                bail!(
                    "step {step} {path}: saved {:?}\n{got}",
                    texts(&reread.document)
                );
            }
        }
    }
    println!(
        "[roundtrip] step 61b OK — accept-all resolves the nested table's revisions; saves carry no w:ins and re-read clean"
    );
    println!(
        "[roundtrip] step 61c OK — reject-all removes the nested inserted row (and its table) and the inserted text; saves clean"
    );
    Ok(())
}
