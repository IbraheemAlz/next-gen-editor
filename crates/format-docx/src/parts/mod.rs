//! Per-part parsers (one module per `word/*.xml` member of the OPC archive).
//!
//! Phase 1 ships `document` only. Phase 2+ adds `styles`, `numbering`,
//! `settings`, `theme`, `header`, `footer`, `footnotes`, `endnotes`,
//! `comments`. See `OOXML_ROADMAP.md` §1.1 for the full target layout.

pub mod comments;
pub mod document;
pub mod footer;
pub mod footnotes;
pub mod header;
pub mod numbering;
pub mod rels;
pub mod settings;
pub mod styles;
pub mod table;
