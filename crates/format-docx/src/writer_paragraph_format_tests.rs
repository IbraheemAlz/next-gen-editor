//! Paragraph formatting through edits and saves: issue #292 (a paragraph
//! merge keeps the head's style and source identity and carries the
//! tail's hyperlinks / tracked changes into the saved file).

use super::tests::document_xml_of;
use super::*;
use crate::opc::archive::read_docx;
use engine::{BlockPath, LogicalPos};

const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

/// Heading1 (bold, keepNext, next = Normal), Normal, Hyperlink (character).
const STYLES_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/><w:qFormat/></w:style><w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/><w:basedOn w:val="Normal"/><w:next w:val="Normal"/><w:qFormat/><w:pPr><w:keepNext/><w:outlineLvl w:val="0"/></w:pPr><w:rPr><w:b/><w:sz w:val="32"/></w:rPr></w:style><w:style w:type="character" w:styleId="Hyperlink"><w:name w:val="Hyperlink"/><w:rPr><w:color w:val="0563C1"/><w:u w:val="single"/></w:rPr></w:style></w:styles>"#;

const DOC_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/><Relationship Id="rId9" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="https://example.com/" TargetMode="External"/></Relationships>"#;

/// `word/document.xml` around `body` (root binds `w`, `r`, `w14`).
fn document(body: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="{W}" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml"><w:body>{body}<w:sectPr/></w:body></w:document>"#
    )
}

/// A package with `styles.xml`, a document rels part (styles + one
/// external hyperlink, `rId9`) and `document_xml`.
pub(super) fn package(styles_xml: &str, document_xml: &str) -> Vec<u8> {
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/></Types>"#;
    let dot_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let mut buf: Vec<u8> = Vec::new();
    {
        let mut zip = ZipWriter::new(Cursor::new(&mut buf));
        let opts = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(0o644);
        for (name, body) in [
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", dot_rels),
            ("word/_rels/document.xml.rels", DOC_RELS),
            ("word/styles.xml", styles_xml),
            ("word/document.xml", document_xml),
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

/// A Heading1 paragraph, then a body paragraph holding a hyperlink and a
/// tracked insertion.
const MERGE_BODY: &str = concat!(
    r#"<w:p w14:paraId="11111111" w:rsidR="00AA0001"><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Title</w:t></w:r></w:p>"#,
    r#"<w:p w14:paraId="22222222" w:rsidR="00AA0002"><w:r><w:t xml:space="preserve">Body </w:t></w:r><w:hyperlink r:id="rId9"><w:r><w:rPr><w:rStyle w:val="Hyperlink"/></w:rPr><w:t>link</w:t></w:r></w:hyperlink><w:ins w:id="5" w:author="Rev" w:date="2026-01-01T00:00:00Z"><w:r><w:t>new</w:t></w:r></w:ins></w:p>"#,
);

/// Issue #292 — Backspace at the start of the body paragraph saves ONE
/// paragraph that is still a Heading1 (verified source `<w:pPr>` — the
/// merge used to drop the `<w:pStyle>` and bake the resolved props as
/// direct formatting), keeps the head's `w14:paraId` (left ids win) and
/// the tail's hyperlink (its source `r:id`) and tracked insertion.
#[test]
fn a_paragraph_merge_saves_the_heading_with_the_tails_overlays() {
    let parsed = read_docx(&package(STYLES_XML, &document(MERGE_BODY))).expect("read");
    let merged = parsed.document.delete_range(at(0, 5), at(1, 0));
    let bytes = write_docx(&parsed, &merged).expect("write");
    crate::check_document_xml_well_formed(&bytes).expect("well-formed");
    let out = document_xml_of(&bytes);
    assert_eq!(out.matches("<w:p ").count(), 1, "{out}");
    assert!(out.contains(r#"w14:paraId="11111111""#), "{out}");
    assert!(
        !out.contains("22222222"),
        "the tail's identity is gone: {out}"
    );
    assert!(
        out.contains(r#"<w:pPr><w:pStyle w:val="Heading1"/></w:pPr>"#),
        "{out}"
    );
    assert!(out.contains(r#"<w:hyperlink r:id="rId9""#), "{out}");
    assert!(out.contains(r#"<w:ins w:id="5" w:author="Rev""#), "{out}");

    let back = read_docx(&bytes).expect("re-read");
    let p = back.document.nth_paragraph(0).unwrap();
    assert_eq!(p.text, "TitleBody linknew");
    assert_eq!(p.style_id.as_deref(), Some("Heading1"));
    assert_eq!(p.hyperlinks.len(), 1);
    let h = &p.hyperlinks[0];
    assert_eq!(&p.text[h.start as usize..h.end as usize], "link");
    assert_eq!(p.revisions.len(), 1);
    let r = &p.revisions[0];
    assert_eq!(&p.text[r.start as usize..r.end as usize], "new");
}
