//! Issue #335 — run-content elements with no text of their own:
//! `<w:softHyphen/>` / `<w:noBreakHyphen/>` (`soft_hyphen.docx`) through
//! read → edit → save. The reader maps them onto U+00AD / U+2011, an
//! untouched save is byte-identical, and a regenerated paragraph re-emits
//! the ELEMENTS (never the raw characters) as a pure insertion.

use super::{
    assert_document_xml_well_formed, extract_doc_xml, read_docx, rewritten_region, write_docx,
};
use anyhow::{Context, Result, bail};
use engine::{BlockPath, LogicalPos};
use format_docx::test_fixtures::{NB_HYPHEN_TEXT, SOFT_HYPHEN_TEXT, soft_hyphen_docx};

fn at(block: u32, offset: usize) -> LogicalPos {
    LogicalPos::new(BlockPath::top(block), offset as u32)
}

/// Step 47:
///
/// a. Both paragraphs read with U+00AD / U+2011 where the elements stood.
/// b. A zero-edit save is byte-identical.
/// c. Typing at the end of each paragraph regenerates it as a pure
///    insertion: every `<w:softHyphen/>` / `<w:noBreakHyphen/>` comes
///    back as the element in its source run, no raw U+00AD / U+2011
///    reaches a `<w:t>`, and the reread text equals the edited text.
/// d. A soft hyphen the user pastes (a raw U+00AD in inserted text) is
///    saved as `<w:softHyphen/>` too and rereads as U+00AD.
pub fn run_soft_hyphen_roundtrip() -> Result<()> {
    let src = soft_hyphen_docx();
    let archive = read_docx(&src).context("read soft_hyphen.docx")?;
    let doc = &archive.document;
    for (idx, want) in [(0, SOFT_HYPHEN_TEXT), (1, NB_HYPHEN_TEXT)] {
        let got = &doc.nth_paragraph(idx).context("paragraph")?.text;
        if got != want {
            bail!("47a: paragraph {idx} read as {got:?}, expected {want:?}");
        }
    }
    let soft_count = SOFT_HYPHEN_TEXT.matches('\u{AD}').count();
    let nb_count = NB_HYPHEN_TEXT.matches('\u{2011}').count();
    let doc_a = extract_doc_xml(&src)?;

    /* b. Zero-edit identity. */
    let untouched = write_docx(&archive, doc).context("47b: untouched save")?;
    if extract_doc_xml(&untouched)? != doc_a {
        bail!("47b: zero-edit save drifted");
    }

    /* c. Typing regenerates each paragraph (one edit per save: the
    rewritten-bytes metric measures ONE region). */
    for idx in [0, 1] {
        regenerated_paragraph_step(&archive, &doc_a, idx, soft_count, nb_count)?;
    }

    /* d. A pasted soft hyphen. */
    let pasted = doc.insert_text(at(1, 0), "co\u{AD}operative ");
    let bytes = write_docx(&archive, &pasted).context("47d: save")?;
    assert_document_xml_well_formed(&bytes).context("47d")?;
    let out = String::from_utf8(extract_doc_xml(&bytes)?).context("utf8")?;
    if out.contains('\u{AD}') || out.matches("<w:softHyphen/>").count() != soft_count + 1 {
        bail!("47d: a pasted soft hyphen is not written as <w:softHyphen/>");
    }
    let back = read_docx(&bytes).context("47d: reread")?;
    if !back
        .document
        .nth_paragraph(1)
        .context("paragraph")?
        .text
        .starts_with("co\u{AD}operative ")
    {
        bail!("47d: the pasted soft hyphen did not reread as U+00AD");
    }
    println!(
        "[roundtrip] step 47 OK — soft_hyphen.docx: {soft_count} soft + {nb_count} non-breaking \
         hyphens read, zero-edit identical, regenerated as elements (pure insertion), a pasted \
         soft hyphen written as the element (#335)"
    );
    Ok(())
}

/// Step 47c — typing at the end of paragraph `idx` regenerates it: a pure
/// insertion that re-emits every hyphen element (and no raw character);
/// the reread text equals the edited text.
fn regenerated_paragraph_step(
    archive: &format_docx::DocxArchive,
    doc_a: &[u8],
    idx: u32,
    soft_count: usize,
    nb_count: usize,
) -> Result<()> {
    let doc = &archive.document;
    let len = doc.nth_paragraph(idx).context("paragraph")?.text.len();
    let edited = doc.insert_text(at(idx, len), " Edited");
    let bytes = write_docx(archive, &edited).context("47c: edited save")?;
    assert_document_xml_well_formed(&bytes).context("47c")?;
    let doc_b = extract_doc_xml(&bytes)?;
    let (prefix, rewritten, _) = rewritten_region(doc_a, &doc_b);
    let out = String::from_utf8(doc_b.clone()).context("utf8")?;
    if out.contains('\u{AD}') || out.contains('\u{2011}') {
        bail!("47c: paragraph {idx}: a raw hyphen character reached the saved XML");
    }
    let (soft, nb) = (
        out.matches("<w:softHyphen/>").count(),
        out.matches("<w:noBreakHyphen/>").count(),
    );
    if soft != soft_count || nb != nb_count {
        bail!(
            "47c: paragraph {idx}: elements not re-emitted ({soft} / {soft_count} soft, \
             {nb} / {nb_count} non-breaking)"
        );
    }
    if rewritten != 0 {
        let lo = prefix.saturating_sub(80);
        let hi = (prefix + 160).min(doc_b.len());
        bail!(
            "47c: paragraph {idx}: the edited save rewrote {rewritten} source bytes at {prefix}:\n{}",
            String::from_utf8_lossy(&doc_b[lo..hi])
        );
    }
    let back = read_docx(&bytes).context("47c: reread")?;
    let a = &edited.nth_paragraph(idx).context("paragraph")?.text;
    let b = &back.document.nth_paragraph(idx).context("paragraph")?.text;
    if a != b {
        bail!("47c: paragraph {idx} reread as {b:?}, expected {a:?}");
    }
    Ok(())
}
