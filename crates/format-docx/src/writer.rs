//! `.docx` writer: serialize `DocumentTree` → `word/document.xml`, repack
//! into a ZIP archive. When called via `write_docx(archive, doc)`, the
//! original archive's other entries are written back verbatim; only
//! `word/document.xml` is regenerated.

use crate::reader::{DOC_XML, DocxArchive, DocxError};
use engine::DocumentTree;
use std::io::{Cursor, Write};
use zip::write::{SimpleFileOptions, ZipWriter};

/// Standard OOXML document namespace boilerplate (matches what Word emits).
const DOC_XML_HEADER: &str = concat!(
    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
    "\n",
    r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">"#,
    "<w:body>",
);
const DOC_XML_FOOTER: &str = "<w:sectPr/></w:body></w:document>";

/// Serialize one paragraph as a single-run `<w:p><w:r><w:t xml:space="preserve">…</w:t></w:r></w:p>`.
/// `xml:space="preserve"` keeps leading/trailing whitespace; matters for Arabic
/// trailing-shadda etc.
fn serialize_paragraph(text: &str, out: &mut String) {
    out.push_str("<w:p><w:r><w:t xml:space=\"preserve\">");
    /* XML escape: &, <, >. Quotes don't matter inside character data. */
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out.push_str("</w:t></w:r></w:p>");
}

fn build_document_xml(doc: &DocumentTree) -> String {
    let mut out = String::with_capacity(2048);
    out.push_str(DOC_XML_HEADER);
    for para in &doc.paragraphs {
        serialize_paragraph(&para.text, &mut out);
    }
    out.push_str(DOC_XML_FOOTER);
    out
}

/// Repack `archive`'s sibling entries verbatim + a freshly serialized
/// `word/document.xml` from `doc`. Returns the assembled `.docx` bytes.
pub fn write_docx(archive: &DocxArchive, doc: &DocumentTree) -> Result<Vec<u8>, DocxError> {
    let mut buf: Vec<u8> = Vec::with_capacity(8192);
    {
        let mut zip = ZipWriter::new(Cursor::new(&mut buf));
        let opts = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(0o644);

        /* Write sibling entries verbatim, in original order. */
        for (name, bytes) in &archive.other_entries {
            zip.start_file(name, opts)?;
            zip.write_all(bytes)?;
        }

        /* Write the regenerated document.xml. */
        zip.start_file(DOC_XML, opts)?;
        let xml = build_document_xml(doc);
        zip.write_all(xml.as_bytes())?;

        zip.finish()?;
    }
    Ok(buf)
}

/// Build a minimal valid `.docx` from a freshly created document — no
/// existing archive to base on. Used by tests to manufacture fixtures.
pub fn build_minimal_docx(doc: &DocumentTree) -> Result<Vec<u8>, DocxError> {
    let archive = DocxArchive {
        other_entries: vec![
            (
                "[Content_Types].xml".into(),
                CONTENT_TYPES_XML.as_bytes().to_vec(),
            ),
            ("_rels/.rels".into(), DOT_RELS_XML.as_bytes().to_vec()),
            (
                "word/_rels/document.xml.rels".into(),
                DOC_RELS_XML.as_bytes().to_vec(),
            ),
        ],
        document: doc.clone(),
    };
    write_docx(&archive, doc)
}

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

const DOC_RELS_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
</Relationships>"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reader::read_docx;

    #[test]
    fn round_trip_minimal() {
        let doc = DocumentTree::from_text("hello world");
        let bytes = build_minimal_docx(&doc).expect("build");
        let parsed = read_docx(&bytes).expect("read");
        assert_eq!(parsed.document.paragraph_count(), 1);
        assert_eq!(parsed.document.paragraph_text(0), Some("hello world"));
    }

    #[test]
    fn round_trip_arabic() {
        let doc = DocumentTree::from_text("السلام عليكم ورحمة الله");
        let bytes = build_minimal_docx(&doc).expect("build");
        let parsed = read_docx(&bytes).expect("read");
        assert_eq!(
            parsed.document.paragraph_text(0),
            Some("السلام عليكم ورحمة الله")
        );
    }

    #[test]
    fn round_trip_xml_escapes() {
        let doc = DocumentTree::from_text("<a> & </a>");
        let bytes = build_minimal_docx(&doc).expect("build");
        let parsed = read_docx(&bytes).expect("read");
        assert_eq!(parsed.document.paragraph_text(0), Some("<a> & </a>"));
    }
}
