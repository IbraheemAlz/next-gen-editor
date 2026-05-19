//! `.docx` reader: unzip + parse `word/document.xml` into a `DocumentTree`.
//!
//! Also stashes the other archive entries verbatim so the writer can repack
//! them unchanged. This keeps the round-trip diff localized to
//! `word/document.xml` (we don't re-serialize content types or rels).

use engine::DocumentTree;
use quick_xml::events::Event;
use quick_xml::reader::Reader;
use std::io::{Cursor, Read};
use thiserror::Error;
use zip::ZipArchive;

#[derive(Debug, Error)]
pub enum DocxError {
    #[error("ZIP error: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("XML error: {0}")]
    Xml(#[from] quick_xml::Error),
    #[error("XML attribute error: {0}")]
    XmlAttr(#[from] quick_xml::events::attributes::AttrError),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("required entry missing: {0}")]
    MissingEntry(String),
    #[error("UTF-8 error: {0}")]
    Utf8(#[from] std::str::Utf8Error),
}

pub const DOC_XML: &str = "word/document.xml";

/// All raw archive entries except `word/document.xml`. Carried through the
/// round-trip so the writer can re-emit them verbatim.
#[derive(Debug, Clone)]
pub struct DocxArchive {
    /// (entry_name, raw bytes). Order preserved from the original archive.
    pub other_entries: Vec<(String, Vec<u8>)>,
    /// Pre-parsed paragraphs from `word/document.xml`.
    pub document: DocumentTree,
}

/// Read a `.docx` byte blob → parsed document + stashed sibling entries.
pub fn read_docx(bytes: &[u8]) -> Result<DocxArchive, DocxError> {
    let mut archive = ZipArchive::new(Cursor::new(bytes))?;
    let mut other_entries: Vec<(String, Vec<u8>)> = Vec::new();
    let mut document_xml: Option<Vec<u8>> = None;

    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        let name = file.name().to_owned();
        let mut buf = Vec::with_capacity(file.size() as usize);
        file.read_to_end(&mut buf)?;
        if name == DOC_XML {
            document_xml = Some(buf);
        } else {
            other_entries.push((name, buf));
        }
    }

    let xml = document_xml.ok_or_else(|| DocxError::MissingEntry(DOC_XML.into()))?;
    let document = parse_document_xml(&xml)?;

    Ok(DocxArchive {
        other_entries,
        document,
    })
}

/// Parse `word/document.xml` into paragraphs. Each `<w:p>` is a paragraph;
/// concatenate all `<w:t>` text inside as plain text.
fn parse_document_xml(xml: &[u8]) -> Result<DocumentTree, DocxError> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(false);

    let mut paragraphs: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut in_text_elt = false;
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Start(e) if e.name().as_ref() == b"w:t" => {
                in_text_elt = true;
            }
            Event::End(e) if e.name().as_ref() == b"w:t" => {
                in_text_elt = false;
            }
            Event::Text(t) if in_text_elt => {
                let s = t.unescape()?;
                current.push_str(&s);
            }
            Event::End(e) if e.name().as_ref() == b"w:p" => {
                paragraphs.push(std::mem::take(&mut current));
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }

    Ok(DocumentTree::from_paragraphs(paragraphs))
}
