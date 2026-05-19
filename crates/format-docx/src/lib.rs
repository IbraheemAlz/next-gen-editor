//! `format-docx` — minimal `.docx` reader + writer.
//!
//! Phase 1 weeks 19–24 scope: parse `word/document.xml` paragraphs + runs,
//! ignore complex formatting. Preserve all other archive entries verbatim on
//! round-trip so the saved file diffs only inside the edited region.

pub mod reader;
pub mod writer;

pub use reader::{DocxArchive, DocxError, read_docx};
pub use writer::write_docx;
