//! Issue #345 — encrypted packages and document protection.
//!
//! - An MS-OFFCRYPTO encrypted package (an OLE compound file, not a ZIP)
//!   answers the typed `Event::Error { kind: EncryptedDocument }`, and the
//!   open document stays open.

use super::*;

fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    use std::task::{Context, Poll, Waker};
    let mut cx = Context::from_waker(Waker::noop());
    let mut fut = Box::pin(fut);
    match fut.as_mut().poll(&mut cx) {
        Poll::Ready(v) => v,
        Poll::Pending => panic!("Engine::apply suspended in a native test"),
    }
}

fn apply(e: &mut Engine, cmd: Command) -> Event {
    block_on(e.apply(cmd))
}

fn engine_with(doc: DocumentTree) -> Engine {
    let mut e = assemble_engine(None, None);
    let bytes = include_bytes!("../../../ts/fonts/LiberationSans-Regular.ttf").to_vec();
    let font = LoadedFont::parse("test-latin".to_string(), bytes).expect("parse test font");
    e.fonts.insert("test-latin".to_string(), Arc::new(font));
    e.layout_cfg = Some(RenderConfig {
        font_id: "test-latin".to_string(),
        base_direction: ShapingDirection::Ltr,
        px_size: 16.0,
        line_height: 26.0,
        alignment: Alignment::Start,
        scale: 1.0,
        base_scale: 1.0,
        zoom: 1.0,
    });
    e.undo = UndoStack::new(doc, UNDO_CAP);
    e.selection = Some(SelectionState {
        anchor: bpos_top(0, 0),
        caret: bpos_top(0, 0),
        ideal_x: None,
        kind: SelectionKind::Linear,
    });
    e.review_date = "2026-01-01T00:00:00Z".into();
    e
}

fn open_docx(e: &mut Engine, bytes: Vec<u8>) -> Event {
    apply(
        e,
        Command::OpenDocument {
            bytes,
            format: DocFormat::Docx,
            name: Some("x.docx".into()),
            defaults: None,
            limits: None,
        },
    )
}

/// The Apache POI corpus fixture (`bug53475-password-is-pass.docx`, agile
/// encryption, password `pass`) when the local corpus is present; the
/// tests that use it skip silently elsewhere (CI has no corpus).
fn corpus_password_fixture() -> Option<Vec<u8>> {
    std::fs::read("/data/corpus/files/apache-poi-test-data-document/bug53475-password-is-pass.docx")
        .ok()
}

/// Issue #345 — an encrypted package (synthesized compound file with the
/// two MS-OFFCRYPTO streams, and the real corpus fixture when present)
/// answers `EncryptedDocument` through BOTH open commands, never a ZIP
/// error, and the open document is untouched.
#[test]
fn an_encrypted_package_answers_the_typed_error() {
    let synthesized = format_docx::opc::cfb::test_writer::build(&[
        (
            "EncryptionInfo",
            b"\x04\x00\x04\x00\x40\x00\x00\x00<encryption/>",
        ),
        ("EncryptedPackage", &[0u8; 5000]),
    ]);
    let mut packages = vec![synthesized];
    packages.extend(corpus_password_fixture());
    for bytes in packages {
        let mut e = engine_with(DocumentTree::from_text("keep me"));
        let evt = open_docx(&mut e, bytes.clone());
        let Event::Error { message, kind } = evt else {
            panic!("an encrypted package answers Error, got {evt:?}");
        };
        assert_eq!(
            kind,
            Some(bridge::ErrorKind::EncryptedDocument),
            "{message}"
        );
        assert!(message.contains("encrypted"), "{message}");
        assert!(!message.to_ascii_lowercase().contains("zip"), "{message}");
        assert_eq!(e.undo.current().to_plain_text(), "keep me");

        let evt = apply(&mut e, Command::LoadDocx { bytes });
        assert!(
            matches!(
                evt,
                Event::Error {
                    kind: Some(bridge::ErrorKind::EncryptedDocument),
                    ..
                }
            ),
            "{evt:?}"
        );
        assert_eq!(e.undo.current().to_plain_text(), "keep me");
    }
}

/// Issue #345 — a compound file WITHOUT the encryption streams (a legacy
/// binary `.doc`) is an honest, untyped refusal naming the format.
#[test]
fn a_legacy_compound_file_is_not_reported_as_encrypted() {
    let doc = format_docx::opc::cfb::test_writer::build(&[("WordDocument", &[1u8; 600])]);
    let mut e = engine_with(DocumentTree::from_text("keep me"));
    let evt = open_docx(&mut e, doc);
    let Event::Error { message, kind } = evt else {
        panic!("a legacy compound file answers Error, got {evt:?}");
    };
    assert_eq!(kind, None, "{message}");
    assert!(message.contains("97-2003"), "{message}");
    assert_eq!(e.undo.current().to_plain_text(), "keep me");
}

fn selection_protection(e: &mut Engine) -> Option<bridge::ProtectionMode> {
    let evt = apply(
        e,
        Command::SetSelection {
            range: BridgeLogicalRange {
                start: bpos_top(0, 0),
                end: bpos_top(0, 0),
            },
            caret: bpos_top(0, 0),
        },
    );
    let Event::SelectionChanged { protection, .. } = evt else {
        panic!("SetSelection answers SelectionChanged, got {evt:?}");
    };
    protection
}

/// Issue #345 — `SelectionChanged.protection` reports the enforced mode of
/// the open document, and a new document clears it.
#[test]
fn selection_changed_reports_the_enforced_mode() {
    let mut e = engine_with(DocumentTree::from_text("plain"));
    assert_eq!(selection_protection(&mut e), None);
    let evt = open_docx(&mut e, format_docx::test_fixtures::forms_protected_docx());
    assert!(matches!(evt, Event::DocumentLoaded { .. }), "{evt:?}");
    assert_eq!(
        selection_protection(&mut e),
        Some(bridge::ProtectionMode::Forms)
    );
    let evt = apply(&mut e, Command::CloseDocument);
    assert!(
        matches!(
            evt,
            Event::SelectionChanged {
                protection: None,
                ..
            }
        ),
        "{evt:?}"
    );
    /* Not enforced → not reported. */
    let off = format_docx::test_fixtures::protected_docx(
        "<w:p><w:r><w:t>x</w:t></w:r></w:p>",
        "<w:documentProtection w:edit=\"readOnly\" w:enforcement=\"0\"/>",
    );
    open_docx(&mut e, off);
    assert_eq!(selection_protection(&mut e), None);
}
