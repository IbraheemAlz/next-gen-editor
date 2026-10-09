//! Issues #439 / #434 — strict well-formedness of the WordprocessingML parts
//! the reader walks, and the up-front repair that keeps a malformed part
//! readable while every save of it stays well-formed.
//!
//! The typed parsers stream with quick-xml, which tolerates much that XML
//! 1.0 forbids — bytes that are not UTF-8 among them — as long as it never
//! has to decode them. Such bytes used to sit in regions the reader skips
//! or keeps verbatim (an unselected `mc:Choice`, an unmodeled property, a
//! tag name): a capture that needed UTF-8 then fell back to regeneration
//! and lost the markup around it, so a zero-edit save read back with
//! different text (issue #439, a `docx_roundtrip` finding).
//!
//! Policy: every WordprocessingML part is checked BEFORE any typed walk
//! ([`repair_part`]; it runs after `limits::check_xml_part`, so the part is
//! already inside the [`PackageLimits`] shape bounds):
//!
//! - a well-formed part is returned untouched — the byte-identical fast
//!   path every real-world document takes;
//! - a repairable defect is fixed in place and reported as
//!   [`DocxWarning::MalformedPart`] `{ repaired: true }`. The part is then
//!   **regenerate-only**, exactly like a #325 namespace-normalised part:
//!   the repaired bytes REPLACE the source bytes before any capture, so
//!   every passthrough, verbatim span and in-place patch starts from them
//!   and a zero-edit save re-emits the repaired (well-formed) part, never
//!   the source;
//! - a defect with no faithful repair leaves the part as it is, reported
//!   with `repaired: false` (the typed parse decides, as before).

use crate::error::{DocxError, DocxWarning};
use crate::opc::limits::{PackageLimit, PackageLimits};

const BOM: &[u8] = b"\xEF\xBB\xBF";

/// `(bom, body)` — a leading UTF-8 byte-order mark split off (quick-xml
/// strips it without counting it in its positions, so every scan runs on
/// the body alone).
fn split_bom(xml: &[u8]) -> (&[u8], &[u8]) {
    match xml.strip_prefix(BOM) {
        Some(body) => (BOM, body),
        None => (&[][..], xml),
    }
}

/// `true` for a part encoded as UTF-16 (a UTF-16 byte-order mark): the
/// reader decodes UTF-8 only, so such a part is reported, never "repaired"
/// byte by byte into noise.
fn is_utf16(xml: &[u8]) -> bool {
    xml.starts_with(b"\xFF\xFE") || xml.starts_with(b"\xFE\xFF")
}

/// Issues #439 / #434 — check one WordprocessingML part (`name`, its
/// archive entry) and return the bytes the reader must parse: `xml`
/// itself when it is well-formed, else its repaired spelling (reported on
/// `warnings`; see the module docs). A repaired part larger than
/// `limits.max_part_bytes` is refused like any oversized part.
pub(crate) fn repair_part(
    name: &str,
    xml: Vec<u8>,
    limits: &PackageLimits,
    warnings: &mut Vec<DocxWarning>,
) -> Result<Vec<u8>, DocxError> {
    if is_utf16(&xml) {
        warnings.push(DocxWarning::MalformedPart {
            part: name.to_string(),
            detail: "UTF-16 encoded (only UTF-8 parts are read)".to_string(),
            repaired: false,
        });
        return Ok(xml);
    }
    let (bom, body) = split_bom(&xml);
    let bad_at = match std::str::from_utf8(body) {
        Ok(_) => return Ok(xml),
        Err(e) => e.valid_up_to(),
    };
    /* Every byte sequence that is not UTF-8 becomes U+FFFD — a valid
    character in text, attribute values AND names, so the markup around
    it keeps its shape. */
    let mut out = Vec::with_capacity(xml.len() + 16);
    out.extend_from_slice(bom);
    out.extend_from_slice(String::from_utf8_lossy(body).as_bytes());
    if out.len() as u64 > limits.max_part_bytes {
        return Err(DocxError::PackageTooLarge {
            limit: PackageLimit::PartBytes,
            max: limits.max_part_bytes,
            part: Some(name.to_string()),
        });
    }
    warnings.push(DocxWarning::MalformedPart {
        part: name.to_string(),
        detail: format!("not UTF-8 (first invalid byte at offset {bad_at})"),
        repaired: true,
    });
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(xml: &[u8]) -> (Vec<u8>, Vec<DocxWarning>) {
        let mut w = Vec::new();
        let out = repair_part(
            "word/document.xml",
            xml.to_vec(),
            &PackageLimits::DEFAULT,
            &mut w,
        )
        .expect("repair");
        (out, w)
    }

    #[test]
    fn a_well_formed_part_is_returned_untouched() {
        let xml =
            "\u{feff}<w:document><w:body><w:p><w:r><w:t>é</w:t></w:r></w:p></w:body></w:document>";
        let (out, w) = run(xml.as_bytes());
        assert_eq!(out, xml.as_bytes());
        assert!(w.is_empty(), "{w:?}");
    }

    #[test]
    fn bytes_that_are_not_utf8_become_replacement_characters() {
        let xml = b"\xEF\xBB\xBF<w:document><w:fld\xC3har/><w:t>a\xFFb</w:t></w:document>";
        let (out, w) = run(xml);
        assert_eq!(
            out,
            "\u{feff}<w:document><w:fld\u{FFFD}har/><w:t>a\u{FFFD}b</w:t></w:document>".as_bytes()
        );
        assert!(
            matches!(
                w.as_slice(),
                [DocxWarning::MalformedPart { part, repaired: true, .. }] if part == "word/document.xml"
            ),
            "{w:?}"
        );
    }

    #[test]
    fn utf16_parts_are_reported_not_rewritten() {
        let xml = b"\xFF\xFE<\0w\0/\0>\0";
        let (out, w) = run(xml);
        assert_eq!(out, xml);
        assert!(matches!(
            w.as_slice(),
            [DocxWarning::MalformedPart {
                repaired: false,
                ..
            }]
        ));
    }

    #[test]
    fn a_repair_past_the_part_budget_is_refused() {
        let limits = PackageLimits {
            max_part_bytes: 8,
            ..PackageLimits::DEFAULT
        };
        let err = repair_part(
            "p.xml",
            b"<a>\xFF\xFF</a>".to_vec(),
            &limits,
            &mut Vec::new(),
        )
        .expect_err("grows past 8 bytes");
        assert!(matches!(
            err,
            DocxError::PackageTooLarge {
                limit: PackageLimit::PartBytes,
                ..
            }
        ));
    }
}
