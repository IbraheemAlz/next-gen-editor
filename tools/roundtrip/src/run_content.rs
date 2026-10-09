//! Issue #335 — run-content elements with no text of their own:
//! `<w:softHyphen/>` / `<w:noBreakHyphen/>` (`soft_hyphen.docx`) through
//! read → edit → save. The reader maps them onto U+00AD / U+2011, an
//! untouched save is byte-identical, and a regenerated paragraph re-emits
//! the ELEMENTS (never the raw characters) as a pure insertion.

use super::{
    assert_document_xml_well_formed, extract_doc_xml, read_docx, rewritten_region, write_docx,
};
use anyhow::{Context, Result, bail};
use engine::{BlockPath, LogicalPos};
use format_docx::test_fixtures::{NB_HYPHEN_TEXT, SOFT_HYPHEN_TEXT, soft_hyphen_docx};

fn at(block: u32, offset: usize) -> LogicalPos {
    LogicalPos::new(BlockPath::top(block), offset as u32)
}

/// Step 55:
///
/// a. Both paragraphs read with U+00AD / U+2011 where the elements stood.
/// b. A zero-edit save is byte-identical.
/// c. Typing at the end of each paragraph regenerates it as a pure
///    insertion: every `<w:softHyphen/>` / `<w:noBreakHyphen/>` comes
///    back as the element in its source run, no raw U+00AD / U+2011
///    reaches a `<w:t>`, and the reread text equals the edited text.
/// d. A soft hyphen the user pastes (a raw U+00AD in inserted text) is
///    saved as `<w:softHyphen/>` too and rereads as U+00AD.
pub fn run_soft_hyphen_roundtrip() -> Result<()> {
    let src = soft_hyphen_docx();
    let archive = read_docx(&src).context("read soft_hyphen.docx")?;
    let doc = &archive.document;
    for (idx, want) in [(0, SOFT_HYPHEN_TEXT), (1, NB_HYPHEN_TEXT)] {
        let got = &doc.nth_paragraph(idx).context("paragraph")?.text;
        if got != want {
            bail!("55a: paragraph {idx} read as {got:?}, expected {want:?}");
        }
    }
    let soft_count = SOFT_HYPHEN_TEXT.matches('\u{AD}').count();
    let nb_count = NB_HYPHEN_TEXT.matches('\u{2011}').count();
    let doc_a = extract_doc_xml(&src)?;

    /* b. Zero-edit identity. */
    let untouched = write_docx(&archive, doc).context("55b: untouched save")?;
    if extract_doc_xml(&untouched)? != doc_a {
        bail!("55b: zero-edit save drifted");
    }

    /* c. Typing regenerates each paragraph (one edit per save: the
    rewritten-bytes metric measures ONE region). */
    for idx in [0, 1] {
        regenerated_paragraph_step(&archive, &doc_a, idx, soft_count, nb_count)?;
    }

    /* d. A pasted soft hyphen. */
    let pasted = doc.insert_text(at(1, 0), "co\u{AD}operative ");
    let bytes = write_docx(&archive, &pasted).context("55d: save")?;
    assert_document_xml_well_formed(&bytes).context("55d")?;
    let out = String::from_utf8(extract_doc_xml(&bytes)?).context("utf8")?;
    if out.contains('\u{AD}') || out.matches("<w:softHyphen/>").count() != soft_count + 1 {
        bail!("55d: a pasted soft hyphen is not written as <w:softHyphen/>");
    }
    let back = read_docx(&bytes).context("55d: reread")?;
    if !back
        .document
        .nth_paragraph(1)
        .context("paragraph")?
        .text
        .starts_with("co\u{AD}operative ")
    {
        bail!("55d: the pasted soft hyphen did not reread as U+00AD");
    }
    println!(
        "[roundtrip] step 55 OK — soft_hyphen.docx: {soft_count} soft + {nb_count} non-breaking \
         hyphens read, zero-edit identical, regenerated as elements (pure insertion), a pasted \
         soft hyphen written as the element (#335)"
    );
    Ok(())
}

/// Step 55c — typing at the end of paragraph `idx` regenerates it: a pure
/// insertion that re-emits every hyphen element (and no raw character);
/// the reread text equals the edited text.
fn regenerated_paragraph_step(
    archive: &format_docx::DocxArchive,
    doc_a: &[u8],
    idx: u32,
    soft_count: usize,
    nb_count: usize,
) -> Result<()> {
    let doc = &archive.document;
    let len = doc.nth_paragraph(idx).context("paragraph")?.text.len();
    let edited = doc.insert_text(at(idx, len), " Edited");
    let bytes = write_docx(archive, &edited).context("55c: edited save")?;
    assert_document_xml_well_formed(&bytes).context("55c")?;
    let doc_b = extract_doc_xml(&bytes)?;
    let (prefix, rewritten, _) = rewritten_region(doc_a, &doc_b);
    let out = String::from_utf8(doc_b.clone()).context("utf8")?;
    if out.contains('\u{AD}') || out.contains('\u{2011}') {
        bail!("55c: paragraph {idx}: a raw hyphen character reached the saved XML");
    }
    let (soft, nb) = (
        out.matches("<w:softHyphen/>").count(),
        out.matches("<w:noBreakHyphen/>").count(),
    );
    if soft != soft_count || nb != nb_count {
        bail!(
            "55c: paragraph {idx}: elements not re-emitted ({soft} / {soft_count} soft, \
             {nb} / {nb_count} non-breaking)"
        );
    }
    if rewritten != 0 {
        let lo = prefix.saturating_sub(80);
        let hi = (prefix + 160).min(doc_b.len());
        bail!(
            "55c: paragraph {idx}: the edited save rewrote {rewritten} source bytes at {prefix}:\n{}",
            String::from_utf8_lossy(&doc_b[lo..hi])
        );
    }
    let back = read_docx(&bytes).context("55c: reread")?;
    let a = &edited.nth_paragraph(idx).context("paragraph")?.text;
    let b = &back.document.nth_paragraph(idx).context("paragraph")?.text;
    if a != b {
        bail!("55c: paragraph {idx} reread as {b:?}, expected {a:?}");
    }
    Ok(())
}

/// Step 57 (issue #326) — `hyphenation_on.docx` / `hyphenation_off.docx`:
///
/// a. The hyphenation settings, the docDefaults `<w:lang>` and a direct
///    `<w:suppressAutoHyphens/>` are read into the typed model.
/// b. A zero-edit save is byte-identical — `document.xml` AND
///    `word/settings.xml` (the settings are read-only: the part passes
///    through).
/// c. Typing into each paragraph is a pure insertion: the read-only
///    elements (`<w:lang>` on the French run, `<w:suppressAutoHyphens/>`
///    in a pPr) ride the grab bags verbatim, never regenerated or doubled.
pub fn run_hyphenation_roundtrip() -> Result<()> {
    use format_docx::test_fixtures::{HYPHENATION_TEXTS, hyphenation_docx};
    for auto in [true, false] {
        let src = hyphenation_docx(auto);
        let archive = read_docx(&src).context("read hyphenation fixture")?;
        let doc = &archive.document;
        let s = &doc.settings;
        if s.auto_hyphenation != auto
            || s.hyphenation_zone != Some(360)
            || s.consecutive_hyphen_limit != Some(2)
            || !s.do_not_hyphenate_caps
        {
            bail!("57a: settings misread: {s:?}");
        }
        let lang = doc.style_run_defaults.lang.as_deref();
        if lang.and_then(|l| l.val.as_deref()) != Some("en-US")
            || lang.and_then(|l| l.bidi.as_deref()) != Some("ar-SA")
        {
            bail!("57a: docDefaults language misread: {lang:?}");
        }
        if doc
            .nth_paragraph(1)
            .context("p1")?
            .props
            .suppress_auto_hyphens
            != Some(true)
        {
            bail!("57a: <w:suppressAutoHyphens/> not read");
        }
        let fr = doc.nth_paragraph(4).context("p4")?.style_at(0);
        if fr.lang.as_deref().and_then(|l| l.val.as_deref()) != Some("fr-FR") {
            bail!("57a: run language misread: {:?}", fr.lang);
        }
        let doc_a = extract_doc_xml(&src)?;
        let untouched = write_docx(&archive, doc).context("57b: untouched save")?;
        if extract_doc_xml(&untouched)? != doc_a {
            bail!("57b: zero-edit document.xml drifted");
        }
        let settings_of = |bytes: &[u8]| -> Result<Vec<u8>> {
            use std::io::Read;
            let mut z = zip::ZipArchive::new(std::io::Cursor::new(bytes))?;
            let mut f = z.by_name("word/settings.xml")?;
            let mut out = Vec::new();
            f.read_to_end(&mut out)?;
            Ok(out)
        };
        if settings_of(&untouched)? != settings_of(&src)? {
            bail!("57b: settings.xml drifted");
        }
        for (idx, text) in HYPHENATION_TEXTS.iter().enumerate() {
            let edited = doc.insert_text(at(idx as u32, text.len()), " Edited");
            let bytes = write_docx(&archive, &edited).context("57c: edited save")?;
            assert_document_xml_well_formed(&bytes).context("57c")?;
            let doc_b = extract_doc_xml(&bytes)?;
            let (prefix, rewritten, _) = rewritten_region(&doc_a, &doc_b);
            if rewritten != 0 {
                let lo = prefix.saturating_sub(80);
                let hi = (prefix + 160).min(doc_b.len());
                bail!(
                    "57c: paragraph {idx}: rewrote {rewritten} source bytes at {prefix}:\n{}",
                    String::from_utf8_lossy(&doc_b[lo..hi])
                );
            }
            let out = String::from_utf8(doc_b).context("utf8")?;
            for (elem, n) in [("<w:lang ", 1), ("<w:suppressAutoHyphens/>", 1)] {
                if out.matches(elem).count() != n {
                    bail!("57c: paragraph {idx}: {elem} not kept exactly once");
                }
            }
            if settings_of(&bytes)? != settings_of(&src)? {
                bail!("57c: settings.xml drifted on an edit");
            }
        }
    }
    println!(
        "[roundtrip] step 57 OK — hyphenation_on/off.docx: settings, docDefaults and run \
         languages, suppressAutoHyphens read; zero-edit identical (settings.xml included); \
         typing a pure insertion with the read-only elements kept verbatim (#326)"
    );
    Ok(())
}

/// The run-content elements of `run_content.docx`, with the count each
/// saved `document.xml` must carry.
const RUN_CONTENT_ELEMENTS: [(&str, usize); 5] = [
    ("<w:sym ", 9),
    ("<w:cr/>", 1),
    ("<w:ptab ", 2),
    ("<w:bdo w:val=\"rtl\">", 1),
    ("<w:dir w:val=\"rtl\">", 1),
];

/// Step 56 (issue #357) — `run_content.docx` (`<w:sym>`, `<w:cr/>`,
/// `<w:ptab>`, `<w:bdo>` / `<w:dir>`):
///
/// a. The reader models every element (U+FFFC + a typed object, U+000D,
///    the UAX #9 controls).
/// b. A zero-edit save is byte-identical.
/// c. Typing at the end of each paragraph regenerates it as a pure
///    insertion: every element comes back as itself (never a U+FFFC, a
///    raw CR or a bidi control) and the reread text equals the edit.
/// d. Deleting the override's closing control (the user backspaced it)
///    still saves balanced XML: the `<w:bdo>` closes at the paragraph end.
pub fn run_run_content_roundtrip() -> Result<()> {
    use format_docx::test_fixtures::{RUN_CONTENT_TEXTS, run_content_docx};
    let src = run_content_docx();
    let archive = read_docx(&src).context("read run_content.docx")?;
    let doc = &archive.document;
    for (idx, want) in RUN_CONTENT_TEXTS.iter().enumerate() {
        let got = &doc.nth_paragraph(idx as u32).context("paragraph")?.text;
        if got != want {
            bail!("56a: paragraph {idx} read as {got:?}, expected {want:?}");
        }
    }
    let objects: usize = (0..4)
        .filter_map(|i| doc.nth_paragraph(i))
        .map(|p| p.inline_objects.len())
        .sum();
    if objects != 11 {
        bail!("56a: {objects} inline objects, expected 9 symbols + 2 positional tabs");
    }
    let doc_a = extract_doc_xml(&src)?;
    let untouched = write_docx(&archive, doc).context("56b: untouched save")?;
    if extract_doc_xml(&untouched)? != doc_a {
        bail!("56b: zero-edit save drifted");
    }
    for idx in 0..4u32 {
        let len = doc.nth_paragraph(idx).context("paragraph")?.text.len();
        let edited = doc.insert_text(at(idx, len), " Edited");
        let bytes = write_docx(&archive, &edited).context("56c: edited save")?;
        assert_document_xml_well_formed(&bytes).context("56c")?;
        let doc_b = extract_doc_xml(&bytes)?;
        let (prefix, rewritten, _) = rewritten_region(&doc_a, &doc_b);
        let out = String::from_utf8(doc_b.clone()).context("utf8")?;
        for raw in [
            '\u{FFFC}', '\r', '\u{202A}', '\u{202B}', '\u{202C}', '\u{202D}', '\u{202E}',
        ] {
            if out.contains(raw) {
                bail!("56c: paragraph {idx}: raw {raw:?} reached the saved XML");
            }
        }
        for (elem, n) in RUN_CONTENT_ELEMENTS {
            if out.matches(elem).count() != n {
                bail!("56c: paragraph {idx}: {elem} not re-emitted {n}×");
            }
        }
        if rewritten != 0 {
            let lo = prefix.saturating_sub(80);
            let hi = (prefix + 160).min(doc_b.len());
            bail!(
                "56c: paragraph {idx}: rewrote {rewritten} source bytes at {prefix}:\n{}",
                String::from_utf8_lossy(&doc_b[lo..hi])
            );
        }
        let back = read_docx(&bytes).context("56c: reread")?;
        let a = &edited.nth_paragraph(idx).context("paragraph")?.text;
        let b = &back.document.nth_paragraph(idx).context("paragraph")?.text;
        if a != b {
            bail!("56c: paragraph {idx} reread as {b:?}, expected {a:?}");
        }
    }
    /* d. The override loses its closing control. */
    let text3 = RUN_CONTENT_TEXTS[3];
    let pop = text3.find('\u{202C}').context("pop")?;
    let edited = doc.delete_range(at(3, pop), at(3, pop + '\u{202C}'.len_utf8()));
    let bytes = write_docx(&archive, &edited).context("56d: save")?;
    assert_document_xml_well_formed(&bytes).context("56d: unbalanced override")?;
    let out = String::from_utf8(extract_doc_xml(&bytes)?).context("utf8")?;
    if out.matches("<w:bdo").count() != out.matches("</w:bdo>").count() {
        bail!("56d: <w:bdo> not balanced after deleting its pop");
    }
    println!(
        "[roundtrip] step 56 OK — run_content.docx: 9 symbols, a carriage return, 2 positional \
         tabs, an override and an embedding read, zero-edit identical, regenerated as elements \
         (pure insertion), an orphaned override still balanced (#357)"
    );
    Ok(())
}
