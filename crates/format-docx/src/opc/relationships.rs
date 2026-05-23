//! `_rels/*.rels` — typed read-only parser (Phase 1).
//!
//! A `.rels` file lists every outbound relationship from one OPC part. The
//! package-level `_rels/.rels` points at the main document; the
//! `word/_rels/document.xml.rels` points at styles, numbering, settings,
//! theme, headers, footers, images, hyperlinks, etc.
//!
//! Phase 1 parses the file so later phases can navigate the relationship
//! graph; emission stays under the `DocxArchive` pass-through.

use crate::error::DocxError;
use quick_xml::events::Event;
use quick_xml::reader::Reader;

/// One `<Relationship .../>` entry inside a `.rels` part.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relationship {
    /// The local id (`"rId1"`); referenced from `document.xml` etc.
    pub id: String,
    /// Schema URL identifying what *kind* of relationship this is
    /// (e.g. `…/officeDocument`, `…/styles`, `…/image`).
    pub rel_type: String,
    /// Target part name (resolved relative to the `.rels`' base part).
    pub target: String,
    /// `Internal` (default) or `External` — external = absolute URL link.
    pub target_mode: TargetMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TargetMode {
    #[default]
    Internal,
    External,
}

#[derive(Debug, Clone, Default)]
pub struct Relationships {
    pub items: Vec<Relationship>,
}

impl Relationships {
    pub fn by_id(&self, id: &str) -> Option<&Relationship> {
        self.items.iter().find(|r| r.id == id)
    }

    /// All relationships of a given `rel_type` (the schema URL). Useful for
    /// e.g. "give me every `…/image` relationship" once Phase 7 lands.
    pub fn by_type<'a>(&'a self, rel_type: &'a str) -> impl Iterator<Item = &'a Relationship> {
        self.items.iter().filter(move |r| r.rel_type == rel_type)
    }
}

fn attr_value(e: &quick_xml::events::BytesStart, key: &[u8]) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.as_ref() == key)
        .and_then(|a| a.unescape_value().ok().map(|v| v.into_owned()))
}

pub fn parse_relationships(xml: &[u8]) -> Result<Relationships, DocxError> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(true);
    let mut out = Relationships::default();
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Empty(e) | Event::Start(e) if e.name().as_ref() == b"Relationship" => {
                let id = attr_value(&e, b"Id");
                let rel_type = attr_value(&e, b"Type");
                let target = attr_value(&e, b"Target");
                let target_mode = match attr_value(&e, b"TargetMode").as_deref() {
                    Some("External") => TargetMode::External,
                    _ => TargetMode::Internal,
                };
                if let (Some(id), Some(rel_type), Some(target)) = (id, rel_type, target) {
                    out.items.push(Relationship {
                        id,
                        rel_type,
                        target,
                        target_mode,
                    });
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &[u8] = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
<Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="https://example.com" TargetMode="External"/>
</Relationships>"#;

    #[test]
    fn parses_package_rels() {
        let r = parse_relationships(SAMPLE).expect("parse");
        assert_eq!(r.items.len(), 2);
        assert_eq!(r.items[0].id, "rId1");
        assert_eq!(r.items[0].target, "word/document.xml");
        assert_eq!(r.items[0].target_mode, TargetMode::Internal);
        assert_eq!(r.items[1].target_mode, TargetMode::External);
    }

    #[test]
    fn by_id_and_by_type() {
        let r = parse_relationships(SAMPLE).expect("parse");
        assert_eq!(
            r.by_id("rId1").map(|r| r.target.as_str()),
            Some("word/document.xml")
        );
        assert_eq!(
            r.by_type(
                "http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink"
            )
            .count(),
            1
        );
    }
}
