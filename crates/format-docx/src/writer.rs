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

fn build_document_xml(doc: &DocumentTree) -> String {
    let mut out = String::with_capacity(2048);
    out.push_str(DOC_XML_HEADER);
    for para in &doc.paragraphs {
        serialize_paragraph(para, &mut out);
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
