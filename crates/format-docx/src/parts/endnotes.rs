//! `word/endnotes.xml` — mirror of [`crate::parts::footnotes`] (issue
//! #80). The schema is identical modulo element names (`<w:endnotes>`,
//! `<w:endnote>`, `<w:endnoteRef/>`); the shared parser takes the kind.

use crate::error::DocxError;
use crate::parts::footnotes::{NotesPart, parse_notes_xml};
use crate::style_resolver::StyleResolver;
use engine::NoteKind;

/// Parse `word/endnotes.xml`.
pub fn parse_endnotes_xml(
    xml: &[u8],
    resolver: &StyleResolver<'_>,
) -> Result<NotesPart, DocxError> {
    parse_notes_xml(xml, NoteKind::Endnote, resolver)
}
