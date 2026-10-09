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
    CS_SIZE_CASCADE_TEXT, CS_SIZE_MIXED_TEXT, complex_script_size_docx, docx_with_body,
};

/// Issue #104 — an Arabic run in rtl.docx's shape: bold by `<w:bCs/>`
/// only, with a character style reference.
const BCS_RUN: &str = r#"<w:p><w:pPr><w:bidi/></w:pPr><w:r><w:rPr><w:rStyle w:val="Emph"/><w:bCs/><w:rtl/></w:rPr><w:t>مملكة إسبانيا</w:t></w:r><w:r><w:t xml:space="preserve"> (</w:t></w:r></w:p>"#;

/// Issue #249 — a Word-shaped Arabic run: a Latin face, an Arabic face and
/// `w:hint` in one `<w:rFonts>`, an explicit OFF bold, both sizes, `rtl`
/// and a language tag.
const WORD_ARABIC_RPR: &str = r#"<w:rFonts w:ascii="Times New Roman" w:hAnsi="Times New Roman" w:cs="Simplified Arabic" w:hint="cs"/><w:b w:val="false"/><w:sz w:val="22"/><w:szCs w:val="28"/><w:rtl/><w:lang w:bidi="ar-SA"/>"#;
const WORD_ARABIC_TEXT: &str = "الإصدار الأول";

/// Step 45e — regeneration fidelity (#249): changing bold rewrites only
/// `<w:b>`; changing the font rewrites only the font names, keeping
/// `w:hint`, the explicit OFF bold and both sizes.
fn run_regeneration_fidelity_step() -> Result<()> {
    let body = format!(
        r#"<w:p><w:pPr><w:bidi/></w:pPr><w:r><w:rPr>{WORD_ARABIC_RPR}</w:rPr><w:t>{WORD_ARABIC_TEXT}</w:t></w:r></w:p>"#
    );
    let src = docx_with_body(&body);
    let archive = read_docx(&src).context("read Word-shaped Arabic run")?;
    let doc_a = doc_xml_string(&src)?;
    let s = archive
        .document
        .nth_paragraph(0)
        .context("paragraph")?
        .style_at(0);
    let names = (
        s.font_family.as_ref().map(|f| f.display_name().to_string()),
        s.font_family_cs
            .as_ref()
            .map(|f| f.display_name().to_string()),
    );
    if names
        != (
            Some("Times New Roman".into()),
            Some("Simplified Arabic".into()),
        )
    {
        bail!("w:ascii / w:cs not read into their own slots: {names:?}");
    }
    let untouched = write_docx(&archive, &archive.document).context("untouched save")?;
    if doc_xml_string(&untouched)? != doc_a {
        bail!("untouched Word-shaped Arabic run drifted");
    }
    let len = WORD_ARABIC_TEXT.len();
    let write = |patch: SpanStyle| -> Result<String> {
        let edited = archive.document.apply_style(pos(0, 0), pos(0, len), patch);
        let bytes = write_docx(&archive, &edited).context("write edited")?;
        assert_document_xml_well_formed(&bytes).context("edited Arabic run")?;
        doc_xml_string(&bytes)
    };
    let bolded = write(
        SpanStyle {
            bold: Some(true),
            ..SpanStyle::default()
        }
        .with_cs_twins(),
    )?;
    let expected = doc_a.replacen(r#"<w:b w:val="false"/>"#, "<w:b/><w:bCs/>", 1);
    if bolded != expected {
        bail!(
            "bold change rewrote more than <w:b>\n--- expected ---\n{expected}\n--- got ---\n{bolded}"
        );
    }
    let refonted = write(
        SpanStyle {
            font_family: Some(engine::FontFamily::Amiri),
            ..SpanStyle::default()
        }
        .with_cs_twins(),
    )?;
    let expected = doc_a.replacen(
        r#"<w:rFonts w:ascii="Times New Roman" w:hAnsi="Times New Roman" w:cs="Simplified Arabic" w:hint="cs"/>"#,
        r#"<w:rFonts w:ascii="Amiri" w:hAnsi="Amiri" w:cs="Amiri" w:hint="cs"/>"#,
        1,
    );
    if refonted != expected {
        bail!(
            "font change dropped unmodeled values\n--- expected ---\n{expected}\n--- got ---\n{refonted}"
        );
    }
    println!(
        "[roundtrip] step 45e OK — changing bold rewrites only <w:b>; a font change keeps w:hint, the OFF bold and both sizes (#249)"
    );
    Ok(())
}

/// Step 45d — Ctrl+B on an Arabic `<w:bCs/>` run writes `<w:b/>` next to
/// the existing `<w:bCs/>` and keeps its `<w:rStyle>`: a pure insertion.
fn run_bcs_rstyle_step() -> Result<()> {
    let src = docx_with_body(BCS_RUN);
    let archive = read_docx(&src).context("read bCs fixture")?;
    let doc_a = doc_xml_string(&src)?;
    let p = archive.document.nth_paragraph(0).context("bCs paragraph")?;
    let s = p.style_at(0);
    if (s.bold, s.bold_cs, s.char_style.as_deref()) != (None, Some(true), Some("Emph")) {
        bail!("bCs / rStyle not modeled: {s:?}");
    }
    let untouched = write_docx(&archive, &archive.document).context("untouched save")?;
    if doc_xml_string(&untouched)? != doc_a {
        bail!("untouched bCs document drifted");
    }
    let len = "مملكة إسبانيا".len();
    let bolded = archive.document.apply_style(
        pos(0, 0),
        pos(0, len),
        SpanStyle {
            bold: Some(true),
            ..SpanStyle::default()
        }
        .with_cs_twins(),
    );
    let bytes = write_docx(&archive, &bolded).context("write bolded")?;
    assert_document_xml_well_formed(&bytes).context("bolded bCs .docx")?;
    let xml = doc_xml_string(&bytes)?;
    let expected = doc_a.replacen(
        r#"<w:rStyle w:val="Emph"/><w:bCs/>"#,
        r#"<w:rStyle w:val="Emph"/><w:b/><w:bCs/>"#,
        1,
    );
    if xml != expected {
        bail!(
            "bold toggle on a bCs run is not a pure <w:b/> insertion\n--- expected ---\n{expected}\n--- got ---\n{xml}"
        );
    }
    let back = read_docx(&bytes).context("re-read bolded")?.document;
    let s = back.nth_paragraph(0).context("paragraph")?.style_at(0);
    if (s.bold, s.bold_cs, s.char_style.as_deref()) != (Some(true), Some(true), Some("Emph")) {
        bail!("b / bCs / rStyle lost on re-read: {s:?}");
    }
    println!(
        "[roundtrip] step 45d OK — bold on an Arabic <w:bCs/> run inserts <w:b/> and keeps <w:rStyle> (both slots re-read)"
    );
    Ok(())
}

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

/// Step 45 — the mixed-size complex-script fixture.
pub(crate) fn run_complex_script_roundtrip() -> Result<()> {
    let src = complex_script_size_docx();
    let archive = read_docx(&src).context("read complex-script fixture")?;
    let doc_a = doc_xml_string(&src)?;

    /* 45a — each slot reads into its own field; both save paths
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
        "[roundtrip] step 45a OK — w:sz / w:szCs read into their own slots; zero-edit save byte-identical (both save paths)"
    );

    /* 45b — typing into the mixed-size run is a pure insertion. */
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
        "[roundtrip] step 45b OK — typing into the mixed-size run is a pure insertion (Δ {} B)",
        xml.len() - doc_a.len()
    );

    /* 45c — a size set on part of the run writes BOTH slots, the rest
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
        "[roundtrip] step 45c OK — a set size writes w:sz + w:szCs, untouched text keeps its pair, no synthesized w:szCs"
    );
    run_bcs_rstyle_step()?;
    run_regeneration_fidelity_step()
}

/// Step 48 (issue #420) — the Font dialog's per-section Apply: each
/// section is ONE formatting patch on its script slot (`setSlotFormat` →
/// `ApplyFormatting { font_slot }`, routed by engine-wasm's
/// `patch_to_span_style`: `Latin` as is, `ComplexScript` through
/// `SpanStyle::into_cs_only`). On the mixed-size run (`w:sz="22"
/// w:szCs="28"`):
///
/// a. "Complex scripts": Amiri, 20 pt, bold → `w:cs`, `<w:bCs/>`,
///    `<w:szCs w:val="40"/>` only; the Latin `<w:sz w:val="22"/>` stays and
///    no `w:ascii` / `<w:b>` appears.
/// b. "Latin text": italic, 9 pt on top → `<w:i/>` + `<w:sz w:val="18"/>`;
///    the complex-script set from (a) is untouched.
/// c. The reread keeps every slot apart.
pub(crate) fn run_font_dialog_slots_roundtrip() -> Result<()> {
    let src = complex_script_size_docx();
    let archive = read_docx(&src).context("read complex-script fixture")?;
    let len = CS_SIZE_MIXED_TEXT.len();
    let cs_section = SpanStyle {
        font_family: Some(engine::FontFamily::Amiri),
        font_size: Some(20.0),
        bold: Some(true),
        ..SpanStyle::default()
    }
    .into_cs_only();
    let cs_doc = archive
        .document
        .apply_style(pos(0, 0), pos(0, len), cs_section);
    let bytes = write_docx(&archive, &cs_doc).context("write cs-only")?;
    assert_document_xml_well_formed(&bytes).context("cs-only .docx")?;
    let xml = doc_xml_string(&bytes)?;
    let p0 = nth_paragraph_xml(&xml, 0);
    let want =
        r#"<w:rPr><w:rFonts w:cs="Amiri"/><w:bCs/><w:sz w:val="22"/><w:szCs w:val="40"/></w:rPr>"#;
    if !p0.contains(want) {
        bail!(
            "step 48a: the Complex scripts section must write only the cs set\nwant {want}\n got {p0}"
        );
    }
    println!(
        "[roundtrip] step 48a OK — the Font dialog's Complex scripts section writes w:cs / <w:bCs/> / <w:szCs> only"
    );

    let latin_section = SpanStyle {
        italic: Some(true),
        font_size: Some(9.0),
        ..SpanStyle::default()
    };
    let both = cs_doc.apply_style(pos(0, 0), pos(0, len), latin_section);
    let bytes = write_docx(&archive, &both).context("write Latin on top")?;
    assert_document_xml_well_formed(&bytes).context("Latin-on-top .docx")?;
    let xml = doc_xml_string(&bytes)?;
    let p0 = nth_paragraph_xml(&xml, 0);
    let want = r#"<w:rPr><w:rFonts w:cs="Amiri"/><w:bCs/><w:i/><w:sz w:val="18"/><w:szCs w:val="40"/></w:rPr>"#;
    if !p0.contains(want) {
        bail!("step 48b: the Latin text section must leave the cs set\nwant {want}\n got {p0}");
    }
    let back = read_docx(&bytes).context("re-read")?.document;
    let s = back.nth_paragraph(0).context("paragraph")?.style_at(0);
    let got = (
        s.font_family.clone(),
        s.font_family_cs.clone(),
        (s.font_size, s.font_size_cs),
        (s.bold, s.bold_cs),
        (s.italic, s.italic_cs),
    );
    let expected = (
        None,
        Some(engine::FontFamily::Amiri),
        (Some(9.0), Some(20.0)),
        (None, Some(true)),
        (Some(true), None),
    );
    if got != expected {
        bail!("step 48c: slots lost on re-read: {got:?}, expected {expected:?}");
    }
    println!(
        "[roundtrip] step 48 OK — the Font dialog's two sections write their own script slots (Latin: <w:i/> <w:sz>; complex: w:cs <w:bCs/> <w:szCs>) and re-read apart"
    );
    Ok(())
}
