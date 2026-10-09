//! Reader hardening (issues #350 / #351): field phases and
//! markup-compatibility branches. Each fixture must read into the
//! visible text Word shows, save byte-identical with no edit, and save
//! every edit as a pure insertion (issue #251) on both save paths.

use super::{
    INSERT_TEXT, assert_document_xml_well_formed, build_styled_docx, extract_doc_xml, read_docx,
    write_docx,
};
use anyhow::{Context, Result, bail};
use engine::{BlockPath, LogicalPos};

const STYLES_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"/>"#;

fn document(body: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape" xmlns:v="urn:schemas-microsoft-com:vml" xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml" mc:Ignorable="w14"><w:body>{body}<w:sectPr/></w:body></w:document>"#
    )
}

fn at(block: u32, offset: usize) -> LogicalPos {
    LogicalPos {
        path: BlockPath::top(block),
        offset: offset as u32,
    }
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

/// Read `xml`, check the zero-edit save is byte-identical and every
/// `(block, offset)` insertion saves as a pure insertion on both save
/// paths, re-reading with the inserted text visible.
fn check_fixture(
    step: &str,
    xml: &str,
    texts: &[&str],
    edits: &[(u32, usize)],
) -> Result<format_docx::DocxArchive> {
    let bytes = build_styled_docx(STYLES_XML, xml);
    let archive = read_docx(&bytes).with_context(|| format!("{step}: read"))?;
    let got: Vec<String> = (0..texts.len() as u32)
        .filter_map(|i| archive.document.paragraph_text(i).map(str::to_owned))
        .collect();
    if got != texts {
        bail!("{step}: visible text {got:?}, expected {texts:?}");
    }
    let untouched = write_docx(&archive, &archive.document).context("untouched save")?;
    if extract_doc_xml(&untouched)? != xml.as_bytes() {
        bail!("{step}: untouched save drifted");
    }
    for (block, offset) in edits {
        let edited = archive
            .document
            .insert_text(at(*block, *offset), INSERT_TEXT);
        for (path, saved) in [
            (
                "write_docx",
                write_docx(&archive, &edited).context("write")?,
            ),
            (
                "save_docx",
                format_docx::save_docx(&edited).context("ui save")?,
            ),
        ] {
            assert_document_xml_well_formed(&saved).with_context(|| format!("{step} {path}"))?;
            let out = extract_doc_xml(&saved)?;
            let rewritten = source_bytes_rewritten(xml.as_bytes(), &out);
            if rewritten != 0 {
                bail!(
                    "{step} {path}: edit at {block}:{offset} rewrote {rewritten} source byte(s)\n{}",
                    String::from_utf8_lossy(&out)
                );
            }
            let back = read_docx(&saved).context("re-read")?;
            let text = back
                .document
                .paragraph_text(*block)
                .unwrap_or_default()
                .to_owned();
            if !text.contains(INSERT_TEXT.trim()) {
                bail!("{step} {path}: the insert is not visible on re-read: {text:?}");
            }
        }
    }
    Ok(archive)
}

fn fld(kind: &str) -> String {
    format!(r#"<w:r><w:fldChar w:fldCharType="{kind}"/></w:r>"#)
}

fn instr(code: &str) -> String {
    format!(r#"<w:r><w:instrText xml:space="preserve">{code}</w:instrText></w:r>"#)
}

fn text(t: &str) -> String {
    format!(r#"<w:r><w:t xml:space="preserve">{t}</w:t></w:r>"#)
}

/* ============================================ #350 — field phases ==== */

/// Issue #350 — step 41: a nested field's result inside an instruction
/// (`IF { MERGEFIELD x } = "a" "yes" "no"`), field characters before any
/// `begin`, and 200 `begin`s that never separate.
pub(crate) fn run_field_phases_roundtrip() -> Result<()> {
    /* 41a — the nested IF renders only its outer result. */
    let nested = [
        text("Pre "),
        fld("begin"),
        instr(" IF "),
        fld("begin"),
        instr(" MERGEFIELD x "),
        fld("separate"),
        text("«x»"),
        fld("end"),
        instr(r#" = "a" "yes" "no" "#),
        fld("separate"),
        text("no"),
        fld("end"),
        text(" post"),
    ]
    .concat();
    let xml = document(&format!("<w:p>{nested}</w:p>"));
    let archive = check_fixture("step 41a", &xml, &["Pre no post"], &[(0, 0), (0, 11)])?;
    let p = archive
        .document
        .nth_paragraph(0)
        .context("nested-field paragraph")?;
    if p.fields.len() != 1 || p.fields[0].instruction != r#"IF { MERGEFIELD x } = "a" "yes" "no""# {
        bail!("step 41a: fields {:?}", p.fields);
    }
    println!(
        "[roundtrip] step 41a OK — a nested field's result inside an instruction is hidden; edits are pure insertions"
    );

    /* 41b — `end` / `separate` before `begin`. */
    let stray = [
        fld("end"),
        fld("separate"),
        text("visible "),
        fld("begin"),
        instr(" PAGE "),
        fld("separate"),
        text("1"),
        fld("end"),
    ]
    .concat();
    let xml = document(&format!("<w:p>{stray}</w:p>"));
    check_fixture("step 41b", &xml, &["visible 1"], &[(0, 0), (0, 9)])?;
    println!("[roundtrip] step 41b OK — stray separate / end are ignored and survive regeneration");

    /* 41c — 200 unclosed begins, then an ordinary paragraph. */
    let mut broken = fld("begin").repeat(200);
    broken.push_str(&instr(" PAGE "));
    broken.push_str(&text("code"));
    let xml = document(&format!("<w:p>{broken}</w:p><w:p>{}</w:p>", text("normal")));
    let archive = check_fixture("step 41c", &xml, &["", "normal"], &[(0, 0), (1, 6)])?;
    if !archive
        .warnings
        .contains(&format_docx::DocxWarning::UnclosedField { count: 200 })
    {
        bail!("step 41c: warnings {:?}", archive.warnings);
    }
    println!(
        "[roundtrip] step 41c OK — 200 unclosed begins close at their paragraph end; the next paragraph is visible"
    );
    Ok(())
}
