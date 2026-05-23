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
pub mod opc;
pub mod parts;
pub mod reader;
pub mod schema;
pub mod style_resolver;
pub mod writer;

pub use error::DocxError;
pub use opc::archive::{DOC_XML, DocxArchive, read_docx};
pub use writer::{build_minimal_docx, write_docx};
