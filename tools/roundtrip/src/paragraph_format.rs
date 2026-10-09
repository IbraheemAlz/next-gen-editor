//! Issues #292 / #293 / #297 — paragraph formatting through edits and
//! saves on a Word-shaped package: a paragraph merge after a heading, the
//! paragraph mark's run properties (`<w:pPr><w:rPr>`) and the style
//! display names of a regenerated `styles.xml`.

use super::{
    WORD_ROOT, assert_document_xml_well_formed, entry_bytes, extract_doc_xml, read_docx, write_docx,
};
use anyhow::{Context, Result, bail};
use engine::{BlockPath, LogicalPos, SpanStyle};

const STYLES_XML: &str = concat!(
    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
    "\r\n",
    r#"<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">"#,
    r#"<w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/><w:qFormat/></w:style>"#,
    r#"<w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/><w:basedOn w:val="Normal"/>"#,
    r#"<w:next w:val="Normal"/><w:qFormat/><w:pPr><w:keepNext/><w:outlineLvl w:val="0"/></w:pPr>"#,
    r#"<w:rPr><w:b/><w:sz w:val="32"/></w:rPr></w:style>"#,
    r#"<w:style w:type="character" w:styleId="Hyperlink"><w:name w:val="Hyperlink"/>"#,
    r#"<w:rPr><w:color w:val="0563C1"/><w:u w:val="single"/></w:rPr></w:style>"#,
    r#"</w:styles>"#,
);

/// Paragraph 0: a Heading1 "Title".
const P_TITLE: &str = r#"<w:p w14:paraId="0A000001" w:rsidR="00AA0001" w:rsidRDefault="00AA0001"><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Title</w:t></w:r></w:p>"#;
/// Paragraph 1: "Body " + a hyperlink.
const P_BODY: &str = r#"<w:p w14:paraId="0A000002" w:rsidR="00AA0002" w:rsidRDefault="00AA0002"><w:r><w:t xml:space="preserve">Body </w:t></w:r><w:hyperlink r:id="rId9" w:history="1"><w:r><w:rPr><w:rStyle w:val="Hyperlink"/></w:rPr><w:t>link</w:t></w:r></w:hyperlink></w:p>"#;
/// Paragraph 2: "Hello " + a bold "world"; no mark rPr.
const P_HELLO: &str = r#"<w:p w14:paraId="0A000003" w:rsidR="00AA0003" w:rsidRDefault="00AA0003"><w:r><w:t xml:space="preserve">Hello </w:t></w:r><w:r w:rsidRPr="00AA0003"><w:rPr><w:b/></w:rPr><w:t>world</w:t></w:r></w:p>"#;
/// Paragraph 3: EMPTY, its mark italic + `en-GB`.
const P_EMPTY: &str = r#"<w:p w14:paraId="0A000004" w:rsidR="00AA0004" w:rsidRDefault="00AA0004"><w:pPr><w:rPr><w:i/><w:lang w:val="en-GB"/></w:rPr></w:pPr></w:p>"#;

fn document_xml() -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n{WORD_ROOT}<w:body>{P_TITLE}{P_BODY}{P_HELLO}{P_EMPTY}<w:sectPr/></w:body></w:document>"
    )
}

pub(crate) fn build_paragraph_format_docx() -> Vec<u8> {
    use std::io::Write;
    use zip::write::{SimpleFileOptions, ZipWriter};
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
<Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/>
</Types>"#;
    let dot_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/>
<Relationship Id="rId9" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="https://example.com/" TargetMode="External"/>
</Relationships>"#;
    let document = document_xml();
    let mut buf: Vec<u8> = Vec::new();
    {
        let mut zip = ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(0o644);
        for (name, body) in [
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", dot_rels),
            ("word/_rels/document.xml.rels", doc_rels),
            ("word/styles.xml", STYLES_XML),
            ("word/document.xml", document.as_str()),
        ] {
            zip.start_file(name, opts).unwrap();
            zip.write_all(body.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    buf
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

/// Issues #292 / #293 / #297 — step 40.
///
/// a. A zero-edit save is byte-identical (`document.xml`, `styles.xml`),
///    and the paragraph marks are modeled.
/// b. Backspace at the start of the body paragraph after the heading
///    saves ONE Heading1 paragraph: its verified `<w:pPr>`, its
///    `w14:paraId`, then the body's text and hyperlink (its `r:id`).
/// c. Enter at the end of the bold run + typing saves the new paragraph
///    with a bold mark and a bold run, the original paragraph byte-
///    identical; typing into the italic-marked empty paragraph is a pure
///    insertion of an italic run.
/// d. `ModifyStyle` regenerates `styles.xml` with every display name as
///    read (`<w:name w:val="heading 1"/>`).
pub(crate) fn run_paragraph_format_roundtrip() -> Result<()> {
    let fixture = build_paragraph_format_docx();
    let a = read_docx(&fixture).context("read paragraph-format fixture")?;
    let doc = &a.document;
    let zero = save_both(&a, doc)?;
    if extract_doc_xml(&fixture)? != extract_doc_xml(&zero)? {
        bail!("document.xml drifted on a zero-edit save");
    }
    let z = read_docx(&zero).context("re-read zero-edit save")?;
    if entry_bytes(&z, "word/styles.xml") != Some(STYLES_XML.as_bytes()) {
        bail!("styles.xml drifted on a zero-edit save");
    }
    let mark = |d: &engine::DocumentTree, i: u32| {
        d.nth_paragraph(i)
            .and_then(|p| p.mark_style.as_deref().cloned())
    };
    if mark(doc, 3).and_then(|m| m.italic) != Some(true) || mark(doc, 2).is_some() {
        bail!("paragraph marks: {:?} / {:?}", mark(doc, 2), mark(doc, 3));
    }
    println!("[roundtrip] step 40a OK — untouched save byte-identical; paragraph marks modeled");

    let merged = doc.delete_range(at(0, 5), at(1, 0));
    let bytes = save_both(&a, &merged)?;
    let out = String::from_utf8(extract_doc_xml(&bytes)?)?;
    let want = concat!(
        r#"<w:p w14:paraId="0A000001" w:rsidR="00AA0001" w:rsidRDefault="00AA0001"><w:pPr><w:pStyle w:val="Heading1"/></w:pPr>"#,
        r#"<w:r><w:t>Title</w:t></w:r><w:r><w:t xml:space="preserve">Body </w:t></w:r>"#,
        r#"<w:hyperlink r:id="rId9" w:history="1"><w:r><w:rPr><w:rStyle w:val="Hyperlink"/></w:rPr><w:t>link</w:t></w:r></w:hyperlink></w:p>"#,
    );
    if !out.contains(want) || out.contains("0A000002") {
        bail!("merged heading paragraph:\n{out}");
    }
    let b = read_docx(&bytes).context("re-read the merge")?;
    let p = b.document.nth_paragraph(0).context("merged paragraph")?;
    if p.style_id.as_deref() != Some("Heading1") || p.hyperlinks.len() != 1 {
        bail!(
            "re-read merge: {:?} / {} links",
            p.style_id,
            p.hyperlinks.len()
        );
    }
    println!(
        "[roundtrip] step 40b OK — Backspace after a heading keeps its pPr, paraId and the tail's hyperlink"
    );

    let split = doc.split_paragraph(at(2, 11)).insert_text(at(3, 0), "Next");
    let bytes = save_both(&a, &split)?;
    let out = String::from_utf8(extract_doc_xml(&bytes)?)?;
    let new_para = r#"<w:p w:rsidR="00AA0003" w:rsidRDefault="00AA0003"><w:pPr><w:rPr><w:b/></w:rPr></w:pPr><w:r><w:rPr><w:b/></w:rPr><w:t xml:space="preserve">Next</w:t></w:r></w:p>"#;
    if !out.contains(&format!("{P_HELLO}{new_para}{P_EMPTY}")) {
        bail!("Enter after a bold run, then typing:\n{out}");
    }
    let b = read_docx(&bytes).context("re-read the split")?;
    let p = b.document.nth_paragraph(3).context("new paragraph")?;
    if p.style_at(0).bold != Some(true) || mark(&b.document, 3).and_then(|m| m.bold) != Some(true) {
        bail!("re-read new paragraph: {:?}", p.spans);
    }
    let typed = doc.insert_text(at(3, 0), "slanted");
    let bytes = save_both(&a, &typed)?;
    let out = String::from_utf8(extract_doc_xml(&bytes)?)?;
    let want = document_xml().replacen(
        r#"<w:lang w:val="en-GB"/></w:rPr></w:pPr></w:p>"#,
        r#"<w:lang w:val="en-GB"/></w:rPr></w:pPr><w:r><w:rPr><w:i/></w:rPr><w:t xml:space="preserve">slanted</w:t></w:r></w:p>"#,
        1,
    );
    if out != want {
        bail!("typing into the italic-marked empty paragraph:\n{out}");
    }
    println!(
        "[roundtrip] step 40c OK — Enter after a bold run saves a bold mark + run; an empty marked paragraph types its mark"
    );

    let bigger = SpanStyle {
        font_size: Some(20.0),
        ..Default::default()
    };
    let modified = doc.modify_style("Heading1", None, Some(bigger), None, None);
    let bytes = save_both(&a, &modified)?;
    let b = read_docx(&bytes).context("re-read the style edit")?;
    let styles = std::str::from_utf8(entry_bytes(&b, "word/styles.xml").context("styles.xml")?)?;
    for name in ["heading 1", "Normal"] {
        if !styles.contains(&format!(r#"<w:name w:val="{name}"/>"#)) {
            bail!("`{name}` lost from the regenerated styles.xml:\n{styles}");
        }
    }
    if styles.contains(r#"<w:name w:val="Heading1"/>"#) {
        bail!("the regenerated styles.xml renamed Heading1:\n{styles}");
    }
    if b.document.styles.get("Heading1").map(|s| s.name.as_str()) != Some("heading 1") {
        bail!("re-read Heading1 name");
    }
    println!("[roundtrip] step 40d OK — ModifyStyle keeps every style's display name");
    Ok(())
}
