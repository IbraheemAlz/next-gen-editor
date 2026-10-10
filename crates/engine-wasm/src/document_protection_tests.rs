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
            password: None,
        },
    )
}

fn open_docx_with_password(e: &mut Engine, bytes: Vec<u8>, password: &str) -> Event {
    apply(
        e,
        Command::OpenDocument {
            bytes,
            format: DocFormat::Docx,
            name: Some("locked.docx".into()),
            defaults: None,
            limits: None,
            password: Some(password.into()),
        },
    )
}

/// Issue #345 — `OpenDocument.password` opens a real agile-encrypted
/// package (and the Office-written POI fixture when the corpus is
/// present); a wrong password is the typed `WrongPassword`, no password
/// `EncryptedDocument`, and neither touches the open document.
#[test]
fn open_document_with_a_password_decrypts() {
    let mut packages = vec![(
        format_docx::test_fixtures::encrypted_agile_docx(),
        Some(format_docx::test_fixtures::ENCRYPTED_FIXTURE_TEXT),
    )];
    packages.extend(corpus_password_fixture().map(|b| (b, None)));
    for (bytes, text) in packages {
        let mut e = engine_with(DocumentTree::from_text("keep me"));
        let evt = open_docx(&mut e, bytes.clone());
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
        let evt = open_docx_with_password(&mut e, bytes.clone(), "wrong");
        assert!(
            matches!(
                evt,
                Event::Error {
                    kind: Some(bridge::ErrorKind::WrongPassword),
                    ..
                }
            ),
            "{evt:?}"
        );
        assert_eq!(e.undo.current().to_plain_text(), "keep me");
        let evt = open_docx_with_password(&mut e, bytes, "pass");
        assert!(matches!(evt, Event::DocumentLoaded { .. }), "{evt:?}");
        if let Some(text) = text {
            assert_eq!(e.undo.current().to_plain_text(), text);
        } else {
            assert!(!e.undo.current().to_plain_text().trim().is_empty());
        }
    }
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
        let Event::Error { message, kind, .. } = evt else {
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
/// binary `.doc`) is an honest refusal naming the format — typed
/// `InvalidDocument` (issue #427), never `EncryptedDocument`.
#[test]
fn a_legacy_compound_file_is_not_reported_as_encrypted() {
    let doc = format_docx::opc::cfb::test_writer::build(&[("WordDocument", &[1u8; 600])]);
    let mut e = engine_with(DocumentTree::from_text("keep me"));
    let evt = open_docx(&mut e, doc);
    let Event::Error { message, kind, .. } = evt else {
        panic!("a legacy compound file answers Error, got {evt:?}");
    };
    assert_eq!(kind, Some(bridge::ErrorKind::InvalidDocument), "{message}");
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

/* ------------------------------------------------------------------ */
/* Enforcement                                                          */
/* ------------------------------------------------------------------ */

fn protected_engine(mode: &str, body: &str) -> Engine {
    let mut e = engine_with(DocumentTree::from_text(""));
    let bytes = format_docx::test_fixtures::protected_docx(
        body,
        &format!("<w:documentProtection w:edit=\"{mode}\" w:enforcement=\"1\"/>"),
    );
    let evt = open_docx(&mut e, bytes);
    assert!(matches!(evt, Event::DocumentLoaded { .. }), "{evt:?}");
    e
}

fn forms_engine() -> Engine {
    let mut e = engine_with(DocumentTree::from_text(""));
    let evt = open_docx(&mut e, format_docx::test_fixtures::forms_protected_docx());
    assert!(matches!(evt, Event::DocumentLoaded { .. }), "{evt:?}");
    e
}

fn select(e: &mut Engine, (ab, ao): (u32, u32), (cb, co): (u32, u32)) {
    let evt = apply(
        e,
        Command::SetSelection {
            range: BridgeLogicalRange {
                start: bpos_top(ab, ao),
                end: bpos_top(cb, co),
            },
            caret: bpos_top(cb, co),
        },
    );
    assert!(matches!(evt, Event::SelectionChanged { .. }), "{evt:?}");
}

fn caret(e: &mut Engine, b: u32, o: u32) {
    select(e, (b, o), (b, o));
}

fn type_text(e: &mut Engine, text: &str) -> Event {
    apply(
        e,
        Command::InsertText {
            at: None,
            text: text.into(),
        },
    )
}

fn is_protected(evt: &Event) -> bool {
    matches!(
        evt,
        Event::Error {
            kind: Some(bridge::ErrorKind::Protected),
            ..
        }
    )
}

fn text_of(e: &Engine, b: u32) -> String {
    e.undo
        .current()
        .paragraph_text(b)
        .unwrap_or_default()
        .to_string()
}

fn saved_entry(e: &mut Engine, name: &str) -> Vec<u8> {
    use std::io::Read;
    let evt = apply(
        e,
        Command::SaveDocument {
            format: DocFormat::Docx,
        },
    );
    let Event::DocumentSaved { bytes, .. } = evt else {
        panic!("save answers DocumentSaved, got {evt:?}");
    };
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(bytes)).expect("zip");
    let mut out = Vec::new();
    z.by_name(name)
        .expect(name)
        .read_to_end(&mut out)
        .expect("read");
    out
}

const BODY: &str =
    "<w:p><w:r><w:t>alpha beta</w:t></w:r></w:p><w:p><w:r><w:t>gamma</w:t></w:r></w:p>";

/// Issue #345 — `readOnly`: every document change is refused with the
/// typed error and changes nothing; selection, queries, undo and saving
/// still work.
#[test]
fn read_only_refuses_every_change() {
    let mut e = protected_engine("readOnly", BODY);
    caret(&mut e, 0, 5);
    let before = e.undo.current().clone();
    let seq = e.mutation_seq;
    for cmd in [
        Command::InsertText {
            at: None,
            text: "x".into(),
        },
        Command::DeleteAtCaret {
            forward: false,
            by_word: false,
        },
        Command::SplitParagraph { at: None },
        Command::PastePlain { text: "p".into() },
        Command::PasteHtml {
            html: "<p>p</p>".into(),
        },
        Command::ToggleFormatting {
            attr: bridge::FormattingToggle::Bold,
            underline_style: None,
        },
        Command::InsertComment {
            range: BridgeLogicalRange {
                start: bpos_top(0, 0),
                end: bpos_top(0, 5),
            },
            text: "note".into(),
            author: "A".into(),
        },
        Command::ToggleTrackChanges { enabled: true },
        Command::InsertPageBreak { at: bpos_top(0, 0) },
    ] {
        let name = cmd.kind().name();
        let evt = apply(&mut e, cmd);
        assert!(is_protected(&evt), "{name}: {evt:?}");
        let Event::Error { message, .. } = &evt else {
            unreachable!()
        };
        assert!(message.contains("read-only"), "{message}");
    }
    assert!(
        e.undo.current().blocks.ptr_eq(&before.blocks),
        "nothing changed"
    );
    assert_eq!(e.mutation_seq, seq);
    assert!(
        e.pending_announcements
            .iter()
            .any(|(p, m)| *p == AnnouncementPriority::Assertive && m.contains("protected")),
        "the refusal is announced"
    );
    /* Exempt commands. */
    assert!(matches!(
        apply(&mut e, Command::Undo),
        Event::SelectionChanged { .. } | Event::UndoStateChanged { .. }
    ));
    assert!(matches!(
        apply(&mut e, Command::SelectAll),
        Event::SelectionChanged { .. }
    ));
    assert!(!saved_entry(&mut e, "word/settings.xml").is_empty());
}

/// Issue #345 acceptance — `comments`: comments are allowed, text is
/// blocked. Runs on the POI corpus fixture when present, else on the
/// same settings synthesized.
#[test]
fn comments_protection_allows_comments_and_blocks_text() {
    let mut e = match std::fs::read(
        "/data/corpus/files/apache-poi-test-data-document/documentProtection_comments_no_password.docx",
    ) {
        Ok(bytes) => {
            let mut e = engine_with(DocumentTree::from_text(""));
            assert!(matches!(
                open_docx(&mut e, bytes),
                Event::DocumentLoaded { .. }
            ));
            e
        }
        Err(_) => protected_engine("comments", BODY),
    };
    assert_eq!(
        selection_protection(&mut e),
        Some(bridge::ProtectionMode::Comments)
    );
    let text0 = text_of(&e, 0);
    caret(&mut e, 0, 1);
    let evt = type_text(&mut e, "x");
    assert!(is_protected(&evt), "{evt:?}");
    assert_eq!(text_of(&e, 0), text0);
    let evt = apply(
        &mut e,
        Command::InsertComment {
            range: BridgeLogicalRange {
                start: bpos_top(0, 0),
                end: bpos_top(0, 2),
            },
            text: "Reviewed".into(),
            author: "Reviewer".into(),
        },
    );
    assert!(!matches!(evt, Event::Error { .. }), "{evt:?}");
    assert_eq!(e.undo.current().comment_defs.len(), 1);
    let id = *e.undo.current().comment_defs.keys().next().unwrap();
    let evt = apply(&mut e, Command::DeleteComment { id });
    assert!(!matches!(evt, Event::Error { .. }), "{evt:?}");
    assert!(e.undo.current().comment_defs.is_empty());
}

/// Issue #345 — `trackedChanges`: review mode is on from the open and
/// cannot be turned off; typing and formatting are recorded as
/// revisions; accepting is refused, and so is an edit the engine cannot
/// record (a multi-paragraph paste).
#[test]
fn tracked_changes_protection_forces_review_mode() {
    let mut e = protected_engine("trackedChanges", BODY);
    caret(&mut e, 0, 5);
    let Event::SelectionChanged {
        is_tracking_changes,
        protection,
        ..
    } = type_text(&mut e, "X")
    else {
        panic!("typing is allowed (tracked)");
    };
    assert!(is_tracking_changes);
    assert_eq!(protection, Some(bridge::ProtectionMode::TrackedChanges));
    assert_eq!(text_of(&e, 0), "alphaX beta");
    let p = e
        .undo
        .current()
        .paragraph_at_path(&engine::BlockPath::top(0))
        .unwrap();
    assert!(
        p.revisions
            .iter()
            .any(|r| r.kind == engine::RevisionKind::Insert && (r.start, r.end) == (5, 6)),
        "{:?}",
        p.revisions
    );
    /* A deletion is marked, not removed. */
    caret(&mut e, 1, 5);
    let evt = apply(
        &mut e,
        Command::DeleteAtCaret {
            forward: false,
            by_word: false,
        },
    );
    assert!(!matches!(evt, Event::Error { .. }), "{evt:?}");
    assert_eq!(text_of(&e, 1), "gamma", "tracked delete keeps the text");
    /* Formatting is a tracked format change. */
    select(&mut e, (1, 0), (1, 2));
    let evt = apply(
        &mut e,
        Command::ToggleFormatting {
            attr: bridge::FormattingToggle::Bold,
            underline_style: None,
        },
    );
    assert!(!matches!(evt, Event::Error { .. }), "{evt:?}");
    let p = e
        .undo
        .current()
        .paragraph_at_path(&engine::BlockPath::top(1))
        .unwrap();
    assert!(
        p.revisions
            .iter()
            .any(|r| r.kind == engine::RevisionKind::FormatChange)
    );
    /* Review mode stays on. */
    let evt = apply(&mut e, Command::ToggleTrackChanges { enabled: false });
    assert!(is_protected(&evt), "{evt:?}");
    assert!(e.tracking_changes);
    let evt = apply(&mut e, Command::ToggleTrackChanges { enabled: true });
    assert!(!matches!(evt, Event::Error { .. }), "{evt:?}");
    /* Pastes are tracked insertions (issue #366): admitted. */
    caret(&mut e, 0, 0);
    let evt = apply(
        &mut e,
        Command::PastePlain {
            text: "one\ntwo ".into(),
        },
    );
    assert!(!matches!(evt, Event::Error { .. }), "{evt:?}");
    let first = e
        .undo
        .current()
        .paragraph_at_path(&engine::BlockPath::top(0))
        .unwrap();
    assert!(
        first
            .revisions
            .iter()
            .any(|r| r.kind == engine::RevisionKind::Insert),
        "the pasted line is a tracked insertion: {:?}",
        first.revisions
    );
    let evt = apply(
        &mut e,
        Command::PasteHtml {
            html: "<p>rich</p>".into(),
        },
    );
    assert!(!matches!(evt, Event::Error { .. }), "{evt:?}");
    /* Accept / reject and untrackable edits are refused. */
    for cmd in [
        Command::AcceptAllRevisions,
        Command::RejectAllRevisions,
        Command::SetParagraphAlign {
            range: BridgeLogicalRange {
                start: bpos_top(0, 0),
                end: bpos_top(0, 0),
            },
            align: BridgeAlignment::Center,
        },
    ] {
        let name = cmd.kind().name();
        let evt = apply(&mut e, cmd);
        assert!(is_protected(&evt), "{name}: {evt:?}");
    }
    /* Comments are allowed. */
    let evt = apply(
        &mut e,
        Command::InsertComment {
            range: BridgeLogicalRange {
                start: bpos_top(0, 0),
                end: bpos_top(0, 2),
            },
            text: "c".into(),
            author: "R".into(),
        },
    );
    assert!(!matches!(evt, Event::Error { .. }), "{evt:?}");
    /* Leaving the document releases the forced review mode. */
    apply(&mut e, Command::CloseDocument);
    assert!(!e.tracking_changes);
}

/// Issue #345 acceptance — `forms`: typing outside a field is refused
/// with the typed error; inside a run-level content control, a legacy
/// text form field and a block-level content control it lands, inside.
#[test]
fn forms_protection_confines_edits_to_form_content() {
    let mut e = forms_engine();
    /* Outside: refused, typed, nothing changes. */
    caret(&mut e, 0, 6);
    let evt = type_text(&mut e, "!");
    assert!(is_protected(&evt), "{evt:?}");
    assert_eq!(text_of(&e, 0), "Please fill in the form below.");
    caret(&mut e, 1, 2);
    assert!(is_protected(&type_text(&mut e, "!")));
    caret(&mut e, 1, 6);
    assert!(
        is_protected(&type_text(&mut e, "!")),
        "at the control's opener the text would land before it"
    );
    caret(&mut e, 1, 10);
    assert!(is_protected(&apply(
        &mut e,
        Command::SplitParagraph { at: None }
    )));
    assert!(
        is_protected(&apply(
            &mut e,
            Command::PasteHtml {
                html: "<p>x</p>".into(),
            },
        )),
        "a rich paste needs a block-level control"
    );

    /* Run-level content control `Your name here` ([6, 20) of paragraph 1). */
    caret(&mut e, 1, 20);
    let evt = type_text(&mut e, "!");
    assert!(matches!(evt, Event::SelectionChanged { .. }), "{evt:?}");
    assert_eq!(text_of(&e, 1), "Name: Your name here!.");
    /* Replacing the whole content keeps the new text inside. */
    select(&mut e, (1, 6), (1, 21));
    let evt = type_text(&mut e, "Ada Lovelace");
    assert!(matches!(evt, Event::SelectionChanged { .. }), "{evt:?}");
    assert_eq!(text_of(&e, 1), "Name: Ada Lovelace.");
    /* Backspace inside works; across the opener is refused. */
    caret(&mut e, 1, 18);
    let evt = apply(
        &mut e,
        Command::DeleteAtCaret {
            forward: false,
            by_word: false,
        },
    );
    assert!(matches!(evt, Event::SelectionChanged { .. }), "{evt:?}");
    assert_eq!(text_of(&e, 1), "Name: Ada Lovelac.");
    caret(&mut e, 1, 6);
    assert!(is_protected(&apply(
        &mut e,
        Command::DeleteAtCaret {
            forward: false,
            by_word: false,
        },
    )));

    /* The text form field: select it whole (what a click does) and type. */
    select(&mut e, (2, 6), (2, 21));
    let evt = type_text(&mut e, "C");
    assert!(matches!(evt, Event::SelectionChanged { .. }), "{evt:?}");
    let evt = type_text(&mut e, "airo");
    assert!(matches!(evt, Event::SelectionChanged { .. }), "{evt:?}");
    assert_eq!(text_of(&e, 2), "City: Cairo");
    let field = &e
        .undo
        .current()
        .paragraph_at_path(&engine::BlockPath::top(2))
        .unwrap()
        .fields[0];
    assert_eq!(
        (field.start, field.end),
        (6, 11),
        "the field covers the typed result"
    );
    let evt = apply(
        &mut e,
        Command::DeleteAtCaret {
            forward: false,
            by_word: false,
        },
    );
    assert!(matches!(evt, Event::SelectionChanged { .. }), "{evt:?}");
    assert_eq!(
        text_of(&e, 2),
        "City: Cair",
        "one character, not the whole field"
    );
    caret(&mut e, 2, 6);
    assert!(is_protected(&apply(
        &mut e,
        Command::DeleteAtCaret {
            forward: false,
            by_word: false,
        },
    )));

    /* The block-level control: typing and Enter. */
    caret(&mut e, 3, 14);
    assert!(matches!(
        type_text(&mut e, " More."),
        Event::SelectionChanged { .. }
    ));
    let evt = apply(&mut e, Command::SplitParagraph { at: None });
    assert!(matches!(evt, Event::SelectionChanged { .. }), "{evt:?}");
    assert!(matches!(
        type_text(&mut e, "Line two."),
        Event::SelectionChanged { .. }
    ));
    assert_eq!(text_of(&e, 3), "Notes go here. More.");
    assert_eq!(text_of(&e, 4), "Line two.");
    /* The last paragraph is protected again. */
    caret(&mut e, 5, 0);
    assert!(is_protected(&type_text(&mut e, "x")));
    /* Undo walks the admitted edits back. */
    assert!(!matches!(apply(&mut e, Command::Undo), Event::Error { .. }));

    /* The save keeps every edit inside its control and the protection
    element byte-identical. */
    let settings = saved_entry(&mut e, "word/settings.xml");
    let src = format_docx::test_fixtures::forms_protected_docx();
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(src)).unwrap();
    let mut want = Vec::new();
    std::io::Read::read_to_end(&mut z.by_name("word/settings.xml").unwrap(), &mut want).unwrap();
    assert_eq!(settings, want);
    let xml = String::from_utf8(saved_entry(&mut e, "word/document.xml")).unwrap();
    let name_ctl = xml.find("<w:sdtContent>").expect("run-level control");
    let lovelace = xml.find("Ada Lovelac").expect("typed name");
    let name_end = xml[name_ctl..].find("</w:sdtContent>").unwrap() + name_ctl;
    assert!(name_ctl < lovelace && lovelace < name_end, "{xml}");
    assert!(xml.contains("FORMTEXT"), "the field survives: {xml}");
    assert!(xml.contains("Cair"), "{xml}");
}

/// Issue #345 — the forms gate is state-free on refusal: a refused IME
/// commit drops its preview, and a header/footer story refuses edits.
#[test]
fn forms_refused_composition_drops_the_preview() {
    let mut e = forms_engine();
    caret(&mut e, 0, 3);
    let evt = apply(
        &mut e,
        Command::BeginComposition {
            at: Some(bpos_top(0, 3)),
        },
    );
    assert!(!matches!(evt, Event::Error { .. }), "{evt:?}");
    apply(
        &mut e,
        Command::UpdateComposition {
            text: "ka".into(),
            target_range: None,
        },
    );
    let evt = apply(&mut e, Command::EndComposition { commit: true });
    assert!(is_protected(&evt), "{evt:?}");
    assert!(e.composition.is_none());
    assert_eq!(text_of(&e, 0), "Please fill in the form below.");
}
