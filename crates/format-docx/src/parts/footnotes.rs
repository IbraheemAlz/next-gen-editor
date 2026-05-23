//! `word/footnotes.xml` — minimal read-only model (Phase 8a).
//!
//! Each `<w:footnote w:id="N">` carries one or more `<w:p>` paragraphs with
//! the footnote body. Word's footnotes file always ships with two sentinel
//! entries — id `-1` (continuation separator) and id `0` (separator) — that
//! the paginator must skip; the parser keeps them on the map so the
//! passthrough writer round-trips byte-identical.

use crate::error::DocxError;
use quick_xml::events::Event;
use quick_xml::reader::Reader;
use std::collections::HashMap;

/// Map from footnote `w:id` to its body's per-paragraph plain text.
#[derive(Debug, Clone, Default)]
pub struct FootnoteTable {
    pub footnotes: HashMap<u32, Vec<String>>,
    /// `w:id`s the file marks as separator / continuation. Currently
    /// captured for round-trip awareness but not consumed.
    pub sentinel_ids: Vec<i32>,
}

/// Parse `word/footnotes.xml`. Tolerant of unknown tags.
pub fn parse_footnotes_xml(xml: &[u8]) -> Result<FootnoteTable, DocxError> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(false);

    let mut out = FootnoteTable::default();
    let mut buf = Vec::new();

    let mut cur_id: Option<i32> = None;
    let mut cur_paras: Vec<String> = Vec::new();
    let mut cur_text = String::new();
    let mut in_text_elt = false;
    let mut in_p = false;

    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Start(e) => match e.name().as_ref() {
                b"w:footnote" => {
                    cur_id = e
                        .attributes()
                        .flatten()
                        .find(|a| a.key.as_ref() == b"w:id")
                        .and_then(|a| a.unescape_value().ok())
                        .and_then(|v| v.parse().ok());
                    cur_paras.clear();
                }
                b"w:p" => {
                    in_p = true;
                    cur_text.clear();
                }
                b"w:t" => in_text_elt = true,
                _ => {}
            },
            Event::End(e) => match e.name().as_ref() {
                b"w:footnote" => {
                    if let Some(id) = cur_id.take() {
                        if id < 0 {
                            out.sentinel_ids.push(id);
                        } else if id == 0 {
                            /* `w:id="0"` is the body separator entry —
                            keep it logged so the round-trip stays
                            symmetrical; the paginator skips it. */
                            out.sentinel_ids.push(id);
                        } else {
                            out.footnotes
                                .insert(id as u32, std::mem::take(&mut cur_paras));
                        }
                    }
                    cur_paras.clear();
                }
                b"w:p" => {
                    if in_p {
                        cur_paras.push(std::mem::take(&mut cur_text));
                    }
                    in_p = false;
                }
                b"w:t" => in_text_elt = false,
                _ => {}
            },
            Event::Text(t) if in_text_elt => {
                cur_text.push_str(&t.unescape()?);
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(out)
}
