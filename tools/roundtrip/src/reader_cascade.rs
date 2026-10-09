//! Reader cascade correctness on Word-shaped packages: issue #369 (the
//! paragraph mark's `<w:pPr><w:rPr>` formats the mark only, never the
//! paragraph's runs).

use super::{WORD_ROOT, assert_document_xml_well_formed, extract_doc_xml, read_docx, write_docx};
use anyhow::{Context, Result, bail};
use engine::{BlockPath, LogicalPos};

const CONTENT_TYPES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#;

const DOT_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#;

/// A package of `[Content_Types].xml`, `_rels/.rels` and `parts`.
fn package(parts: &[(&str, &str)]) -> Vec<u8> {
    use std::io::Write;
    use zip::write::{SimpleFileOptions, ZipWriter};
    let mut buf: Vec<u8> = Vec::new();
    {
        let mut zip = ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(0o644);
        let fixed = [
            ("[Content_Types].xml", CONTENT_TYPES),
            ("_rels/.rels", DOT_RELS),
        ];
        for (name, body) in fixed.iter().chain(parts) {
            zip.start_file(*name, opts).unwrap();
            zip.write_all(body.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    buf
}

fn document_xml(body: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n{WORD_ROOT}<w:body>{body}<w:sectPr/></w:body></w:document>"
    )
}

fn at(block: u32, offset: u32) -> LogicalPos {
    LogicalPos::new(BlockPath::top(block), offset)
}

/// Both save paths: `write_docx` against the source archive and the live
/// editor's `save_docx` (tree + retained source package, issue #134).
fn save_both(a: &format_docx::DocxArchive, doc: &engine::DocumentTree) -> Result<Vec<u8>> {
    let bytes = write_docx(a, doc).context("write_docx")?;
    let ui = format_docx::save_docx(doc).context("save_docx")?;
    if ui != bytes {
        bail!("the UI save path diverges from write_docx");
    }
    assert_document_xml_well_formed(&bytes)?;
    Ok(bytes)
}

/* ============================================ mark formatting (#369) ==== */

/// Paragraph 0: a bold + red mark over a plain run and an italic run.
const P_MARKED: &str = concat!(
    r#"<w:p w14:paraId="0B000001" w:rsidR="00BB0001" w:rsidRDefault="00BB0001"><w:pPr><w:rPr><w:b/><w:color w:val="C00000"/><w:lang w:val="en-GB"/></w:rPr></w:pPr>"#,
    r#"<w:r><w:t xml:space="preserve">plain </w:t></w:r><w:r><w:rPr><w:i/></w:rPr><w:t>slanted</w:t></w:r></w:p>"#,
);
/// Paragraph 1: EMPTY, its mark bold.
const P_EMPTY_BOLD: &str = r#"<w:p w14:paraId="0B000002" w:rsidR="00BB0002" w:rsidRDefault="00BB0002"><w:pPr><w:rPr><w:b/></w:rPr></w:pPr></w:p>"#;

/// Issue #369 — step 47.
///
/// a. A bold + red mark over plain runs: the runs read plain (the italic
///    one italic only), the mark keeps its formatting, a zero-edit save
///    is byte-identical.
/// b. Typing into the plain run is a pure insertion that re-reads plain.
/// c. Typing into the empty bold-marked paragraph (#293) saves a bold
///    run and re-reads bold.
pub(crate) fn run_mark_formatting_roundtrip() -> Result<()> {
    let xml = document_xml(&format!("{P_MARKED}{P_EMPTY_BOLD}"));
    let fixture = package(&[("word/document.xml", xml.as_str())]);
    let a = read_docx(&fixture).context("read mark fixture")?;
    let doc = &a.document;
    let p = doc.nth_paragraph(0).context("paragraph 0")?;
    let (plain, slanted) = (p.style_at(0), p.style_at(7));
    if plain.bold.is_some() || plain.color.is_some() {
        bail!("the mark leaked into the plain run: {:?}", p.spans);
    }
    if slanted.italic != Some(true) || slanted.bold.is_some() || slanted.color.is_some() {
        bail!("the mark leaked into the italic run: {:?}", p.spans);
    }
    let mark = p.mark_style.as_deref().context("mark modeled")?;
    if mark.bold != Some(true) || mark.color != Some([0xC0, 0, 0, 255]) {
        bail!("mark: {mark:?}");
    }
    let zero = save_both(&a, doc)?;
    if extract_doc_xml(&zero)? != xml.as_bytes() {
        bail!("document.xml drifted on a zero-edit save");
    }
    println!(
        "[roundtrip] step 47a OK — a bold mark over plain runs reads plain runs; zero-edit save byte-identical"
    );

    let typed = doc.insert_text(at(0, 2), "ai");
    let bytes = save_both(&a, &typed)?;
    let out = String::from_utf8(extract_doc_xml(&bytes)?)?;
    if out != xml.replacen(">plain <", ">plaiain <", 1) {
        bail!("typing into the plain run is not a pure insertion:\n{out}");
    }
    let b = read_docx(&bytes).context("re-read the insertion")?;
    let s = b
        .document
        .nth_paragraph(0)
        .context("re-read 0")?
        .style_at(2);
    if s.bold.is_some() || s.color.is_some() {
        bail!("the re-read inserted text took the mark's formatting: {s:?}");
    }
    println!("[roundtrip] step 47b OK — typing under a bold mark stays plain through a save");

    let typed = doc.insert_text(at(1, 0), "loud");
    let bytes = save_both(&a, &typed)?;
    let out = String::from_utf8(extract_doc_xml(&bytes)?)?;
    let want = xml.replacen(
        P_EMPTY_BOLD,
        &P_EMPTY_BOLD.replacen(
            "</w:pPr></w:p>",
            r#"</w:pPr><w:r><w:rPr><w:b/></w:rPr><w:t xml:space="preserve">loud</w:t></w:r></w:p>"#,
            1,
        ),
        1,
    );
    if out != want {
        bail!("typing into the bold-marked empty paragraph:\n{out}");
    }
    let b = read_docx(&bytes).context("re-read the typed paragraph")?;
    if b.document
        .nth_paragraph(1)
        .context("re-read 1")?
        .style_at(0)
        .bold
        != Some(true)
    {
        bail!("the typed run lost its bold");
    }
    println!("[roundtrip] step 47c OK — an empty bold-marked paragraph still types bold (#293)");
    Ok(())
}
