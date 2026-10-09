//! `docProps/core.xml` — OPC core properties (Dublin Core subset).
//!
//! Issue #77 — the `AUTHOR` field resolves to `<dc:creator>`; `<dc:title>`
//! rides along for a future `TITLE` field. Issue #360 — the PDF exporter's
//! document information (`/Info` + XMP `dc:*`) also reads `<dc:subject>`,
//! `<cp:keywords>` and `<dc:language>`. Read-only: the part stays in
//! `other_entries` and the writer passes it through byte-identical.

use crate::error::DocxError;
use quick_xml::events::Event;
use quick_xml::reader::Reader;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CoreProps {
    /// `<dc:creator>` — the document author.
    pub creator: Option<String>,
    /// `<dc:title>`.
    pub title: Option<String>,
    /// Issue #360 — `<dc:subject>` (Word's "Subject" property).
    pub subject: Option<String>,
    /// Issue #360 — `<cp:keywords>` (Word's "Tags"), verbatim.
    pub keywords: Option<String>,
    /// Issue #360 — `<dc:language>` (a BCP 47 tag, e.g. `ar-SA`).
    pub language: Option<String>,
}

/// Local (un-prefixed) element name — `dc:creator` → `creator`.
fn local_name(name: &[u8]) -> &[u8] {
    name.rsplit(|&b| b == b':').next().unwrap_or(name)
}

pub fn parse_core_props_xml(xml: &[u8]) -> Result<CoreProps, DocxError> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(false);
    let mut out = CoreProps::default();
    let mut buf = Vec::new();
    /* Which slot the current text event fills. */
    let mut slot: Option<u8> = None;
    let mut acc = String::new();
    loop {
        let evt = reader.read_event_into(&mut buf)?;
        match evt {
            Event::Start(e) => {
                slot = match local_name(e.name().as_ref()) {
                    b"creator" => Some(0),
                    b"title" => Some(1),
                    b"subject" => Some(2),
                    b"keywords" => Some(3),
                    b"language" => Some(4),
                    _ => None,
                };
                acc.clear();
            }
            Event::Text(t) if slot.is_some() => {
                acc.push_str(&t.unescape()?);
            }
            Event::End(e) => {
                let which = slot.take();
                match (which, local_name(e.name().as_ref())) {
                    (Some(0), b"creator") => out.creator = non_empty(&acc),
                    (Some(1), b"title") => out.title = non_empty(&acc),
                    (Some(2), b"subject") => out.subject = non_empty(&acc),
                    (Some(3), b"keywords") => out.keywords = non_empty(&acc),
                    (Some(4), b"language") => out.language = non_empty(&acc),
                    _ => {}
                }
                acc.clear();
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(out)
}

fn non_empty(s: &str) -> Option<String> {
    let t = s.trim();
    (!t.is_empty()).then(|| t.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creator_and_title_lift_from_core_xml() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:dcterms="http://purl.org/dc/terms/" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
<dc:title>Quarterly &amp; Annual</dc:title>
<dc:subject>Finance</dc:subject>
<dc:creator>Ibrahim Z.</dc:creator>
<cp:keywords>budget; Q3</cp:keywords>
<dc:language>ar-SA</dc:language>
<cp:lastModifiedBy>Someone Else</cp:lastModifiedBy>
<dcterms:created xsi:type="dcterms:W3CDTF">2026-01-01T00:00:00Z</dcterms:created>
</cp:coreProperties>"#;
        let props = parse_core_props_xml(xml).expect("parse");
        assert_eq!(props.creator.as_deref(), Some("Ibrahim Z."));
        assert_eq!(props.title.as_deref(), Some("Quarterly & Annual"));
        assert_eq!(props.subject.as_deref(), Some("Finance"));
        assert_eq!(props.keywords.as_deref(), Some("budget; Q3"));
        assert_eq!(props.language.as_deref(), Some("ar-SA"));
    }

    #[test]
    fn blank_creator_is_none() {
        let xml = br#"<cp:coreProperties xmlns:cp="x" xmlns:dc="y"><dc:creator>  </dc:creator></cp:coreProperties>"#;
        let props = parse_core_props_xml(xml).expect("parse");
        assert_eq!(props.creator, None);
        assert_eq!(props.title, None);
    }
}
