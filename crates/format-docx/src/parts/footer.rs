//! `word/footer*.xml` — mirror of [`crate::parts::header`].
//!
//! Phase 2 audit (gap D.1 follow-up). The XML schema is identical to
//! the header's; the engine model is the same `Vec<Block>` (issue #72
//! widened both from `Vec<Paragraph>` so tables survive). Kept as a
//! separate type so callers reading both can distinguish them by type
//! without an enum tag.

use crate::error::DocxError;
use crate::parts::header::{HeaderPart, parse_header_xml};
use crate::style_resolver::StyleResolver;
use engine::Block;

#[derive(Debug, Clone, Default)]
pub struct FooterPart {
    pub blocks: Vec<Block>,
}

impl From<HeaderPart> for FooterPart {
    fn from(h: HeaderPart) -> Self {
        Self { blocks: h.blocks }
    }
}

/// Parse a `word/footer*.xml` byte blob.
pub fn parse_footer_xml(xml: &[u8], resolver: &StyleResolver<'_>) -> Result<FooterPart, DocxError> {
    parse_header_xml(xml, resolver).map(FooterPart::from)
}
