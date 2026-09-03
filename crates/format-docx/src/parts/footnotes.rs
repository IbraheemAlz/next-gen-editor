//! `word/footnotes.xml` / `word/endnotes.xml` — full-fidelity note story
//! parser (issue #80).
//!
//! The Phase-8a reader lifted plain text per `<w:p>` into a
//! `HashMap<u32, Vec<String>>`. Issue #80 widens the part to the body's
//! own block model: every `<w:footnote>` / `<w:endnote>` element becomes
//! an [`engine::NoteStory`] whose `body` is parsed by the SAME
//! [`parse_document_xml`] pipeline `word/document.xml` uses (style
//! spans, hyperlinks, fields, lists, tables, grab bags), so the story
//! adapter, the paginator and the writer reuse every body code path.
//!
//! Two fidelity points:
//!
//! - Each entry's raw `<w:footnote …>…</w:footnote>` bytes ride
//!   [`engine::NoteStory::source_xml`]. A regenerated part re-emits clean
//!   entries verbatim, so an edit to note 3 never disturbs note 2's
//!   `rsid`s / `proofErr` / `w14:paraId` markup.
//! - The body sub-parse is wrapped in a synthetic root carrying the
//!   part root's own attributes (`xmlns:w14`, `mc:Ignorable`, …) so
//!   grab-bag fragments in a foreign namespace pass the writer's
//!   [`crate::schema::grab_bag::bound_by_root`] check exactly as body
//!   fragments do.
//!
//! Word's separator sentinels (`w:id="-1"` continuation separator,
//! `w:id="0"` separator) and any `continuationNotice` land on the map
//! with their `note_type`; the paginator draws its own rules and only
//! consults the notice story's content.
//!
//! The `<w:footnotePr>` / `<w:endnotePr>` child parser
//! ([`apply_note_pr_child`]) is shared with `parts::document` (section
//! level) and `parts::settings` (document level).

use crate::error::DocxError;
use crate::opc::archive::root_attributes;
use crate::parts::document::parse_document_xml;
use crate::schema::grab_bag::capture_subtree;
use crate::style_resolver::StyleResolver;
use engine::{Block, NoteKind, NoteNumRestart, NotePosition, NoteProps, NoteStory, NoteType};
use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;

/// One parsed note part: every entry in file order plus the root
/// element's attributes (the writer re-declares them on a regenerated
/// root).
#[derive(Debug, Clone, Default)]
pub struct NotesPart {
    pub notes: Vec<NoteStory>,
    pub root_attrs: Vec<(String, String)>,
}

/// Parse `word/footnotes.xml`.
pub fn parse_footnotes_xml(
    xml: &[u8],
    resolver: &StyleResolver<'_>,
) -> Result<NotesPart, DocxError> {
    parse_notes_xml(xml, NoteKind::Footnote, resolver)
}

/// Parse `word/footnotes.xml` or `word/endnotes.xml` — the two parts
/// share one schema modulo element names.
pub fn parse_notes_xml(
    xml: &[u8],
    kind: NoteKind,
    resolver: &StyleResolver<'_>,
) -> Result<NotesPart, DocxError> {
    let (root_name, entry_name): (&[u8], &[u8]) = match kind {
        NoteKind::Footnote => (b"w:footnotes", b"w:footnote"),
        NoteKind::Endnote => (b"w:endnotes", b"w:endnote"),
    };
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(false);

    let mut out = NotesPart {
        notes: Vec::new(),
        root_attrs: root_attributes(xml),
    };
    /* The synthetic root every entry body is parsed under: the real
    root's start tag verbatim (attributes included) with a matching end
    tag. `<w:footnote>` is just a container to `parse_document_xml`'s
    `<w:p>` / `<w:tbl>` scanner. */
    let root_open = root_start_tag(xml, root_name).unwrap_or_else(|| {
        format!(
            "<{} xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">",
            String::from_utf8_lossy(root_name)
        )
        .into_bytes()
    });
    let root_close = format!("</{}>", String::from_utf8_lossy(root_name)).into_bytes();

    let mut buf = Vec::new();
    let mut prev_pos = 0usize;
    let mut depth = 0usize;
    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Start(e) => {
                if depth == 1 && e.name().as_ref() == entry_name {
                    let (id, note_type) = entry_attrs(&e);
                    let start = prev_pos;
                    let raw = capture_subtree(xml, start, &mut reader, &e)?;
                    let body = match raw.as_deref() {
                        Some(bytes) => parse_entry_body(bytes, &root_open, &root_close, resolver)?,
                        None => Vec::new(),
                    };
                    out.notes.push(NoteStory {
                        id,
                        kind,
                        note_type,
                        body: ensure_non_empty(body),
                        source_xml: raw,
                        dirty: false,
                    });
                    /* `capture_subtree` consumed through the end tag;
                    the depth is unchanged. */
                } else {
                    depth += 1;
                }
            }
            Event::Empty(e) if depth == 1 && e.name().as_ref() == entry_name => {
                /* `<w:footnote w:id="3"/>` — degenerate but legal. */
                let (id, note_type) = entry_attrs(&e);
                let end = reader.buffer_position() as usize;
                let raw = crate::schema::grab_bag::slice_fragment(xml, prev_pos, end);
                out.notes.push(NoteStory {
                    id,
                    kind,
                    note_type,
                    body: ensure_non_empty(Vec::new()),
                    source_xml: raw,
                    dirty: false,
                });
            }
            Event::End(_) => {
                depth = depth.saturating_sub(1);
            }
            Event::Eof => break,
            _ => {}
        }
        prev_pos = reader.buffer_position() as usize;
        buf.clear();
    }
    Ok(out)
}

/// A note body always holds at least one paragraph so the story has a
/// caret home and the writer emits a valid `<w:footnote>`.
fn ensure_non_empty(mut body: Vec<Block>) -> Vec<Block> {
    if body.is_empty() {
        body.push(Block::Paragraph(engine::Paragraph::default()));
    }
    body
}

/// `(w:id, w:type)` of a `<w:footnote>` / `<w:endnote>` start tag.
fn entry_attrs(e: &BytesStart<'_>) -> (i32, NoteType) {
    let mut id: i32 = 0;
    let mut note_type = NoteType::Normal;
    for a in e.attributes().flatten() {
        match a.key.as_ref() {
            b"w:id" => {
                if let Ok(v) = a.unescape_value()
                    && let Ok(n) = v.trim().parse::<i32>()
                {
                    id = n;
                }
            }
            b"w:type" => {
                if let Ok(v) = a.unescape_value() {
                    note_type = match v.trim() {
                        "separator" => NoteType::Separator,
                        "continuationSeparator" => NoteType::ContinuationSeparator,
                        "continuationNotice" => NoteType::ContinuationNotice,
                        _ => NoteType::Normal,
                    };
                }
            }
            _ => {}
        }
    }
    (id, note_type)
}

/// The raw bytes of the part root's start tag (`<w:footnotes …>`), so
/// the entry sub-parse sees the same namespace bindings the part does.
fn root_start_tag(xml: &[u8], root_name: &[u8]) -> Option<Vec<u8>> {
    let mut reader = Reader::from_reader(xml);
    let mut buf = Vec::new();
    let mut prev = 0usize;
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) if e.name().as_ref() == root_name => {
                let end = reader.buffer_position() as usize;
                return crate::schema::grab_bag::slice_fragment(xml, prev, end);
            }
            Ok(Event::Eof) | Err(_) => return None,
            _ => {}
        }
        prev = reader.buffer_position() as usize;
        buf.clear();
    }
}

/// Parse one entry's `<w:p>` / `<w:tbl>` children through the body
/// pipeline under the synthetic part root.
fn parse_entry_body(
    entry: &[u8],
    root_open: &[u8],
    root_close: &[u8],
    resolver: &StyleResolver<'_>,
) -> Result<Vec<Block>, DocxError> {
    let mut wrapped = Vec::with_capacity(root_open.len() + entry.len() + root_close.len());
    wrapped.extend_from_slice(root_open);
    wrapped.extend_from_slice(entry);
    wrapped.extend_from_slice(root_close);
    let tree = parse_document_xml(&wrapped, resolver)?;
    /* A sectPr inside a note is meaningless — never let a note body
    masquerade as a section-boundary carrier. */
    Ok(tree
        .blocks
        .iter()
        .cloned()
        .map(|b| match b {
            Block::Paragraph(mut p) => {
                p.section_end = None;
                Block::Paragraph(p)
            }
            table => table,
        })
        .collect())
}

/// Fold one child of `<w:footnotePr>` / `<w:endnotePr>` (§17.11.11 /
/// §17.11.4) into `props`: `<w:pos>`, `<w:numFmt>`, `<w:numStart>`,
/// `<w:numRestart>`. The settings-level `<w:footnote w:id>` references
/// (which special stories act as separators) are not modelled — the
/// stories themselves carry `w:type`. Returns `true` when the element
/// was consumed.
pub fn apply_note_pr_child(name: &[u8], e: &BytesStart<'_>, props: &mut NoteProps) -> bool {
    let val = || {
        e.attributes()
            .flatten()
            .find(|a| a.key.as_ref() == b"w:val")
            .and_then(|a| a.unescape_value().ok().map(|v| v.trim().to_string()))
    };
    match name {
        b"w:pos" => {
            props.position = Some(match val().as_deref() {
                Some("beneathText") => NotePosition::BeneathText,
                Some("sectEnd") => NotePosition::SectEnd,
                Some("docEnd") => NotePosition::DocEnd,
                _ => NotePosition::PageBottom,
            });
            true
        }
        b"w:numFmt" => {
            props.num_format = Some(match val().as_deref() {
                Some("lowerRoman") => engine::PageNumFormat::LowerRoman,
                Some("upperRoman") => engine::PageNumFormat::UpperRoman,
                Some("lowerLetter") => engine::PageNumFormat::LowerLetter,
                Some("upperLetter") => engine::PageNumFormat::UpperLetter,
                _ => engine::PageNumFormat::Decimal,
            });
            true
        }
        b"w:numStart" => {
            props.num_start = val().and_then(|v| v.parse::<u32>().ok());
            true
        }
        b"w:numRestart" => {
            props.num_restart = Some(match val().as_deref() {
                Some("eachSect") => NoteNumRestart::EachSect,
                Some("eachPage") => NoteNumRestart::EachPage,
                _ => NoteNumRestart::Continuous,
            });
            true
        }
        _ => false,
    }
}

/// Serialize `<w:footnotePr>` / `<w:endnotePr>` for `props`; emits
/// nothing when every field is unset. Child order follows the schema
/// sequence: pos, numFmt, numStart, numRestart.
pub fn emit_note_pr(elem: &str, props: &NoteProps, out: &mut String) {
    if props.is_empty() {
        return;
    }
    out.push('<');
    out.push_str(elem);
    out.push('>');
    if let Some(pos) = props.position {
        let v = match pos {
            NotePosition::PageBottom => "pageBottom",
            NotePosition::BeneathText => "beneathText",
            NotePosition::SectEnd => "sectEnd",
            NotePosition::DocEnd => "docEnd",
        };
        out.push_str(&format!("<w:pos w:val=\"{v}\"/>"));
    }
    if let Some(fmt) = props.num_format {
        let v = match fmt {
            engine::PageNumFormat::Decimal => "decimal",
            engine::PageNumFormat::LowerRoman => "lowerRoman",
            engine::PageNumFormat::UpperRoman => "upperRoman",
            engine::PageNumFormat::LowerLetter => "lowerLetter",
            engine::PageNumFormat::UpperLetter => "upperLetter",
        };
        out.push_str(&format!("<w:numFmt w:val=\"{v}\"/>"));
    }
    if let Some(start) = props.num_start {
        out.push_str(&format!("<w:numStart w:val=\"{start}\"/>"));
    }
    if let Some(restart) = props.num_restart {
        let v = match restart {
            NoteNumRestart::Continuous => "continuous",
            NoteNumRestart::EachSect => "eachSect",
            NoteNumRestart::EachPage => "eachPage",
        };
        out.push_str(&format!("<w:numRestart w:val=\"{v}\"/>"));
    }
    out.push_str("</");
    out.push_str(elem);
    out.push('>');
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parts::styles::StyleTable;

    const FOOTNOTES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:footnotes xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml"><w:footnote w:type="separator" w:id="-1"><w:p><w:r><w:separator/></w:r></w:p></w:footnote><w:footnote w:type="continuationSeparator" w:id="0"><w:p><w:r><w:continuationSeparator/></w:r></w:p></w:footnote><w:footnote w:id="1"><w:p w14:paraId="0A1B2C3D"><w:pPr><w:pStyle w:val="FootnoteText"/></w:pPr><w:r><w:rPr><w:rStyle w:val="FootnoteReference"/></w:rPr><w:footnoteRef/></w:r><w:r><w:t xml:space="preserve"> First note</w:t></w:r></w:p></w:footnote><w:footnote w:id="2"><w:p><w:r><w:footnoteRef/></w:r><w:r><w:t xml:space="preserve"> Second</w:t></w:r></w:p><w:p><w:r><w:t xml:space="preserve">second paragraph</w:t></w:r></w:p></w:footnote></w:footnotes>"#;

    #[test]
    fn parses_entries_with_types_bodies_and_source_bytes() {
        let table = StyleTable::default();
        let resolver = StyleResolver::new(&table);
        let part = parse_footnotes_xml(FOOTNOTES.as_bytes(), &resolver).unwrap();
        assert_eq!(part.notes.len(), 4);
        assert_eq!(part.notes[0].id, -1);
        assert_eq!(part.notes[0].note_type, NoteType::Separator);
        assert_eq!(part.notes[1].note_type, NoteType::ContinuationSeparator);
        let first = &part.notes[2];
        assert_eq!(first.id, 1);
        assert_eq!(first.note_type, NoteType::Normal);
        assert_eq!(first.kind, NoteKind::Footnote);
        assert_eq!(first.body.len(), 1);
        let p = first.body[0].as_paragraph().unwrap();
        assert_eq!(p.text, "\u{FFFC} First note");
        assert!(matches!(
            p.inline_objects.first().map(|o| &o.kind),
            Some(engine::InlineKind::NoteSelfRef {
                kind: NoteKind::Footnote
            })
        ));
        assert_eq!(p.style_id.as_deref(), Some("FootnoteText"));
        assert!(!p.dirty, "clean on load");
        assert!(p.source_xml.is_some(), "paragraph passthrough bytes kept");
        let raw = first.source_xml.as_deref().unwrap();
        assert!(raw.starts_with(b"<w:footnote w:id=\"1\">"));
        assert!(raw.ends_with(b"</w:footnote>"));
        let second = &part.notes[3];
        assert_eq!(second.body.len(), 2);
        assert_eq!(
            second.body[1].as_paragraph().unwrap().text,
            "second paragraph"
        );
        assert!(
            part.root_attrs.iter().any(|(k, _)| k == "xmlns:w14"),
            "root bindings captured"
        );
    }

    #[test]
    fn endnotes_parse_with_their_own_element_names() {
        let xml = r#"<w:endnotes xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:endnote w:id="1"><w:p><w:r><w:endnoteRef/></w:r><w:r><w:t xml:space="preserve"> End</w:t></w:r></w:p></w:endnote></w:endnotes>"#;
        let table = StyleTable::default();
        let resolver = StyleResolver::new(&table);
        let part = parse_notes_xml(xml.as_bytes(), NoteKind::Endnote, &resolver).unwrap();
        assert_eq!(part.notes.len(), 1);
        let p = part.notes[0].body[0].as_paragraph().unwrap();
        assert_eq!(p.text, "\u{FFFC} End");
        assert!(matches!(
            p.inline_objects[0].kind,
            engine::InlineKind::NoteSelfRef {
                kind: NoteKind::Endnote
            }
        ));
    }

    #[test]
    fn note_pr_round_trips_every_child() {
        let mut props = NoteProps::default();
        let start = BytesStart::from_content(r#"w:pos w:val="beneathText""#, 5);
        assert!(apply_note_pr_child(b"w:pos", &start, &mut props));
        let fmt = BytesStart::from_content(r#"w:numFmt w:val="lowerRoman""#, 8);
        assert!(apply_note_pr_child(b"w:numFmt", &fmt, &mut props));
        let st = BytesStart::from_content(r#"w:numStart w:val="4""#, 10);
        assert!(apply_note_pr_child(b"w:numStart", &st, &mut props));
        let rs = BytesStart::from_content(r#"w:numRestart w:val="eachSect""#, 12);
        assert!(apply_note_pr_child(b"w:numRestart", &rs, &mut props));
        assert_eq!(props.position, Some(NotePosition::BeneathText));
        assert_eq!(props.num_format, Some(engine::PageNumFormat::LowerRoman));
        assert_eq!(props.num_start, Some(4));
        assert_eq!(props.num_restart, Some(NoteNumRestart::EachSect));
        let mut out = String::new();
        emit_note_pr("w:footnotePr", &props, &mut out);
        assert_eq!(
            out,
            r#"<w:footnotePr><w:pos w:val="beneathText"/><w:numFmt w:val="lowerRoman"/><w:numStart w:val="4"/><w:numRestart w:val="eachSect"/></w:footnotePr>"#
        );
        let mut empty = String::new();
        emit_note_pr("w:endnotePr", &NoteProps::default(), &mut empty);
        assert!(empty.is_empty());
    }
}
