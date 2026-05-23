//! `[Content_Types].xml` — typed read-only parser (Phase 1).
//!
//! Every OPC package has a single `[Content_Types].xml` at the archive root.
//! It registers the MIME content type for each part by either a file
//! extension default (`<Default Extension="xml" ContentType="…"/>`) or an
//! exact part-name override (`<Override PartName="/word/styles.xml" …/>`).
//!
//! Phase 1 parses the file so later phases can answer "is part X of type T?"
//! structurally; the bytes themselves still ride the `DocxArchive`
//! pass-through, so the writer is untouched.

use crate::error::DocxError;
use quick_xml::events::Event;
use quick_xml::reader::Reader;

/// `<Default Extension="…" ContentType="…"/>` — extension-keyed default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefaultType {
    pub extension: String,
    pub content_type: String,
}

/// `<Override PartName="/…" ContentType="…"/>` — part-name override.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverrideType {
    pub part_name: String,
    pub content_type: String,
}

#[derive(Debug, Clone, Default)]
pub struct ContentTypes {
    pub defaults: Vec<DefaultType>,
    pub overrides: Vec<OverrideType>,
}

impl ContentTypes {
    /// Resolved content type for `part_name` (e.g. `"/word/document.xml"`).
    /// Overrides win; otherwise the extension default applies.
    pub fn lookup(&self, part_name: &str) -> Option<&str> {
        if let Some(o) = self.overrides.iter().find(|o| o.part_name == part_name) {
            return Some(&o.content_type);
        }
        let ext = part_name.rsplit('.').next()?;
        self.defaults
            .iter()
            .find(|d| d.extension.eq_ignore_ascii_case(ext))
            .map(|d| d.content_type.as_str())
    }
}

fn attr_value(e: &quick_xml::events::BytesStart, key: &[u8]) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.as_ref() == key)
        .and_then(|a| a.unescape_value().ok().map(|v| v.into_owned()))
}

pub fn parse_content_types(xml: &[u8]) -> Result<ContentTypes, DocxError> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(true);
    let mut out = ContentTypes::default();
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Empty(e) | Event::Start(e) => match e.name().as_ref() {
                b"Default" => {
                    if let (Some(extension), Some(content_type)) =
                        (attr_value(&e, b"Extension"), attr_value(&e, b"ContentType"))
                    {
                        out.defaults.push(DefaultType {
                            extension,
                            content_type,
                        });
                    }
                }
                b"Override" => {
                    if let (Some(part_name), Some(content_type)) =
                        (attr_value(&e, b"PartName"), attr_value(&e, b"ContentType"))
                    {
                        out.overrides.push(OverrideType {
                            part_name,
                            content_type,
                        });
                    }
                }
                _ => {}
            },
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
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
<Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/>
</Types>"#;

    #[test]
    fn parses_defaults_and_overrides() {
        let ct = parse_content_types(SAMPLE).expect("parse");
        assert_eq!(ct.defaults.len(), 2);
        assert_eq!(ct.overrides.len(), 2);
        assert_eq!(ct.defaults[0].extension, "rels");
        assert_eq!(ct.overrides[0].part_name, "/word/document.xml");
    }

    #[test]
    fn lookup_prefers_override() {
        let ct = parse_content_types(SAMPLE).expect("parse");
        assert_eq!(
            ct.lookup("/word/document.xml"),
            Some(
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"
            )
        );
        /* Falls through to the `xml` extension default. */
        assert_eq!(ct.lookup("/word/footer1.xml"), Some("application/xml"));
        assert_eq!(ct.lookup("/unknown.bin"), None);
    }
}
