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
    zip_entries(entries)
}

/// Issue #352 — four single-line paragraphs on a plain A4 page, each with
/// a 3 pt red border on ONE logical/physical edge:
///
/// 0. LTR, `<w:start>`   → painted on the LEFT
/// 1. RTL (`<w:bidi/>`), `<w:start>` → painted on the RIGHT
/// 2. RTL, `<w:end>`     → painted on the LEFT
/// 3. RTL, physical `<w:left>` → painted on the LEFT (legacy spelling,
///    unchanged by #352)
pub fn paragraph_start_end_borders_docx() -> Vec<u8> {
    let ns = ns_decls();
    let bdr = |edge: &str| {
        format!(
            "<w:pBdr><w:{edge} w:val=\"single\" w:sz=\"24\" w:space=\"4\" w:color=\"FF0000\"/></w:pBdr>"
        )
    };
    let para = |edge: &str, rtl: bool, text: &str| {
        format!(
            "<w:p><w:pPr>{}{}</w:pPr><w:r><w:t>{text}</w:t></w:r></w:p>",
            bdr(edge),
            if rtl { "<w:bidi/>" } else { "" },
        )
    };
    let document = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
         <w:document {ns}><w:body>{}{}{}{}\
         <w:sectPr><w:pgSz w:w=\"11906\" w:h=\"16838\"/>\
         <w:pgMar w:top=\"1440\" w:right=\"1440\" w:bottom=\"1440\" w:left=\"1440\" w:header=\"708\" w:footer=\"708\" w:gutter=\"0\"/>\
         </w:sectPr></w:body></w:document>",
        para("start", false, "LTR start"),
        para("start", true, "RTL start"),
        para("end", true, "RTL end"),
        para("left", true, "RTL left"),
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
    let entries: Vec<(&str, &[u8])> = vec![
        ("[Content_Types].xml", content_types.as_bytes()),
        ("_rels/.rels", dot_rels.as_bytes()),
        ("word/document.xml", document.as_bytes()),
    ];
    zip_entries(entries)
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

/* ------------------------------------------------------------------ */
/* Issue #348 — hostile packages                                       */
/* ------------------------------------------------------------------ */

/// A package holding `document_xml` (a full `word/document.xml`, prolog
/// and root included) plus every `(name, bytes)` of `extra`, all deflated.
pub fn package_with_document_xml(document_xml: &str, extra: &[(&str, &[u8])]) -> Vec<u8> {
    package_with_document_xml_bytes(document_xml.as_bytes(), extra)
}

/// [`package_with_document_xml`] for raw `word/document.xml` bytes —
/// issue #358's hostile parts need bytes that are not valid UTF-8.
pub fn package_with_document_xml_bytes(document_xml: &[u8], extra: &[(&str, &[u8])]) -> Vec<u8> {
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
    let mut entries: Vec<(&str, &[u8])> = vec![
        ("[Content_Types].xml", content_types.as_bytes()),
        ("_rels/.rels", dot_rels.as_bytes()),
        ("word/document.xml", document_xml),
        ("word/_rels/document.xml.rels", doc_rels.as_bytes()),
    ];
    entries.extend_from_slice(extra);
    zip_entries(entries)
}

/// `word/document.xml` with `body` (block content) under the stock root
/// (`w`, `r`, `wp`, `a`, `pic` bound) and an empty trailing `<w:sectPr/>`.
pub fn document_xml_with_body(body: &str) -> String {
    let ns = ns_decls();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
         <w:document {ns}><w:body>{body}<w:sectPr/></w:body></w:document>"
    )
}

/// Issue #348 — `docx` with the central-directory record of `entry`
/// re-declared as `declared` uncompressed bytes through a ZIP64 extended
/// information extra field (the 32-bit field becomes the `0xFFFFFFFF`
/// sentinel). The entry's real data is untouched: only the size the
/// directory CLAIMS changes — the lie a reader must never allocate from.
pub fn with_declared_entry_size(docx: &[u8], entry: &str, declared: u64) -> Vec<u8> {
    let u16_at = |b: &[u8], i: usize| u16::from_le_bytes([b[i], b[i + 1]]) as usize;
    let u32_at = |b: &[u8], i: usize| u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
    let eocd = docx.len() - 22;
    assert_eq!(u32_at(docx, eocd), 0x0605_4b50, "end of central directory");
    let cd_size = u32_at(docx, eocd + 12) as usize;
    let cd_off = u32_at(docx, eocd + 16) as usize;
    let mut out = docx[..cd_off].to_vec();
    let mut cd = Vec::with_capacity(cd_size + 12);
    let mut p = cd_off;
    while p < cd_off + cd_size {
        assert_eq!(u32_at(docx, p), 0x0201_4b50, "central directory record");
        let name_len = u16_at(docx, p + 28);
        let extra_len = u16_at(docx, p + 30);
        let comment_len = u16_at(docx, p + 32);
        let rec_len = 46 + name_len + extra_len + comment_len;
        let mut rec = docx[p..p + rec_len].to_vec();
        if &docx[p + 46..p + 46 + name_len] == entry.as_bytes() {
            rec[24..28].copy_from_slice(&u32::MAX.to_le_bytes());
            let mut zip64 = vec![0x01, 0x00, 0x08, 0x00];
            zip64.extend_from_slice(&declared.to_le_bytes());
            rec.splice(46 + name_len..46 + name_len, zip64);
            rec[30..32].copy_from_slice(&((extra_len + 12) as u16).to_le_bytes());
        }
        cd.extend_from_slice(&rec);
        p += rec_len;
    }
    let new_cd_size = cd.len() as u32;
    out.extend_from_slice(&cd);
    let mut tail = docx[eocd..].to_vec();
    tail[12..16].copy_from_slice(&new_cd_size.to_le_bytes());
    out.extend_from_slice(&tail);
    out
}

/// Issue #348 — a one-paragraph document (`small`) whose
/// `word/document.xml` directory record declares `declared` uncompressed
/// bytes (see [`with_declared_entry_size`]).
pub fn lying_size_docx(declared: u64) -> Vec<u8> {
    let docx = package_with_document_xml(
        &document_xml_with_body("<w:p><w:r><w:t>small</w:t></w:r></w:p>"),
        &[],
    );
    with_declared_entry_size(&docx, "word/document.xml", declared)
}

/// Issue #348 — a compression bomb: a valid one-paragraph document plus a
/// `word/media/bomb.bin` part of `zero_bytes` zeros (deflate shrinks it
/// roughly 1000:1).
pub fn compressible_bomb_docx(zero_bytes: usize) -> Vec<u8> {
    let zeros = vec![0u8; zero_bytes];
    package_with_document_xml(
        &document_xml_with_body("<w:p><w:r><w:t>bomb</w:t></w:r></w:p>"),
        &[("word/media/bomb.bin", &zeros)],
    )
}

/// Issue #348 — `depth` block-level content controls nested inside each
/// other around one paragraph (`deep`).
pub fn nested_sdt_docx(depth: usize) -> Vec<u8> {
    let mut body = String::with_capacity(depth * 48 + 64);
    for _ in 0..depth {
        body.push_str("<w:sdt><w:sdtContent>");
    }
    body.push_str("<w:p><w:r><w:t>deep</w:t></w:r></w:p>");
    for _ in 0..depth {
        body.push_str("</w:sdtContent></w:sdt>");
    }
    package_with_document_xml(&document_xml_with_body(&body), &[])
}

/// Issue #351 — an `<mc:AlternateContent>` text box as Word writes one: a
/// DrawingML `wps` text box in the `<mc:Choice Requires="{requires}">`
/// (story text `choice story`) and its VML `<w:pict><v:textbox>` duplicate
/// in the `<mc:Fallback>` (story text `fallback story`). The `mc`, `wp`,
/// `a`, `wps` and `v` prefixes must be bound on the part root.
pub fn alternate_content_text_box(requires: &str) -> String {
    format!(
        concat!(
            r#"<mc:AlternateContent><mc:Choice Requires="{requires}"><w:drawing>"#,
            r#"<wp:anchor distT="0" distB="0" distL="0" distR="0" simplePos="0" relativeHeight="1" "#,
            r#"behindDoc="0" locked="0" layoutInCell="1" allowOverlap="1">"#,
            r#"<wp:simplePos x="0" y="0"/>"#,
            r#"<wp:positionH relativeFrom="column"><wp:posOffset>0</wp:posOffset></wp:positionH>"#,
            r#"<wp:positionV relativeFrom="paragraph"><wp:posOffset>0</wp:posOffset></wp:positionV>"#,
            r#"<wp:extent cx="914400" cy="457200"/><wp:wrapNone/><wp:docPr id="1" name="Text Box 1"/>"#,
            r#"<a:graphic><a:graphicData uri="http://schemas.microsoft.com/office/word/2010/wordprocessingShape">"#,
            r#"<wps:wsp><wps:txbx><w:txbxContent><w:p><w:r><w:t>choice story</w:t></w:r></w:p></w:txbxContent></wps:txbx>"#,
            r#"<wps:bodyPr/></wps:wsp></a:graphicData></a:graphic></wp:anchor></w:drawing></mc:Choice>"#,
            r##"<mc:Fallback><w:pict><v:shape id="s1" type="#_x0000_t202" style="position:absolute;width:72pt;height:36pt">"##,
            r#"<v:textbox><w:txbxContent><w:p><w:r><w:t>fallback story</w:t></w:r></w:p></w:txbxContent></v:textbox>"#,
            r#"</v:shape></w:pict></mc:Fallback></mc:AlternateContent>"#,
        ),
        requires = requires
    )
}

/// Issue #348 — `depth` tables nested cell-in-cell, the innermost cell
/// holding the paragraph `deep`; every enclosing cell ends with the
/// paragraph ECMA-376 requires after a nested table.
pub fn nested_tables_docx(depth: usize) -> Vec<u8> {
    let open = "<w:tbl><w:tblGrid><w:gridCol w:w=\"2000\"/></w:tblGrid><w:tr><w:tc>";
    let close = "<w:p/></w:tc></w:tr></w:tbl>";
    let mut body = String::new();
    for _ in 0..depth {
        body.push_str(open);
    }
    body.push_str("<w:p><w:r><w:t>deep</w:t></w:r></w:p>");
    for _ in 0..depth {
        body.push_str(close);
    }
    body.push_str("<w:p/>");
    package_with_document_xml(&document_xml_with_body(&body), &[])
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

/// A minimal package (no styles part) whose `<w:body>` holds `body` plus an
/// A4 `<w:sectPr>`. Downstream crates' tests build small hand-written
/// documents with it.
pub fn docx_with_body(body: &str) -> Vec<u8> {
    let document = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
         <w:document xmlns:w=\"{W_NS}\"><w:body>{body}\
         <w:sectPr><w:pgSz w:w=\"11906\" w:h=\"16838\"/>\
         <w:pgMar w:top=\"1440\" w:right=\"1440\" w:bottom=\"1440\" w:left=\"1440\" \
         w:header=\"720\" w:footer=\"720\" w:gutter=\"0\"/></w:sectPr>\
         </w:body></w:document>"
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
    zip_entries(vec![
        ("[Content_Types].xml", content_types.as_bytes()),
        ("_rels/.rels", dot_rels.as_bytes()),
        ("word/document.xml", document.as_bytes()),
        ("word/_rels/document.xml.rels", doc_rels.as_bytes()),
    ])
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

/* ============================================================
Issue #355 — theme fixtures. Our own XML (no part copied from a
Word package): the structure of Word's stock "Office" theme (2013+)
with its documented font and colour values.
============================================================ */

/// Relationship type of a theme part.
pub const THEME_REL: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/theme";
/// Relationship type of the settings part.
pub const SETTINGS_REL: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/settings";

/// A theme part with the given body (`minor`) / heading (`major`) fonts —
/// `(latin, arab)` typefaces per collection — laid out the way Word writes
/// its stock theme (empty `a:ea` / `a:cs`, the complex-script and East
/// Asian faces per script), with Word's stock colour scheme.
/// [`word_default_theme_xml`] is the stock instance.
pub fn theme_xml(minor: (&str, &str), major: (&str, &str)) -> String {
    let collection = |tag: &str, (latin, arab): (&str, &str), cjk: &str| {
        format!(
            "<a:{tag}><a:latin typeface=\"{latin}\"/><a:ea typeface=\"\"/><a:cs typeface=\"\"/>\
             <a:font script=\"Jpan\" typeface=\"{cjk}\"/><a:font script=\"Arab\" typeface=\"{arab}\"/>\
             <a:font script=\"Hebr\" typeface=\"{arab}\"/><a:font script=\"Thai\" typeface=\"Tahoma\"/></a:{tag}>"
        )
    };
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n\
         <a:theme xmlns:a=\"{A_NS}\" name=\"Office Theme\"><a:themeElements>\
         <a:clrScheme name=\"Office\">\
         <a:dk1><a:sysClr val=\"windowText\" lastClr=\"000000\"/></a:dk1>\
         <a:lt1><a:sysClr val=\"window\" lastClr=\"FFFFFF\"/></a:lt1>\
         <a:dk2><a:srgbClr val=\"44546A\"/></a:dk2><a:lt2><a:srgbClr val=\"E7E6E6\"/></a:lt2>\
         <a:accent1><a:srgbClr val=\"4472C4\"/></a:accent1><a:accent2><a:srgbClr val=\"ED7D31\"/></a:accent2>\
         <a:accent3><a:srgbClr val=\"A5A5A5\"/></a:accent3><a:accent4><a:srgbClr val=\"FFC000\"/></a:accent4>\
         <a:accent5><a:srgbClr val=\"5B9BD5\"/></a:accent5><a:accent6><a:srgbClr val=\"70AD47\"/></a:accent6>\
         <a:hlink><a:srgbClr val=\"0563C1\"/></a:hlink><a:folHlink><a:srgbClr val=\"954F72\"/></a:folHlink>\
         </a:clrScheme><a:fontScheme name=\"Office\">{major}{minor}</a:fontScheme>\
         <a:fmtScheme name=\"Office\"><a:fillStyleLst><a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill>\
         </a:fillStyleLst></a:fmtScheme></a:themeElements><a:objectDefaults/><a:extraClrSchemeLst/></a:theme>",
        major = collection("majorFont", major, "游ゴシック Light"),
        minor = collection("minorFont", minor, "游明朝"),
    )
}

/// Word's stock theme fonts: Calibri / Calibri Light for Latin, Arial /
/// Times New Roman for Arabic (`+Body CS` / `+Headings CS`).
pub fn word_default_theme_xml() -> String {
    theme_xml(("Calibri", "Arial"), ("Calibri Light", "Times New Roman"))
}

/// The `<w:settings>` Word writes beside a theme: `<w:themeFontLang>`
/// (here with an Arabic `w:bidi`) and the identity `<w:clrSchemeMapping>`.
pub fn theme_settings_xml() -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n\
         <w:settings xmlns:w=\"{W_NS}\"><w:zoom w:percent=\"100\"/><w:defaultTabStop w:val=\"720\"/>\
         <w:themeFontLang w:val=\"en-US\" w:bidi=\"ar-SA\"/>\
         <w:clrSchemeMapping w:bg1=\"light1\" w:t1=\"dark1\" w:bg2=\"light2\" w:t2=\"dark2\" \
         w:accent1=\"accent1\" w:accent2=\"accent2\" w:accent3=\"accent3\" w:accent4=\"accent4\" \
         w:accent5=\"accent5\" w:accent6=\"accent6\" w:hyperlink=\"hyperlink\" \
         w:followedHyperlink=\"followedHyperlink\"/></w:settings>"
    )
}

/// Add a theme part (and optionally a settings part) to a package such as
/// a `build_minimal_docx` output: the parts are appended under Word's
/// names, related from `word/_rels/document.xml.rels` and declared in
/// `[Content_Types].xml`. Every other entry keeps its bytes and order.
pub fn with_theme_parts(docx: &[u8], theme_xml: &str, settings_xml: Option<&str>) -> Vec<u8> {
    use std::io::Read;
    let mut zin = zip::ZipArchive::new(Cursor::new(docx)).expect("read package");
    let mut entries: Vec<(String, Vec<u8>)> = Vec::with_capacity(zin.len() + 2);
    for i in 0..zin.len() {
        let mut f = zin.by_index(i).expect("zip entry");
        let mut buf = Vec::new();
        f.read_to_end(&mut buf).expect("read entry");
        entries.push((f.name().to_owned(), buf));
    }
    let insert_before = |xml: &[u8], close: &str, add: &str| -> Vec<u8> {
        let xml = std::str::from_utf8(xml).expect("utf8 part");
        let at = xml.rfind(close).expect("closing tag");
        format!("{}{add}{}", &xml[..at], &xml[at..]).into_bytes()
    };
    for (name, buf) in &mut entries {
        if name == "word/_rels/document.xml.rels" {
            let mut add = format!(
                "<Relationship Id=\"rIdTheme1\" Type=\"{THEME_REL}\" Target=\"theme/theme1.xml\"/>"
            );
            if settings_xml.is_some() {
                add.push_str(&format!(
                    "<Relationship Id=\"rIdSettings1\" Type=\"{SETTINGS_REL}\" Target=\"settings.xml\"/>"
                ));
            }
            *buf = insert_before(buf, "</Relationships>", &add);
        } else if name == "[Content_Types].xml" {
            let mut add = String::from(
                "<Override PartName=\"/word/theme/theme1.xml\" \
                 ContentType=\"application/vnd.openxmlformats-officedocument.theme+xml\"/>",
            );
            if settings_xml.is_some() {
                add.push_str(
                    "<Override PartName=\"/word/settings.xml\" \
                     ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.settings+xml\"/>",
                );
            }
            *buf = insert_before(buf, "</Types>", &add);
        }
    }
    entries.push((
        "word/theme/theme1.xml".into(),
        theme_xml.as_bytes().to_vec(),
    ));
    if let Some(s) = settings_xml {
        entries.push(("word/settings.xml".into(), s.as_bytes().to_vec()));
    }
    let mut out: Vec<u8> = Vec::new();
    {
        let mut zip = ZipWriter::new(Cursor::new(&mut out));
        let opts =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (name, bytes) in &entries {
            zip.start_file(name.as_str(), opts).expect("zip entry");
            zip.write_all(bytes).expect("zip write");
        }
        zip.finish().expect("zip finish");
    }
    out
}

/// Text of [`theme_fonts_docx`]'s paragraphs, in order.
pub const THEME_FIXTURE_TEXTS: [&str; 4] = [
    "Theme heading عنوان السمة",
    "Body text in the minor font, explicit Amiri, then the theme again.",
    "نص عربي بخط السمة بخط العناوين.",
    "Accent two and soft text.",
];

/// Issue #355 — a Word default-template document over `theme_xml`, built
/// with [`crate::writer::build_minimal_docx`] + [`with_theme_parts`]:
///
/// * docDefaults bind every slot to the body font (`minorHAnsi` ×
///   ascii / hAnsi, `minorEastAsia`, `minorBidi`) at 11 pt — Word's own
///   `w:rPrDefault`, so body text names no font at all;
/// * `Heading1` rebinds them to the heading font (`major*`), bold 16 pt,
///   coloured `accent1` shaded `BF` (cached `2F5496`);
/// * paragraph 0 is a mixed Latin + Arabic heading; paragraph 1 Latin
///   body text with one run naming Amiri explicitly (`w:ascii` / `w:hAnsi`
///   / `w:cs`); paragraph 2 an RTL Arabic paragraph with one run rebinding
///   only its complex-script slot (`w:cstheme="majorBidi"`); paragraph 3
///   theme-coloured runs (`accent2`; `text1` tinted `A6`, cached
///   `595959`).
///
/// Settings: `<w:themeFontLang w:bidi="ar-SA">` and the identity
/// `<w:clrSchemeMapping>` ([`theme_settings_xml`]).
pub fn theme_fonts_docx(theme_xml: &str) -> Vec<u8> {
    use engine::{FontBinding, RunFontBindings, SpanStyle, StyleRun, ThemeColorRef};
    let bind = |v: &str| Some(FontBinding::Theme(v.into()));
    let slots = |latin: &str, ea: &str, cs: &str| {
        Some(Box::new(RunFontBindings {
            ascii: bind(latin),
            h_ansi: bind(latin),
            east_asia: bind(ea),
            cs: bind(cs),
        }))
    };
    let theme_color = |color: &str, tint: Option<&str>, shade: Option<&str>| {
        Some(Box::new(ThemeColorRef {
            color: color.into(),
            tint: tint.map(Into::into),
            shade: shade.map(Into::into),
        }))
    };
    /* A run over the first occurrence of `needle` in `text`. */
    let run = |text: &str, needle: &str, style: SpanStyle| {
        let start = text.find(needle).expect("needle") as u32;
        StyleRun {
            start,
            end: start + needle.len() as u32,
            style,
        }
    };
    let [heading, body, arabic, colours] = THEME_FIXTURE_TEXTS;
    let paras = [
        engine::Paragraph {
            text: heading.into(),
            style_id: Some("Heading1".into()),
            ..Default::default()
        },
        engine::Paragraph {
            text: body.into(),
            /* Issue #249 — both script slots, as the toolbar names them
            (`w:ascii` / `w:hAnsi` / `w:cs`). */
            spans: vec![run(
                body,
                "explicit Amiri",
                SpanStyle {
                    font_family: Some(engine::FontFamily::Amiri),
                    ..Default::default()
                }
                .with_cs_twins(),
            )],
            ..Default::default()
        },
        engine::Paragraph {
            text: arabic.into(),
            props: engine::ParaProperties {
                direction: Some(engine::TextDirection::Rtl),
                ..Default::default()
            },
            spans: vec![run(
                arabic,
                "بخط العناوين",
                SpanStyle {
                    font_bindings: Some(Box::new(RunFontBindings {
                        cs: bind("majorBidi"),
                        ..Default::default()
                    })),
                    ..Default::default()
                },
            )],
            ..Default::default()
        },
        engine::Paragraph {
            text: colours.into(),
            spans: vec![
                run(
                    colours,
                    "Accent two",
                    SpanStyle {
                        color: Some([0xED, 0x7D, 0x31, 255]),
                        color_theme: theme_color("accent2", None, None),
                        ..Default::default()
                    },
                ),
                run(
                    colours,
                    "soft text",
                    SpanStyle {
                        color: Some([0x59, 0x59, 0x59, 255]),
                        color_theme: theme_color("text1", Some("A6"), None),
                        ..Default::default()
                    },
                ),
            ],
            ..Default::default()
        },
    ];
    let mut doc = engine::DocumentTree::from_rich_paragraphs(paras);
    /* Issues #359 / #104 — every size / bold below names both script
    slots (`w:sz` + `w:szCs`, `w:b` + `w:bCs`), as Word's template does. */
    doc.style_run_defaults = SpanStyle {
        font_size: Some(11.0),
        font_size_cs: Some(11.0),
        font_theme: Some("minorHAnsi".into()),
        font_bindings: slots("minorHAnsi", "minorEastAsia", "minorBidi"),
        ..Default::default()
    };
    doc.styles.insert(
        "Heading1".into(),
        engine::ParagraphStyle {
            id: "Heading1".into(),
            name: "heading 1".into(),
            run: SpanStyle {
                bold: Some(true),
                bold_cs: Some(true),
                font_size: Some(16.0),
                font_size_cs: Some(16.0),
                color: Some([0x2F, 0x54, 0x96, 255]),
                color_theme: theme_color("accent1", None, Some("BF")),
                font_theme: Some("majorHAnsi".into()),
                font_bindings: slots("majorHAnsi", "majorEastAsia", "majorBidi"),
                ..Default::default()
            },
            ..Default::default()
        },
    );
    /* `build_minimal_docx` writes `word/styles.xml` from the table only
    for a dirty one. */
    doc.styles_dirty = true;
    let base = crate::writer::build_minimal_docx(&doc).expect("minimal package");
    with_theme_parts(&base, theme_xml, Some(&theme_settings_xml()))
}

/// [`theme_fonts_docx`] over Word's stock theme ([`word_default_theme_xml`]):
/// Calibri / Calibri Light, Arabic in Arial / Times New Roman.
pub fn theme_word_default_docx() -> Vec<u8> {
    theme_fonts_docx(&word_default_theme_xml())
}

/// [`theme_fonts_docx`] over a theme whose faces the editor ships, so the
/// resolution is visible on canvas: body Latin in Liberation Sans, body
/// Arabic in Noto Naskh Arabic, headings in Amiri (both scripts).
pub fn theme_loaded_faces_docx() -> Vec<u8> {
    theme_fonts_docx(&theme_xml(
        ("Liberation Sans", "Noto Naskh Arabic"),
        ("Amiri", "Amiri"),
    ))
}

/// Issue #395 — the `Title` style of Word 2007's default template, as the
/// corpus carries it (`docx4j-sample-docs/toc.docx`, `Headers.docx`,
/// `sample-docx.docx`, `Symbols.docx`, `ArialUnicodeMS.docx`): a bottom
/// border in accent 1 (theme references kept; the fixture ships no theme
/// part, so `w:color` is what paints).
pub const WORD_TITLE_STYLE: &str = "<w:style w:type=\"paragraph\" w:styleId=\"Title\"><w:name w:val=\"Title\"/>\
<w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/><w:link w:val=\"TitleChar\"/><w:qFormat/><w:rsid w:val=\"00C33400\"/>\
<w:pPr><w:pBdr><w:bottom w:val=\"single\" w:sz=\"8\" w:space=\"4\" w:color=\"4F81BD\" w:themeColor=\"accent1\"/></w:pBdr>\
<w:spacing w:after=\"300\"/><w:contextualSpacing/></w:pPr>\
<w:rPr><w:color w:val=\"17365D\" w:themeColor=\"text2\" w:themeShade=\"BF\"/><w:spacing w:val=\"5\"/><w:kern w:val=\"28\"/>\
<w:sz w:val=\"52\"/><w:szCs w:val=\"52\"/></w:rPr></w:style>";

/// Issue #395 — paragraph borders defined on STYLES, one paragraph each
/// (A4, 72 pt margins; red `FF0000` and blue `0000FF` 3 pt strokes):
///
/// 0. `Title` ([`WORD_TITLE_STYLE`]) → bottom border, `4F81BD`
/// 1. `Box` (basedOn `BoxBase`: top + bottom red; adds left red) → top,
///    bottom and left — per-edge `basedOn` cascade
/// 2. `Box` + direct `<w:top w:val="nil"/>` → bottom and left only
/// 3. `StartRule` (`<w:start>` blue, no bidi) → blue LEFT
/// 4. `StartRule` + direct `<w:bidi/>` → blue RIGHT (the style's logical
///    edge resolves against the PARAGRAPH's direction)
/// 5. `RtlRule` (`<w:bidi/>` + `<w:start>` blue) → blue RIGHT
/// 6. `RtlRule` + direct `<w:bidi w:val="0"/>` → blue LEFT
pub fn styled_paragraph_borders_docx() -> Vec<u8> {
    let ns = ns_decls();
    let edge = |name: &str, color: &str| {
        format!("<w:{name} w:val=\"single\" w:sz=\"24\" w:space=\"4\" w:color=\"{color}\"/>")
    };
    let styles = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
         <w:styles xmlns:w=\"{W_NS}\">\
         <w:style w:type=\"paragraph\" w:default=\"1\" w:styleId=\"Normal\"><w:name w:val=\"Normal\"/><w:qFormat/></w:style>\
         {WORD_TITLE_STYLE}\
         <w:style w:type=\"paragraph\" w:styleId=\"BoxBase\"><w:name w:val=\"Box Base\"/><w:basedOn w:val=\"Normal\"/>\
         <w:pPr><w:pBdr>{}{}</w:pBdr></w:pPr></w:style>\
         <w:style w:type=\"paragraph\" w:styleId=\"Box\"><w:name w:val=\"Box\"/><w:basedOn w:val=\"BoxBase\"/>\
         <w:pPr><w:pBdr>{}</w:pBdr></w:pPr></w:style>\
         <w:style w:type=\"paragraph\" w:styleId=\"StartRule\"><w:name w:val=\"Start Rule\"/><w:basedOn w:val=\"Normal\"/>\
         <w:pPr><w:pBdr>{}</w:pBdr></w:pPr></w:style>\
         <w:style w:type=\"paragraph\" w:styleId=\"RtlRule\"><w:name w:val=\"RTL Rule\"/><w:basedOn w:val=\"Normal\"/>\
         <w:pPr><w:pBdr>{}</w:pBdr><w:bidi/></w:pPr></w:style>\
         </w:styles>",
        edge("top", "FF0000"),
        edge("bottom", "FF0000"),
        edge("left", "FF0000"),
        edge("start", "0000FF"),
        edge("start", "0000FF"),
    );
    let para = |style: &str, direct: &str, text: &str| {
        format!(
            "<w:p><w:pPr><w:pStyle w:val=\"{style}\"/>{direct}</w:pPr><w:r><w:t>{text}</w:t></w:r></w:p>"
        )
    };
    let document = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
         <w:document {ns}><w:body>{}{}{}{}{}{}{}\
         <w:sectPr><w:pgSz w:w=\"11906\" w:h=\"16838\"/>\
         <w:pgMar w:top=\"1440\" w:right=\"1440\" w:bottom=\"1440\" w:left=\"1440\" w:header=\"708\" w:footer=\"708\" w:gutter=\"0\"/>\
         </w:sectPr></w:body></w:document>",
        para("Title", "", "Title"),
        para("Box", "", "boxed"),
        para("Box", "<w:pBdr><w:top w:val=\"nil\"/></w:pBdr>", "no top"),
        para("StartRule", "", "LTR start"),
        para("StartRule", "<w:bidi/>", "RTL start"),
        para("RtlRule", "", "RTL style start"),
        para("RtlRule", "<w:bidi w:val=\"0\"/>", "LTR override"),
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
        ("word/_rels/document.xml.rels", doc_rels.as_bytes()),
        ("word/document.xml", document.as_bytes()),
        ("word/styles.xml", styles.as_bytes()),
    ])
}

/// Issue #367 — the paragraph texts of [`section_break_revision_docx`].
pub const SECTION_BREAK_TEXTS: [&str; 3] = [
    "The first section ends here",
    "The second section",
    "and its last paragraph",
];

/// Issue #367 — a section break under review, in Word's shape (our own
/// XML): paragraph 0's mark carries the first section's `<w:sectPr>`
/// (a small 5.5 × 4 in page, a default header `rIdH1` "Header A", a
/// first-page header `rIdH2` "Header A first" with `<w:titlePg/>`) and
/// is a tracked DELETION (`<w:pPr><w:rPr><w:del/>`). The final section
/// (A4) owns no header reference: it inherits both (absence =
/// link-to-previous). Accepting the deletion makes paragraph 0 part of
/// the final section; its empty header slots take `rIdH1` / `rIdH2`.
/// With `inserted`, the mark is a tracked INSERTION instead (a tracked
/// section break: rejecting removes it). Source of `tools/roundtrip`'s
/// `section_break_revision.docx` (step 54).
pub fn section_break_revision_docx(inserted: bool) -> Vec<u8> {
    let ns = ns_decls();
    let [t0, t1, t2] = SECTION_BREAK_TEXTS;
    let tag = if inserted { "w:ins" } else { "w:del" };
    let document = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
         <w:document {ns}><w:body>\
         <w:p w:rsidR=\"00A1B2C3\"><w:pPr><w:rPr><{tag} w:id=\"1\" w:author=\"Author\" \
         w:date=\"2026-01-02T00:00:00Z\"/></w:rPr><w:sectPr w:rsidR=\"00A1B2C3\">\
         <w:headerReference w:type=\"default\" r:id=\"rIdH1\"/>\
         <w:headerReference w:type=\"first\" r:id=\"rIdH2\"/>\
         <w:pgSz w:w=\"7920\" w:h=\"5760\"/>\
         <w:pgMar w:top=\"720\" w:right=\"720\" w:bottom=\"720\" w:left=\"720\" \
         w:header=\"360\" w:footer=\"360\" w:gutter=\"0\"/><w:titlePg/></w:sectPr></w:pPr>\
         <w:r><w:t>{t0}</w:t></w:r></w:p>\
         <w:p><w:r><w:t>{t1}</w:t></w:r></w:p>\
         <w:p><w:r><w:t>{t2}</w:t></w:r></w:p>\
         <w:sectPr><w:pgSz w:w=\"11906\" w:h=\"16838\"/>\
         <w:pgMar w:top=\"1440\" w:right=\"1440\" w:bottom=\"1440\" w:left=\"1440\" \
         w:header=\"720\" w:footer=\"720\" w:gutter=\"0\"/></w:sectPr>\
         </w:body></w:document>"
    );
    let header = |text: &str| {
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
             <w:hdr {ns}><w:p><w:r><w:t>{text}</w:t></w:r></w:p></w:hdr>"
        )
    };
    let (h1, h2) = (header("Header A"), header("Header A first"));
    let content_types = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
<Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
<Default Extension=\"xml\" ContentType=\"application/xml\"/>\
<Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/>\
<Override PartName=\"/word/header1.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml\"/>\
<Override PartName=\"/word/header2.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml\"/>\
</Types>";
    let dot_rels = rels(&[(
        "rId1",
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument",
        "word/document.xml",
    )]);
    let doc_rels = rels(&[
        ("rIdH1", HEADER_REL, "header1.xml"),
        ("rIdH2", HEADER_REL, "header2.xml"),
    ]);
    zip_entries(vec![
        ("[Content_Types].xml", content_types.as_bytes()),
        ("_rels/.rels", dot_rels.as_bytes()),
        ("word/document.xml", document.as_bytes()),
        ("word/_rels/document.xml.rels", doc_rels.as_bytes()),
        ("word/header1.xml", h1.as_bytes()),
        ("word/header2.xml", h2.as_bytes()),
    ])
}

/// Issue #365 — the cell texts of [`tracked_table_rows_docx`], row by row.
pub const TRACKED_ROWS_CELLS: [[&str; 2]; 3] = [
    ["kept A", "kept B"],
    ["gone A", "gone B"],
    ["new A", "new B"],
];

/// Issue #365 — a table under review, in Word's shape (no corpus document
/// with a tracked row was at hand; this is our own XML): row 0 untouched
/// but carrying a `<w:trPrChange>` (and the table a `<w:tblPrChange>`) —
/// property history the model keeps as verbatim grab-bag bytes; row 1 a
/// tracked row DELETION (`<w:trPr><w:del/>`, its cells' text in `<w:del>`
/// runs and their marks deleted); row 2 a tracked row INSERTION
/// (`<w:trPr><w:ins/>`, inserted text and marks). Source of
/// `tools/roundtrip`'s `tracked_table_rows.docx` (step 53).
pub fn tracked_table_rows_docx() -> Vec<u8> {
    const D1: &str = "w:author=\"Author\" w:date=\"2026-01-01T00:00:00Z\"";
    const D2: &str = "w:author=\"Author\" w:date=\"2026-01-02T00:00:00Z\"";
    const D3: &str = "w:author=\"Author\" w:date=\"2026-01-03T00:00:00Z\"";
    let cell = |inner: &str| {
        format!("<w:tc><w:tcPr><w:tcW w:w=\"4675\" w:type=\"dxa\"/></w:tcPr>{inner}</w:tc>")
    };
    let [[k0, k1], [g0, g1], [n0, n1]] = TRACKED_ROWS_CELLS;
    let kept = |t: &str| cell(&format!("<w:p><w:r><w:t>{t}</w:t></w:r></w:p>"));
    let gone = |t: &str, mark: u32| {
        cell(&format!(
            "<w:p><w:pPr><w:rPr><w:del w:id=\"{mark}\" {D2}/></w:rPr></w:pPr>\
             <w:del w:id=\"{}\" {D2}><w:r><w:delText>{t}</w:delText></w:r></w:del></w:p>",
            mark + 1
        ))
    };
    let new = |t: &str, mark: u32| {
        cell(&format!(
            "<w:p><w:pPr><w:rPr><w:ins w:id=\"{mark}\" {D3}/></w:rPr></w:pPr>\
             <w:ins w:id=\"{}\" {D3}><w:r><w:t>{t}</w:t></w:r></w:ins></w:p>",
            mark + 1
        ))
    };
    let body = format!(
        "<w:p w:rsidR=\"00D0A1B2\"><w:r><w:t>Rows under review</w:t></w:r></w:p>\
         <w:tbl><w:tblPr><w:tblStyle w:val=\"TableGrid\"/><w:tblW w:w=\"0\" w:type=\"auto\"/>\
         <w:tblLook w:val=\"04A0\"/><w:tblPrChange w:id=\"1\" {D1}><w:tblPr>\
         <w:tblW w:w=\"5000\" w:type=\"pct\"/></w:tblPr></w:tblPrChange></w:tblPr>\
         <w:tblGrid><w:gridCol w:w=\"4675\"/><w:gridCol w:w=\"4675\"/></w:tblGrid>\
         <w:tr w:rsidR=\"00D0A1B2\" w:rsidTr=\"00E3F4A5\"><w:trPr><w:trHeight w:val=\"400\"/>\
         <w:trPrChange w:id=\"2\" {D1}><w:trPr/></w:trPrChange></w:trPr>{}{}</w:tr>\
         <w:tr w:rsidR=\"00D0A1B2\" w:rsidTr=\"00E3F4A5\"><w:trPr><w:del w:id=\"3\" {D2}/></w:trPr>{}{}</w:tr>\
         <w:tr w:rsidR=\"00F6A7B8\"><w:trPr><w:ins w:id=\"8\" {D3}/></w:trPr>{}{}</w:tr>\
         </w:tbl><w:p><w:r><w:t>after</w:t></w:r></w:p>",
        kept(k0),
        kept(k1),
        gone(g0, 4),
        gone(g1, 6),
        new(n0, 9),
        new(n1, 11),
    );
    docx_with_body(&body)
}

/* Issues #335 / #357 / #326 — run-content elements and hyphenation. */
#[path = "test_fixtures_run_content.rs"]
mod run_content;
pub use run_content::*;
