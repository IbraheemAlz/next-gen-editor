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
        ("word/document.xml", document_xml.as_bytes()),
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
