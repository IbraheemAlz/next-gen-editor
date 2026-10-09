//! Fixture packages synthesized at test time — no binary blobs in the
//! tree. Public (hidden) only so downstream crates' tests (engine-wasm's
//! end-to-end canvas + PDF checks) share them; nothing in the read / write
//! path calls these, so the linker drops them from the wasm artifact.

use std::io::{Cursor, Write};
use zip::ZipWriter;
use zip::write::SimpleFileOptions;

const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const R_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const WP_NS: &str = "http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing";
const A_NS: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";
const PIC_NS: &str = "http://schemas.openxmlformats.org/drawingml/2006/picture";
const IMAGE_REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/image";
const HEADER_REL: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/header";
const FOOTER_REL: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer";

/// One `<w:r><w:drawing><wp:inline>` picture referencing `rel_id`, 1" × ½".
pub fn inline_picture_run(rel_id: &str) -> String {
    format!(
        "<w:r><w:drawing><wp:inline distT=\"0\" distB=\"0\" distL=\"0\" distR=\"0\">\
         <wp:extent cx=\"914400\" cy=\"457200\"/><wp:docPr id=\"1\" name=\"Picture\"/>\
         <a:graphic><a:graphicData uri=\"{PIC_NS}\"><pic:pic>\
         <pic:nvPicPr><pic:cNvPr id=\"1\" name=\"p\"/><pic:cNvPicPr/></pic:nvPicPr>\
         <pic:blipFill><a:blip r:embed=\"{rel_id}\"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill>\
         <pic:spPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"914400\" cy=\"457200\"/></a:xfrm>\
         <a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom></pic:spPr>\
         </pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r>"
    )
}

fn ns_decls() -> String {
    format!(
        "xmlns:w=\"{W_NS}\" xmlns:r=\"{R_NS}\" xmlns:wp=\"{WP_NS}\" xmlns:a=\"{A_NS}\" xmlns:pic=\"{PIC_NS}\""
    )
}

fn rels(rows: &[(&str, &str, &str)]) -> String {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
         <Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">",
    );
    for (id, ty, target) in rows {
        out.push_str(&format!(
            "<Relationship Id=\"{id}\" Type=\"{ty}\" Target=\"{target}\"/>"
        ));
    }
    out.push_str("</Relationships>");
    out
}

/// Issue #188 — a package whose body, header and footer parts each
/// declare the SAME relationship id `rId5` for a picture:
///
/// * `word/_rels/document.xml.rels`: `rId5` → `media/image1.jpeg`
///   (`body_image`), plus `rId7` → `header1.xml`, `rId8` → `footer1.xml`;
/// * `word/_rels/header1.xml.rels`: `rId5` → `media/image2.jpeg`
///   (`header_image`) — a DIFFERENT picture under the same id;
/// * `word/_rels/footer1.xml.rels`: `rId5` → `/word/media/image1.jpeg`
///   (absolute form) — the body's picture again, so an identical target
///   must dedupe into one media entry.
///
/// Every part holds one paragraph: a label (`Body ` / `Header ` /
/// `Footer `) followed by one inline picture `r:embed="rId5"`.
pub fn part_scoped_media_docx(body_image: &[u8], header_image: &[u8]) -> Vec<u8> {
    let ns = ns_decls();
    let pic = inline_picture_run("rId5");
    let document = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
         <w:document {ns}><w:body>\
         <w:p><w:r><w:t xml:space=\"preserve\">Body </w:t></w:r>{pic}</w:p>\
         <w:sectPr><w:headerReference w:type=\"default\" r:id=\"rId7\"/>\
         <w:footerReference w:type=\"default\" r:id=\"rId8\"/>\
         <w:pgSz w:w=\"11906\" w:h=\"16838\"/>\
         <w:pgMar w:top=\"1440\" w:right=\"1440\" w:bottom=\"1440\" w:left=\"1440\" w:header=\"708\" w:footer=\"708\" w:gutter=\"0\"/>\
         </w:sectPr></w:body></w:document>"
    );
    let header = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
         <w:hdr {ns}><w:p><w:r><w:t xml:space=\"preserve\">Header </w:t></w:r>{pic}</w:p></w:hdr>"
    );
    let footer = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
         <w:ftr {ns}><w:p><w:r><w:t xml:space=\"preserve\">Footer </w:t></w:r>{pic}</w:p></w:ftr>"
    );
    let content_types = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
<Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
<Default Extension=\"xml\" ContentType=\"application/xml\"/>\
<Default Extension=\"jpeg\" ContentType=\"image/jpeg\"/>\
<Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/>\
<Override PartName=\"/word/header1.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml\"/>\
<Override PartName=\"/word/footer1.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml\"/>\
</Types>";
    let dot_rels = rels(&[(
        "rId1",
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument",
        "word/document.xml",
    )]);
    let doc_rels = rels(&[
        ("rId5", IMAGE_REL, "media/image1.jpeg"),
        ("rId7", HEADER_REL, "header1.xml"),
        ("rId8", FOOTER_REL, "footer1.xml"),
    ]);
    let header_rels = rels(&[("rId5", IMAGE_REL, "media/image2.jpeg")]);
    let footer_rels = rels(&[("rId5", IMAGE_REL, "/word/media/image1.jpeg")]);

    let entries: Vec<(&str, &[u8])> = vec![
        ("[Content_Types].xml", content_types.as_bytes()),
        ("_rels/.rels", dot_rels.as_bytes()),
        ("word/document.xml", document.as_bytes()),
        ("word/_rels/document.xml.rels", doc_rels.as_bytes()),
        ("word/header1.xml", header.as_bytes()),
        ("word/_rels/header1.xml.rels", header_rels.as_bytes()),
        ("word/footer1.xml", footer.as_bytes()),
        ("word/_rels/footer1.xml.rels", footer_rels.as_bytes()),
        ("word/media/image1.jpeg", body_image),
        ("word/media/image2.jpeg", header_image),
    ];
    let mut buf: Vec<u8> = Vec::new();
    {
        let mut zip = ZipWriter::new(Cursor::new(&mut buf));
        let opts =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (name, bytes) in entries {
            zip.start_file(name, opts).expect("zip entry");
            zip.write_all(bytes).expect("zip write");
        }
        zip.finish().expect("zip finish");
    }
    buf
}

/// Issue #221 — a minimal one-paragraph package whose `<w:sectPr>` is
/// entirely empty: no `<w:pgSz>` at all (ECMA-376 requires it, but real
/// "wild" documents skip it — issue #109's original finding). Deliberately
/// NOT added to `crates/format-docx/tests/fixtures/` — issue #109 pinned
/// an explicit A4 `<w:pgSz>` onto every fixture in that shared corpus
/// specifically so `tools/roundtrip --fixtures` never silently exercises
/// the reader's fallback path (see `tools/roundtrip/src/main.rs`'s
/// `A4_SECT_PR_EXPLICIT` doc comment). This fixture exists to prove the
/// opposite: that a host's chosen `engine::DefaultPageSize`, threaded
/// through the bridge `OpenDocument.defaults` surface, reaches
/// `read_docx_with_settings`'s fallback end to end.
pub fn no_pgsz_docx(paragraph_text: &str) -> Vec<u8> {
    let ns = ns_decls();
    let document = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
         <w:document {ns}><w:body>\
         <w:p><w:r><w:t xml:space=\"preserve\">{paragraph_text}</w:t></w:r></w:p>\
         <w:sectPr/></w:body></w:document>"
    );
    let content_types = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
<Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
<Default Extension=\"xml\" ContentType=\"application/xml\"/>\
<Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/>\
</Types>";
    let dot_rels = rels(&[(
        "rId1",
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument",
        "word/document.xml",
    )]);
    let doc_rels = rels(&[]);

    let entries: Vec<(&str, &[u8])> = vec![
        ("[Content_Types].xml", content_types.as_bytes()),
        ("_rels/.rels", dot_rels.as_bytes()),
        ("word/document.xml", document.as_bytes()),
        ("word/_rels/document.xml.rels", doc_rels.as_bytes()),
    ];
    zip_entries(entries)
}

/// Deflated zip of `entries`, in order.
fn zip_entries(entries: Vec<(&str, &[u8])>) -> Vec<u8> {
    let mut buf: Vec<u8> = Vec::new();
    {
        let mut zip = ZipWriter::new(Cursor::new(&mut buf));
        let opts =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (name, bytes) in entries {
            zip.start_file(name, opts).expect("zip entry");
            zip.write_all(bytes).expect("zip write");
        }
        zip.finish().expect("zip finish");
    }
    buf
}

/// Issue #359 — paragraph 0 of [`complex_script_size_docx`]: ONE run
/// (`w:sz="22" w:szCs="28"`) mixing Latin and Arabic words, long enough to
/// wrap, so the line breaks depend on BOTH sizes.
pub const CS_SIZE_MIXED_TEXT: &str = "Latin words stay at eleven points while \
النص العربي يكبر إلى أربعة عشر نقطة inside the very same run, and every line \
break follows both sizes at once كما يفعل وورد تماما when the run mixes scripts.";

/// Issue #359 — paragraph 1: a `<w:rtl/>` run — every character, the
/// digits and the Latin word included, takes `w:szCs`.
pub const CS_SIZE_RTL_TEXT: &str = "صدر الإصدار 2026 باسم Engine للمرة الأولى";

/// Issue #359 — paragraph 2: only `<w:sz w:val="22"/>`; the Arabic takes
/// the `w:szCs` the docDefaults cascade (16 pt), never the run's Latin
/// size.
pub const CS_SIZE_CASCADE_TEXT: &str = "Only w:sz here: العربية تأخذ حجم القالب";

/// Issue #359 — the mixed-size complex-script fixture: docDefaults
/// `w:sz="24"` / `w:szCs="32"`, then the three paragraphs above (an LTR
/// mixed run at 11 / 14 pt, an RTL `<w:rtl/>` run at 11 / 14 pt, a run
/// with only `w:sz="22"`), A4 with 1-inch margins. Hand-written OOXML in
/// Word's own shape; the source of `tools/roundtrip`'s
/// `complex_script_size.docx` and the engine-wasm layout pin.
pub fn complex_script_size_docx() -> Vec<u8> {
    let document = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
         <w:document xmlns:w=\"{W_NS}\"><w:body>\
         <w:p><w:r><w:rPr><w:sz w:val=\"22\"/><w:szCs w:val=\"28\"/></w:rPr>\
         <w:t xml:space=\"preserve\">{CS_SIZE_MIXED_TEXT}</w:t></w:r></w:p>\
         <w:p><w:pPr><w:bidi/></w:pPr><w:r><w:rPr><w:sz w:val=\"22\"/><w:szCs w:val=\"28\"/>\
         <w:rtl/></w:rPr><w:t xml:space=\"preserve\">{CS_SIZE_RTL_TEXT}</w:t></w:r></w:p>\
         <w:p><w:r><w:rPr><w:sz w:val=\"22\"/></w:rPr>\
         <w:t xml:space=\"preserve\">{CS_SIZE_CASCADE_TEXT}</w:t></w:r></w:p>\
         <w:sectPr><w:pgSz w:w=\"11906\" w:h=\"16838\"/>\
         <w:pgMar w:top=\"1440\" w:right=\"1440\" w:bottom=\"1440\" w:left=\"1440\" \
         w:header=\"720\" w:footer=\"720\" w:gutter=\"0\"/></w:sectPr>\
         </w:body></w:document>"
    );
    let styles = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
         <w:styles xmlns:w=\"{W_NS}\"><w:docDefaults><w:rPrDefault><w:rPr>\
         <w:sz w:val=\"24\"/><w:szCs w:val=\"32\"/></w:rPr></w:rPrDefault>\
         </w:docDefaults></w:styles>"
    );
    let content_types = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
<Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
<Default Extension=\"xml\" ContentType=\"application/xml\"/>\
<Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/>\
<Override PartName=\"/word/styles.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml\"/>\
</Types>";
    let dot_rels = rels(&[(
        "rId1",
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument",
        "word/document.xml",
    )]);
    let doc_rels = rels(&[(
        "rId1",
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles",
        "styles.xml",
    )]);
    zip_entries(vec![
        ("[Content_Types].xml", content_types.as_bytes()),
        ("_rels/.rels", dot_rels.as_bytes()),
        ("word/document.xml", document.as_bytes()),
        ("word/styles.xml", styles.as_bytes()),
        ("word/_rels/document.xml.rels", doc_rels.as_bytes()),
    ])
}
