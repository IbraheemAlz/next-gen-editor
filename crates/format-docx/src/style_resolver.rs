//! Style cascade resolver (Phase 3 placeholder).
//!
//! Phase 3 will read `word/styles.xml` into a `StyleTable`, resolve
//! `<w:basedOn>` chains, apply the §17.7.2 cascade
//! (`Document Defaults → Table Style → Numbering Style → Paragraph Style →
//! Character Style → Direct Para Props → Direct Run Props`), and bake the
//! resolved formatting into `engine::SpanStyle` / `engine::ParaProperties`
//! so the engine still sees a flat document.
//!
//! Phase 1 ships the module as a stub so later phases can drop their
//! implementation in without touching the public crate layout.
