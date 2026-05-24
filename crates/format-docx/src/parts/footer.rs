//! `word/footer*.xml` — mirror of [`crate::parts::header`].
//!
//! Phase 2 audit (gap D.1 follow-up). The XML schema is identical to
//! the header's; the engine model is the same `Vec<Paragraph>`. Kept
//! as a separate type so callers reading both can distinguish them by
//! type without an enum tag.

use crate::error::DocxError;
use crate::parts::header::{HeaderPart, parse_header_xml};
use crate::style_resolver::StyleResolver;
use engine::Paragraph;

#[derive(Debug, Clone, Default)]
pub struct FooterPart {
    pub paragraphs: Vec<Paragraph>,
}

impl From<HeaderPart> for FooterPart {
    fn from(h: HeaderPart) -> Self {
        Self {
            paragraphs: h.paragraphs,
        }
    }
}

/// Parse a `word/footer*.xml` byte blob.
pub fn parse_footer_xml(xml: &[u8], resolver: &StyleResolver<'_>) -> Result<FooterPart, DocxError> {
    parse_header_xml(xml, resolver).map(FooterPart::from)
}
