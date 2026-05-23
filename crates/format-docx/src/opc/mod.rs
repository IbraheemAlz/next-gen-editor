//! Open Packaging Conventions (ECMA-376 Part 2 / ISO/IEC 29500-2).
//!
//! The OPC layer knows nothing about WordprocessingML. It speaks
//! ZIP-based packages of XML *parts* glued together by `_rels/*.rels`
//! relationship files and a single `[Content_Types].xml` manifest.
//!
//! `archive` is the runtime container (the pass-through `DocxArchive`).
//! `content_types` and `relationships` are read-only typed parsers
//! (Phase 1): we parse them so later phases can navigate the package
//! structurally; the bytes themselves still ride the pass-through.

pub mod archive;
pub mod content_types;
pub mod relationships;
