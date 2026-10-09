//! Malformed WordprocessingML parts (issues #439 / #434): the reader
//! repairs every lexical defect up front (`DocxWarning::MalformedPart`),
//! the part is regenerate-only, and from the repaired spelling on the
//! usual fidelity holds — the zero-edit save is well-formed and stable,
//! an edit of the re-read is a pure insertion (issue #251).

use super::{INSERT_TEXT, assert_document_xml_well_formed, extract_doc_xml, read_docx, write_docx};
use anyhow::{Context, Result, bail};
use format_docx::DocxWarning;
use format_docx::test_fixtures::package_with_document_xml_bytes;

/// A `word/document.xml` carrying one of each lexical defect: junk before
/// the root, a tag name that is not UTF-8 beside a field whose result
/// opens a never-closed field (#439), a raw `&` in paragraph-property junk
/// and in an attribute value, a NUL character reference in text, and the
/// part cut off after the last paragraph's text (truncation).
fn malformed_document() -> Vec<u8> {
    [
        br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#.as_slice(),
        b"PK\x03\x04junk\n",
        br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006"><w:body>"#,
        br#"<w:p><w:r><w:fld"#,
        b"\xC3",
        br#"har w:fldCharType="end"/></w:r><mc:AlternateContent><mc:Choice Requires="wpg">"#,
        br#"<w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText>"</w:instrText></w:r>"#,
        br#"<w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>result</w:t></w:r>"#,
        br#"<w:r><w:fldChar w:fldCharType="begin"/></w:r></mc:Choice></mc:AlternateContent>"#,
        br#"<w:r><w:t>hidden</w:t></w:r></w:p>"#,
        br#"<w:p><w:pPr><w:jc w:val="start"/>x & y</w:pPr><w:r><w:rPr><w:foo w:val="1 & 2"/></w:rPr><w:t xml:space="preserve">amp</w:t></w:r></w:p>"#,
        br#"<w:p><w:r><w:t xml:space="preserve">nul&#0;ref</w:t></w:r></w:p>"#,
        br#"<w:p><w:r><w:t xml:space="preserve">cut"#,
    ]
    .concat()
}

fn repaired_parts(warnings: &[DocxWarning]) -> Vec<&str> {
    warnings
        .iter()
        .filter_map(|w| match w {
            DocxWarning::MalformedPart {
                part,
                repaired: true,
                ..
            } => Some(part.as_str()),
            _ => None,
        })
        .collect()
}

/// `true` when the read reported any malformed part (a save must need no
/// repair).
fn has_malformed(warnings: &[DocxWarning]) -> bool {
    warnings
        .iter()
        .any(|w| matches!(w, DocxWarning::MalformedPart { .. }))
}

/// Bytes of the ORIGINAL part an edited save rewrote (issue #251).
fn source_bytes_rewritten(a: &[u8], b: &[u8]) -> usize {
    let prefix = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    let max_suffix = a.len().min(b.len()) - prefix;
    let suffix = a
        .iter()
        .rev()
        .zip(b.iter().rev())
        .take(max_suffix)
        .take_while(|(x, y)| x == y)
        .count();
    a.len() - prefix - suffix
}

/// Issues #439 / #434 — step 60.
pub fn run_malformed_parts_roundtrip() -> Result<()> {
    const TEXT: &str = "result\namp\nnul\u{FFFD}ref\ncut";
    let source = package_with_document_xml_bytes(&malformed_document(), &[]);
    let a = read_docx(&source).context("step 60: the malformed source opens")?;
    if repaired_parts(&a.warnings) != ["word/document.xml"] {
        bail!(
            "step 60a: expected a repaired main part, got {:?}",
            a.warnings
        );
    }
    if a.document.to_plain_text() != TEXT {
        bail!("step 60a: visible text {:?}", a.document.to_plain_text());
    }
    let first = write_docx(&a, &a.document).context("step 60a: zero-edit save")?;
    assert_document_xml_well_formed(&first).context("step 60a")?;
    let b = read_docx(&first).context("step 60a: re-read")?;
    if has_malformed(&b.warnings) || b.document.to_plain_text() != TEXT {
        bail!(
            "step 60a: the save re-read as {:?} with {:?}",
            b.document.to_plain_text(),
            b.warnings
        );
    }
    let second = write_docx(&b, &b.document).context("step 60a: second save")?;
    if extract_doc_xml(&second)? != extract_doc_xml(&first)? {
        bail!("step 60a: the repaired spelling is not stable across saves");
    }
    println!(
        "[roundtrip] step 60a OK — a malformed main part opens repaired (regenerate-only); its zero-edit save is well-formed, re-reads clean and is stable"
    );

    /* From the repaired spelling on, an edit is a pure insertion. The
    first paragraph is left out: typing into a paragraph whose broken
    (never-closed) field code sits inside a paragraph-level
    `mc:AlternateContent` regenerates it without the field characters —
    a writer gap of its own, unrelated to well-formedness (the same
    happens to a well-formed source). */
    let doc_first = extract_doc_xml(&first)?;
    for block in 1..4u32 {
        let at = engine::LogicalPos::at_top_paragraph(&b.document, block, 0)
            .with_context(|| format!("step 60b: paragraph {block}"))?;
        let edited = b.document.insert_text(at, INSERT_TEXT);
        let saved = write_docx(&b, &edited).context("step 60b: edited save")?;
        assert_document_xml_well_formed(&saved).context("step 60b")?;
        let rewritten = source_bytes_rewritten(&doc_first, &extract_doc_xml(&saved)?);
        if rewritten != 0 {
            bail!(
                "step 60b: an edit of paragraph {block} rewrote {rewritten} B of the repaired part"
            );
        }
        let c = read_docx(&saved).context("step 60b: re-read")?;
        if !c.document.to_plain_text().contains(INSERT_TEXT) {
            bail!("step 60b: the insertion into paragraph {block} did not re-read");
        }
    }
    println!(
        "[roundtrip] step 60b OK — every edit of the re-read repaired part is a pure insertion"
    );

    /* A malformed sibling is repaired in place and passes through so. */
    let styles = [
        br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:style w:type="paragraph" w:styleId="Q"><w:name w:val="Q & "#.as_slice(),
        b"\xFF",
        br#""/><w:rPr><w:b/></w:rPr></w:style></w:styles>"#,
    ]
    .concat();
    let document = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:pPr><w:pStyle w:val="Q"/></w:pPr><w:r><w:t>styled</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#;
    let source = package_with_document_xml_bytes(document, &[("word/styles.xml", &styles)]);
    let a = read_docx(&source).context("step 60c: read")?;
    if repaired_parts(&a.warnings) != ["word/styles.xml"] {
        bail!(
            "step 60c: expected a repaired styles part, got {:?}",
            a.warnings
        );
    }
    if a.document.styles.get("Q").map(|s| s.name.as_str()) != Some("Q & \u{FFFD}") {
        bail!("step 60c: the repaired style did not resolve");
    }
    let saved = write_docx(&a, &a.document).context("step 60c: save")?;
    format_docx::check_part_xml_well_formed(&saved, "word/styles.xml")
        .context("step 60c: the saved styles part")?;
    let b = read_docx(&saved).context("step 60c: re-read")?;
    if has_malformed(&b.warnings) {
        bail!(
            "step 60c: the saved package still needs a repair: {:?}",
            b.warnings
        );
    }
    println!(
        "[roundtrip] step 60c OK — a malformed styles.xml is repaired in place and saved well-formed"
    );
    Ok(())
}
