//! Issue #556 — the DEFAULT paragraph style (`w:default="1"`, here
//! `Normal`: RTL, spacing, a run font + size) formats every paragraph
//! that names no style or an undefined one, on read; the writer stays
//! byte-neutral — a zero-edit save is byte-identical, an edited
//! paragraph regenerates to exactly source + insert, and a paragraph
//! regenerated with no source `<w:pPr>` does not spell what only
//! `Normal` gives it.

use super::*;
use crate::opc::archive::read_docx;
use crate::test_fixtures::{
    DEFAULT_STYLE_BODY, DEFAULT_STYLE_STYLES_XML, DEFAULT_STYLE_TEXTS, default_style_rtl_docx,
};
use engine::{BlockPath, LineHeight, LogicalPos, Spacing};

fn entry(bytes: &[u8], name: &str) -> String {
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).expect("zip");
    let mut f = zip.by_name(name).expect(name);
    let mut s = String::new();
    std::io::Read::read_to_string(&mut f, &mut s).expect("utf8");
    s
}

fn open() -> DocxArchive {
    read_docx(&default_style_rtl_docx()).expect("read fixture")
}

const NORMAL_SPACING: Spacing = Spacing {
    before_twips: 120,
    after_twips: 240,
};

type Dirs = Vec<(Option<TextDirection>, Option<TextDirection>)>;

/// `(resolved, direct)` direction of every paragraph.
fn dirs(doc: &DocumentTree) -> Dirs {
    (0..DEFAULT_STYLE_TEXTS.len() as u32)
        .map(|i| {
            let p = doc.nth_paragraph(i).expect("paragraph");
            (p.props.direction, p.direct_overrides.direction)
        })
        .collect()
}

const EXPECTED_DIRS: [(Option<TextDirection>, Option<TextDirection>); 6] = [
    (Some(TextDirection::Rtl), None),
    (Some(TextDirection::Rtl), None),
    (Some(TextDirection::Rtl), None),
    (Some(TextDirection::Rtl), None),
    (None, None),
    (Some(TextDirection::Ltr), Some(TextDirection::Ltr)),
];

#[test]
fn unstyled_paragraphs_read_through_the_default_style() {
    let archive = open();
    let doc = &archive.document;
    let texts: Vec<&str> = (0..6)
        .map(|i| doc.nth_paragraph(i).unwrap().text.as_str())
        .collect();
    assert_eq!(texts, DEFAULT_STYLE_TEXTS);
    assert_eq!(dirs(doc), EXPECTED_DIRS, "Normal's <w:bidi/>, not direct");
    let style_ids: Vec<Option<&str>> = (0..6)
        .map(|i| doc.nth_paragraph(i).unwrap().style_id.as_deref())
        .collect();
    assert_eq!(
        style_ids,
        [None, None, None, Some("Missing"), Some("Plain"), None],
        "the default style is applied, never written onto the paragraph"
    );
    for i in [0, 1, 2, 3, 5] {
        let p = doc.nth_paragraph(i).unwrap();
        assert_eq!(p.props.spacing, NORMAL_SPACING, "paragraph {i}");
        assert_eq!(p.props.line_height, Some(LineHeight::Auto { twips: 360 }));
        assert_eq!(p.direct_overrides.spacing, Spacing::default(), "not direct");
    }
    let plain = doc.nth_paragraph(4).unwrap();
    assert_eq!(plain.props.spacing.after_twips, 160, "docDefaults only");
    assert_eq!(
        plain.props.line_height,
        Some(LineHeight::Auto { twips: 259 })
    );

    assert!(doc.styles["Normal"].is_default);
    assert!(!doc.styles["Plain"].is_default);
    assert!(!doc.styles.contains_key("DefaultParagraphFont"));
    assert_eq!(
        doc.resolve_style_cascade(None).direction,
        Some(TextDirection::Rtl)
    );
    let run = doc.resolve_style_run_cascade(None);
    assert_eq!((run.font_size, run.font_size_cs), (Some(14.0), Some(16.0)));
    assert_eq!(
        doc.resolve_style_run_cascade(Some("Missing")).font_size,
        Some(14.0)
    );
    assert_eq!(
        doc.resolve_style_run_cascade(Some("Plain")).font_size,
        Some(11.0),
        "the docDefaults' size: Plain is not based on Normal"
    );
    /* Issue #29 — spans never bake the cascade. */
    let p0 = doc.nth_paragraph(0).unwrap();
    assert!(p0.spans.iter().all(|s| s.style.font_size.is_none()));
}

#[test]
fn a_zero_edit_save_is_byte_identical() {
    let source = default_style_rtl_docx();
    let archive = read_docx(&source).expect("read");
    let saved = write_docx(&archive, &archive.document).expect("write");
    assert_eq!(
        entry(&saved, "word/document.xml"),
        entry(&source, "word/document.xml")
    );
    assert_eq!(entry(&saved, "word/styles.xml"), DEFAULT_STYLE_STYLES_XML);
    /* The UI save path (the tree's own source package) too. */
    let ui = save_docx(&archive.document).expect("save");
    assert_eq!(
        entry(&ui, "word/document.xml"),
        entry(&source, "word/document.xml")
    );
}

/// Typing into the Latin-first unstyled paragraph regenerates it as
/// exactly source + insert: no `<w:bidi/>`, `<w:spacing>` or run font
/// from `Normal`; the re-read resolves the same directions.
#[test]
fn an_edited_unstyled_paragraph_gains_no_inherited_formatting() {
    let archive = open();
    let edited = archive.document.insert_text(
        LogicalPos {
            path: BlockPath::top(2),
            offset: "Latin".len() as u32,
        },
        " text",
    );
    let bytes = write_docx(&archive, &edited).expect("write");
    let xml = entry(&bytes, "word/document.xml");
    let source = entry(&default_style_rtl_docx(), "word/document.xml");
    assert_eq!(
        xml,
        source.replacen("Latin first", "Latin text first", 1),
        "source + insert"
    );
    let back = read_docx(&bytes).expect("re-read");
    assert_eq!(dirs(&back.document), EXPECTED_DIRS);
}

/// A paragraph regenerated with no source `<w:pPr>` (a pasted / minted
/// paragraph the engine cascaded) writes nothing `Normal` alone gives
/// it; what the paragraph set directly, or what differs from `Normal`,
/// is still written; the re-read resolves the same properties.
#[test]
fn a_fresh_unstyled_paragraph_writes_no_inherited_ppr() {
    let archive = open();
    let mut doc = archive.document.clone();
    let cascaded = |direct: ParaProperties| {
        let mut p = Paragraph {
            direct_overrides: direct,
            ..Default::default()
        };
        doc.style_cascade().recompute(&mut p, &doc.style_defaults);
        p
    };
    let fresh = Paragraph {
        text: "fresh".into(),
        ..cascaded(ParaProperties::default())
    };
    let direct_spacing = Paragraph {
        text: "direct".into(),
        ..cascaded(ParaProperties {
            spacing: NORMAL_SPACING,
            ..Default::default()
        })
    };
    let centred = Paragraph {
        text: "centred".into(),
        ..cascaded(ParaProperties::default())
    };
    let mut centred = centred;
    centred.props.alignment = Some(Alignment::Center);
    assert_eq!(fresh.props.direction, Some(TextDirection::Rtl));
    assert_eq!(fresh.props.spacing, NORMAL_SPACING);
    for p in [fresh, direct_spacing, centred] {
        doc.blocks.push_back(Block::Paragraph(p));
    }
    let bytes = write_docx(&archive, &doc).expect("write");
    let xml = entry(&bytes, "word/document.xml");
    assert!(
        xml.contains(r#"<w:p><w:r><w:t xml:space="preserve">fresh</w:t></w:r></w:p>"#),
        "no inherited pPr on the fresh paragraph: {xml}"
    );
    assert!(
        xml.contains(r#"<w:p><w:pPr><w:spacing w:before="120" w:after="240"/></w:pPr><w:r><w:t xml:space="preserve">direct</w:t>"#),
        "direct spacing equal to Normal's stays direct (its line rule does not): {xml}"
    );
    assert!(
        xml.contains(r#"<w:p><w:pPr><w:jc w:val="center"/></w:pPr><w:r><w:t xml:space="preserve">centred</w:t>"#),
        "a value Normal does not give is written: {xml}"
    );
    let source = entry(&default_style_rtl_docx(), "word/document.xml");
    assert_eq!(
        xml.split("<w:body>").next(),
        source.split("<w:body>").next(),
        "the prolog is untouched"
    );
    assert!(
        xml.contains(DEFAULT_STYLE_BODY),
        "the source paragraphs replay"
    );

    let back = read_docx(&bytes).expect("re-read").document;
    for i in 6..9 {
        let p = back.nth_paragraph(i).unwrap();
        assert_eq!(p.props.direction, Some(TextDirection::Rtl), "paragraph {i}");
        assert_eq!(p.props.spacing, NORMAL_SPACING, "paragraph {i}");
        assert_eq!(p.props.line_height, Some(LineHeight::Auto { twips: 360 }));
    }
    assert_eq!(
        back.nth_paragraph(8).unwrap().props.alignment,
        Some(Alignment::Center)
    );
}

/// A style table regenerated from the model (`build_styles_xml`: a
/// package without `styles.xml`) keeps the default claim, so the re-read
/// formats unstyled paragraphs with the same style.
#[test]
fn a_regenerated_style_table_keeps_the_default_claim() {
    let mut doc = open().document;
    doc.source_package = None;
    let xml = String::from_utf8(build_styles_xml(&doc)).expect("utf8");
    assert!(
        xml.contains(r#"<w:style w:type="paragraph" w:default="1" w:styleId="Normal">"#),
        "{xml}"
    );
    assert!(xml.contains(r#"<w:style w:type="paragraph" w:styleId="Plain">"#));
    doc.styles_dirty = true;
    let bytes = build_minimal_docx(&doc).expect("build");
    let back = read_docx(&bytes).expect("re-read").document;
    assert!(back.styles["Normal"].is_default);
    assert_eq!(
        back.nth_paragraph(1).unwrap().props.direction,
        Some(TextDirection::Rtl)
    );
    assert_eq!(back.nth_paragraph(4).unwrap().props.direction, None);
}
