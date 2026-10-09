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
    /// Issue #353 — a relationship target (or archive path) resolves
    /// outside the package (`../../x`) or carries a NUL: refused instead
    /// of being clamped into some other part's name.
    #[error("unsafe part name: {0}")]
    UnsafePartName(String),
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
    /// Issue #325 — the main part binds WordprocessingML under a prefix
    /// (or as the default namespace) the literal-qname reader does not
    /// match. `normalized` = the part was re-prefixed into the canonical
    /// spelling and read; it is then **regenerate-only** (a zero-edit
    /// save re-emits the normalised bytes, not the source's). `false` =
    /// normalisation itself failed and the part read as-is (likely empty).
    NonCanonicalNamespaces { detail: String, normalized: bool },
    /// Issue #325 — the main part's root is not a WordprocessingML element
    /// in either namespace family; it reads as an empty document.
    NotWordprocessingMl,
    /// Issue #353 — `_rels/.rels` names an `officeDocument` part the
    /// archive does not contain; the fixed `word/document.xml` was used.
    MainPartFallback { target: String },
    /// Issue #353 — a relationship target of a sibling part escapes the
    /// package; it was ignored (the fixed sibling name applies).
    UnsafeRelationshipTarget { target: String },
}

/// Issues #244 / #245 — non-fatal writer diagnostics: a best-effort
/// decision [`crate::writer::write_docx_with_notes`] took to keep
/// unmodeled content instead of dropping it. The file was written and is
/// well-formed; the content may sit at an approximate position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteNote {
    /// A regenerated paragraph's source markup was stale (an edit path
    /// that does not remap its offsets): its must-survive markup — legacy
    /// form fields, content-control boundaries — was written at offsets
    /// clamped to the current text.
    StaleMarkupClamped { markers: u32 },
    /// Issue #245 — the run-level content control `id` (the source byte
    /// offset of its `<w:sdt>`) would have crossed a regenerated wrapper
    /// (hyperlink, revision, field) or another control after an edit; its
    /// range was widened to enclose it so the part stays well-formed.
    InlineWrapperWidened { id: u32 },
}
