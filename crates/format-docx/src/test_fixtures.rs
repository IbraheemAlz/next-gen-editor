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
            spans: vec![run(
                body,
                "explicit Amiri",
                SpanStyle {
                    font_family: Some(engine::FontFamily::Amiri),
                    ..Default::default()
                },
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
    doc.style_run_defaults = SpanStyle {
        font_size: Some(11.0),
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
                font_size: Some(16.0),
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
