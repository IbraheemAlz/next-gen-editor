//! Back-compat shim. `read_docx`, `DocxArchive`, `DOC_XML`, and `DocxError`
//! moved to `opc::archive` / `error` in the Phase 1 OPC refactor; the old
//! paths (`format_docx::reader::*`) are re-exported here so external
//! consumers keep building unchanged.

pub use crate::error::DocxError;
pub use crate::opc::archive::{DOC_XML, DocxArchive, read_docx};
