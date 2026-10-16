//! Issue #491 — ONE place that classifies an open-time `DocxError` into the
//! typed `Event::Error` the shell presents (`bridge::ErrorKind`).
//!
//! `OpenDocument` / `LoadDocx` used to answer every reader failure other than
//! the four typed refusals (`PackageTooLarge`, `EncryptedDocument`,
//! `WrongPassword`) as one catch-all kind; a damaged part (`MalformedXml`,
//! #483) was indistinguishable from a legacy `.doc`. The table:
//!
//! | `DocxError`                                           | `ErrorKind`           |
//! |-------------------------------------------------------|-----------------------|
//! | `PackageTooLarge`                                     | `PackageTooLarge`     |
//! | `Encrypted`, `UnsupportedEncryption`                  | `EncryptedDocument`   |
//! | `WrongPassword`                                       | `WrongPassword`       |
//! | `MalformedXml`, `Xml`, `XmlAttr`, `Utf8`              | `DocumentUnreadable`  |
//! | `Zip`, `MissingEntry`, `UnsafePartName`               | `DocumentUnreadable`  |
//! | `CompoundFile` (a legacy `.doc`), `Io`                | `InvalidDocument`     |
//!
//! A `DocumentUnreadable` message is `<origin>: <part>: <detail>` and is
//! SCRUBBED: it names the part and an offset, never document bytes (a tag
//! name, a namespace prefix or URI, a quick-xml cause that quotes them) —
//! the shell shows it verbatim in the File menu banner and the Dev HUD.

use bridge::{ErrorKind, Event};
use format_docx::DocxError;

/// Longest scrubbed detail kept (the shell shows it in a banner).
const MAX_DETAIL_CHARS: usize = 240;

/// The `Event::Error` an open that failed with `e` answers. `origin` is the
/// command name (`"OpenDocument"` / `"LoadDocx"`).
pub(crate) fn open_error_event(origin: &str, e: &DocxError) -> Event {
    let (kind, message) = match e {
        DocxError::PackageTooLarge { .. } => (ErrorKind::PackageTooLarge, e.to_string()),
        /* Issue #345 — an encrypted (password-protected) package: the
        shell says so instead of "not a zip". */
        DocxError::Encrypted | DocxError::UnsupportedEncryption(_) => {
            (ErrorKind::EncryptedDocument, e.to_string())
        }
        DocxError::WrongPassword => (ErrorKind::WrongPassword, e.to_string()),
        DocxError::MalformedXml(detail) => (ErrorKind::DocumentUnreadable, scrub_detail(detail)),
        DocxError::MissingEntry(part) => (
            ErrorKind::DocumentUnreadable,
            format!("{}: the part is missing from the package", scrub_part(part)),
        ),
        DocxError::UnsafePartName(_) => (
            ErrorKind::DocumentUnreadable,
            "a relationship points outside the package".to_string(),
        ),
        /* quick-xml's errors carry no part name (and their text can quote
        element names): a fixed class phrase, nothing from the document. */
        DocxError::Xml(_) | DocxError::XmlAttr(_) => (
            ErrorKind::DocumentUnreadable,
            "a package part is not well-formed XML".to_string(),
        ),
        DocxError::Utf8(_) => (
            ErrorKind::DocumentUnreadable,
            "a package part is not valid UTF-8 text".to_string(),
        ),
        /* The zip crate's messages are static strings, never entry bytes. */
        DocxError::Zip(z) => (
            ErrorKind::DocumentUnreadable,
            format!("the file is not a readable ZIP package ({z})"),
        ),
        DocxError::CompoundFile | DocxError::Io(_) => (ErrorKind::InvalidDocument, e.to_string()),
    };
    Event::Error {
        message: format!("{origin}: {message}"),
        kind: Some(kind),
        command: None,
    }
}

/// A part name is attacker-controlled text too (a relationship target can
/// point anywhere in the package): keep it short and printable.
fn scrub_part(part: &str) -> String {
    part.chars()
        .filter(|c| !c.is_control())
        .take(80)
        .collect::<String>()
}

/// Drop everything document-derived from a reader `MalformedXml` detail,
/// keeping the part name, the defect class and the offsets. The producers
/// (`opc::archive::check_document_xml_well_formed`, `opc::well_formed`,
/// `schema::ns_normalize`) quote what they found in backticks, list
/// undeclared prefixes in parentheses and append quick-xml's own cause text;
/// each of those is cut here, and the result is bounded.
pub(crate) fn scrub_detail(detail: &str) -> String {
    let mut out = String::with_capacity(detail.len().min(MAX_DETAIL_CHARS * 2));
    let mut in_quote = false;
    for c in detail.chars() {
        match c {
            '`' => {
                if in_quote {
                    in_quote = false;
                } else {
                    in_quote = true;
                    out.push('…');
                }
            }
            _ if in_quote => {}
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
    // `markup cut off or unreadable at byte N: <quick-xml cause>`.
    let out = cut_after_offset(&out, "unreadable at byte ");
    // `N undeclared namespace prefixes (a, b, c)`.
    let out = drop_list(&out, "undeclared namespace prefixes (");
    // `…` quoted twice in a row (`close the open …` shapes) reads fine; a
    // lone empty quote pair left behind by a cut does not.
    let mut s: String = out.chars().take(MAX_DETAIL_CHARS).collect();
    if out.chars().count() > MAX_DETAIL_CHARS {
        s.push('…');
    }
    s
}

/// Keep `marker` + the digits after it; drop whatever follows them.
fn cut_after_offset(s: &str, marker: &str) -> String {
    match s.find(marker) {
        Some(at) => {
            let digits_from = at + marker.len();
            let digits = s[digits_from..]
                .chars()
                .take_while(char::is_ascii_digit)
                .count();
            s[..digits_from + digits].to_string()
        }
        None => s.to_string(),
    }
}

/// Remove the parenthesised list that follows `marker` (`marker` ends with
/// the opening parenthesis) — and the marker's own `(`.
fn drop_list(s: &str, marker: &str) -> String {
    match s.find(marker) {
        Some(at) => {
            let open = at + marker.len() - 1;
            let after = s[open..]
                .find(')')
                .map_or(s.len(), |close| open + close + 1);
            format!("{}{}", s[..open].trim_end(), &s[after..])
        }
        None => s.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind_and_message(e: &DocxError) -> (Option<ErrorKind>, String) {
        match open_error_event("OpenDocument", e) {
            Event::Error { kind, message, .. } => (kind, message),
            other => panic!("expected an Error, got {other:?}"),
        }
    }

    #[test]
    fn a_malformed_part_is_document_unreadable_and_keeps_part_and_offset() {
        let e = DocxError::MalformedXml(
            "word/document.xml: a second root element at byte 1234 (no faithful repair)".into(),
        );
        let (kind, message) = kind_and_message(&e);
        assert_eq!(kind, Some(ErrorKind::DocumentUnreadable));
        assert_eq!(
            message,
            "OpenDocument: word/document.xml: a second root element at byte 1234 (no faithful repair)"
        );
    }

    /// Every producer's quoting shape loses the document's own text.
    #[test]
    fn the_message_never_echoes_document_bytes() {
        for raw in [
            "word/document.xml: `</w:secretTag>` at byte 77 does not close the open `<w:otherSecret>`",
            "word/document.xml: unbound namespace prefix on element `zq:secretName` at byte 9",
            "word/document.xml: markup cut off or unreadable at byte 31: ill-formed document: expected `</w:secretTag>`, but `</w:p>` was found",
            "word/document.xml: 2 undeclared namespace prefixes (zqsecret, yqsecret); first defect near offset 40",
            "namespace prefix `w` would need two URIs (`urn:secret-a` and `urn:secret-b`)",
        ] {
            let scrubbed = scrub_detail(raw);
            assert!(
                !scrubbed.to_lowercase().contains("secret"),
                "{raw:?} leaked through as {scrubbed:?}"
            );
        }
        // …and what is kept names the part, the class and the offset.
        let kept = scrub_detail(
            "word/document.xml: 2 undeclared namespace prefixes (zqsecret, yqsecret); first defect near offset 40",
        );
        assert_eq!(
            kept,
            "word/document.xml: 2 undeclared namespace prefixes; first defect near offset 40"
        );
        let cut = scrub_detail(
            "word/document.xml: markup cut off or unreadable at byte 31: ill-formed document: x",
        );
        assert_eq!(
            cut,
            "word/document.xml: markup cut off or unreadable at byte 31"
        );
    }

    #[test]
    fn a_long_or_control_laden_detail_is_bounded_and_printable() {
        let raw = format!("word/document.xml: {}\u{7}x", "a".repeat(1000));
        let scrubbed = scrub_detail(&raw);
        assert!(scrubbed.chars().count() <= MAX_DETAIL_CHARS + 1);
        assert!(!scrubbed.chars().any(char::is_control));
    }

    #[test]
    fn the_other_open_failures_keep_their_kinds() {
        let (k, _) = kind_and_message(&DocxError::Encrypted);
        assert_eq!(k, Some(ErrorKind::EncryptedDocument));
        let (k, _) = kind_and_message(&DocxError::UnsupportedEncryption("rc4".into()));
        assert_eq!(k, Some(ErrorKind::EncryptedDocument));
        let (k, _) = kind_and_message(&DocxError::WrongPassword);
        assert_eq!(k, Some(ErrorKind::WrongPassword));
        let (k, m) = kind_and_message(&DocxError::CompoundFile);
        assert_eq!(k, Some(ErrorKind::InvalidDocument));
        assert!(m.contains("97-2003"), "{m}");
        let (k, _) = kind_and_message(&DocxError::PackageTooLarge {
            limit: format_docx::opc::limits::PackageLimit::XmlDepth,
            max: 256,
            part: Some("word/document.xml".into()),
        });
        assert_eq!(k, Some(ErrorKind::PackageTooLarge));
    }

    #[test]
    fn structural_failures_without_a_part_say_so_without_quoting_anything() {
        let (k, m) = kind_and_message(&DocxError::MissingEntry("word/document.xml".into()));
        assert_eq!(k, Some(ErrorKind::DocumentUnreadable));
        assert_eq!(
            m,
            "OpenDocument: word/document.xml: the part is missing from the package"
        );
        let (k, _) = kind_and_message(&DocxError::UnsafePartName("../../etc/passwd".into()));
        assert_eq!(k, Some(ErrorKind::DocumentUnreadable));
        let (_, m) = kind_and_message(&DocxError::UnsafePartName("../../etc/passwd".into()));
        assert!(!m.contains("passwd"), "{m}");
        let zip = format_docx::read_docx(b"PK\x03\x04 truncated").expect_err("not a package");
        assert!(matches!(zip, DocxError::Zip(_)), "{zip:?}");
        let (k, _) = kind_and_message(&zip);
        assert_eq!(k, Some(ErrorKind::DocumentUnreadable));
    }
}
