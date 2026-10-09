//! Issue #345 — `w:documentProtection`: the forms-protected fixture
//! (`format_docx::test_fixtures::forms_protected_docx` — a run-level
//! content control, a legacy `FORMTEXT` field, a block-level content
//! control) through read → fill in → save.

use super::{assert_document_xml_well_formed, entry_bytes, extract_doc_xml, read_docx, write_docx};
use anyhow::{Context, Result, bail};
use engine::{BlockPath, FormEdit, FormRegion, LogicalPos, ProtectionEdit};
use format_docx::test_fixtures::{FORMS_FIXTURE_TEXTS, forms_protected_docx};

fn at(block: u32, offset: u32) -> LogicalPos {
    LogicalPos::new(BlockPath::top(block), offset)
}

/// Step 47:
///
/// a. The restriction is modeled (`forms`, enforced) and the zero-edit
///    save is byte-identical — `settings.xml` (the protection element)
///    included.
/// b. Filling in the run-level content control is a pure insertion INSIDE
///    `<w:sdtContent>`; the settings part is untouched.
/// c. Filling in the `FORMTEXT` field replaces its placeholder result and
///    keeps the field — `<w:ffData>` prologue, `FORMTEXT` instruction and
///    end — around the typed text; the reread models the same field over
///    it and the same restriction.
pub fn run_document_protection_roundtrip() -> Result<()> {
    let fixture = forms_protected_docx();
    let archive = read_docx(&fixture).context("read forms fixture")?;
    let doc = &archive.document;
    if doc.protection_mode() != Some(ProtectionEdit::Forms) {
        bail!("protection misread: {:?}", doc.settings.protection);
    }
    let settings = "word/settings.xml";
    let saved = write_docx(&archive, doc).context("zero-edit save")?;
    let saved_archive = read_docx(&saved).context("reread zero-edit save")?;
    if extract_doc_xml(&saved)? != extract_doc_xml(&fixture)?
        || entry_bytes(&saved_archive, settings) != entry_bytes(&archive, settings)
        || entry_bytes(&saved_archive, settings).is_none()
    {
        bail!("zero-edit save drifted");
    }
    println!("[roundtrip] step 47a OK — forms protection modeled; zero-edit save byte-identical");

    /* b. The run-level control holds bytes [6, 20) of paragraph 1. */
    let closer = at(1, 20);
    if doc.form_region_for_edit(&closer, &closer, FormEdit::Text { inserts: true })
        != Some(FormRegion::RunSdt { open: 6, close: 20 })
    {
        bail!("the run-level content control is not form content");
    }
    let source = String::from_utf8(extract_doc_xml(&fixture)?)?;
    let edited = doc.insert_text(closer, ", Esq");
    let bytes = write_docx(&archive, &edited).context("write filled control")?;
    assert_document_xml_well_formed(&bytes).context("filled control")?;
    let out = String::from_utf8(extract_doc_xml(&bytes)?)?;
    let expected = source.replacen(">Your name here<", ">Your name here, Esq<", 1);
    if out != expected {
        bail!("filling the control is not a pure insertion:\n{out}\nexpected:\n{expected}");
    }
    let back = read_docx(&bytes).context("reread filled control")?;
    if entry_bytes(&back, settings) != entry_bytes(&archive, settings) {
        bail!("settings part drifted on an edited save");
    }
    println!("[roundtrip] step 47b OK — a filled content control is a pure insertion inside it");

    /* c. The text form field (paragraph 2, result = the placeholder). */
    let field_start = FORMS_FIXTURE_TEXTS[2]
        .find('\u{2002}')
        .context("placeholder")? as u32;
    let field_end = FORMS_FIXTURE_TEXTS[2].len() as u32;
    let region = doc.form_region_for_edit(
        &at(2, field_start),
        &at(2, field_end),
        FormEdit::Text { inserts: true },
    );
    let Some(FormRegion::TextField { field }) = region else {
        bail!("the FORMTEXT result is not form content: {region:?}");
    };
    let (filled, caret) = doc
        .fill_form_text_field(&BlockPath::top(2), field, field_start, field_end, "Cairo")
        .context("fill the text form field")?;
    if caret != field_start + 5 {
        bail!("caret after the fill: {caret}");
    }
    let bytes = write_docx(&archive, &filled).context("write filled field")?;
    assert_document_xml_well_formed(&bytes).context("filled field")?;
    let out = String::from_utf8(extract_doc_xml(&bytes)?)?;
    let ff = out.find("<w:ffData>").context("ffData prologue lost")?;
    let instr = out.find("FORMTEXT").context("instruction lost")?;
    let cairo = out.find(">Cairo<").context("typed result missing")?;
    let end = out[cairo..]
        .find("w:fldCharType=\"end\"")
        .context("field end lost")?;
    if !(ff < instr && instr < cairo && end > 0) || out.contains('\u{2002}') {
        bail!("the field does not wrap the typed result:\n{out}");
    }
    let back = read_docx(&bytes).context("reread filled field")?;
    let p = back
        .document
        .paragraph_at_path(&BlockPath::top(2))
        .context("paragraph 2")?;
    let f = p.fields.first().context("field lost on reread")?;
    if f.keyword() != "FORMTEXT" || &p.text[f.start as usize..f.end as usize] != "Cairo" {
        bail!("reread field: {f:?} over {:?}", p.text);
    }
    if back.document.protection_mode() != Some(ProtectionEdit::Forms)
        || entry_bytes(&back, settings) != entry_bytes(&archive, settings)
    {
        bail!("protection drifted on a filled save");
    }
    println!(
        "[roundtrip] step 47c OK — a filled FORMTEXT field keeps its prologue + end around the result"
    );
    Ok(())
}
