//! Step 64 — issue #556: the DEFAULT paragraph style (`Normal`,
//! `w:default="1"`) formats every paragraph that names no style (or an
//! undefined one) on read, and the writer stays byte-neutral
//! (`format_docx::test_fixtures::default_style_rtl_docx`, committed as
//! `default_style_rtl.docx`); 64e — issue #560: a table the engine inserts
//! carries the same cascade in its minted paragraphs and writes none of it.

use super::{assert_document_xml_well_formed, extract_doc_xml, read_docx};
use anyhow::{Context, Result, bail};
use engine::{BlockPath, DocumentTree, LogicalPos, PathStep, Spacing, TextDirection};
use format_docx::test_fixtures::{DEFAULT_STYLE_TEXTS, default_style_rtl_docx};
use format_docx::write_docx;

const NORMAL_SPACING: Spacing = Spacing {
    before_twips: 120,
    after_twips: 240,
};

/// `(resolved direction, spacing)` of the first six paragraphs.
fn shape(doc: &DocumentTree) -> Vec<(Option<TextDirection>, Spacing)> {
    (0..DEFAULT_STYLE_TEXTS.len() as u32)
        .filter_map(|i| doc.nth_paragraph(i))
        .map(|p| (p.props.direction, p.props.spacing))
        .collect()
}

fn expected() -> Vec<(Option<TextDirection>, Spacing)> {
    let plain = Spacing {
        before_twips: 0,
        after_twips: 160,
    };
    vec![
        (Some(TextDirection::Rtl), NORMAL_SPACING),
        (Some(TextDirection::Rtl), NORMAL_SPACING),
        (Some(TextDirection::Rtl), NORMAL_SPACING),
        (Some(TextDirection::Rtl), NORMAL_SPACING),
        (None, plain),
        (Some(TextDirection::Ltr), NORMAL_SPACING),
    ]
}

/// a. On read, the unstyled paragraphs — the EMPTY one included — and
///    the one naming an undefined style resolve RTL with `Normal`'s
///    spacing; `Plain` (not based on `Normal`) and a direct LTR do not.
/// b. An untouched save is byte-identical (both save paths).
/// c. Typing into an unstyled paragraph regenerates it as exactly the
///    source plus the insert (no `<w:bidi/>` / `<w:spacing>` from `Normal`).
/// d. Enter at the end of the empty paragraph and typing into the new
///    one: nothing `Normal` gives is written as direct formatting, and
///    the re-read resolves the new paragraph RTL with `Normal`'s spacing.
/// e. Issue #560 — a table inserted at the end: its cells and escape
///    paragraph carry docDefaults + `Normal` in the engine, write no
///    `<w:pPr>` and re-read to the same properties.
pub(super) fn run_default_style_roundtrip() -> Result<()> {
    let fixture = default_style_rtl_docx();
    let archive = read_docx(&fixture).context("read default-style fixture")?;
    if shape(&archive.document) != expected() {
        bail!(
            "the default paragraph style is not applied on read: {:?}",
            shape(&archive.document)
        );
    }
    println!(
        "[roundtrip] step 64a OK — unstyled paragraphs (the empty one included) read through Normal"
    );

    let source = String::from_utf8(extract_doc_xml(&fixture)?).context("utf8 source")?;
    let untouched = write_docx(&archive, &archive.document).context("untouched save")?;
    if extract_doc_xml(&untouched)? != source.as_bytes() {
        bail!("untouched default-style document drifted");
    }
    let ui = format_docx::save_docx(&archive.document).context("UI save")?;
    if extract_doc_xml(&ui)? != source.as_bytes() {
        bail!("untouched default-style document drifted on the UI save path");
    }
    println!("[roundtrip] step 64b OK — untouched save byte-identical (write_docx + save_docx)");

    let edited = archive.document.insert_text(
        LogicalPos {
            path: BlockPath::top(2),
            offset: "Latin".len() as u32,
        },
        " text",
    );
    let bytes = write_docx(&archive, &edited).context("write edited")?;
    assert_document_xml_well_formed(&bytes).context("edited default-style .docx")?;
    let xml = String::from_utf8(extract_doc_xml(&bytes)?).context("utf8 edited")?;
    let expected_xml = source.replacen("Latin first", "Latin text first", 1);
    if xml != expected_xml {
        bail!(
            "edited unstyled paragraph is not source + edit (Normal's formatting leaked?)\n--- expected ---\n{expected_xml}\n--- got ---\n{xml}"
        );
    }
    if shape(&read_docx(&bytes)?.document) != expected() {
        bail!("re-read of the edited document lost Normal's formatting");
    }
    println!("[roundtrip] step 64c OK — an edited unstyled paragraph is source + insert");

    let split = archive.document.split_paragraph(LogicalPos {
        path: BlockPath::top(1),
        offset: 0,
    });
    let typed = split.insert_text(
        LogicalPos {
            path: BlockPath::top(2),
            offset: 0,
        },
        "Latin",
    );
    let bytes = write_docx(&archive, &typed).context("write after Enter")?;
    assert_document_xml_well_formed(&bytes).context("Enter default-style .docx")?;
    let xml = String::from_utf8(extract_doc_xml(&bytes)?).context("utf8 Enter")?;
    for leaked in ["<w:bidi/>", "w:before=\"120\"", "w:line=\"360\"", "<w:sz"] {
        if xml.contains(leaked) {
            bail!("Normal's `{leaked}` was written as direct formatting:\n{xml}");
        }
    }
    let back = read_docx(&bytes).context("re-read after Enter")?.document;
    let new = back.nth_paragraph(2).context("the new paragraph")?;
    if new.text != "Latin"
        || new.props.direction != Some(TextDirection::Rtl)
        || new.props.spacing != NORMAL_SPACING
    {
        bail!(
            "the new paragraph does not re-read through Normal: {:?} {:?} {:?}",
            new.text,
            new.props.direction,
            new.props.spacing
        );
    }
    println!(
        "[roundtrip] step 64d OK — Enter + typing write nothing Normal gives; the new paragraph re-reads RTL"
    );

    /* e. Issue #560 — a table inserted at the end: its cells and escape
    paragraph carry docDefaults + Normal in the engine (they lay out as
    the package re-opens), the save writes no `<w:pPr>` for them (as
    before #560) and the re-read gives them the same properties. */
    let end = DEFAULT_STYLE_TEXTS.len() as u32;
    let with_table = archive.document.insert_table(BlockPath::top(end), 2, 2);
    let cell = |row: u32, col: u32| {
        BlockPath::top(end)
            .push(PathStep::Cell { row, col })
            .push(PathStep::Block(0))
    };
    let unstyled = with_table.resolve_style_cascade(None);
    let minted: Vec<BlockPath> = vec![cell(0, 0), cell(1, 1), BlockPath::top(end + 1)];
    for path in &minted {
        let props = with_table.paragraph_at_path(path).map(|p| &p.props);
        if props != Some(&unstyled) {
            bail!("minted paragraph {path:?} does not carry docDefaults + Normal: {props:?}");
        }
    }
    let bytes = write_docx(&archive, &with_table).context("write with a table")?;
    assert_document_xml_well_formed(&bytes).context("table default-style .docx")?;
    let xml = String::from_utf8(extract_doc_xml(&bytes)?).context("utf8 table")?;
    let table = xml
        .find("<w:tbl>")
        .map(|at| &xml[at..])
        .context("the inserted table")?;
    if table.contains("<w:pPr>") {
        bail!("a minted paragraph wrote inherited properties as direct formatting:\n{xml}");
    }
    if !xml.starts_with(source.split(DEFAULT_STYLE_LAST).next().unwrap_or_default()) {
        bail!("the source paragraphs did not replay in front of the table:\n{xml}");
    }
    let back = read_docx(&bytes).context("re-read with a table")?.document;
    for path in &minted {
        if back.paragraph_at_path(path).map(|p| &p.props) != Some(&unstyled) {
            bail!("minted paragraph {path:?} re-reads differently");
        }
    }
    println!(
        "[roundtrip] step 64e OK — an inserted table's minted paragraphs carry Normal, write no pPr, re-read the same"
    );
    Ok(())
}

/// The last source paragraph's text: everything in front of it replays.
const DEFAULT_STYLE_LAST: &str = "Direct LTR override";
