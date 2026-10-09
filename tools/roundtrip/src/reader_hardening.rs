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
                .blocks
                .get(*block as usize)
                .and_then(engine::Block::as_paragraph)
                .map(|p| p.text.clone())
                .unwrap_or_default();
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

/* =================================== #351 — markup compatibility ==== */

/// The story text of the single text box in paragraph `idx`.
fn text_box_story(archive: &format_docx::DocxArchive, idx: u32) -> Result<String> {
    let p = archive
        .document
        .nth_paragraph(idx)
        .context("text-box paragraph")?;
    let stories: Vec<String> = p
        .inline_objects
        .iter()
        .filter_map(|o| match &o.kind {
            engine::InlineKind::TextBox { story, .. } => Some(
                story
                    .body
                    .iter()
                    .filter_map(engine::Block::as_paragraph)
                    .map(|q| q.text.clone())
                    .collect::<Vec<_>>()
                    .join("|"),
            ),
            _ => None,
        })
        .collect();
    match stories.as_slice() {
        [one] => Ok(one.clone()),
        other => bail!("expected one text box, found {other:?}"),
    }
}

/// Issue #351 — step 42: `mc:AlternateContent` at paragraph, block and
/// cell level reads ONE branch (the first satisfiable choice, else the
/// fallback) and keeps every branch through a save.
pub(crate) fn run_alternate_content_roundtrip() -> Result<()> {
    /* 42a — paragraph level: a `wps` text box choice with a VML fallback. */
    let ac = format_docx::test_fixtures::alternate_content_text_box("wps")
        .replace("<w:drawing>", "<w:r><w:drawing>")
        .replace("</w:drawing>", "</w:drawing></w:r>")
        .replace("<w:pict>", "<w:r><w:pict>")
        .replace("</w:pict>", "</w:pict></w:r>");
    let xml = document(&format!(
        "<w:p>{}{ac}{}</w:p>",
        text("before "),
        text(" after")
    ));
    let archive = check_fixture(
        "step 42a",
        &xml,
        &["before \u{FFFC} after"],
        &[(0, 0), (0, 7), (0, 15)],
    )?;
    let story = text_box_story(&archive, 0)?;
    if story != "choice story" {
        bail!("step 42a: text box story {story:?}, expected the choice's");
    }
    println!(
        "[roundtrip] step 42a OK — paragraph-level AlternateContent reads its wps choice once; edits are pure insertions"
    );

    /* 42b — an unknown requirement takes the fallback. */
    let xml = xml.replace(r#"Requires="wps""#, r#"Requires="w99""#);
    let archive = check_fixture("step 42b", &xml, &["before \u{FFFC} after"], &[(0, 15)])?;
    let story = text_box_story(&archive, 0)?;
    if story != "fallback story" {
        bail!("step 42b: text box story {story:?}, expected the fallback's");
    }
    println!("[roundtrip] step 42b OK — Requires=\"w99\" takes the VML fallback");

    /* 42c — block level and between the paragraphs of a table cell. */
    let body = concat!(
        r#"<mc:AlternateContent><mc:Choice Requires="w14"><w:p><w:r><w:t>choice</w:t></w:r></w:p></mc:Choice>"#,
        r#"<mc:Fallback><w:p><w:r><w:t>fallback</w:t></w:r></w:p></mc:Fallback></mc:AlternateContent>"#,
        r#"<w:tbl><w:tblGrid><w:gridCol w:w="2000"/></w:tblGrid><w:tr><w:tc>"#,
        r#"<mc:AlternateContent><mc:Choice Requires="w14"><w:p><w:r><w:t>cell</w:t></w:r></w:p></mc:Choice>"#,
        r#"<mc:Fallback><w:p><w:r><w:t>cell</w:t></w:r></w:p></mc:Fallback></mc:AlternateContent>"#,
        r#"</w:tc></w:tr></w:tbl><w:p><w:r><w:t>last</w:t></w:r></w:p>"#,
    );
    let xml = document(body);
    let archive = check_fixture("step 42c", &xml, &["choice", "last"], &[(0, 6), (2, 4)])?;
    let cell_blocks = archive
        .document
        .blocks
        .get(1)
        .and_then(engine::Block::as_table)
        .map(|t| t.rows[0].cells[0].blocks.len());
    if cell_blocks != Some(1) {
        bail!("step 42c: the cell holds {cell_blocks:?} blocks, expected 1");
    }
    println!(
        "[roundtrip] step 42c OK — block- and cell-level AlternateContent read one branch and keep both"
    );
    Ok(())
}
