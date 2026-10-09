//! Issues #359 / #104 / #249 — complex-script run properties through a
//! read → edit → write cycle: each twin (`<w:szCs>`, `<w:bCs>`, `<w:iCs>`,
//! `<w:rFonts w:cs>`) is read into its own slot and written from it, the
//! run's `<w:rStyle>` survives a regeneration, and a regenerated run never
//! drops what its source said nor synthesizes what it did not.

use super::tests::{build_docx_with_styles, document_xml_of};
use super::*;
use crate::opc::archive::read_docx;
use engine::{BlockPath, LogicalPos};

const STYLES_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:style w:type="character" w:styleId="Emph"><w:name w:val="Emphasis"/><w:rPr><w:color w:val="1F4E79"/></w:rPr></w:style></w:styles>"#;

fn document(body: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}<w:sectPr/></w:body></w:document>"#
    )
}

fn open(body: &str) -> (String, DocxArchive) {
    let xml = document(body);
    let archive = read_docx(&build_docx_with_styles(STYLES_XML, &xml)).expect("read fixture");
    (xml, archive)
}

fn at(block: u32, offset: usize) -> LogicalPos {
    LogicalPos {
        path: BlockPath::top(block),
        offset: offset as u32,
    }
}

fn bold(on: bool) -> SpanStyle {
    SpanStyle {
        bold: Some(on),
        ..SpanStyle::default()
    }
}

const ARABIC: &str = "نص عربي";

/// Issue #104 acceptance — bolding an Arabic run through the UI (both
/// slots, `with_cs_twins`) writes `<w:b/>` AND `<w:bCs/>`, so Word shows
/// the Arabic bold; un-bolding clears both; each re-reads into its slot.
#[test]
fn arabic_bold_toggle_round_trips_b_and_bcs() {
    let body = format!(
        r#"<w:p><w:pPr><w:bidi/></w:pPr><w:r><w:rPr><w:rtl/></w:rPr><w:t>{ARABIC}</w:t></w:r></w:p>"#
    );
    let (_, archive) = open(&body);
    let len = ARABIC.len();
    let bolded = archive
        .document
        .apply_style(at(0, 0), at(0, len), bold(true).with_cs_twins());
    let xml = document_xml_of(&write_docx(&archive, &bolded).expect("write"));
    assert!(
        xml.contains("<w:rPr><w:b/><w:bCs/><w:rtl/></w:rPr>"),
        "{xml}"
    );
    let back = read_docx(&write_docx(&archive, &bolded).unwrap()).unwrap();
    let s = back.document.nth_paragraph(0).unwrap().style_at(0);
    assert_eq!((s.bold, s.bold_cs), (Some(true), Some(true)));

    let plain = bolded.apply_style(at(0, 0), at(0, len), bold(false).with_cs_twins());
    let xml = document_xml_of(&write_docx(&archive, &plain).expect("write"));
    assert!(
        !xml.contains("<w:b/>") && !xml.contains("<w:bCs/>"),
        "{xml}"
    );
}

/// The twins read into their own slots: `<w:bCs/>` alone bolds only
/// complex-script text (rtl.docx's shape), `<w:iCs w:val="0"/>` is an
/// explicit "not italic" for it; neither rides the grab bag any more.
#[test]
fn bcs_and_ics_read_into_their_own_slots() {
    let body = format!(
        r#"<w:p><w:r><w:rPr><w:bCs/><w:i/><w:iCs w:val="0"/></w:rPr><w:t>{ARABIC}</w:t></w:r></w:p>"#
    );
    let (xml, archive) = open(&body);
    let s = archive.document.nth_paragraph(0).unwrap().style_at(0);
    assert_eq!((s.bold, s.bold_cs), (None, Some(true)));
    assert_eq!((s.italic, s.italic_cs), (Some(true), Some(false)));
    assert!(s.grab_bag.is_none(), "modeled twins left the bag");
    /* Zero-edit: verified reuse keeps the source bytes. */
    let resaved = document_xml_of(&write_docx(&archive, &archive.document).unwrap());
    assert_eq!(resaved, xml);
}

/// Issue #104 acceptance — `<w:rStyle>` survives a regeneration: an edit
/// that changes the run's formatting writes the character-style reference
/// back (its folded properties follow as direct formatting, as before).
#[test]
fn rstyle_survives_an_edit() {
    let body =
        r#"<w:p><w:r><w:rPr><w:rStyle w:val="Emph"/></w:rPr><w:t>emphasis here</w:t></w:r></w:p>"#;
    let (_, archive) = open(body);
    let s = archive.document.nth_paragraph(0).unwrap().style_at(0);
    assert_eq!(s.char_style.as_deref(), Some("Emph"));
    assert_eq!(s.color, Some([0x1F, 0x4E, 0x79, 255]), "style folded in");
    let edited = archive
        .document
        .apply_style(at(0, 0), at(0, 8), bold(true).with_cs_twins());
    let xml = document_xml_of(&write_docx(&archive, &edited).expect("write"));
    assert_eq!(
        xml.matches(r#"<w:rStyle w:val="Emph"/>"#).count(),
        2,
        "both the bolded and the untouched piece keep the style: {xml}"
    );
    let back = read_docx(&write_docx(&archive, &edited).unwrap()).unwrap();
    let p = back.document.nth_paragraph(0).unwrap();
    assert_eq!(p.style_at(0).char_style.as_deref(), Some("Emph"));
    assert_eq!(p.style_at(10).char_style.as_deref(), Some("Emph"));
}

/// Issues #104 × #295 — a tracked formatting change records the run's
/// previous `<w:rStyle>`: the modeled `FormatChange` keeps it as the
/// recorded style's `char_style`, so rejecting the change writes the
/// character style back instead of flattening it.
#[test]
fn rejecting_a_format_change_restores_the_recorded_rstyle() {
    let body = concat!(
        r#"<w:p><w:r><w:rPr><w:b/><w:bCs/><w:rPrChange w:id="4" w:author="A" w:date="2026-01-01T00:00:00Z">"#,
        r#"<w:rPr><w:rStyle w:val="Emph"/></w:rPr></w:rPrChange></w:rPr><w:t>was emphasis</w:t></w:r></w:p>"#,
    );
    let (xml, archive) = open(body);
    let p = archive.document.nth_paragraph(0).unwrap();
    let prev = p.revisions[0].prev_attrs.as_ref().expect("recorded");
    assert_eq!(prev.char_style.as_deref(), Some("Emph"));
    assert_eq!(prev.color, Some([0x1F, 0x4E, 0x79, 255]), "style folded in");
    assert_eq!((prev.bold, prev.bold_cs), (None, None));
    assert_eq!(
        document_xml_of(&write_docx(&archive, &archive.document).unwrap()),
        xml
    );
    let rejected = archive.document.resolve_all_revisions(false);
    let out = document_xml_of(&write_docx(&archive, &rejected).expect("write"));
    assert!(out.contains(r#"<w:rStyle w:val="Emph"/>"#), "{out}");
    assert!(
        !out.contains("<w:b/>") && !out.contains("<w:bCs/>"),
        "{out}"
    );
    assert!(!out.contains("w:rPrChange"), "{out}");
}

/// Issues #104 / #359 × #293 — the paragraph mark's `<w:rPr>` folds its
/// complex-script twins into `mark_style` like a run's, and a mark
/// regenerated by an edit writes each twin from its own slot.
#[test]
fn the_paragraph_mark_carries_the_complex_script_twins() {
    let body = format!(
        r#"<w:p><w:pPr><w:bidi/><w:rPr><w:bCs/><w:sz w:val="22"/><w:szCs w:val="28"/><w:rtl/></w:rPr></w:pPr><w:r><w:rPr><w:rtl/></w:rPr><w:t>{ARABIC}</w:t></w:r></w:p>"#
    );
    let (xml, archive) = open(&body);
    let mark = archive
        .document
        .nth_paragraph(0)
        .unwrap()
        .mark_style
        .clone()
        .expect("modeled mark");
    assert_eq!(mark.bold_cs, Some(true));
    assert_eq!(mark.bold, None);
    assert_eq!(
        (mark.font_size, mark.font_size_cs),
        (Some(11.0), Some(14.0))
    );
    assert_eq!(
        document_xml_of(&write_docx(&archive, &archive.document).unwrap()),
        xml
    );
    /* Enter at the end: the new empty paragraph's mark takes the typing
    style of the run (only `<w:rtl/>`), the original keeps its mark. */
    let split = archive.document.split_paragraph(at(0, ARABIC.len()));
    let out = document_xml_of(&write_docx(&archive, &split).expect("write"));
    assert!(
        out.contains(
            r#"<w:rPr><w:bCs/><w:sz w:val="22"/><w:szCs w:val="28"/><w:rtl/></w:rPr></w:pPr>"#
        ),
        "{out}"
    );
    let back = read_docx(&write_docx(&archive, &split).unwrap()).unwrap();
    let m0 = back.document.nth_paragraph(0).unwrap().mark_style.clone();
    assert_eq!(m0.as_deref().and_then(|m| m.bold_cs), Some(true));
}

/// A grab bag captured before `<w:bCs>` was modeled (an older build's
/// crash-recovery snapshot) never writes a second `<w:bCs>` next to the
/// modeled one.
#[test]
fn a_stale_bag_twin_never_duplicates_the_modeled_child() {
    let mut style = bold(true).with_cs_twins();
    engine::GrabBag::push_into(&mut style.grab_bag, b"<w:bCs/>".to_vec());
    engine::GrabBag::push_into(&mut style.grab_bag, br#"<w:lang w:bidi="ar-SA"/>"#.to_vec());
    let mut out = String::new();
    emit_rpr(&style, &mut out);
    assert_eq!(
        out,
        r#"<w:rPr><w:b/><w:bCs/><w:lang w:bidi="ar-SA"/></w:rPr>"#
    );
}

/* ================================================================
Issue #249 — the complex-script font slot and regeneration fidelity.
================================================================ */

/// One run with `rpr` around `text`; returns the source document.xml and
/// the archive.
fn open_run(rpr: &str, text: &str) -> (String, DocxArchive) {
    open(&format!(
        r#"<w:p><w:r><w:rPr>{rpr}</w:rPr><w:t>{text}</w:t></w:r></w:p>"#
    ))
}

/// Restyle all of paragraph 0's `len` bytes with `patch` and return the
/// written document.xml.
fn restyle(archive: &DocxArchive, len: usize, patch: SpanStyle) -> String {
    let edited = archive.document.apply_style(at(0, 0), at(0, len), patch);
    let bytes = write_docx(archive, &edited).expect("write");
    crate::opc::archive::check_document_xml_well_formed(&bytes).expect("well-formed");
    document_xml_of(&bytes)
}

/// `w:cs` / `w:cstheme` read into the complex-script slot; `w:cs` alone
/// no longer becomes the Latin face; each slot writes back only what it
/// holds.
#[test]
fn rfonts_cs_slot_reads_apart_from_ascii() {
    let (_, both) = open_run(
        r#"<w:rFonts w:ascii="Times New Roman" w:hAnsi="Times New Roman" w:cs="Simplified Arabic" w:cstheme="minorBidi"/>"#,
        ARABIC,
    );
    let s = both.document.nth_paragraph(0).unwrap().style_at(0);
    assert_eq!(
        s.font_family.as_ref().map(|f| f.display_name()),
        Some("Times New Roman")
    );
    assert_eq!(
        s.font_family_cs.as_ref().map(|f| f.display_name()),
        Some("Simplified Arabic")
    );
    assert_eq!(s.font_theme, None);
    /* Issue #355 — `w:cstheme` is the `cs` slot's binding. */
    assert_eq!(
        s.font_bindings.as_deref().and_then(|b| b.cs.clone()),
        Some(engine::FontBinding::Theme("minorBidi".into()))
    );

    let (_, cs_only) = open_run(r#"<w:rFonts w:cs="Arial"/>"#, ARABIC);
    let s = cs_only.document.nth_paragraph(0).unwrap().style_at(0);
    assert_eq!(s.font_family, None, "w:cs alone is not the Latin face");
    assert_eq!(
        s.font_family_cs.as_ref().map(|f| f.display_name()),
        Some("Arial")
    );

    /* Engine-authored (no source): each slot writes its own attributes. */
    let mut out = String::new();
    emit_rpr(
        &SpanStyle {
            font_family: Some(engine::FontFamily::LiberationSans),
            font_family_cs: Some(engine::FontFamily::Amiri),
            ..SpanStyle::default()
        },
        &mut out,
    );
    assert_eq!(
        out,
        r#"<w:rPr><w:rFonts w:ascii="Liberation Sans" w:hAnsi="Liberation Sans" w:cs="Amiri"/></w:rPr>"#
    );
    let mut out = String::new();
    emit_rpr(
        &SpanStyle {
            font_family: Some(engine::FontFamily::LiberationSans),
            ..SpanStyle::default()
        },
        &mut out,
    );
    assert!(!out.contains("w:cs="), "no synthesized w:cs: {out}");
}

/// Issue #249 acceptance (`55733.docx`'s run): changing bold rewrites only
/// `<w:b>` — the source `<w:rFonts w:ascii="Cambria" />` gains no
/// `w:hAnsi` / `w:cs`, `<w:sz>` gains no `<w:szCs>`, and the spelling
/// (spaces before `/>`) is kept.
#[test]
fn changing_bold_rewrites_only_the_b_element() {
    let rpr = r#"<w:rFonts w:ascii="Cambria" /><w:b w:val="false" /><w:sz w:val="22" /><w:u w:val="none" />"#;
    let (src, archive) = open_run(rpr, "TEST");
    let xml = restyle(&archive, 4, bold(true));
    assert_eq!(
        xml,
        src.replace(r#"<w:b w:val="false" />"#, "<w:b/>"),
        "only <w:b> changes"
    );
    /* The UI toggle sets both slots: `<w:bCs/>` is an insertion. */
    let xml = restyle(&archive, 4, bold(true).with_cs_twins());
    assert_eq!(
        xml,
        src.replace(r#"<w:b w:val="false" />"#, "<w:b/><w:bCs/>")
    );
    /* A change elsewhere keeps `<w:b w:val="false" />` as written (it
    used to be dropped: the regenerated run turned bold in Word). */
    let xml = restyle(
        &archive,
        4,
        SpanStyle {
            color: Some([0xC0, 0, 0, 255]),
            ..SpanStyle::default()
        },
    );
    assert_eq!(
        xml,
        src.replace(
            r#"<w:sz w:val="22" />"#,
            r#"<w:color w:val="C00000"/><w:sz w:val="22" />"#
        )
    );
}

/// What the model reads as nothing still survives a regeneration:
/// `<w:rFonts w:hint="cs"/>` (ubiquitous in Arabic documents), `<w:color
/// w:val="auto"/>`, and a `<w:highlight>` the writer would otherwise
/// respell as `<w:shd>`.
#[test]
fn regeneration_keeps_children_the_model_reads_as_nothing() {
    let rpr =
        r#"<w:rFonts w:hint="cs"/><w:color w:val="auto"/><w:highlight w:val="yellow"/><w:rtl/>"#;
    let (src, archive) = open_run(rpr, ARABIC);
    let xml = restyle(&archive, ARABIC.len(), bold(true).with_cs_twins());
    assert_eq!(
        xml,
        src.replace(
            r#"<w:rFonts w:hint="cs"/>"#,
            r#"<w:rFonts w:hint="cs"/><w:b/><w:bCs/>"#
        )
    );
}

/// A CHANGED font keeps the `<w:rFonts>` attributes the model does not own
/// (`w:eastAsia`, `w:hint`); an engine-authored OFF is written explicitly.
#[test]
fn changed_value_keeps_unowned_attributes_and_off_is_explicit() {
    let rpr = r#"<w:rFonts w:ascii="Times New Roman" w:hAnsi="Times New Roman" w:eastAsia="SimSun" w:cs="Simplified Arabic" w:hint="eastAsia"/><w:b/><w:bCs/>"#;
    let (_, archive) = open_run(rpr, ARABIC);
    let xml = restyle(
        &archive,
        ARABIC.len(),
        SpanStyle {
            font_family: Some(engine::FontFamily::Amiri),
            bold: Some(false),
            ..SpanStyle::default()
        }
        .with_cs_twins(),
    );
    assert!(
        xml.contains(
            r#"<w:rPr><w:rFonts w:ascii="Amiri" w:hAnsi="Amiri" w:cs="Amiri" w:eastAsia="SimSun" w:hint="eastAsia"/><w:b w:val="0"/><w:bCs w:val="0"/></w:rPr>"#
        ),
        "{xml}"
    );
    let back = read_docx(&build_docx_with_styles(STYLES_XML, &xml)).unwrap();
    let s = back.document.nth_paragraph(0).unwrap().style_at(0);
    assert_eq!((s.bold, s.bold_cs), (Some(false), Some(false)));
    assert_eq!(s.font_family_cs, Some(engine::FontFamily::Amiri));
}
