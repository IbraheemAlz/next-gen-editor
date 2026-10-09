//! Issue #419 — a regenerated `<w:pPr>` changes only the children whose
//! meaning changed: `ppr_attributes.docx` (leader tabs, border edges with
//! `w:space` / `w:shadow` / a theme colour, a `pct25` pattern shading,
//! autospacing, `w:left` / `jc="right"` spellings; a section-mark
//! paragraph and a pPr-less one over docDefaults spacing).

use super::{assert_document_xml_well_formed, extract_doc_xml, read_docx, write_docx};
use anyhow::{Context, Result, bail};
use engine::{BlockPath, LogicalPos};
use format_docx::test_fixtures::ppr_attributes_docx;

fn at(block: u32, offset: u32) -> LogicalPos {
    LogicalPos::new(BlockPath::top(block), offset)
}

fn save(archive: &format_docx::DocxArchive, doc: &engine::DocumentTree) -> Result<String> {
    let bytes = write_docx(archive, doc).context("write")?;
    assert_document_xml_well_formed(&bytes)?;
    Ok(String::from_utf8(extract_doc_xml(&bytes)?)?)
}

/// Step 57:
///
/// a. Zero-edit identity; the pattern (`pct25` + colour) and the border
///    `w:space` / `w:shadow` are modeled.
/// b. An indent change respells exactly the `<w:ind>` element (in the
///    source's `w:left` spelling): tabs, borders, shading, autospacing and
///    `jc="right"` stay byte-identical.
/// c. A changed fill keeps the pattern; a changed edge keeps `w:space`
///    and drops the stale `w:themeColor`; the other edges stay.
/// d. Typing into the section-mark paragraph is a pure insertion (the
///    docDefaults spacing is not baked in); aligning the pPr-less
///    paragraph writes `<w:jc>` alone.
pub fn run_ppr_splice_roundtrip() -> Result<()> {
    let fixture = ppr_attributes_docx();
    let archive = read_docx(&fixture).context("read ppr_attributes.docx")?;
    let doc = &archive.document;
    let source = String::from_utf8(extract_doc_xml(&fixture)?)?;
    if save(&archive, doc)? != source {
        bail!("zero-edit save drifted");
    }
    let p = doc.nth_paragraph(0).context("paragraph 0")?;
    let top = p
        .props
        .borders
        .as_ref()
        .and_then(|b| b.top.clone())
        .context("top edge")?;
    if p.props.shading_pattern.as_ref().map(|s| s.val.as_str()) != Some("pct25")
        || top.space_pt != Some(1)
        || !top.shadow
    {
        bail!("pattern / border extras not modeled: {:?}", p.props);
    }
    println!(
        "[roundtrip] step 57a OK — zero-edit identity; pattern shading and border space / shadow modeled"
    );

    let indented = doc.set_paragraph_indent(at(0, 0), at(0, 0), 72.0, 0.0, 18.0);
    let expected = source.replacen(
        r#"<w:ind w:left="720" w:firstLine="360"/>"#,
        r#"<w:ind w:left="1440" w:firstLine="360"/>"#,
        1,
    );
    let out = save(&archive, &indented)?;
    if out != expected {
        bail!("an indent change rewrote more than <w:ind>:\n{out}");
    }
    println!(
        "[roundtrip] step 57b OK — an indent change rewrites only <w:ind>; every other pPr child byte-identical"
    );

    let shaded = doc.set_paragraph_shading(at(0, 0), at(0, 0), Some([0, 0, 0xFF, 0xFF]));
    let mut borders = p.props.borders.clone().context("borders")?;
    if let Some(left) = borders.left.as_mut() {
        left.color = Some([0, 0, 0xFF, 0xFF]);
    }
    let both = shaded.set_paragraph_borders(at(0, 0), at(0, 0), Some(borders));
    let expected = source
        .replacen(
            r#"<w:shd w:val="pct25" w:color="FF0000" w:fill="00FF00"/>"#,
            r#"<w:shd w:val="pct25" w:color="FF0000" w:fill="0000FF"/>"#,
            1,
        )
        .replacen(
            r#"<w:left w:val="double" w:sz="6" w:space="4" w:color="FF0000" w:themeColor="accent2"/>"#,
            r#"<w:left w:val="double" w:sz="6" w:space="4" w:color="0000FF"/>"#,
            1,
        );
    let out = save(&archive, &both)?;
    if out != expected {
        bail!("a changed fill / edge lost what the model does not change:\n{out}");
    }
    println!(
        "[roundtrip] step 57c OK — a changed fill keeps pct25 + its colour; a changed edge keeps w:space, drops the stale theme colour"
    );

    let typed = doc.insert_text(at(1, 3), "X");
    let out = save(&archive, &typed)?;
    if out != source.replacen("End of", "EndX of", 1) {
        bail!("typing into the section-mark paragraph is not a pure insertion:\n{out}");
    }
    let aligned = doc.set_alignment(at(2, 0), at(2, 0), engine::Alignment::Center);
    let out = save(&archive, &aligned)?;
    if !out.contains(r#"<w:p w:rsidR="00B3"><w:pPr><w:jc w:val="center"/></w:pPr><w:r>"#) {
        bail!("aligning a pPr-less paragraph baked cascade values:\n{out}");
    }
    println!("[roundtrip] step 57d OK — no cascade value is baked into a regenerated pPr");
    Ok(())
}
