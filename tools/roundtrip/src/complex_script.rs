//! Issues #359 / #104 / #249 — complex-script run properties. OOXML formats
//! Arabic / Hebrew / Thai / … text (and every character of a `<w:rtl/>` run)
//! with the complex-script twins of the run properties — `<w:szCs>` for
//! `<w:sz>`, … — so the twins must stay apart from their Latin slots from
//! read through edit to write: a zero-edit save is byte-identical, typing
//! is a pure insertion, and a regenerated run writes each slot it holds
//! (never a synthesized twin the source did not have).

use super::{INSERT_TEXT, assert_document_xml_well_formed, extract_doc_xml, read_docx, write_docx};
use anyhow::{Context, Result, bail};
use engine::{BlockPath, DocumentTree, LogicalPos, SpanStyle};
use format_docx::test_fixtures::{
    CS_SIZE_CASCADE_TEXT, CS_SIZE_MIXED_TEXT, complex_script_size_docx,
};

fn pos(para: u32, offset: usize) -> LogicalPos {
    LogicalPos {
        path: BlockPath::top(para),
        offset: offset as u32,
    }
}

fn doc_xml_string(bytes: &[u8]) -> Result<String> {
    String::from_utf8(extract_doc_xml(bytes)?).context("document.xml is UTF-8")
}

/// The `n`-th (0-based) top-level `<w:p>` element of `xml`, by its start
/// tag (`<w:p>` or `<w:p …>`), up to the next one.
fn nth_paragraph_xml(xml: &str, n: usize) -> &str {
    let starts: Vec<usize> = xml
        .match_indices("<w:p")
        .filter(|(i, _)| matches!(xml.as_bytes().get(i + 4), Some(b'>' | b' ')))
        .map(|(i, _)| i)
        .collect();
    let start = starts.get(n).copied().unwrap_or(xml.len());
    let end = starts.get(n + 1).copied().unwrap_or(xml.len());
    &xml[start..end]
}

/// Step 41 — the mixed-size complex-script fixture.
pub(crate) fn run_complex_script_roundtrip() -> Result<()> {
    let src = complex_script_size_docx();
    let archive = read_docx(&src).context("read complex-script fixture")?;
    let doc_a = doc_xml_string(&src)?;

    /* 41a — each slot reads into its own field; both save paths
    reproduce the source byte for byte. */
    let slots = |doc: &DocumentTree, para: u32, at: u32| {
        let s = doc
            .nth_paragraph(para)
            .map(|p| p.style_at(at))
            .unwrap_or_default();
        (s.font_size, s.font_size_cs)
    };
    if slots(&archive.document, 0, 0) != (Some(11.0), Some(14.0))
        || slots(&archive.document, 1, 0) != (Some(11.0), Some(14.0))
        || slots(&archive.document, 2, 0) != (Some(11.0), None)
    {
        bail!(
            "w:sz / w:szCs not read into their own slots: {:?} {:?} {:?}",
            slots(&archive.document, 0, 0),
            slots(&archive.document, 1, 0),
            slots(&archive.document, 2, 0)
        );
    }
    let untouched = write_docx(&archive, &archive.document).context("untouched save")?;
    if doc_xml_string(&untouched)? != doc_a {
        bail!("untouched complex-script document drifted (write_docx)");
    }
    let ui = format_docx::save_docx(&archive.document).context("UI save")?;
    if doc_xml_string(&ui)? != doc_a {
        bail!("untouched complex-script document drifted (UI save path)");
    }
    println!(
        "[roundtrip] step 41a OK — w:sz / w:szCs read into their own slots; zero-edit save byte-identical (both save paths)"
    );

    /* 41b — typing into the mixed-size run is a pure insertion. */
    let at = CS_SIZE_MIXED_TEXT
        .find("النص")
        .context("Arabic word in the mixed run")?;
    let typed = archive.document.insert_text(pos(0, at), INSERT_TEXT);
    let typed_text = format!(
        "{}{INSERT_TEXT}{}",
        &CS_SIZE_MIXED_TEXT[..at],
        &CS_SIZE_MIXED_TEXT[at..]
    );
    let expected = doc_a.replacen(CS_SIZE_MIXED_TEXT, &typed_text, 1);
    let bytes = write_docx(&archive, &typed).context("write typed")?;
    assert_document_xml_well_formed(&bytes).context("typed complex-script .docx")?;
    let xml = doc_xml_string(&bytes)?;
    if xml != expected {
        bail!(
            "typing into the mixed-size run is not source + insertion\n--- expected ---\n{expected}\n--- got ---\n{xml}"
        );
    }
    println!(
        "[roundtrip] step 41b OK — typing into the mixed-size run is a pure insertion (Δ {} B)",
        xml.len() - doc_a.len()
    );

    /* 41c — a size set on part of the run writes BOTH slots, the rest
    keeps its source pair, and a run that only had `w:sz` never gains a
    synthesized `w:szCs` when something else about it changes (#249). */
    let sized = archive.document.apply_style(
        pos(0, 0),
        pos(0, 5),
        SpanStyle {
            font_size: Some(18.0),
            ..SpanStyle::default()
        }
        .with_cs_twins(),
    );
    let edited = sized.apply_style(
        pos(2, 0),
        pos(2, CS_SIZE_CASCADE_TEXT.len()),
        SpanStyle {
            color: Some([0xC0, 0, 0, 255]),
            ..SpanStyle::default()
        },
    );
    let bytes = write_docx(&archive, &edited).context("write sized")?;
    assert_document_xml_well_formed(&bytes).context("sized complex-script .docx")?;
    let xml = doc_xml_string(&bytes)?;
    let p0 = nth_paragraph_xml(&xml, 0);
    if !p0.contains(r#"<w:sz w:val="36"/><w:szCs w:val="36"/>"#)
        || !p0.contains(r#"<w:sz w:val="22"/><w:szCs w:val="28"/>"#)
    {
        bail!("sized run must write both slots, the rest its source pair: {p0}");
    }
    let p2 = nth_paragraph_xml(&xml, 2);
    if !p2.contains(r#"<w:color w:val="C00000"/><w:sz w:val="22"/></w:rPr>"#) || p2.contains("szCs")
    {
        bail!("a Latin-only size gained a synthesized w:szCs: {p2}");
    }
    let back = read_docx(&bytes).context("re-read sized")?.document;
    if slots(&back, 0, 0) != (Some(18.0), Some(18.0))
        || slots(&back, 0, 10) != (Some(11.0), Some(14.0))
        || slots(&back, 2, 0) != (Some(11.0), None)
    {
        bail!("slots lost on re-read");
    }
    println!(
        "[roundtrip] step 41c OK — a set size writes w:sz + w:szCs, untouched text keeps its pair, no synthesized w:szCs"
    );
    Ok(())
}
