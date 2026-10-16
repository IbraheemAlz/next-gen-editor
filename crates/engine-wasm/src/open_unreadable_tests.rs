//! Issue #491 (item 2) — a package the reader refuses as damaged answers the
//! typed `ErrorKind::DocumentUnreadable` through the real dispatcher
//! (`Engine::apply_sync`, `fuzz-native`), with the open document, selection,
//! story and undo untouched (issue #341's "an error reply changes nothing").

use super::*;
use format_docx::test_fixtures::{document_xml_with_body, package_with_document_xml_bytes};

fn open(engine: &mut Engine, bytes: Vec<u8>) -> Event {
    engine.apply_sync(Command::OpenDocument {
        bytes,
        format: DocFormat::Docx,
        name: Some("damaged.docx".into()),
        defaults: None,
        limits: None,
        password: None,
    })
}

/// The stock part with a SECOND root element spliced behind the real one:
/// the structural defect `check_main_root` / the strict scan cannot repair
/// (#483).
fn two_root_package() -> Vec<u8> {
    let part = document_xml_with_body(r#"<w:p><w:r><w:t>zzalpha</w:t></w:r></w:p>"#);
    let second = document_xml_with_body(r#"<w:p><w:r><w:t>zzbeta</w:t></w:r></w:p>"#);
    // Drop the second copy's XML declaration so only the root count is wrong.
    let second = second
        .split_once("?>")
        .map_or(second.as_str(), |(_, rest)| rest)
        .to_string();
    package_with_document_xml_bytes(format!("{part}{second}").as_bytes(), &[])
}

fn assert_unreadable(evt: &Event, want_part: &str) -> String {
    let Event::Error { message, kind, .. } = evt else {
        panic!("expected a typed refusal, got {evt:?}");
    };
    assert_eq!(
        *kind,
        Some(bridge::ErrorKind::DocumentUnreadable),
        "{message}"
    );
    assert!(message.contains(want_part), "{message}");
    message.clone()
}

#[test]
fn a_two_root_main_part_is_document_unreadable_and_changes_nothing() {
    let mut engine = Engine::new_headless(DocumentTree::from_text("keep me"));
    let before = engine.state_fingerprint_for_fuzzing();
    let text_before = engine.undo.current().to_plain_text();

    let evt = open(&mut engine, two_root_package());
    let message = assert_unreadable(&evt, "word/document.xml");
    /* The message names the part and says what is wrong; it is not the
    document's text. */
    assert!(message.contains("root"), "{message}");
    assert!(
        !message.contains("zzalpha") && !message.contains("zzbeta"),
        "{message}"
    );
    assert_eq!(engine.state_fingerprint_for_fuzzing(), before);
    assert_eq!(engine.undo.current().to_plain_text(), text_before);

    /* The legacy Phase-1 door classifies identically. */
    let evt = engine.apply_sync(Command::LoadDocx {
        bytes: two_root_package(),
    });
    assert_unreadable(&evt, "word/document.xml");
    assert_eq!(engine.state_fingerprint_for_fuzzing(), before);
}

/// A truncated archive is damaged too (not "an unknown format"); a legacy
/// compound file keeps `InvalidDocument`.
#[test]
fn a_truncated_zip_is_document_unreadable() {
    let mut engine = Engine::new_headless(DocumentTree::from_text("keep me"));
    let before = engine.state_fingerprint_for_fuzzing();
    let evt = open(&mut engine, b"PK\x03\x04 truncated".to_vec());
    let Event::Error { kind, message, .. } = &evt else {
        panic!("expected a typed refusal, got {evt:?}");
    };
    assert_eq!(
        *kind,
        Some(bridge::ErrorKind::DocumentUnreadable),
        "{message}"
    );
    assert_eq!(engine.state_fingerprint_for_fuzzing(), before);
}

/// An intact package still opens (the classification only touches the
/// refusal path).
#[test]
fn an_intact_package_still_opens() {
    let mut engine = Engine::new_headless(DocumentTree::from_text("old"));
    let evt = open(
        &mut engine,
        package_with_document_xml_bytes(
            document_xml_with_body(r#"<w:p><w:r><w:t>new</w:t></w:r></w:p>"#).as_bytes(),
            &[],
        ),
    );
    assert!(matches!(evt, Event::DocumentLoaded { .. }), "{evt:?}");
    assert!(engine.undo.current().to_plain_text().starts_with("new"));
}
