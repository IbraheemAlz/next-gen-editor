//! `word/comments.xml` — minimal read-only model (Phase 8a).
//!
//! Each `<w:comment w:id="N" w:author="..." w:date="...">` wraps one or
//! more `<w:p>` paragraphs. Phase 8a only surfaces the comment's plain
//! text + author + date metadata for the sidebar UI — rich body
//! formatting and threaded replies ship with the Phase 8c track-changes
//! sprint.

use crate::error::DocxError;
use engine::CommentDef;
use quick_xml::events::Event;
use quick_xml::reader::Reader;
use std::collections::HashMap;

/// Map from comment `w:id` to its metadata + body.
#[derive(Debug, Clone, Default)]
pub struct CommentDefinitions {
    pub comments: HashMap<u32, CommentDef>,
}

pub fn parse_comments_xml(xml: &[u8]) -> Result<CommentDefinitions, DocxError> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(false);

    let mut out = CommentDefinitions::default();
    let mut buf = Vec::new();

    let mut cur_id: Option<u32> = None;
    let mut cur_author = String::new();
    let mut cur_date = String::new();
    let mut cur_paras: Vec<String> = Vec::new();
    let mut cur_text = String::new();
    let mut in_text_elt = false;
    let mut in_p = false;

    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Start(e) => match e.name().as_ref() {
                b"w:comment" => {
                    cur_id = e
                        .attributes()
                        .flatten()
                        .find(|a| a.key.as_ref() == b"w:id")
                        .and_then(|a| a.unescape_value().ok())
                        .and_then(|v| v.parse().ok());
                    cur_author = e
                        .attributes()
                        .flatten()
                        .find(|a| a.key.as_ref() == b"w:author")
                        .and_then(|a| a.unescape_value().ok().map(|v| v.into_owned()))
                        .unwrap_or_default();
                    cur_date = e
                        .attributes()
                        .flatten()
                        .find(|a| a.key.as_ref() == b"w:date")
                        .and_then(|a| a.unescape_value().ok().map(|v| v.into_owned()))
                        .unwrap_or_default();
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
                b"w:comment" => {
                    if let Some(id) = cur_id.take() {
                        out.comments.insert(
                            id,
                            CommentDef {
                                author: std::mem::take(&mut cur_author),
                                date: std::mem::take(&mut cur_date),
                                paragraphs: std::mem::take(&mut cur_paras),
                                /* `resolved` round-trip lives in
                                 * `commentsExtended.xml` per OOXML; the
                                 * Sprint 7 path tracks it in-memory only.
                                 * See Core Engine backlog issue. */
                                resolved: false,
                            },
                        );
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
