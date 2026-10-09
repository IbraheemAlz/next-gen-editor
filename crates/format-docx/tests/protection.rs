//! Issue #345 — encrypted packages and `w:documentProtection`, plus the
//! committed e2e fixtures generated from `format_docx::test_fixtures`
//! (rewrite them with `NGE_UPDATE_E2E_FIXTURES=1 cargo test -p
//! format-docx --test protection`).

use format_docx::{DocxError, read_docx, test_fixtures as fx};

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

/// The encrypted-package stub is refused as `Encrypted`, never as a ZIP
/// error, and the e2e copy is current.
#[test]
fn encrypted_stub_is_refused_as_encrypted() {
    let bytes = fx::encrypted_package_stub();
    assert!(matches!(read_docx(&bytes), Err(DocxError::Encrypted)));
    pin_e2e_fixture("encrypted_stub.docx", &bytes);
}
