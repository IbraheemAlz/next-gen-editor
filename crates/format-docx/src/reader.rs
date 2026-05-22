//! `.docx` reader: unzip + parse `word/document.xml` into a `DocumentTree`.
//!
//! Also stashes the other archive entries verbatim so the writer can repack
//! them unchanged. This keeps the round-trip diff localized to
//! `word/document.xml` (we don't re-serialize content types or rels).
//!
//! Each `<w:r>` run's `<w:rPr>` properties (`<w:b>`, `<w:i>`, `<w:u>`,
//! `<w:strike>`, `<w:color>`, `<w:highlight>` / `<w:shd>`, `<w:rFonts>`) map
//! onto an `engine::SpanStyle`, so character formatting survives the
//! round-trip (Backlog #1).

use engine::{DocumentTree, FontFamily, Paragraph, SpanStyle, StyleRun};
use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;
use std::io::{Cursor, Read};
use thiserror::Error;
use zip::ZipArchive;

#[derive(Debug, Error)]
pub enum DocxError {
    #[error("ZIP error: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("XML error: {0}")]
    Xml(#[from] quick_xml::Error),
    #[error("XML attribute error: {0}")]
    XmlAttr(#[from] quick_xml::events::attributes::AttrError),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("required entry missing: {0}")]
    MissingEntry(String),
    #[error("UTF-8 error: {0}")]
    Utf8(#[from] std::str::Utf8Error),
}

pub const DOC_XML: &str = "word/document.xml";

/// All raw archive entries except `word/document.xml`. Carried through the
/// round-trip so the writer can re-emit them verbatim.
#[derive(Debug, Clone)]
pub struct DocxArchive {
    /// (entry_name, raw bytes). Order preserved from the original archive.
    pub other_entries: Vec<(String, Vec<u8>)>,
    /// Pre-parsed paragraphs from `word/document.xml`.
    pub document: DocumentTree,
}

/// Read a `.docx` byte blob → parsed document + stashed sibling entries.
pub fn read_docx(bytes: &[u8]) -> Result<DocxArchive, DocxError> {
    let mut archive = ZipArchive::new(Cursor::new(bytes))?;
    let mut other_entries: Vec<(String, Vec<u8>)> = Vec::new();
    let mut document_xml: Option<Vec<u8>> = None;

    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        let name = file.name().to_owned();
        let mut buf = Vec::with_capacity(file.size() as usize);
        file.read_to_end(&mut buf)?;
        if name == DOC_XML {
            document_xml = Some(buf);
        } else {
            other_entries.push((name, buf));
        }
    }

    let xml = document_xml.ok_or_else(|| DocxError::MissingEntry(DOC_XML.into()))?;
    let document = parse_document_xml(&xml)?;

    Ok(DocxArchive {
        other_entries,
        document,
    })
}

/* ---- `<w:rPr>` property helpers ---------------------------------------- */

/// Value of attribute `key` on a start/empty tag, unescaped.
fn attr_val(e: &BytesStart, key: &[u8]) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.as_ref() == key)
        .and_then(|a| a.unescape_value().ok().map(|v| v.into_owned()))
}

/// A 6-hex-digit `RRGGBB` colour → opaque RGBA. `auto` / malformed → `None`.
fn parse_hex_color(v: &str) -> Option<[u8; 4]> {
    let v = v.trim();
    if v.len() != 6 {
        return None;
    }
    let d = |i: usize| u8::from_str_radix(v.get(i..i + 2)?, 16).ok();
    Some([d(0)?, d(2)?, d(4)?, 255])
}

/// Word `<w:highlight>` uses a fixed named palette (arbitrary RGB lives in
/// `<w:shd w:fill>` instead). Map the names the engine can represent.
fn highlight_color(name: &str) -> Option<[u8; 4]> {
    let [r, g, b] = match name.trim().to_ascii_lowercase().as_str() {
        "yellow" => [255, 255, 0],
        "green" => [0, 255, 0],
        "cyan" => [0, 255, 255],
        "magenta" => [255, 0, 255],
        "blue" => [0, 0, 255],
        "red" => [255, 0, 0],
        "darkblue" => [0, 0, 139],
        "darkcyan" => [0, 139, 139],
        "darkgreen" => [0, 100, 0],
        "darkmagenta" => [139, 0, 139],
        "darkred" => [139, 0, 0],
        "darkyellow" => [128, 128, 0],
        "darkgray" => [169, 169, 169],
        "lightgray" => [211, 211, 211],
        "black" => [0, 0, 0],
        _ => return None,
    };
    Some([r, g, b, 255])
}

fn family_from_docx(name: &str) -> Option<FontFamily> {
    match name.trim().to_ascii_lowercase().as_str() {
        "amiri" => Some(FontFamily::Amiri),
        "liberation sans" | "liberation" => Some(FontFamily::LiberationSans),
        "noto naskh arabic" => Some(FontFamily::NotoNaskhArabic),
        _ => None,
    }
}

/// An OOXML toggle property: bare `<w:b/>` is on; `<w:b w:val="false"/>` off.
fn toggle_on(e: &BytesStart) -> bool {
    match attr_val(e, b"w:val") {
        Some(v) => !matches!(v.to_ascii_lowercase().as_str(), "false" | "0" | "off"),
        None => true,
    }
}

/// Fold one `<w:rPr>` child element into the run's accumulating style.
fn apply_rpr(name: &[u8], e: &BytesStart, style: &mut SpanStyle) {
    match name {
        b"w:b" => style.bold = Some(toggle_on(e)),
        b"w:i" => style.italic = Some(toggle_on(e)),
        b"w:strike" => style.strike = Some(toggle_on(e)),
        b"w:u" => {
            let on = attr_val(e, b"w:val").is_none_or(|v| !v.eq_ignore_ascii_case("none"));
            style.underline = Some(on);
        }
        b"w:color" => style.color = attr_val(e, b"w:val").and_then(|v| parse_hex_color(&v)),
        b"w:highlight" => {
            style.bg_color = attr_val(e, b"w:val").and_then(|v| highlight_color(&v));
        }
        b"w:shd" => style.bg_color = attr_val(e, b"w:fill").and_then(|v| parse_hex_color(&v)),
        b"w:rFonts" => {
            style.font_family = attr_val(e, b"w:ascii")
                .or_else(|| attr_val(e, b"w:hAnsi"))
                .or_else(|| attr_val(e, b"w:cs"))
                .and_then(|v| family_from_docx(&v));
        }
        _ => {}
    }
}

/// Parse `word/document.xml` into paragraphs. Each `<w:p>` is a paragraph;
/// each `<w:r>` is a run whose `<w:rPr>` becomes a `SpanStyle`.
fn parse_document_xml(xml: &[u8]) -> Result<DocumentTree, DocxError> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(false);

    let mut paragraphs: Vec<Paragraph> = Vec::new();
    let mut para_text = String::new();
    let mut spans: Vec<StyleRun> = Vec::new();
    let mut in_run = false;
    let mut in_rpr = false;
    let mut in_text_elt = false;
    let mut run_style = SpanStyle::default();
    let mut run_text = String::new();
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Start(e) => {
                let name = e.name();
                match name.as_ref() {
                    b"w:r" => {
                        in_run = true;
                        run_style = SpanStyle::default();
                        run_text.clear();
                    }
                    b"w:rPr" => in_rpr = true,
                    b"w:t" => in_text_elt = true,
                    n if in_run && in_rpr => apply_rpr(n, &e, &mut run_style),
                    _ => {}
                }
            }
            Event::Empty(e) if in_run && in_rpr => {
                let name = e.name();
                apply_rpr(name.as_ref(), &e, &mut run_style);
            }
            Event::Text(t) if in_text_elt => {
                run_text.push_str(&t.unescape()?);
            }
            Event::End(e) => match e.name().as_ref() {
                b"w:t" => in_text_elt = false,
                b"w:rPr" => in_rpr = false,
                b"w:r" => {
                    in_run = false;
                    let start = para_text.len() as u32;
                    para_text.push_str(&run_text);
                    let end = para_text.len() as u32;
                    /* A styled, non-empty run becomes a span; coalesce with the
                    previous span when its style is identical and adjacent. */
                    if run_style != SpanStyle::default() && start < end {
                        match spans.last_mut() {
                            Some(last) if last.end == start && last.style == run_style => {
                                last.end = end;
                            }
                            _ => spans.push(StyleRun {
                                start,
                                end,
                                style: run_style,
                            }),
                        }
                    }
                }
                b"w:p" => paragraphs.push(Paragraph {
                    text: std::mem::take(&mut para_text),
                    spans: std::mem::take(&mut spans),
                    alignment: None,
                }),
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }

    Ok(DocumentTree::from_rich_paragraphs(paragraphs))
}
