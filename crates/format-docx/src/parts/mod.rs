//! Per-part parsers (one module per `word/*.xml` member of the OPC archive).
//!
//! Phase 1 ships `document` only. Phase 2+ adds `styles`, `numbering`,
//! `settings`, `theme`, `header`, `footer`, `footnotes`, `endnotes`,
//! `comments`. See `OOXML_ROADMAP.md` §1.1 for the full target layout.

pub mod document;
pub mod numbering;
pub mod styles;
pub mod table;
