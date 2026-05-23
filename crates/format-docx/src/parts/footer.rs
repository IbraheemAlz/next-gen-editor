//! `word/footer*.xml` — minimal read-only model (Phase 6).
//!
//! Mirror of [`crate::parts::header`]; the footer XML schema is identical to
//! the header schema and the engine's downstream model is the same plain
//! paragraph list. Kept as a separate type so callers reading both can
//! distinguish them without an enum tag.

use crate::error::DocxError;
use crate::parts::header::{HeaderPart, parse_header_xml};

/// One footer part — a flat list of paragraph plain-text bodies. Phase 6
/// scope matches [`HeaderPart`].
#[derive(Debug, Clone, Default)]
pub struct FooterPart {
    pub paragraphs: Vec<String>,
}

impl From<HeaderPart> for FooterPart {
    fn from(h: HeaderPart) -> Self {
        Self {
            paragraphs: h.paragraphs,
        }
    }
}

/// Parse a `word/footer*.xml` byte blob.
pub fn parse_footer_xml(xml: &[u8]) -> Result<FooterPart, DocxError> {
    parse_header_xml(xml).map(FooterPart::from)
}
