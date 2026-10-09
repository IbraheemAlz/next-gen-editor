//! Issues #325 / #353 / #352 — step 41: package namespace families,
//! relationship-driven part discovery and logical paragraph borders, all
//! derived programmatically from the Word-shaped package fixture (no
//! binaries in the tree).
//!
//! * 41a — a Strict (ISO 29500) twin reads like its Transitional source,
//!   saves byte-identically with no edit, and stays Strict when edited.
//! * 41b — the main part renamed (`word/document2.xml`, referenced from
//!   `_rels/.rels`) round-trips on both save paths without a stray
//!   `word/document.xml`.
//! * 41c — `<w:pBdr>` `w:start` / `w:end` survive a zero-edit save
//!   byte-for-byte and keep their spelling when regenerated.

use super::{
    assert_document_xml_well_formed, build_word_package_parts_docx, read_docx, write_docx,
    zip_entries,
};
use anyhow::{Context, Result, bail};
use engine::{BlockPath, LogicalPos};
use format_docx::schema::NsFamily;
use format_docx::schema::family::to_family;
use std::io::Write;

fn rebuild(entries: &[(String, Vec<u8>)]) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    {
        let mut z = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, bytes) in entries {
            z.start_file(name.as_str(), opts)?;
            z.write_all(bytes)?;
        }
        z.finish()?;
    }
    Ok(buf)
}

fn get<'a>(all: &'a [(String, Vec<u8>)], name: &str) -> Result<&'a [u8]> {
    all.iter()
        .find(|(n, _)| n == name)
        .map(|(_, b)| b.as_slice())
        .with_context(|| format!("missing entry {name}"))
}

/// Every entry of `saved` equals the same-named entry of `source`, and no
/// entry is added or lost.
fn assert_same_package(source: &[u8], saved: &[u8], what: &str) -> Result<()> {
    let (src, out) = (zip_entries(source)?, zip_entries(saved)?);
    if src.len() != out.len() {
        bail!("{what}: {} entries became {}", src.len(), out.len());
    }
    for (name, bytes) in &src {
        if get(&out, name)? != bytes.as_slice() {
            bail!("{what}: `{name}` drifted on a zero-edit save");
        }
    }
    Ok(())
}

fn paragraph_texts(archive: &format_docx::DocxArchive) -> Vec<String> {
    (0..archive.document.paragraph_count())
        .filter_map(|i| archive.document.nth_paragraph(i))
        .map(|p| p.text.clone())
        .collect()
}

pub(crate) fn run_package_families_roundtrip() -> Result<()> {
    let base = build_word_package_parts_docx();
    let transitional = read_docx(&base).context("read word_package_parts")?;

    /* ----- 41a — Strict twin ------------------------------------------ */
    let strict_entries: Vec<(String, Vec<u8>)> = zip_entries(&base)?
        .into_iter()
        .map(|(name, bytes)| {
            let is_xml = name.ends_with(".xml") || name.ends_with(".rels");
            match std::str::from_utf8(&bytes) {
                Ok(text) if is_xml => (
                    name,
                    to_family(text, NsFamily::Strict).into_owned().into_bytes(),
                ),
                _ => (name, bytes),
            }
        })
        .collect();
    let strict = rebuild(&strict_entries)?;
    let archive = read_docx(&strict).context("read strict twin")?;
    if !archive.warnings.is_empty() {
        bail!("41a: Strict twin raised warnings: {:?}", archive.warnings);
    }
    if paragraph_texts(&archive) != paragraph_texts(&transitional) {
        bail!("41a: Strict twin reads differently from its Transitional source");
    }
    let saved = write_docx(&archive, &archive.document).context("write strict zero-edit")?;
    assert_document_xml_well_formed(&saved)?;
    assert_same_package(&strict, &saved, "41a Strict zero-edit")?;
    let edited = archive.document.insert_text(
        LogicalPos {
            path: BlockPath::top(0),
            offset: 0,
        },
        "edited ",
    );
    let saved = write_docx(&archive, &edited).context("write strict dirty")?;
    assert_document_xml_well_formed(&saved)?;
    for (name, bytes) in zip_entries(&saved)? {
        if !(name.ends_with(".xml") || name.ends_with(".rels")) {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes);
        for leaked in [
            "schemas.openxmlformats.org/wordprocessingml/2006",
            "schemas.openxmlformats.org/drawingml/2006",
            "schemas.openxmlformats.org/officeDocument/2006/relationships",
        ] {
            if text.contains(leaked) {
                bail!("41a: a dirty Strict save leaked a Transitional URI ({leaked}) into {name}");
            }
        }
    }
    println!(
        "[roundtrip] step 41a OK — a Strict twin reads identically, resaves byte-identically and stays Strict when edited"
    );

    /* ----- 41b — renamed main part ------------------------------------ */
    let renamed_entries: Vec<(String, Vec<u8>)> = zip_entries(&base)?
        .into_iter()
        .map(|(name, bytes)| match name.as_str() {
            "word/document.xml" => ("word/document2.xml".to_string(), bytes),
            "word/_rels/document.xml.rels" => ("word/_rels/document2.xml.rels".to_string(), bytes),
            "_rels/.rels" | "[Content_Types].xml" => {
                let text = String::from_utf8_lossy(&bytes)
                    .replace("word/document.xml", "word/document2.xml");
                (name, text.into_bytes())
            }
            _ => (name, bytes),
        })
        .collect();
    let renamed = rebuild(&renamed_entries)?;
    let archive = read_docx(&renamed).context("read renamed-main package")?;
    if archive.part_names.main != "word/document2.xml" {
        bail!("41b: main part resolved to {}", archive.part_names.main);
    }
    if paragraph_texts(&archive) != paragraph_texts(&transitional) {
        bail!("41b: renamed main reads differently");
    }
    let saved = write_docx(&archive, &archive.document).context("write renamed zero-edit")?;
    assert_same_package(&renamed, &saved, "41b renamed-main zero-edit")?;
    let edited = archive.document.insert_text(
        LogicalPos {
            path: BlockPath::top(0),
            offset: 0,
        },
        "edited ",
    );
    for (path, saved) in [
        ("write_docx", write_docx(&archive, &edited)?),
        ("save_docx", format_docx::save_docx(&edited)?),
    ] {
        assert_document_xml_well_formed(&saved)?;
        let out = zip_entries(&saved)?;
        if out.iter().any(|(n, _)| n == "word/document.xml") {
            bail!("41b: {path} minted a stray word/document.xml");
        }
        if !String::from_utf8_lossy(get(&out, "word/document2.xml")?).contains("edited ") {
            bail!("41b: {path} lost the edit");
        }
        let again = read_docx(&saved).context("re-read renamed save")?;
        if again.part_names.main != "word/document2.xml" {
            bail!("41b: {path} result lost its renamed main part");
        }
    }
    println!(
        "[roundtrip] step 41b OK — a renamed main part (found through _rels/.rels) resaves byte-identically and edits land in it on both save paths"
    );

    /* ----- 41c — logical paragraph borders ---------------------------- */
    let pbdr = format_docx::test_fixtures::paragraph_start_end_borders_docx();
    let archive = read_docx(&pbdr).context("read pBdr fixture")?;
    let saved = write_docx(&archive, &archive.document).context("write pBdr zero-edit")?;
    assert_same_package(&pbdr, &saved, "41c pBdr zero-edit")?;
    let regenerated = format_docx::build_minimal_docx(&archive.document)?;
    let xml = String::from_utf8(extract_part(&regenerated, "word/document.xml")?)?;
    if xml.matches("<w:start ").count() != 2 || xml.matches("<w:end ").count() != 1 {
        bail!("41c: regenerated pPr lost the logical spelling: {xml}");
    }
    let back = read_docx(&regenerated).context("re-read regenerated pBdr")?;
    for n in 0..4 {
        let (a, b) = (
            archive.document.nth_paragraph(n).context("paragraph")?,
            back.document.nth_paragraph(n).context("paragraph")?,
        );
        if a.props.borders != b.props.borders || a.props.border_spelling != b.props.border_spelling
        {
            bail!("41c: paragraph {n} changed meaning through regeneration");
        }
    }
    println!(
        "[roundtrip] step 41c OK — pBdr w:start / w:end resave byte-identically and keep their spelling and physical side when regenerated"
    );
    Ok(())
}

fn extract_part(docx: &[u8], name: &str) -> Result<Vec<u8>> {
    Ok(get(&zip_entries(docx)?, name)?.to_vec())
}
