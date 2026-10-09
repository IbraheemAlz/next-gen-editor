//! Issue #345 — encrypted packages and `w:documentProtection`, plus the
//! committed e2e fixtures generated from `format_docx::test_fixtures`
//! (rewrite them with `NGE_UPDATE_E2E_FIXTURES=1 cargo test -p
//! format-docx --test protection`).

use engine::{BlockPath, FormEdit, FormRegion, LogicalPos, PathStep, ProtectionEdit};
use format_docx::{DocxError, read_docx, test_fixtures as fx, write_docx};
use std::io::Read;

/// Pin a committed `ts/e2e/fixtures/<name>` to its generator.
fn pin_e2e_fixture(name: &str, bytes: &[u8]) {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../ts/e2e/fixtures")
        .join(name);
    if std::env::var_os("NGE_UPDATE_E2E_FIXTURES").is_some() {
        std::fs::write(&path, bytes).expect("write the e2e fixture");
        return;
    }
    let committed = std::fs::read(&path).unwrap_or_default();
    assert!(
        committed == bytes,
        "ts/e2e/fixtures/{name} is stale — regenerate with \
         `NGE_UPDATE_E2E_FIXTURES=1 cargo test -p format-docx --test protection`"
    );
}

fn entry(docx: &[u8], name: &str) -> Vec<u8> {
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(docx)).expect("zip");
    let mut f = z.by_name(name).expect(name);
    let mut out = Vec::new();
    f.read_to_end(&mut out).expect("read");
    out
}

fn pos(block: u32, offset: u32) -> LogicalPos {
    LogicalPos {
        path: BlockPath::top(block),
        offset,
    }
}

/// The encrypted-package stub is refused as `Encrypted`, never as a ZIP
/// error, and the e2e copy is current.
#[test]
fn encrypted_stub_is_refused_as_encrypted() {
    let bytes = fx::encrypted_package_stub();
    assert!(matches!(read_docx(&bytes), Err(DocxError::Encrypted)));
    pin_e2e_fixture("encrypted_stub.docx", &bytes);
}

/// A real agile-encrypted package opens with its password, is
/// `Encrypted` without one and `WrongPassword` with a wrong one; the e2e
/// copy is current. The stub (no real descriptor) is unsupported, never a
/// panic.
#[test]
fn encrypted_agile_package_opens_with_its_password() {
    use format_docx::{PackageLimits, read_docx_with_password};
    let bytes = fx::encrypted_agile_docx();
    pin_e2e_fixture("encrypted_agile.docx", &bytes);
    let open = |pw: Option<&str>| {
        read_docx_with_password(
            &bytes,
            pw,
            engine::DefaultPageSize::A4,
            true,
            &PackageLimits::DEFAULT,
        )
    };
    let a = open(Some("pass")).expect("decrypt + read");
    assert_eq!(
        a.document.paragraph_text(0),
        Some(fx::ENCRYPTED_FIXTURE_TEXT)
    );
    assert!(matches!(open(None), Err(DocxError::Encrypted)));
    assert!(matches!(open(Some("Pass")), Err(DocxError::WrongPassword)));
    assert!(matches!(read_docx(&bytes), Err(DocxError::Encrypted)));
    /* A tiny package budget refuses the declared plaintext size. */
    let tiny = PackageLimits {
        max_total_bytes: 64,
        ..PackageLimits::DEFAULT
    };
    assert!(matches!(
        read_docx_with_password(
            &bytes,
            Some("pass"),
            engine::DefaultPageSize::A4,
            true,
            &tiny
        ),
        Err(DocxError::PackageTooLarge { .. })
    ));
    let stub = read_docx_with_password(
        &fx::encrypted_package_stub(),
        Some("pass"),
        engine::DefaultPageSize::A4,
        true,
        &PackageLimits::DEFAULT,
    );
    assert!(
        matches!(stub, Err(DocxError::UnsupportedEncryption(_))),
        "{stub:?}"
    );
    /* A password given for a plain package is ignored. */
    let plain = read_docx_with_password(
        &fx::forms_protected_docx(),
        Some("pass"),
        engine::DefaultPageSize::A4,
        true,
        &PackageLimits::DEFAULT,
    )
    .expect("plain package");
    assert_eq!(
        plain.document.protection_mode(),
        Some(ProtectionEdit::Forms)
    );
}

/// The Apache POI password fixtures (local corpus only — skipped when
/// absent) open through the reader with their passwords.
#[test]
fn corpus_password_fixtures_open() {
    use format_docx::{PackageLimits, read_docx_with_password};
    let dir = "/data/corpus/files/apache-poi-test-data-document";
    for (name, pw) in [
        ("bug53475-password-is-pass.docx", "pass"),
        ("bug53475-password-is-solrcell.docx", "solrcell"),
    ] {
        let Ok(bytes) = std::fs::read(format!("{dir}/{name}")) else {
            continue;
        };
        let a = read_docx_with_password(
            &bytes,
            Some(pw),
            engine::DefaultPageSize::A4,
            true,
            &PackageLimits::DEFAULT,
        )
        .expect(name);
        assert!(a.document.paragraph_count() > 0, "{name}");
        assert!(!a.document.to_plain_text().trim().is_empty(), "{name}");
    }
}

/// The forms fixture reads its restriction and its three kinds of form
/// content; the e2e copy is current.
#[test]
fn forms_fixture_reads_its_protection_and_form_content() {
    let bytes = fx::forms_protected_docx();
    pin_e2e_fixture("forms_protected.docx", &bytes);
    let a = read_docx(&bytes).expect("read");
    let doc = &a.document;
    assert_eq!(doc.protection_mode(), Some(ProtectionEdit::Forms));
    let texts: Vec<&str> = (0..5).map(|i| doc.paragraph_text(i).unwrap()).collect();
    assert_eq!(texts, fx::FORMS_FIXTURE_TEXTS);
    let field = &doc.paragraph_at_path(&BlockPath::top(2)).unwrap().fields[0];
    assert_eq!(field.keyword(), "FORMTEXT");
    assert_eq!((field.start, field.end), (6, 6 + 15));

    let ins = FormEdit::Text { inserts: true };
    let region =
        |s: LogicalPos, e: LogicalPos, edit: FormEdit| doc.form_region_for_edit(&s, &e, edit);
    /* Protected text: nothing. */
    assert_eq!(region(pos(0, 3), pos(0, 3), ins), None);
    assert_eq!(region(pos(4, 0), pos(4, 5), ins), None);
    /* Run-level control `Your name here` = bytes [6, 20) of paragraph 1:
    insertion strictly after the opener up to the closer. */
    let name = |o| pos(1, o);
    assert_eq!(
        region(name(6), name(6), ins),
        None,
        "at the opener = outside"
    );
    assert_eq!(
        region(name(7), name(7), ins),
        Some(FormRegion::RunSdt { open: 6, close: 20 })
    );
    assert_eq!(
        region(name(20), name(20), ins),
        Some(FormRegion::RunSdt { open: 6, close: 20 }),
        "at the closer = inside"
    );
    assert_eq!(region(name(21), name(21), ins), None);
    assert_eq!(
        region(name(6), name(20), ins),
        Some(FormRegion::RunSdt { open: 6, close: 20 })
    );
    assert_eq!(region(name(5), name(20), ins), None, "crosses the opener");
    assert_eq!(region(name(10), name(10), FormEdit::Break), None);
    /* The text form field: either boundary and inside. */
    let city = |o| pos(2, o);
    for o in [6, 9, 21] {
        assert_eq!(
            region(city(o), city(o), ins),
            Some(FormRegion::TextField { field: 0 })
        );
    }
    assert_eq!(region(city(5), city(5), ins), None);
    /* The block-level control: anything inside paragraph 3, breaks too. */
    assert_eq!(
        region(pos(3, 0), pos(3, 0), ins),
        Some(FormRegion::BlockSdt)
    );
    assert_eq!(
        region(pos(3, 5), pos(3, 5), FormEdit::Break),
        Some(FormRegion::BlockSdt)
    );
    assert_eq!(region(pos(2, 21), pos(3, 0), ins), None, "crosses into it");
    assert_eq!(
        region(pos(3, 14), pos(4, 0), ins),
        None,
        "crosses out of it"
    );
}

/// The settings part (and the whole package) survives a zero-edit save
/// byte-identically: the restriction is a read-only lift.
#[test]
fn protection_survives_a_save_byte_identically() {
    let bytes = fx::forms_protected_docx();
    let a = read_docx(&bytes).expect("read");
    let saved = write_docx(&a, &a.document).expect("write");
    assert_eq!(
        entry(&saved, "word/settings.xml"),
        entry(&bytes, "word/settings.xml")
    );
    assert_eq!(
        entry(&saved, "word/document.xml"),
        entry(&bytes, "word/document.xml")
    );
    let again = read_docx(&saved).expect("re-read");
    assert_eq!(
        again.document.settings.protection,
        a.document.settings.protection
    );
}

/// A content control whose `<w:lock>` locks its content, a disabled form
/// field and an unprotected section (`<w:formProt w:val="false"/>`).
#[test]
fn locks_disabled_fields_and_unprotected_sections() {
    let body = "<w:p><w:pPr><w:sectPr><w:formProt w:val=\"false\"/><w:type w:val=\"continuous\"/></w:sectPr></w:pPr>\
        <w:r><w:t>free section</w:t></w:r></w:p>\
        <w:p><w:sdt><w:sdtPr><w:lock w:val=\"contentLocked\"/></w:sdtPr>\
        <w:sdtContent><w:r><w:t>locked</w:t></w:r></w:sdtContent></w:sdt></w:p>\
        <w:p><w:r><w:fldChar w:fldCharType=\"begin\"><w:ffData><w:name w:val=\"Off\"/>\
        <w:enabled w:val=\"0\"/><w:textInput/></w:ffData></w:fldChar></w:r>\
        <w:r><w:instrText xml:space=\"preserve\"> FORMTEXT </w:instrText></w:r>\
        <w:r><w:fldChar w:fldCharType=\"separate\"/></w:r><w:r><w:t>value</w:t></w:r>\
        <w:r><w:fldChar w:fldCharType=\"end\"/></w:r></w:p>\
        <w:p><w:r><w:t>protected section</w:t></w:r></w:p>";
    let bytes = fx::protected_docx(
        body,
        "<w:documentProtection w:edit=\"forms\" w:enforcement=\"1\"/>",
    );
    let a = read_docx(&bytes).expect("read");
    let doc = &a.document;
    let ins = FormEdit::Text { inserts: true };
    assert_eq!(
        doc.form_region_for_edit(&pos(0, 0), &pos(0, 4), ins),
        Some(FormRegion::UnprotectedSection)
    );
    assert_eq!(
        doc.form_region_for_edit(&pos(0, 0), &pos(0, 0), FormEdit::Break),
        Some(FormRegion::UnprotectedSection)
    );
    assert_eq!(
        doc.form_region_for_edit(&pos(1, 3), &pos(1, 3), ins),
        None,
        "contentLocked"
    );
    assert_eq!(
        doc.paragraph_at_path(&BlockPath::top(2))
            .unwrap()
            .fields
            .len(),
        1
    );
    assert_eq!(
        doc.form_region_for_edit(&pos(2, 2), &pos(2, 2), ins),
        None,
        "disabled"
    );
    assert_eq!(doc.form_region_for_edit(&pos(3, 0), &pos(3, 0), ins), None);
    assert_eq!(
        doc.form_region_for_edit(&pos(0, 2), &pos(3, 0), ins),
        None,
        "across into the protected section"
    );
}

/// A block-level control wrapping a table makes its cells form content.
#[test]
fn a_control_around_a_table_covers_its_cells() {
    let body = "<w:p><w:r><w:t>before</w:t></w:r></w:p>\
        <w:sdt><w:sdtPr><w:id w:val=\"7\"/></w:sdtPr><w:sdtContent>\
        <w:tbl><w:tblGrid><w:gridCol w:w=\"2000\"/></w:tblGrid>\
        <w:tr><w:tc><w:p><w:r><w:t>cell</w:t></w:r></w:p></w:tc></w:tr></w:tbl>\
        </w:sdtContent></w:sdt><w:p><w:r><w:t>after</w:t></w:r></w:p>";
    let bytes = fx::protected_docx(
        body,
        "<w:documentProtection w:edit=\"forms\" w:enforcement=\"1\"/>",
    );
    let doc = read_docx(&bytes).expect("read").document;
    let cell = LogicalPos {
        path: BlockPath {
            steps: vec![
                PathStep::Block(1),
                PathStep::Cell { row: 0, col: 0 },
                PathStep::Block(0),
            ],
        },
        offset: 2,
    };
    assert_eq!(
        doc.form_region_for_edit(&cell, &cell, FormEdit::Text { inserts: true }),
        Some(FormRegion::BlockSdt)
    );
    assert_eq!(
        doc.form_region_for_edit(&pos(0, 1), &pos(0, 1), FormEdit::Text { inserts: true }),
        None
    );
}

/// The Apache POI `documentProtection_*` fixtures (local corpus only —
/// skipped when absent).
#[test]
fn corpus_protection_fixtures_read_their_mode() {
    let dir = "/data/corpus/files/apache-poi-test-data-document";
    for (name, mode, has_password) in [
        (
            "documentProtection_readonly_no_password.docx",
            Some(ProtectionEdit::ReadOnly),
            false,
        ),
        (
            "documentProtection_comments_no_password.docx",
            Some(ProtectionEdit::Comments),
            false,
        ),
        (
            "documentProtection_forms_no_password.docx",
            Some(ProtectionEdit::Forms),
            false,
        ),
        (
            "documentProtection_trackedChanges_no_password.docx",
            Some(ProtectionEdit::TrackedChanges),
            false,
        ),
        (
            "documentProtection_no_protection_tag_existing.docx",
            None,
            false,
        ),
        ("documentProtection_no_protection.docx", None, false),
        (
            "protected_sample.docx",
            Some(ProtectionEdit::ReadOnly),
            true,
        ),
    ] {
        let Ok(bytes) = std::fs::read(format!("{dir}/{name}")) else {
            continue;
        };
        let a = read_docx(&bytes).expect(name);
        assert_eq!(a.document.protection_mode(), mode, "{name}");
        assert_eq!(
            a.document
                .settings
                .protection
                .as_ref()
                .is_some_and(|p| p.has_password()),
            has_password,
            "{name}"
        );
        let saved = write_docx(&a, &a.document).expect(name);
        assert_eq!(
            entry(&saved, "word/settings.xml"),
            entry(&bytes, "word/settings.xml"),
            "{name}: settings.xml passes through"
        );
    }
}
