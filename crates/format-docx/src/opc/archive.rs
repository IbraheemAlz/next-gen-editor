//! `DocxArchive` — the pass-through ZIP container.
//!
//! The reader (`read_docx`) unzips a `.docx`, parses `word/document.xml` into
//! an `engine::DocumentTree`, and stashes **every** other entry verbatim in
//! `other_entries`. The writer (`crate::writer::write_docx`) repacks those
//! entries byte-identical alongside a freshly serialized `word/document.xml`.
//!
//! This is the foundation of the round-trip diff bound: parts we do not yet
//! model can never drift, because we re-emit them as raw bytes.

use crate::error::DocxError;
use crate::numbering_resolver::resolve_markers;
use crate::parts::document::parse_document_xml;
use crate::parts::numbering::{NumberingDefinitions, parse_numbering_xml};
use crate::parts::styles::{StyleTable, parse_styles_xml};
use crate::style_resolver::StyleResolver;
use engine::DocumentTree;
use std::io::{Cursor, Read};
use zip::ZipArchive;

pub const DOC_XML: &str = "word/document.xml";
pub const STYLES_XML: &str = "word/styles.xml";
pub const NUMBERING_XML: &str = "word/numbering.xml";

/// All raw archive entries except `word/document.xml`. Carried through the
/// round-trip so the writer can re-emit them verbatim.
#[derive(Debug, Clone)]
pub struct DocxArchive {
    /// `(entry_name, raw bytes)`. Order preserved from the original archive.
    pub other_entries: Vec<(String, Vec<u8>)>,
    /// Pre-parsed paragraphs from `word/document.xml`.
    pub document: DocumentTree,
}

impl DocxArchive {
    /// Look up a sibling part by exact archive entry name (e.g.
    /// `"word/styles.xml"`). Returns `None` if the part is absent.
    ///
    /// Useful to later phases that need to fetch a specific sibling —
    /// `parts::styles` reads `word/styles.xml`, `parts::numbering` reads
    /// `word/numbering.xml`, etc. — without scanning the full `Vec`.
    pub fn part_by_name(&self, name: &str) -> Option<&[u8]> {
        self.other_entries
            .iter()
            .find_map(|(n, b)| (n == name).then_some(b.as_slice()))
    }
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

    /* Phase 3 — `word/styles.xml` rides the pass-through but feeds the
    cascade resolver. Absent or malformed → empty table (all paragraphs
    just see direct formatting; behaviour matches pre-Phase-3). */
    let style_table: StyleTable = match other_entries
        .iter()
        .find(|(n, _)| n == STYLES_XML)
        .map(|(_, b)| parse_styles_xml(b))
    {
        Some(Ok(t)) => t,
        _ => StyleTable::default(),
    };
    let resolver = StyleResolver::new(&style_table);
    let mut document = parse_document_xml(&xml, &resolver)?;

    /* Phase 4 — `word/numbering.xml` rides the pass-through and feeds the
    numbering resolver. Second pass over the parsed paragraphs fills each
    list paragraph's `resolved_marker`. */
    let numbering: NumberingDefinitions = match other_entries
        .iter()
        .find(|(n, _)| n == NUMBERING_XML)
        .map(|(_, b)| parse_numbering_xml(b))
    {
        Some(Ok(t)) => t,
        _ => NumberingDefinitions::default(),
    };
    if !numbering.num_instances.is_empty() {
        /* `im::Vector` clones cheaply; we collect into a Vec to mutate
        in-place, then rebuild the tree. `resolve_markers` only writes
        `resolved_marker`, so the structurally-shared `Paragraph` fields
        cost nothing here. */
        let mut paras: Vec<_> = document.paragraphs.iter().cloned().collect();
        resolve_markers(&mut paras, &numbering);
        document = DocumentTree::from_rich_paragraphs(paras);
    }

    Ok(DocxArchive {
        other_entries,
        document,
    })
}
