//! `word/_rels/document.xml.rels` — minimal read-only model (Phase 6b).
//!
//! Each `<Relationship Id="rId1" Type="..." Target="header1.xml"/>`
//! becomes a `(rid, target)` entry. The paginator resolves
//! `Section::header_ref` / `footer_ref` (the `r:id` from
//! `<w:headerReference>` / `<w:footerReference>`) by looking up the
//! relationship table, then loads the target part from the archive's
//! `other_entries`.

use crate::error::DocxError;
use quick_xml::events::Event;
use quick_xml::reader::Reader;
use std::collections::HashMap;

/// Parse `word/_rels/document.xml.rels` into `r:id → target` map. Targets
/// are returned as they appear in the rels XML — typically relative
/// (`header1.xml`); the resolver concatenates with `word/` to reach the
/// archive entry name.
pub fn parse_rels_xml(xml: &[u8]) -> Result<HashMap<String, String>, DocxError> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(false);
    let mut out: HashMap<String, String> = HashMap::new();
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Empty(e) | Event::Start(e) if e.name().as_ref() == b"Relationship" => {
                let mut id: Option<String> = None;
                let mut target: Option<String> = None;
                for attr in e.attributes().flatten() {
                    let key = attr.key.as_ref();
                    let val = attr.unescape_value().ok().map(|v| v.into_owned());
                    if key == b"Id" {
                        id = val;
                    } else if key == b"Target" {
                        target = val;
                    }
                }
                if let (Some(i), Some(t)) = (id, target) {
                    out.insert(i, t);
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(out)
}

/// Resolve a target string from `word/_rels/document.xml.rels` to the full
/// archive entry name (rooted at the zip root). The OOXML convention:
/// relative paths are anchored at `word/`. `header1.xml` → `word/header1.xml`.
pub fn resolve_target(target: &str) -> String {
    if target.starts_with('/') {
        target.trim_start_matches('/').to_string()
    } else if target.starts_with("word/") {
        target.to_string()
    } else {
        format!("word/{target}")
    }
}
