//! Issue #355 — `<w:rFonts>` theme bindings through the writer: a
//! regenerated run writes the theme attributes slot by slot exactly as the
//! source bound them (never the face the theme currently names), a run
//! the user re-fonts drops the bindings its new family claims, and the
//! docDefaults of a regenerated `styles.xml` keep theirs.

use super::tests::{build_docx_with_styles, document_xml_of};
use super::*;
use crate::opc::archive::read_docx;
use engine::{BlockPath, FontBinding, LogicalPos};

const STYLES_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:asciiTheme="minorHAnsi" w:eastAsiaTheme="minorEastAsia" w:hAnsiTheme="minorHAnsi" w:cstheme="minorBidi"/><w:sz w:val="22"/></w:rPr></w:rPrDefault></w:docDefaults></w:styles>"#;

/// Word's own spelling for a theme-bound heading run: Latin bound to the
/// major font, complex script to `majorBidi` — two different references
/// on one element.
const RUN_RFONTS: &str =
    r#"<w:rFonts w:asciiTheme="majorHAnsi" w:hAnsiTheme="majorHAnsi" w:cstheme="majorBidi"/>"#;

fn document(body: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}<w:sectPr/></w:body></w:document>"#
    )
}

fn at(offset: u32) -> LogicalPos {
    LogicalPos::new(BlockPath::top(0), offset)
}

fn open() -> (String, DocxArchive) {
    let xml = document(&format!(
        "<w:p><w:r><w:rPr>{RUN_RFONTS}<w:b/></w:rPr><w:t>Heading</w:t></w:r></w:p>"
    ));
    let archive = read_docx(&build_docx_with_styles(STYLES_XML, &xml)).expect("read");
    (xml, archive)
}

#[test]
fn the_reader_records_each_slot_binding() {
    let (_, archive) = open();
    let style = archive.document.nth_paragraph(0).unwrap().style_at(0);
    let b = style.font_bindings.as_deref().expect("bindings");
    assert_eq!(b.ascii, Some(FontBinding::Theme("majorHAnsi".into())));
    assert_eq!(b.cs, Some(FontBinding::Theme("majorBidi".into())));
    assert_eq!(b.east_asia, None, "not mentioned");
    let d = archive.document.style_run_defaults.font_bindings.as_deref();
    assert_eq!(
        d.and_then(|b| b.east_asia.clone()),
        Some(FontBinding::Theme("minorEastAsia".into()))
    );
}

/// The canonical engine shape (names on ascii / hAnsi / cs, no theme)
/// carries no bindings, so an engine-authored family round-trips equal;
/// any other shape records the slots it mentions.
#[test]
fn the_canonical_name_shape_records_no_bindings() {
    for (rfonts, expect) in [
        (
            r#"<w:rFonts w:ascii="Arial" w:hAnsi="Arial" w:cs="Arial"/>"#,
            None,
        ),
        (r#"<w:rFonts w:hint="cs"/>"#, None),
        (
            r#"<w:rFonts w:ascii="Arial" w:hAnsi="Arial"/>"#,
            Some([Some(FontBinding::Name), Some(FontBinding::Name), None, None]),
        ),
        (
            r#"<w:rFonts w:ascii="Arial" w:hAnsi="Arial" w:cs="Arial" w:asciiTheme="minorHAnsi"/>"#,
            Some([
                Some(FontBinding::Theme("minorHAnsi".into())),
                Some(FontBinding::Name),
                None,
                Some(FontBinding::Name),
            ]),
        ),
    ] {
        let xml = document(&format!(
            "<w:p><w:r><w:rPr>{rfonts}</w:rPr><w:t>x</w:t></w:r></w:p>"
        ));
        let archive = read_docx(&build_docx_with_styles(STYLES_XML, &xml)).expect("read");
        let got = archive
            .document
            .nth_paragraph(0)
            .unwrap()
            .style_at(0)
            .font_bindings
            .map(|b| [b.ascii, b.h_ansi, b.east_asia, b.cs]);
        assert_eq!(got, expect, "{rfonts}");
    }
}

/// A formatting edit elsewhere in the element set regenerates the
/// `<w:rPr>`: the rFonts comes back with the SAME per-slot bindings (so it
/// adopts its source spelling), never `cstheme="majorHAnsi"` and never a
/// resolved family name.
#[test]
fn a_regenerated_run_keeps_its_slot_bindings() {
    let (xml, archive) = open();
    let italic = SpanStyle {
        italic: Some(true),
        ..Default::default()
    };
    let edited = archive.document.apply_style(at(0), at(7), italic);
    let out = document_xml_of(&write_docx(&archive, &edited).expect("write"));
    assert!(
        out.contains(RUN_RFONTS),
        "source rFonts adopted verbatim:\n{out}"
    );
    assert!(out.contains("<w:i/>"), "the edit landed:\n{out}");
    assert_ne!(out, xml);
    for absent in [
        r#"w:cstheme="majorHAnsi""#,
        "w:eastAsiaTheme",
        "w:ascii=",
        "Calibri",
    ] {
        assert!(!out.contains(absent), "{absent} synthesized:\n{out}");
    }
    let back = read_docx(&write_docx(&archive, &edited).expect("write")).expect("reread");
    assert_eq!(
        back.document
            .nth_paragraph(0)
            .unwrap()
            .style_at(0)
            .font_bindings,
        archive
            .document
            .nth_paragraph(0)
            .unwrap()
            .style_at(0)
            .font_bindings,
    );
}

/// The user picks a family: the slots it claims lose their theme binding
/// in the written run (Word would otherwise keep showing the theme face —
/// a theme attribute beats a name on the same element).
#[test]
fn a_new_family_drops_the_bindings_it_claims() {
    let (_, archive) = open();
    let amiri = SpanStyle {
        font_family: Some(engine::FontFamily::Amiri),
        ..Default::default()
    };
    let edited = archive.document.apply_style(at(0), at(7), amiri);
    let out = document_xml_of(&write_docx(&archive, &edited).expect("write"));
    assert!(
        out.contains(r#"<w:rFonts w:ascii="Amiri" w:hAnsi="Amiri" w:cs="Amiri"/>"#),
        "{out}"
    );
    assert!(!out.contains("Theme="), "no stale theme binding:\n{out}");
    assert!(!out.contains("w:cstheme"), "no stale theme binding:\n{out}");
}

/// A `styles.xml` regenerated from the tree (`ModifyStyle`) keeps the
/// docDefaults' four bindings as the source spelled them.
#[test]
fn regenerated_doc_defaults_keep_their_bindings() {
    let (_, archive) = open();
    let mut doc = archive.document.clone();
    doc.styles_dirty = true;
    let xml = String::from_utf8(build_styles_xml(&doc)).expect("utf8");
    assert!(
        xml.contains(r#"<w:rFonts w:asciiTheme="minorHAnsi" w:eastAsiaTheme="minorEastAsia" w:hAnsiTheme="minorHAnsi" w:cstheme="minorBidi"/>"#),
        "{xml}"
    );
}

/// A pre-#355 snapshot style (the legacy single `font_theme`, no
/// bindings) still writes its binding on the three slots it always did.
#[test]
fn a_legacy_single_binding_still_writes() {
    let legacy = SpanStyle {
        font_theme: Some("minorHAnsi".into()),
        ..Default::default()
    };
    let mut out = String::new();
    emit_rpr(&legacy, &mut out);
    assert_eq!(
        out,
        r#"<w:rPr><w:rFonts w:asciiTheme="minorHAnsi" w:hAnsiTheme="minorHAnsi" w:cstheme="minorHAnsi"/></w:rPr>"#
    );
}

/// Issue #355 — `<w:color w:themeColor w:themeShade>`: the reader keeps
/// the theme half next to the cached `w:val`; a regenerated run writes
/// both back as read; the colour picker writes a plain `w:val`.
#[test]
fn theme_colours_round_trip_and_yield_to_a_picked_colour() {
    const COLOR: &str = r#"<w:color w:val="2F5496" w:themeColor="accent1" w:themeShade="BF"/>"#;
    let xml = document(&format!(
        "<w:p><w:r><w:rPr><w:b/>{COLOR}</w:rPr><w:t>Heading</w:t></w:r></w:p>"
    ));
    let archive = read_docx(&build_docx_with_styles(STYLES_XML, &xml)).expect("read");
    let style = archive.document.nth_paragraph(0).unwrap().style_at(0);
    let t = style.color_theme.as_deref().expect("theme colour");
    assert_eq!(
        (t.color.as_str(), t.tint.as_deref(), t.shade.as_deref()),
        ("accent1", None, Some("BF"))
    );
    assert_eq!(style.color, Some([0x2F, 0x54, 0x96, 255]));

    let italic = SpanStyle {
        italic: Some(true),
        ..Default::default()
    };
    let edited = archive.document.apply_style(at(0), at(7), italic);
    let out = document_xml_of(&write_docx(&archive, &edited).expect("write"));
    assert!(out.contains(COLOR), "{out}");
    assert!(out.contains("<w:i/>"), "{out}");

    let red = SpanStyle {
        color: Some([200, 0, 0, 255]),
        ..Default::default()
    };
    let edited = archive.document.apply_style(at(0), at(7), red);
    let out = document_xml_of(&write_docx(&archive, &edited).expect("write"));
    assert!(out.contains(r#"<w:color w:val="C80000"/>"#), "{out}");
    assert!(!out.contains("themeColor"), "{out}");

    /* Only the theme half known (`w:val="auto"`) — still written. */
    let mut s = String::new();
    emit_rpr(
        &SpanStyle {
            color_theme: Some(Box::new(engine::ThemeColorRef {
                color: "text1".into(),
                tint: Some("A6".into()),
                shade: None,
            })),
            ..Default::default()
        },
        &mut s,
    );
    assert_eq!(
        s,
        r#"<w:rPr><w:color w:val="auto" w:themeColor="text1" w:themeTint="A6"/></w:rPr>"#
    );
}
