//! Issue #371 — `ModifyStyle` patches `styles.xml` instead of regenerating
//! it: the edited style element changes, every other byte (character,
//! table and numbering styles, `<w:latentStyles>`, `<w:docDefaults>`, the
//! unmodeled children of the edited style itself) stays.

use super::*;
use crate::opc::archive::read_docx;
use crate::test_fixtures::{STYLES_PATCH_XML, styles_patch_docx};
use engine::{Alignment, ParaProperties, SpanStyle};

fn styles_xml_of(bytes: &[u8]) -> String {
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
    let mut f = zip.by_name("word/styles.xml").unwrap();
    let mut s = String::new();
    std::io::Read::read_to_string(&mut f, &mut s).unwrap();
    s
}

fn open() -> DocxArchive {
    read_docx(&styles_patch_docx()).expect("read fixture")
}

fn save_styles(archive: &DocxArchive, doc: &engine::DocumentTree) -> (Vec<u8>, String) {
    let bytes = write_docx(archive, doc).expect("write");
    let styles = styles_xml_of(&bytes);
    (bytes, styles)
}

/// A run-property change on `Heading 1` rewrites exactly its `<w:sz>`:
/// the theme fonts, the theme colour, `<w:kern>`, `<w:uiPriority>`,
/// `<w:rsid>`, every other style and `<w:latentStyles>` stay.
#[test]
fn modify_style_rewrites_only_the_edited_child() {
    let archive = open();
    let bigger = SpanStyle {
        font_size: Some(20.0),
        ..Default::default()
    };
    let doc = archive
        .document
        .modify_style("Heading1", None, Some(bigger), None, None);
    let (bytes, out) = save_styles(&archive, &doc);
    assert_eq!(
        out,
        STYLES_PATCH_XML.replacen(r#"<w:sz w:val="32"/>"#, r#"<w:sz w:val="40"/>"#, 1)
    );
    let back = read_docx(&bytes).expect("re-read");
    assert_eq!(
        back.document
            .resolve_style_run_cascade(Some("Heading1"))
            .font_size,
        Some(20.0)
    );
}

/// A paragraph-property change splices the style's `<w:pPr>`; a rename
/// rewrites only `<w:name>`; an untouched document keeps the part.
#[test]
fn ppr_change_and_rename_touch_one_child_each() {
    let archive = open();
    let (_, zero) = save_styles(&archive, &archive.document);
    assert_eq!(zero, STYLES_PATCH_XML, "zero-edit identity");

    let centered = ParaProperties {
        alignment: Some(Alignment::Center),
        ..Default::default()
    };
    let doc = archive
        .document
        .modify_style("Quote2", Some(centered), None, None, None);
    let (_, out) = save_styles(&archive, &doc);
    assert_eq!(
        out,
        STYLES_PATCH_XML.replacen(
            r#"<w:ind w:left="720"/></w:pPr>"#,
            r#"<w:ind w:left="720"/><w:jc w:val="center"/></w:pPr>"#,
            1
        )
    );

    let doc = archive
        .document
        .modify_style("Heading1", None, None, None, Some("Chapter".into()));
    let (_, out) = save_styles(&archive, &doc);
    assert_eq!(
        out,
        STYLES_PATCH_XML.replacen(
            r#"<w:name w:val="heading 1"/>"#,
            r#"<w:name w:val="Chapter"/>"#,
            1
        )
    );
}

/// A style the source does not have is appended before `</w:styles>`;
/// nothing else moves.
#[test]
fn a_new_style_is_appended() {
    let archive = open();
    let mut doc = archive.document.clone();
    doc.styles.insert(
        "TOC1".into(),
        engine::ParagraphStyle {
            id: "TOC1".into(),
            name: "toc 1".into(),
            based_on: Some("Normal".into()),
            ..Default::default()
        },
    );
    doc.styles_dirty = true;
    let (_, out) = save_styles(&archive, &doc);
    assert_eq!(
        out,
        STYLES_PATCH_XML.replacen(
            "</w:styles>",
            r#"<w:style w:type="paragraph" w:styleId="TOC1"><w:name w:val="toc 1"/><w:basedOn w:val="Normal"/></w:style></w:styles>"#,
            1
        )
    );
}
