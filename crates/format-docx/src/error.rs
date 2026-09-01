//! `DocxError` — shared error type for every reader / writer / OPC parser.

use thiserror::Error;

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
    /// Issue #110 — `check_document_xml_well_formed` found a part that
    /// quick-xml accepted event-by-event but that is not a single
    /// well-formed document (unclosed elements at EOF, no root element).
    #[error("malformed XML: {0}")]
    MalformedXml(String),
}

/// Non-fatal reader diagnostics. The document opened, but some subtree was
/// degraded on the way into the typed model; its bytes still ride the
/// passthrough so a resave loses nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocxWarning {
    /// Issue #111 — a `<w:tbl>` nested `limit` or more levels deep was kept
    /// as an opaque passthrough block (`source_xml` preserved, `rows`
    /// empty) instead of recursing further. Apache POI's
    /// `deep-table-cell.docx` nests 5000 tables; unbounded recursion
    /// overflowed the stack.
    TableNestingTooDeep { limit: u32 },
}
