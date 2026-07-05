//! `word/header*.xml` — full-fidelity block parser.
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
//! Issue #72 widened the part surface from `Vec<Paragraph>` (which
//! silently dropped `<w:tbl>` inside a header) to the body's own
//! `Vec<Block>` — tables inside header/footer parts now survive the
//! read and ride the same block model everywhere downstream.
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
use engine::Block;

/// One header part — the parsed block sequence with every overlay the
/// body parser captures, tables included.
#[derive(Debug, Clone, Default)]
pub struct HeaderPart {
    pub blocks: Vec<Block>,
}

/// Parse a `word/header*.xml` byte blob. `resolver` is the same style
/// resolver `word/document.xml` parses against — header/footer
/// paragraphs inherit document-wide defaults and named styles.
pub fn parse_header_xml(xml: &[u8], resolver: &StyleResolver<'_>) -> Result<HeaderPart, DocxError> {
    let tree = parse_document_xml(xml, resolver)?;
    /* A stray sectPr inside a header part is meaningless — the body
    parser may have stamped a `section_end` marker; drop it so a part
    can never masquerade as a section boundary carrier. */
    let blocks = tree
        .blocks
        .iter()
        .cloned()
        .map(|b| match b {
            Block::Paragraph(mut p) => {
                p.section_end = None;
                Block::Paragraph(p)
            }
            table => table,
        })
        .collect();
    Ok(HeaderPart { blocks })
}
