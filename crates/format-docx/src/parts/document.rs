//! `word/document.xml` — parse paragraphs + runs + `<w:rPr>` into a
//! `DocumentTree`.
//!
//! Each `<w:r>` run's `<w:rPr>` properties (`<w:b>`, `<w:i>`, `<w:u>`,
//! `<w:strike>`, `<w:color>`, `<w:highlight>` / `<w:shd>`, `<w:rFonts>`)
//! map onto an `engine::SpanStyle`, so character formatting survives the
//! round-trip. Paragraph-level `<w:pPr>` (alignment, indent, spacing,
//! direction) is **not** parsed yet — that lands in Phase 2.

use crate::error::DocxError;
use crate::schema::ct_rpr::apply_rpr;
use engine::{DocumentTree, Paragraph, SpanStyle, StyleRun};
use quick_xml::events::Event;
use quick_xml::reader::Reader;

/// Parse `word/document.xml` into paragraphs. Each `<w:p>` is a paragraph;
/// each `<w:r>` is a run whose `<w:rPr>` becomes a `SpanStyle`.
pub fn parse_document_xml(xml: &[u8]) -> Result<DocumentTree, DocxError> {
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
