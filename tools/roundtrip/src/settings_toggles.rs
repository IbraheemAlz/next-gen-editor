//! Step 65 — issue #354: the `word/settings.xml` toggles the engine reads
//! (`format_docx::test_fixtures::{track_revisions_docx,
//! mirror_margins_docx}`, committed as `track_revisions.docx` /
//! `mirror_margins.docx`).

use super::read_docx;
use anyhow::{Context, Result, bail};
use format_docx::test_fixtures::{
    mirror_margins_docx, track_revisions_docx, track_revisions_settings_xml,
};
use format_docx::{save_docx, write_docx};
use std::collections::BTreeMap;

/// Every entry of a package, by name.
fn entries(zip: &[u8]) -> Result<BTreeMap<String, Vec<u8>>> {
    use std::io::Read;
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(zip))?;
    let mut out = BTreeMap::new();
    for i in 0..z.len() {
        let mut f = z.by_index(i)?;
        let mut bytes = Vec::new();
        f.read_to_end(&mut bytes)?;
        out.insert(f.name().to_string(), bytes);
    }
    Ok(out)
}

/// Every entry of `saved` equals `source`'s, except `settings.xml`, which
/// must be `settings`.
fn only_settings_changed(source: &[u8], saved: &[u8], settings: &str, what: &str) -> Result<()> {
    let (source, saved) = (entries(source)?, entries(saved)?);
    if source.keys().ne(saved.keys()) {
        bail!("{what}: the package's parts changed");
    }
    for (name, bytes) in &source {
        let got = &saved[name];
        if name == "word/settings.xml" {
            if got.as_slice() != settings.as_bytes() {
                bail!(
                    "{what}: settings.xml\n--- expected ---\n{settings}\n--- got ---\n{}",
                    String::from_utf8_lossy(got)
                );
            }
        } else if got != bytes {
            bail!("{what}: {name} drifted");
        }
    }
    Ok(())
}

/// a. `trackRevisions` is read; the zero-edit save is byte-identical
///    (both save paths).
/// b. Review mode turned off (the engine stamps its mode onto the saved
///    tree): exactly `<w:trackRevisions/>` leaves `settings.xml`, every
///    other part stays byte-identical, the re-read is off.
/// c. Turned on again: the element is re-inserted at its schema rank — the
///    source part byte for byte.
/// d. `mirrorMargins` is read; the zero-edit save is byte-identical.
pub(super) fn run_settings_toggles_roundtrip() -> Result<()> {
    let fixture = track_revisions_docx();
    let archive = read_docx(&fixture).context("read track_revisions fixture")?;
    if !archive.document.settings.track_revisions {
        bail!("trackRevisions not read");
    }
    let source_settings = track_revisions_settings_xml();
    for (path, saved) in [
        ("write_docx", write_docx(&archive, &archive.document)?),
        ("save_docx", save_docx(&archive.document)?),
    ] {
        only_settings_changed(&fixture, &saved, &source_settings, path)?;
    }
    println!("[roundtrip] step 65a OK — trackRevisions read; zero-edit save byte-identical");

    let mut off = archive.document.clone();
    off.settings.track_revisions = false;
    let saved_off = save_docx(&off).context("save with review mode off")?;
    let without = source_settings.replacen("<w:trackRevisions/>", "", 1);
    only_settings_changed(&fixture, &saved_off, &without, "review mode off")?;
    let back = read_docx(&saved_off).context("reread review mode off")?;
    if back.document.settings.track_revisions {
        bail!("review mode off did not reach the file");
    }
    println!("[roundtrip] step 65b OK — review mode off removes exactly <w:trackRevisions/>");

    let mut on = back.document.clone();
    on.settings.track_revisions = true;
    let saved_on = save_docx(&on).context("save with review mode on again")?;
    only_settings_changed(
        &fixture,
        &saved_on,
        &source_settings,
        "review mode on again",
    )?;
    println!("[roundtrip] step 65c OK — review mode on again restores the source part exactly");

    let mirror = mirror_margins_docx();
    let archive = read_docx(&mirror).context("read mirror_margins fixture")?;
    if !archive.document.settings.mirror_margins {
        bail!("mirrorMargins not read");
    }
    let saved = write_docx(&archive, &archive.document)?;
    if entries(&saved)? != entries(&mirror)? {
        bail!("mirror_margins zero-edit save drifted");
    }
    println!("[roundtrip] step 65d OK — mirrorMargins read; zero-edit save byte-identical");
    Ok(())
}
