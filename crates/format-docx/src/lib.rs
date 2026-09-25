//! `format-docx` — `.docx` reader + writer.
//!
//! ## Module layout (Phase 1 OPC refactor — see `OOXML_ROADMAP.md` §1.1)
//!
//! - [`opc`] — Open Packaging Conventions (ZIP container, content types,
//!   relationships). Knows nothing about WordprocessingML.
//! - [`parts`] — per-XML-part parsers (`document` ships in Phase 1;
//!   `styles`, `numbering`, `settings`, `theme`, `header`, `footer`,
//!   `footnotes`, `endnotes`, `comments` arrive in later phases).
//! - [`schema`] — shared OOXML element helpers (`ct_rpr`, etc.).
//! - [`error`] — the crate-wide [`DocxError`].
//! - [`style_resolver`] — Phase 3 cascade resolver (stub today).
//! - [`reader`] / [`writer`] — top-level orchestration.
//!
//! Public API stays stable across the refactor: [`read_docx`],
//! [`write_docx`], [`DocxArchive`], [`DocxError`], and
//! [`writer::build_minimal_docx`] keep their pre-refactor paths.

pub mod error;
mod media_plan;
pub mod numbering_resolver;
pub mod opc;
pub mod parts;
pub mod reader;
pub mod schema;
pub mod style_resolver;
#[doc(hidden)]
pub mod test_fixtures;
pub mod writer;

pub use error::{DocxError, DocxWarning};
pub use opc::archive::{
    DOC_XML, DocxArchive, check_document_xml_well_formed, check_part_xml_well_formed, read_docx,
    read_docx_with_settings,
};
pub use writer::{build_minimal_docx, save_docx, write_docx};

/// Issue #213 — build a clipboard `.docx` fragment for `doc`, and — when
/// the source document carries a retained package (`DocumentTree::
/// source_package`, issue #134) — additively splice its `styles.xml` /
/// `numbering.xml` / theme / `fontTable.xml` parts into the fragment
/// (`opc::splice::add_style_parts`), so pasting the fragment into Word or
/// another instance of this editor resolves the style and numbering
/// definitions its paragraphs reference instead of silently falling back
/// to plain defaults. A document with no retained package (never opened
/// from `.docx`, or an in-memory-only tree) falls through byte-identical
/// to [`build_minimal_docx`].
pub fn build_clipboard_fragment_docx(
    doc: &engine::DocumentTree,
    source_package: Option<&engine::SourcePackage>,
) -> Result<Vec<u8>, DocxError> {
    let minimal = writer::build_minimal_docx(doc)?;
    match source_package {
        Some(package) => opc::splice::add_style_parts(&minimal, package),
        None => Ok(minimal),
    }
}
