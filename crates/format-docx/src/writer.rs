//! `.docx` writer: serialize `DocumentTree` → `word/document.xml`, repack
//! into a ZIP archive. When called via `write_docx(archive, doc)`, the
//! original archive's other entries are written back verbatim; only
//! `word/document.xml` is regenerated.

use crate::error::DocxError;
use crate::opc::archive::{DOC_XML, DocxArchive};
use engine::{
    Alignment, DocumentTree, FontFamily, LineHeight, ParaProperties, Paragraph, SpanStyle,
    TextDirection,
};
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

fn family_docx_name(f: FontFamily) -> &'static str {
    match f {
        FontFamily::Amiri => "Amiri",
        FontFamily::LiberationSans => "Liberation Sans",
        FontFamily::NotoNaskhArabic => "Noto Naskh Arabic",
    }
}

/// XML-escape character data (`&`, `<`, `>`) into `out`. Quotes don't matter
/// inside character data.
fn push_escaped(text: &str, out: &mut String) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
}

/// Emit a `<w:rPr>` block for `style`, or nothing when it is the default.
/// Children follow the CT_RPr schema order (rFonts, b, i, strike, color, u,
/// shd). A background colour is written as `<w:shd w:fill>` — that carries
/// arbitrary hex, where `<w:highlight>` is limited to a named palette.
fn emit_rpr(style: &SpanStyle, out: &mut String) {
    if *style == SpanStyle::default() {
        return;
    }
    out.push_str("<w:rPr>");
    if let Some(f) = style.font_family {
        let n = family_docx_name(f);
        out.push_str("<w:rFonts w:ascii=\"");
        out.push_str(n);
        out.push_str("\" w:hAnsi=\"");
        out.push_str(n);
        out.push_str("\" w:cs=\"");
        out.push_str(n);
        out.push_str("\"/>");
    }
    if style.bold == Some(true) {
        out.push_str("<w:b/>");
    }
    if style.italic == Some(true) {
        out.push_str("<w:i/>");
    }
    if style.strike == Some(true) {
        out.push_str("<w:strike/>");
    }
    if let Some([r, g, b, _]) = style.color {
        out.push_str(&format!("<w:color w:val=\"{r:02X}{g:02X}{b:02X}\"/>"));
    }
    if style.underline == Some(true) {
        out.push_str("<w:u w:val=\"single\"/>");
    }
    if let Some([r, g, b, _]) = style.bg_color {
        out.push_str(&format!(
            "<w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"{r:02X}{g:02X}{b:02X}\"/>"
        ));
    }
    out.push_str("</w:rPr>");
}

/// Serialize one run: `<w:r>[<w:rPr>…]<w:t xml:space="preserve">…</w:t></w:r>`.
/// `xml:space="preserve"` keeps leading/trailing whitespace; matters for
/// Arabic trailing-shadda etc.
fn serialize_run(text: &str, style: &SpanStyle, out: &mut String) {
    out.push_str("<w:r>");
    emit_rpr(style, out);
    out.push_str("<w:t xml:space=\"preserve\">");
    push_escaped(text, out);
    out.push_str("</w:t></w:r>");
}

/// `<w:jc w:val="…"/>` token for an `Alignment`. Word emits writing-direction-
/// relative `start` / `end` in modern docs; we match that.
fn jc_val(a: Alignment) -> &'static str {
    match a {
        Alignment::Start => "start",
        Alignment::End => "end",
        Alignment::Center => "center",
        Alignment::Justify => "both",
    }
}

/// Emit `<w:pPr>` for `props`, or nothing when it is the default — a default
/// `ParaProperties` must produce an empty pPr to keep the Phase 1 plain
/// fixtures byte-stable. Children follow the CT_PPr schema order: keepNext,
/// keepLines, pageBreakBefore, spacing, ind, jc, bidi.
fn emit_ppr(props: &ParaProperties, out: &mut String) {
    if *props == ParaProperties::default() {
        return;
    }
    out.push_str("<w:pPr>");
    if props.keep_next {
        out.push_str("<w:keepNext/>");
    }
    if props.keep_lines {
        out.push_str("<w:keepLines/>");
    }
    if props.page_break_before {
        out.push_str("<w:pageBreakBefore/>");
    }
    /* `<w:spacing>` carries both before/after gaps and the line rule. We
    omit the element entirely when none of its attributes are set. */
    let has_gap = props.spacing.before_twips != 0 || props.spacing.after_twips != 0;
    if has_gap || props.line_height.is_some() {
        out.push_str("<w:spacing");
        if props.spacing.before_twips != 0 {
            out.push_str(&format!(" w:before=\"{}\"", props.spacing.before_twips));
        }
        if props.spacing.after_twips != 0 {
            out.push_str(&format!(" w:after=\"{}\"", props.spacing.after_twips));
        }
        if let Some(lh) = props.line_height {
            let (line, rule) = match lh {
                LineHeight::Auto { twips } => (twips, "auto"),
                LineHeight::Exact { twips } => (twips, "exact"),
                LineHeight::AtLeast { twips } => (twips, "atLeast"),
            };
            out.push_str(&format!(" w:line=\"{line}\" w:lineRule=\"{rule}\""));
        }
        out.push_str("/>");
    }
    let ind = &props.indent;
    if ind.start_twips != 0
        || ind.end_twips != 0
        || ind.first_line_twips != 0
        || ind.hanging_twips != 0
    {
        out.push_str("<w:ind");
        if ind.start_twips != 0 {
            out.push_str(&format!(" w:start=\"{}\"", ind.start_twips));
        }
        if ind.end_twips != 0 {
            out.push_str(&format!(" w:end=\"{}\"", ind.end_twips));
        }
        if ind.first_line_twips != 0 {
            out.push_str(&format!(" w:firstLine=\"{}\"", ind.first_line_twips));
        }
        if ind.hanging_twips != 0 {
            out.push_str(&format!(" w:hanging=\"{}\"", ind.hanging_twips));
        }
        out.push_str("/>");
    }
    if let Some(a) = props.alignment {
        out.push_str(&format!("<w:jc w:val=\"{}\"/>", jc_val(a)));
    }
    if let Some(d) = props.direction {
        match d {
            TextDirection::Rtl => out.push_str("<w:bidi/>"),
            /* LTR is the default; we still emit `<w:bidi w:val="false"/>` so
            an explicit user override round-trips faithfully. */
            TextDirection::Ltr => out.push_str("<w:bidi w:val=\"false\"/>"),
        }
    }
    out.push_str("</w:pPr>");
}

/// Serialize one paragraph. A span-free paragraph with default `props` emits
/// a single default run — byte-identical to the pre-Phase-2 writer, so the
/// round-trip harness's plain fixtures see no drift. A styled paragraph
/// emits one `<w:r>` per maximal (range, style) segment, the default-styled
/// gaps included.
fn serialize_paragraph(para: &Paragraph, out: &mut String) {
    out.push_str("<w:p>");
    emit_ppr(&para.props, out);
    if para.spans.is_empty() {
        serialize_run(&para.text, &SpanStyle::default(), out);
    } else {
        let len = para.text.len();
        let mut cursor = 0usize;
        for run in &para.spans {
            let (rs, re) = (run.start as usize, run.end as usize);
            if rs > cursor {
                serialize_run(&para.text[cursor..rs], &SpanStyle::default(), out);
            }
            if re > rs {
                serialize_run(&para.text[rs..re], &run.style, out);
            }
            cursor = re;
        }
        if cursor < len {
            serialize_run(&para.text[cursor..len], &SpanStyle::default(), out);
        }
    }
    out.push_str("</w:p>");
}

/// Phase 3 passthrough optimisation. A paragraph that was loaded from a
/// `.docx` (so `source_xml` is `Some`) and has not been mutated by the
/// engine (`dirty == false`) is re-emitted **verbatim** from its captured
/// source bytes. Every other paragraph regenerates via
/// `serialize_paragraph` — the lossy-vs-stylesheet but engine-state-
/// preserving fallback path.
///
/// This is what keeps stylesheet-heavy real-world `.docx` files
/// byte-stable on round-trip: paragraphs the user didn't touch carry
/// their original `<w:pStyle>` / `<w:rsidR>` / `<w:proofErr>` markup
/// unmodified.
fn emit_paragraph(para: &Paragraph, out: &mut String) {
    if !para.dirty
        && let Some(raw) = &para.source_xml
    {
        /* Source bytes are valid UTF-8 — they came out of a well-formed
        XML document that the reader already round-tripped through
        `quick_xml`'s decoder. Defensively fall back on regenerate if the
        bytes aren't UTF-8. */
        match std::str::from_utf8(raw) {
            Ok(s) => out.push_str(s),
            Err(_) => serialize_paragraph(para, out),
        }
    } else {
        serialize_paragraph(para, out);
    }
}

fn build_document_xml(doc: &DocumentTree) -> String {
    let mut out = String::with_capacity(2048);
    out.push_str(DOC_XML_HEADER);
    for para in &doc.paragraphs {
        emit_paragraph(para, &mut out);
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
    use crate::opc::archive::read_docx;
    use engine::{ParaProperties, Paragraph, StyleRun};

    #[test]
    fn round_trip_run_properties() {
        /* "hello world": bold + red over "hello", underline over "world". */
        let bold_red = SpanStyle {
            bold: Some(true),
            color: Some([255, 0, 0, 255]),
            ..Default::default()
        };
        let ul = SpanStyle {
            underline: Some(true),
            ..Default::default()
        };
        let para = Paragraph {
            text: "hello world".into(),
            spans: vec![
                StyleRun {
                    start: 0,
                    end: 5,
                    style: bold_red,
                },
                StyleRun {
                    start: 6,
                    end: 11,
                    style: ul,
                },
            ],
            props: ParaProperties::default(),
            dirty: false,
            source_xml: None,
        };
        let doc = DocumentTree::from_rich_paragraphs([para]);
        let bytes = build_minimal_docx(&doc).expect("build");
        let parsed = read_docx(&bytes).expect("read");
        let p = &parsed.document.paragraphs[0];
        assert_eq!(p.text, "hello world");
        assert_eq!(p.style_at(0), bold_red);
        assert_eq!(p.style_at(4), bold_red);
        assert_eq!(p.style_at(5), SpanStyle::default());
        assert_eq!(p.style_at(6), ul);
    }

    #[test]
    fn round_trip_highlight_and_family() {
        let styled = SpanStyle {
            bg_color: Some([255, 235, 120, 255]),
            font_family: Some(FontFamily::LiberationSans),
            ..Default::default()
        };
        let para = Paragraph {
            text: "abc".into(),
            spans: vec![StyleRun {
                start: 0,
                end: 3,
                style: styled,
            }],
            props: ParaProperties::default(),
            dirty: false,
            source_xml: None,
        };
        let doc = DocumentTree::from_rich_paragraphs([para]);
        let bytes = build_minimal_docx(&doc).expect("build");
        let parsed = read_docx(&bytes).expect("read");
        assert_eq!(parsed.document.paragraphs[0].style_at(1), styled);
    }

    #[test]
    fn plain_paragraph_emits_no_run_properties() {
        /* A span-free paragraph must serialize without a <w:rPr> so the
        round-trip harness's plain fixtures stay byte-stable. */
        let doc = DocumentTree::from_text("plain text");
        let xml = build_document_xml(&doc);
        assert!(!xml.contains("<w:rPr>"));
        assert!(!xml.contains("<w:pPr>"));
        assert!(xml.contains("<w:p><w:r><w:t xml:space=\"preserve\">plain text</w:t></w:r></w:p>"));
    }

    #[test]
    fn round_trip_para_properties() {
        use engine::{Indent, LineHeight, Spacing, TextDirection};
        let props = ParaProperties {
            alignment: Some(engine::Alignment::Center),
            indent: Indent {
                start_twips: 720,
                first_line_twips: 360,
                ..Default::default()
            },
            spacing: Spacing {
                before_twips: 120,
                after_twips: 240,
            },
            line_height: Some(LineHeight::Auto { twips: 360 }),
            direction: Some(TextDirection::Rtl),
            keep_next: true,
            keep_lines: false,
            page_break_before: false,
        };
        let para = Paragraph {
            text: "hello world".into(),
            spans: Vec::new(),
            props: props.clone(),
            dirty: false,
            source_xml: None,
        };
        let doc = DocumentTree::from_rich_paragraphs([para]);
        let bytes = build_minimal_docx(&doc).expect("build");
        let parsed = read_docx(&bytes).expect("read");
        assert_eq!(parsed.document.paragraphs[0].props, props);
    }

    #[test]
    fn ppr_child_order_matches_schema() {
        /* Schema mandates keepNext, keepLines, pageBreakBefore, spacing, ind,
        jc, bidi in that order. Word rejects out-of-order children with a
        repair dialog, so this is load-bearing. */
        use engine::{Indent, Spacing, TextDirection};
        let para = Paragraph {
            text: "x".into(),
            spans: Vec::new(),
            props: ParaProperties {
                alignment: Some(engine::Alignment::End),
                indent: Indent {
                    start_twips: 720,
                    ..Default::default()
                },
                spacing: Spacing {
                    before_twips: 120,
                    ..Default::default()
                },
                direction: Some(TextDirection::Rtl),
                keep_next: true,
                keep_lines: true,
                page_break_before: true,
                ..Default::default()
            },
            dirty: false,
            source_xml: None,
        };
        let xml = build_document_xml(&DocumentTree::from_rich_paragraphs([para]));
        let p = xml.find("<w:pPr>").unwrap();
        let order = [
            "<w:keepNext/>",
            "<w:keepLines/>",
            "<w:pageBreakBefore/>",
            "<w:spacing",
            "<w:ind",
            "<w:jc",
            "<w:bidi",
        ];
        let mut cursor = p;
        for tag in order {
            let off = xml[cursor..]
                .find(tag)
                .unwrap_or_else(|| panic!("expected {tag} after offset {cursor}; xml={xml}"));
            cursor += off + tag.len();
        }
    }

    #[test]
    fn round_trip_minimal() {
        let doc = DocumentTree::from_text("hello world");
        let bytes = build_minimal_docx(&doc).expect("build");
        let parsed = read_docx(&bytes).expect("read");
        assert_eq!(parsed.document.paragraph_count(), 1);
        assert_eq!(parsed.document.paragraph_text(0), Some("hello world"));
    }

    /* ---- Phase 3: cascade + passthrough ------------------------------ */

    /// A minimal `.docx` whose `word/styles.xml` defines `BaseStyle` (bold)
    /// and `ChildStyle` (italic, basedOn BaseStyle), with `document.xml`
    /// referencing `ChildStyle`. Used by the cascade + passthrough tests.
    fn build_style_cascade_docx() -> Vec<u8> {
        let styles_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
<w:style w:type="paragraph" w:styleId="BaseStyle"><w:name w:val="Base"/><w:rPr><w:b/></w:rPr></w:style>
<w:style w:type="paragraph" w:styleId="ChildStyle"><w:name w:val="Child"/><w:basedOn w:val="BaseStyle"/><w:rPr><w:i/></w:rPr></w:style>
</w:styles>"#;
        let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:pPr><w:pStyle w:val="ChildStyle"/></w:pPr><w:r><w:t xml:space="preserve">hello cascade</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#;
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
</Relationships>"#;

        let mut buf: Vec<u8> = Vec::new();
        {
            let mut zip = ZipWriter::new(Cursor::new(&mut buf));
            let opts = SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated)
                .unix_permissions(0o644);
            for (name, body) in [
                ("[Content_Types].xml", content_types),
                ("_rels/.rels", dot_rels),
                ("word/_rels/document.xml.rels", doc_rels),
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

    #[test]
    fn cascade_resolves_basedon_chain_into_flat_span() {
        let bytes = build_style_cascade_docx();
        let parsed = read_docx(&bytes).expect("read");
        let p = &parsed.document.paragraphs[0];
        assert_eq!(p.text, "hello cascade");
        /* Single run, covering the whole text, with the cascaded style:
        bold (from BaseStyle) + italic (from ChildStyle). */
        assert_eq!(p.spans.len(), 1, "expected one resolved run span");
        assert_eq!((p.spans[0].start, p.spans[0].end), (0, 13));
        assert_eq!(p.spans[0].style.bold, Some(true), "BaseStyle bold");
        assert_eq!(p.spans[0].style.italic, Some(true), "ChildStyle italic");
    }

    #[test]
    fn passthrough_keeps_unedited_paragraph_byte_identical() {
        let bytes = build_style_cascade_docx();
        let archive = read_docx(&bytes).expect("read");
        /* Loaded paragraph must be clean, with captured source bytes. */
        let p = &archive.document.paragraphs[0];
        assert!(!p.dirty, "loaded paragraph must not be dirty");
        let raw = p.source_xml.as_deref().expect("source_xml captured");
        let raw_str = std::str::from_utf8(raw).unwrap();
        assert!(raw_str.starts_with("<w:p>"));
        assert!(raw_str.ends_with("</w:p>"));
        assert!(raw_str.contains("<w:pStyle w:val=\"ChildStyle\"/>"));

        /* Save unedited → document.xml must be byte-identical inside the
        <w:p>...</w:p> region. */
        let saved = write_docx(&archive, &archive.document).expect("write");
        let saved_doc_xml = {
            let mut z = zip::ZipArchive::new(Cursor::new(&saved)).unwrap();
            let mut f = z.by_name("word/document.xml").unwrap();
            let mut s = String::new();
            std::io::Read::read_to_string(&mut f, &mut s).unwrap();
            s
        };
        assert!(
            saved_doc_xml.contains(raw_str),
            "passthrough must re-emit raw <w:p> bytes verbatim"
        );
    }

    #[test]
    fn edit_falls_back_to_serialized_model() {
        let bytes = build_style_cascade_docx();
        let archive = read_docx(&bytes).expect("read");
        /* Mutate the paragraph — engine flips dirty=true, drops source_xml. */
        let edited = archive
            .document
            .insert_text(engine::LogicalPos { para: 0, offset: 5 }, " EDITED");
        let p = &edited.paragraphs[0];
        assert!(p.dirty, "edit must flip dirty=true");
        assert!(p.source_xml.is_none(), "edit must drop source_xml");

        /* Save → document.xml regenerates (no pStyle ref, but the cascaded
        style spans are emitted as direct rPr). */
        let saved = write_docx(&archive, &edited).expect("write");
        let saved_doc_xml = {
            let mut z = zip::ZipArchive::new(Cursor::new(&saved)).unwrap();
            let mut f = z.by_name("word/document.xml").unwrap();
            let mut s = String::new();
            std::io::Read::read_to_string(&mut f, &mut s).unwrap();
            s
        };
        assert!(saved_doc_xml.contains("hello EDITED cascade"));
        /* Re-read: cascade is gone (regenerated doc.xml carries direct
        rPr instead of `<w:pStyle>`); but styles.xml still rides
        passthrough, so the StyleTable is still populated — the bold+italic
        span survives as direct formatting on the regenerated run. */
        let reparsed = read_docx(&saved).expect("re-read");
        let q = &reparsed.document.paragraphs[0];
        assert_eq!(q.text, "hello EDITED cascade");
        assert!(
            q.spans
                .iter()
                .any(|s| s.style.bold == Some(true) && s.style.italic == Some(true))
        );
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
