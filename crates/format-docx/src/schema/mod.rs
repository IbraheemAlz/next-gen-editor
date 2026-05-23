//! Shared OOXML schema constants + element helpers reused across part parsers.
//!
//! The `ct_*` submodules wrap individual `CT_*` complex types from the
//! WordprocessingML schema (ECMA-376 Part 1, §17). They are read-side helpers
//! shared between every `parts::*` parser — writer-side serialization stays
//! local to each part for now.

pub mod ct_ppr;
pub mod ct_rpr;

/// WordprocessingML namespace.
pub const NS_W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

/// Relationships namespace (used by `*.rels` parts).
pub const NS_R: &str = "http://schemas.openxmlformats.org/package/2006/relationships";

/// Content-types namespace (used by `[Content_Types].xml`).
pub const NS_CT: &str = "http://schemas.openxmlformats.org/package/2006/content-types";
