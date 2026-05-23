//! `word/settings.xml` — minimal read-only model (Phase 6).
//!
//! Phase 6 only needs to know the part exists for pass-through; the
//! engine has no Settings consumer yet. The struct lands so later
//! phases can grow typed fields (zoom, default tab stop, document
//! direction) without touching the OPC reader.

use crate::error::DocxError;
use quick_xml::events::Event;
use quick_xml::reader::Reader;

#[derive(Debug, Clone, Default)]
pub struct SettingsPart {
    /// `<w:defaultTabStop w:val="N"/>` — twips. `None` when absent.
    pub default_tab_stop_twips: Option<u32>,
}

pub fn parse_settings_xml(xml: &[u8]) -> Result<SettingsPart, DocxError> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(false);
    let mut out = SettingsPart::default();
    let mut buf = Vec::new();
    loop {
        let evt = reader.read_event_into(&mut buf)?;
        match evt {
            Event::Empty(e) | Event::Start(e) if e.name().as_ref() == b"w:defaultTabStop" => {
                out.default_tab_stop_twips = e
                    .attributes()
                    .flatten()
                    .find(|a| a.key.as_ref() == b"w:val")
                    .and_then(|a| a.unescape_value().ok())
                    .and_then(|v| v.parse().ok());
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(out)
}
