//! `word/header*.xml` — minimal read-only model (Phase 6).
//!
//! The header XML carries the same paragraph / run shape `word/document.xml`
//! does; the engine routes header / footer text through the same paragraph
//! pipeline. The Phase 6 cut only needs to extract the plain text — the
//! renderer paints header / footer paragraphs with the section's geometry.
//!
//! The header part rides the archive's `other_entries` verbatim. This
//! module never serialises.

use crate::error::DocxError;
use quick_xml::events::Event;
use quick_xml::reader::Reader;

/// One header part — a flat list of paragraph plain-text bodies. The Phase 6
/// cut keeps the model deliberately minimal: rich formatting + tables in
/// headers / footers ship in a follow-up sprint.
#[derive(Debug, Clone, Default)]
pub struct HeaderPart {
    pub paragraphs: Vec<String>,
}

/// Parse a `word/header*.xml` byte blob.
pub fn parse_header_xml(xml: &[u8]) -> Result<HeaderPart, DocxError> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(false);

    let mut paragraphs: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut in_text_elt = false;
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Start(e) if e.name().as_ref() == b"w:t" => {
                in_text_elt = true;
            }
            Event::End(e) => match e.name().as_ref() {
                b"w:t" => in_text_elt = false,
                b"w:p" => paragraphs.push(std::mem::take(&mut cur)),
                _ => {}
            },
            Event::Text(t) if in_text_elt => {
                cur.push_str(&t.unescape()?);
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(HeaderPart { paragraphs })
}
