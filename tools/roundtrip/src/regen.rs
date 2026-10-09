//! Issue #384 — the paragraph regenerator measured on its own:
//! `regen_classes.docx` holds one paragraph per class the corpus showed
//! not regenerating byte-identically (`<w:proofErr/>` at a link's / a
//! field's ends, pretty-print whitespace around a tracked insertion, a
//! partial `_Toc*` bookmark, smart tag / custom XML wrappers, a field
//! inside a deletion, whitespace between a run's children, an empty
//! `<w:pict/>` run, an empty paragraph, a TOC's prologue and end run).

use super::{
    assert_document_xml_well_formed, extract_doc_xml, read_docx, rewritten_region, write_docx,
};
use anyhow::{Context, Result, bail};
use engine::{Block, BlockPath, LogicalPos};
use format_docx::test_fixtures::regen_classes_docx;
use format_docx::writer::regen_check::regen_check;

/// Step 49:
///
/// a. A zero-edit save is byte-identical, and the regeneration probe
///    (`format_docx::writer::regen_check`) regenerates all ten paragraphs
///    byte-identically.
/// b. One character typed into each non-empty paragraph is a pure
///    insertion (0 source bytes rewritten).
/// c. A split inside the `_Toc*` bookmark's text keeps exactly one start
///    and one end, both in the left half.
pub fn run_regen_classes_roundtrip() -> Result<()> {
    let fixture = regen_classes_docx();
    let archive = read_docx(&fixture).context("read regen_classes.docx")?;
    let doc = &archive.document;
    let source = extract_doc_xml(&fixture)?;
    let saved = write_docx(&archive, doc).context("zero-edit save")?;
    if extract_doc_xml(&saved)? != source {
        bail!("zero-edit save drifted");
    }
    let report = regen_check(&archive, doc).context("regen check")?;
    if report.checked != 10 || !report.mismatches.is_empty() {
        let first = report.mismatches.first();
        bail!(
            "regeneration probe: {} of {} paragraphs differ; first:\n{:?}\nvs\n{:?}",
            report.mismatches.len(),
            report.checked,
            first.map(|m| &m.source),
            first.map(|m| &m.regenerated),
        );
    }
    println!(
        "[roundtrip] step 49a OK — zero-edit identity; all {} paragraphs regenerate byte-identically",
        report.checked
    );

    let mut edited_paragraphs = 0;
    for (i, block) in doc.blocks.iter().enumerate() {
        let Block::Paragraph(p) = block else {
            continue;
        };
        let Some((at, _)) = p.text.char_indices().nth(1) else {
            continue;
        };
        let edited = doc.insert_text(LogicalPos::new(BlockPath::top(i as u32), at as u32), "X");
        let bytes = write_docx(&archive, &edited).context("write edited")?;
        assert_document_xml_well_formed(&bytes).with_context(|| format!("paragraph {i} edited"))?;
        let out = extract_doc_xml(&bytes)?;
        let (_, rewritten, inserted) = rewritten_region(&source, &out);
        if rewritten != 0 || inserted != 1 {
            bail!(
                "paragraph {i} ({:?}): {rewritten} source bytes rewritten, {inserted} inserted",
                p.text
            );
        }
        edited_paragraphs += 1;
    }
    println!(
        "[roundtrip] step 49b OK — typing into each of {edited_paragraphs} paragraphs is a pure insertion"
    );

    let heading = doc
        .blocks
        .iter()
        .position(|b| matches!(b, Block::Paragraph(p) if !p.bookmarks.is_empty()))
        .context("no _Toc heading")? as u32;
    let split = doc.split_paragraph(LogicalPos::new(BlockPath::top(heading), 3));
    let bytes = write_docx(&archive, &split).context("write split")?;
    assert_document_xml_well_formed(&bytes).context("split heading")?;
    let back = read_docx(&bytes).context("reread split")?;
    let (left, right) = (
        back.document.nth_paragraph(heading).context("left half")?,
        back.document
            .nth_paragraph(heading + 1)
            .context("right half")?,
    );
    let out = String::from_utf8(extract_doc_xml(&bytes)?)?;
    if left.bookmarks.len() != 1
        || !right.bookmarks.is_empty()
        || out.matches("w:name=\"_Toc467580795\"").count() != 1
        || out.matches("<w:bookmarkEnd w:id=\"2\"/>").count() != 1
    {
        bail!("split heading: bookmark not balanced:\n{out}");
    }
    println!("[roundtrip] step 49c OK — a split keeps one _Toc bookmark start and end, left");
    Ok(())
}
