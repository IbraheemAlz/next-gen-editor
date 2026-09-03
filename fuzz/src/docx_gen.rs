//! Structure-aware `.docx` generator for `docx_reader` / `docx_roundtrip`
//! (D5.5, issue #90).
//!
//! Builds schema-shaped WML — random `<w:pPr>` / `<w:rPr>` / `<w:tbl>` /
//! `<w:sectPr>` trees with a mix of valid and deliberately-invalid
//! attribute values — wrapped in a minimal real OPC package (`zip` crate:
//! `[Content_Types].xml`, `_rels/.rels`, `word/document.xml`, and
//! optionally `word/_rels/document.xml.rels`), rather than handing the
//! fuzzer's raw input bytes straight to `read_docx`. The OPC skeleton
//! mirrors `format_docx::writer`'s own `zip_minimal_docx` test helper
//! (`crates/format-docx/src/writer.rs`) — reimplemented here rather than
//! imported since that helper is `#[cfg(test)]`-private to its crate.

use crate::util::pick;
use arbitrary::Unstructured;
use std::io::{Cursor, Write};
use zip::{ZipWriter, write::SimpleFileOptions};

const CONTENT_TYPES_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#;

const DOT_RELS_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#;

const HYPERLINK_RELS_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="https://example.invalid" TargetMode="External"/>
</Relationships>"#;

/// `&` / `<` / `>` only — matches the writer's own escape set
/// (`.claude/rules/docx.md`).
fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// A numeric OOXML attribute value — usually a plausible integer,
/// occasionally deliberately-invalid garbage (issue #90: "valid and
/// invalid attributes"). The reader must reject or ignore the garbage
/// case, never panic on it.
fn attr_num(u: &mut Unstructured, max: i64) -> String {
    if u.ratio(1, 6).unwrap_or(false) {
        pick(
            u,
            &[
                "not_a_number",
                "-99999999999999",
                "",
                "3.14.15",
                "0x1F",
                "999999999999999999999999",
            ],
        )
        .to_string()
    } else {
        u.int_in_range(0..=max).unwrap_or(0).to_string()
    }
}

fn maybe_bool_element(u: &mut Unstructured, tag: &str) -> String {
    if u.ratio(1, 2).unwrap_or(false) {
        format!("<{tag}/>")
    } else {
        String::new()
    }
}

fn gen_text_run_content(u: &mut Unstructured) -> String {
    const POOL: &[&str] = &[
        "hello",
        "world",
        "السلام عليكم",
        "<injected>",
        "a & b",
        "",
        "\u{0301}\u{0301}",
        "line\nbreak",
        "tab\ttab",
    ];
    pick(u, POOL).to_string()
}

fn gen_rpr(u: &mut Unstructured) -> String {
    if u.ratio(1, 4).unwrap_or(false) {
        return String::new();
    }
    let mut s = String::from("<w:rPr>");
    s.push_str(&maybe_bool_element(u, "w:b"));
    s.push_str(&maybe_bool_element(u, "w:i"));
    if u.ratio(1, 2).unwrap_or(false) {
        let val = pick(u, &["single", "double", "wave", "garbage-style", ""]);
        s.push_str(&format!(r#"<w:u w:val="{val}"/>"#));
    }
    if u.ratio(1, 2).unwrap_or(false) {
        s.push_str(&format!(r#"<w:sz w:val="{}"/>"#, attr_num(u, 200)));
    }
    if u.ratio(1, 2).unwrap_or(false) {
        let color = pick(u, &["FF0000", "not-a-color", "000000", ""]);
        s.push_str(&format!(r#"<w:color w:val="{color}"/>"#));
    }
    s.push_str("</w:rPr>");
    s
}

fn gen_run(u: &mut Unstructured) -> String {
    let rpr = gen_rpr(u);
    let text = gen_text_run_content(u);
    // Deliberately skip escaping some of the time to feed the parser
    // malformed XML — `read_docx` must return `Err`, never panic.
    let body = if u.ratio(1, 10).unwrap_or(false) {
        text
    } else {
        escape_xml(&text)
    };
    format!(r#"<w:r>{rpr}<w:t xml:space="preserve">{body}</w:t></w:r>"#)
}

/// One run, optionally wrapped in `<w:hyperlink r:id="rId2">`. The caller
/// tracks whether any hyperlink was emitted so `build_docx` knows whether
/// to include the matching `word/_rels/document.xml.rels` part.
fn gen_run_or_hyperlink(u: &mut Unstructured, used_hyperlink: &mut bool) -> String {
    let run = gen_run(u);
    if u.ratio(1, 8).unwrap_or(false) {
        *used_hyperlink = true;
        format!(r#"<w:hyperlink r:id="rId2">{run}</w:hyperlink>"#)
    } else {
        run
    }
}

fn gen_ppr(u: &mut Unstructured, allow_sect_pr: bool) -> String {
    if u.ratio(1, 5).unwrap_or(false) {
        return String::new();
    }
    let mut s = String::from("<w:pPr>");
    if u.ratio(1, 2).unwrap_or(false) {
        let jc = pick(u, &["start", "end", "center", "both", "not-a-jc", ""]);
        s.push_str(&format!(r#"<w:jc w:val="{jc}"/>"#));
    }
    if u.ratio(1, 2).unwrap_or(false) {
        s.push_str(&format!(
            r#"<w:ind w:start="{}" w:end="{}" w:firstLine="{}" w:hanging="{}"/>"#,
            attr_num(u, 5000),
            attr_num(u, 5000),
            attr_num(u, 2000),
            attr_num(u, 2000)
        ));
    }
    if u.ratio(1, 2).unwrap_or(false) {
        s.push_str(&format!(
            r#"<w:spacing w:before="{}" w:after="{}"/>"#,
            attr_num(u, 2000),
            attr_num(u, 2000)
        ));
    }
    s.push_str(&maybe_bool_element(u, "w:bidi"));
    s.push_str(&maybe_bool_element(u, "w:pageBreakBefore"));
    if u.ratio(1, 3).unwrap_or(false) {
        s.push_str(&format!(
            r#"<w:numPr><w:ilvl w:val="{}"/><w:numId w:val="{}"/></w:numPr>"#,
            attr_num(u, 8),
            attr_num(u, 20)
        ));
    }
    // A mid-document section break: `<w:sectPr>` nested inside a
    // paragraph's `<w:pPr>` closes out the PREVIOUS section.
    if allow_sect_pr && u.ratio(1, 6).unwrap_or(false) {
        s.push_str(&gen_sect_pr(u));
    }
    s.push_str("</w:pPr>");
    s
}

fn gen_sect_pr(u: &mut Unstructured) -> String {
    format!(
        r#"<w:sectPr><w:pgSz w:w="{}" w:h="{}"/><w:pgMar w:top="{}" w:right="{}" w:bottom="{}" w:left="{}" w:header="720" w:footer="720"/></w:sectPr>"#,
        attr_num(u, 30000),
        attr_num(u, 30000),
        attr_num(u, 5000),
        attr_num(u, 5000),
        attr_num(u, 5000),
        attr_num(u, 5000),
    )
}

fn gen_paragraph(u: &mut Unstructured, allow_sect_pr: bool, used_hyperlink: &mut bool) -> String {
    let ppr = gen_ppr(u, allow_sect_pr);
    let run_count = u.int_in_range(0u32..=4).unwrap_or(0);
    let mut runs = String::new();
    for _ in 0..run_count {
        runs.push_str(&gen_run_or_hyperlink(u, used_hyperlink));
    }
    format!("<w:p>{ppr}{runs}</w:p>")
}

fn gen_cell(u: &mut Unstructured, used_hyperlink: &mut bool) -> String {
    let mut tc_pr = String::from("<w:tcPr>");
    if u.ratio(1, 2).unwrap_or(false) {
        let wtype = pick(u, &["dxa", "pct", "auto", "nil", "bogus"]);
        tc_pr.push_str(&format!(
            r#"<w:tcW w:w="{}" w:type="{wtype}"/>"#,
            attr_num(u, 10000)
        ));
    }
    if u.ratio(1, 3).unwrap_or(false) {
        tc_pr.push_str(&format!(r#"<w:gridSpan w:val="{}"/>"#, attr_num(u, 8)));
    }
    if u.ratio(1, 3).unwrap_or(false) {
        let v = pick(u, &["restart", "continue", "bogus"]);
        tc_pr.push_str(&format!(r#"<w:vMerge w:val="{v}"/>"#));
    }
    tc_pr.push_str("</w:tcPr>");
    let para_count = u.int_in_range(1u32..=2).unwrap_or(1);
    let mut paras = String::new();
    for _ in 0..para_count {
        paras.push_str(&gen_paragraph(u, false, used_hyperlink));
    }
    format!("<w:tc>{tc_pr}{paras}</w:tc>")
}

fn gen_table(u: &mut Unstructured, used_hyperlink: &mut bool) -> String {
    let cols = u.int_in_range(1u32..=5).unwrap_or(1);
    let rows = u.int_in_range(0u32..=5).unwrap_or(0);
    let mut grid = String::from("<w:tblGrid>");
    for _ in 0..cols {
        grid.push_str(&format!(r#"<w:gridCol w:w="{}"/>"#, attr_num(u, 5000)));
    }
    grid.push_str("</w:tblGrid>");
    let mut rows_xml = String::new();
    for _ in 0..rows {
        // Deliberately allow the per-row cell count to diverge from `cols`
        // — a mismatched grid/row cell count is exactly the kind of
        // "valid but hostile" shape the reader must tolerate.
        let cells_in_row = u.int_in_range(0u32..=cols.max(1) + 1).unwrap_or(0);
        let mut cells = String::new();
        for _ in 0..cells_in_row {
            cells.push_str(&gen_cell(u, used_hyperlink));
        }
        rows_xml.push_str(&format!("<w:tr>{cells}</w:tr>"));
    }
    format!("<w:tbl><w:tblPr/>{grid}{rows_xml}</w:tbl>")
}

fn gen_body(u: &mut Unstructured) -> (String, bool) {
    let mut used_hyperlink = false;
    let block_count = u.int_in_range(0u32..=8).unwrap_or(0);
    let mut body = String::new();
    for _ in 0..block_count {
        if u.ratio(1, 4).unwrap_or(false) {
            body.push_str(&gen_table(u, &mut used_hyperlink));
        } else {
            body.push_str(&gen_paragraph(u, true, &mut used_hyperlink));
        }
    }
    // Body-level trailing `<w:sectPr>` — required-ish per `.claude/rules/docx.md`.
    body.push_str(&gen_sect_pr(u));
    (body, used_hyperlink)
}

/// Assemble the OPC zip exactly like `format_docx::writer`'s private
/// `zip_minimal_docx` test helper.
fn zip_opc(document_xml: &str, doc_rels: Option<&str>) -> Vec<u8> {
    let mut buf: Vec<u8> = Vec::new();
    {
        let mut zip = ZipWriter::new(Cursor::new(&mut buf));
        let opts = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(0o644);
        let _ = zip.start_file("[Content_Types].xml", opts);
        let _ = zip.write_all(CONTENT_TYPES_XML.as_bytes());
        let _ = zip.start_file("_rels/.rels", opts);
        let _ = zip.write_all(DOT_RELS_XML.as_bytes());
        if let Some(rels) = doc_rels {
            let _ = zip.start_file("word/_rels/document.xml.rels", opts);
            let _ = zip.write_all(rels.as_bytes());
        }
        let _ = zip.start_file("word/document.xml", opts);
        let _ = zip.write_all(document_xml.as_bytes());
        let _ = zip.finish();
    }
    buf
}

/// Build one fuzz input's `.docx` bytes: a schema-shaped `word/document.xml`
/// wrapped in a minimal OPC package. Returns `None` only when `data` is
/// too small to make any decision at all (the libFuzzer minimal-input
/// case), so the target can bail out cheaply.
pub fn build_docx(u: &mut Unstructured) -> Option<Vec<u8>> {
    if u.is_empty() {
        return None;
    }
    let (body, used_hyperlink) = gen_body(u);
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:body>{body}</w:body></w:document>"#
    );
    let rels = used_hyperlink.then_some(HYPERLINK_RELS_XML);
    let mut bytes = zip_opc(&document_xml, rels);
    // Occasionally truncate the finished archive — a corrupt/incomplete
    // zip is a real thing a hostile or crashed upload can produce, and
    // `read_docx` must return `Err`, not panic, on it.
    if !bytes.is_empty() && u.ratio(1, 20).unwrap_or(false) {
        let cut = u.int_in_range(0..=bytes.len() as u64).unwrap_or(0) as usize;
        bytes.truncate(cut);
    }
    Some(bytes)
}
