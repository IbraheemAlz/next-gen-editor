//! `word/header*.xml` — full-fidelity paragraph parser.
//!
//! Phase 2 audit (gap D.1 follow-up). The Phase-6 version of this
//! reader extracted only paragraph text into `Vec<String>`, which lost
//! every overlay the body parser captures (style spans, hyperlinks,
//! revisions, fields). The new model routes the header XML through the
//! exact same [`parse_document_xml`] pipeline `word/document.xml`
//! uses, so a `<w:fldChar>` PAGE field in a footer hits the engine
//! with the same `engine::Field` overlay it would in body content —
//! and the paginator's per-page field evaluator picks it up.
//!
//! The header XML root is `<w:hdr>` (footer is `<w:ftr>`) instead of
//! `<w:document><w:body>`, but the [`parse_document_xml`] event loop
//! is structurally a `<w:p>` / `<w:r>` / `<w:tbl>` scanner — the root
//! element name doesn't drive any branch. Reusing it costs one extra
//! function call per part and saves ~700 lines of duplicated parser
//! state machine.

use crate::error::DocxError;
use crate::parts::document::parse_document_xml;
use crate::style_resolver::StyleResolver;
use engine::Paragraph;

/// One header part — the parsed paragraph sequence with every overlay
/// the body parser captures. Tables nested inside a header land here
/// too (the parser emits them on the `DocumentTree.blocks` list); the
/// flattener below keeps the legacy paragraph-only surface for the
/// paginator's header band, which doesn't yet render header-internal
/// tables.
#[derive(Debug, Clone, Default)]
pub struct HeaderPart {
    pub paragraphs: Vec<Paragraph>,
}

/// Parse a `word/header*.xml` byte blob. `resolver` is the same style
/// resolver `word/document.xml` parses against — header/footer
/// paragraphs inherit document-wide defaults and named styles.
pub fn parse_header_xml(xml: &[u8], resolver: &StyleResolver<'_>) -> Result<HeaderPart, DocxError> {
    let tree = parse_document_xml(xml, resolver)?;
    /* Headers in practice are paragraph-only; if the file does carry a
    `<w:tbl>` inside the header we drop it for now (rendering header
    tables is a backlog item). The paragraph-only filter keeps the
    paginator's band layout API surface unchanged. */
    let paragraphs = tree
        .blocks
        .iter()
        .filter_map(|b| match b {
            engine::Block::Paragraph(p) => Some(p.clone()),
            engine::Block::Table(_) => None,
        })
        .collect();
    Ok(HeaderPart { paragraphs })
}
