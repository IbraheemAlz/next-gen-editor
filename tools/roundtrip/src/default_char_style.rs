//! Step 67 — issue #558: the DEFAULT CHARACTER style
//! (`DefaultParagraphFont`, `w:default="1"`) formats every run that names
//! no character style, and the writer stays byte-neutral: it is mirrored
//! into the engine (`DocumentTree::default_char_style`) and folded at
//! span-materialize time, never baked into a span
//! (`format_docx::test_fixtures::default_char_style_docx`, committed as
//! `default_char_style.docx`). The same fixture's empty `Normal` under
//! Word's docDefaults spacing is the issue #560 case of an inherited value
//! the default paragraph style does not override.

use super::{assert_document_xml_well_formed, extract_doc_xml, read_docx};
use anyhow::{Context, Result, bail};
use engine::{BlockPath, DocumentTree, LogicalPos, PathStep};
use format_docx::test_fixtures::{DEFAULT_CHAR_TEXTS, default_char_style_docx};
use format_docx::write_docx;

/// `(bold, italic, Latin size)` of a run.
type Look = (Option<bool>, Option<bool>, Option<f32>);

/// The [`Look`] text at the start of paragraph `i` resolves to — the
/// engine's one text-style fold.
fn effective(doc: &DocumentTree, i: u32) -> Option<Look> {
    let p = doc.nth_paragraph(i)?;
    let own = p.spans.iter().find(|s| s.start == 0).map(|s| &s.style);
    let s = doc.resolve_text_style(p.style_id.as_deref(), own);
    Some((s.bold, s.italic, s.font_size))
}

fn shape(doc: &DocumentTree) -> Vec<Option<Look>> {
    [0, 1, 2, 3, 5]
        .into_iter()
        .map(|i| effective(doc, i))
        .collect()
}

fn expected() -> Vec<Option<Look>> {
    vec![
        Some((Some(true), None, Some(16.0))),
        Some((None, Some(true), Some(11.0))),
        Some((Some(true), None, Some(16.0))),
        Some((Some(false), None, Some(16.0))),
        Some((Some(true), Some(true), Some(16.0))),
    ]
}

/// a. On read, the default character style is mirrored (bold, 16 pt) and
///    formats the plain run, the direct-formatted one and the `Aside`
///    paragraph; `Quiet` (not based on it) does not take it; no span
///    carries it.
/// b. An untouched save is byte-identical (both save paths).
/// c. Typing into the plain run regenerates it as exactly source + insert.
/// d. Enter + typing: the new run writes no `<w:rPr>`, the re-read
///    formats it in the default character style.
/// e. Issue #560 — a table inserted at the end: its cells carry the
///    docDefaults' spacing in the engine and write no `<w:pPr>`.
pub(super) fn run_default_char_style_roundtrip() -> Result<()> {
    let fixture = default_char_style_docx();
    let archive = read_docx(&fixture).context("read default-char-style fixture")?;
    let doc = &archive.document;
    let dc = doc
        .default_char_style
        .as_ref()
        .context("DefaultParagraphFont not mirrored")?;
    if (dc.bold, dc.font_size) != (Some(true), Some(16.0)) {
        bail!("the default character style read wrong: {dc:?}");
    }
    if shape(doc) != expected() {
        bail!(
            "the default character style is not applied: {:?}",
            shape(doc)
        );
    }
    if !doc.nth_paragraph(0).is_some_and(|p| p.spans.is_empty()) {
        bail!("the default character style was baked into a span");
    }
    println!(
        "[roundtrip] step 67a OK — runs naming no character style take DefaultParagraphFont, no span bakes it"
    );

    let source = String::from_utf8(extract_doc_xml(&fixture)?).context("utf8 source")?;
    let untouched = write_docx(&archive, doc).context("untouched save")?;
    if extract_doc_xml(&untouched)? != source.as_bytes() {
        bail!("untouched default-char-style document drifted");
    }
    let ui = format_docx::save_docx(doc).context("UI save")?;
    if extract_doc_xml(&ui)? != source.as_bytes() {
        bail!("untouched default-char-style document drifted on the UI save path");
    }
    println!("[roundtrip] step 67b OK — untouched save byte-identical (write_docx + save_docx)");

    let edited = doc.insert_text(
        LogicalPos {
            path: BlockPath::top(0),
            offset: "Plain".len() as u32,
        },
        " text",
    );
    let bytes = write_docx(&archive, &edited).context("write edited")?;
    assert_document_xml_well_formed(&bytes).context("edited default-char-style .docx")?;
    let xml = String::from_utf8(extract_doc_xml(&bytes)?).context("utf8 edited")?;
    let expected_xml = source.replacen("Plain run", "Plain text run", 1);
    if xml != expected_xml {
        bail!(
            "edited plain run is not source + edit (the default character style leaked?)\n--- expected ---\n{expected_xml}\n--- got ---\n{xml}"
        );
    }
    if shape(&read_docx(&bytes)?.document) != expected() {
        bail!("re-read of the edited document lost the default character style");
    }
    println!("[roundtrip] step 67c OK — an edited plain run is source + insert");

    let split = doc.split_paragraph(LogicalPos {
        path: BlockPath::top(0),
        offset: DEFAULT_CHAR_TEXTS[0].len() as u32,
    });
    let typed = split.insert_text(
        LogicalPos {
            path: BlockPath::top(1),
            offset: 0,
        },
        "New",
    );
    let bytes = write_docx(&archive, &typed).context("write after Enter")?;
    assert_document_xml_well_formed(&bytes).context("Enter default-char-style .docx")?;
    let xml = String::from_utf8(extract_doc_xml(&bytes)?).context("utf8 Enter")?;
    for leaked in ["<w:b/>", "<w:sz ", "Liberation Serif"] {
        if xml.contains(leaked) {
            bail!(
                "the default character style's `{leaked}` was written as direct formatting:\n{xml}"
            );
        }
    }
    let back = read_docx(&bytes).context("re-read after Enter")?.document;
    if back.nth_paragraph(1).map(|p| p.text.as_str()) != Some("New")
        || effective(&back, 1) != Some((Some(true), None, Some(16.0)))
    {
        bail!(
            "the new run does not re-read in the default character style: {:?}",
            effective(&back, 1)
        );
    }
    println!(
        "[roundtrip] step 67d OK — Enter + typing write no run properties; the new run re-reads bold 16 pt"
    );

    let end = DEFAULT_CHAR_TEXTS.len() as u32;
    let with_table = doc.insert_table(BlockPath::top(end), 1, 2);
    let cell = BlockPath::top(end)
        .push(PathStep::Cell { row: 0, col: 1 })
        .push(PathStep::Block(0));
    let unstyled = with_table.resolve_style_cascade(None);
    if unstyled.spacing.after_twips != 160
        || with_table.paragraph_at_path(&cell).map(|p| &p.props) != Some(&unstyled)
    {
        bail!("the minted cell does not carry the docDefaults' spacing");
    }
    let bytes = write_docx(&archive, &with_table).context("write with a table")?;
    assert_document_xml_well_formed(&bytes).context("table default-char-style .docx")?;
    let xml = String::from_utf8(extract_doc_xml(&bytes)?).context("utf8 table")?;
    let table = xml
        .find("<w:tbl>")
        .map(|at| &xml[at..])
        .context("the inserted table")?;
    if table.contains("<w:pPr>") || table.contains("<w:spacing") {
        bail!("a minted cell wrote the docDefaults' spacing as direct formatting:\n{xml}");
    }
    let back = read_docx(&bytes).context("re-read with a table")?.document;
    if back.paragraph_at_path(&cell).map(|p| &p.props) != Some(&unstyled) {
        bail!("the minted cell re-reads differently");
    }
    println!(
        "[roundtrip] step 67e OK — minted cells carry the docDefaults' spacing, write no pPr, re-read the same"
    );
    Ok(())
}
