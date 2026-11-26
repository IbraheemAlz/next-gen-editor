//! Issue #560 — a paragraph the ENGINE mints (a table's cells and escape
//! paragraph, a new row, a note body, …) carries its style cascade in
//! `props` (`StyleCascade::new_paragraph`: docDefaults + `Normal`, or its
//! own style), so it lays out now as it will after save + reopen. The
//! writer stays byte-neutral: a regenerated `<w:pPr>` with no source to
//! splice into leaves out everything the paragraph only inherits
//! (`without_inherited`, the #431 rule for every link of the cascade —
//! the docDefaults included, not only `Normal`), so a minted paragraph is
//! written exactly as before #560 and re-reads to the same properties.

use super::*;
use crate::opc::archive::read_docx;
use crate::test_fixtures::{
    DEFAULT_CHAR_TEXTS, DEFAULT_STYLE_TEXTS, default_char_style_docx, default_style_rtl_docx,
};
use engine::{BlockPath, PathStep};

fn entry(bytes: &[u8], name: &str) -> String {
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).expect("zip");
    let mut f = zip.by_name(name).expect(name);
    let mut s = String::new();
    std::io::Read::read_to_string(&mut f, &mut s).expect("utf8");
    s
}

fn cell(table: u32, row: u32, col: u32) -> BlockPath {
    BlockPath::top(table)
        .push(PathStep::Cell { row, col })
        .push(PathStep::Block(0))
}

/// From the first `<w:tbl>` through the paragraph after it.
fn table_and_escape(xml: &str) -> &str {
    let from = xml.find("<w:tbl>").expect("a table");
    let after = from + xml[from..].find("</w:tbl>").expect("table end") + "</w:tbl>".len();
    let end = after + xml[after..].find("</w:p>").expect("escape paragraph") + "</w:p>".len();
    &xml[from..end]
}

/// The same edit on a copy of `doc` whose style table and docDefaults are
/// gone: the cascade is empty, so its minted paragraphs are what the
/// engine minted before #560 (`Paragraph::default()`).
fn uncascaded(doc: &DocumentTree) -> DocumentTree {
    let mut bare = doc.clone();
    bare.styles = Default::default();
    bare.style_defaults = Default::default();
    bare
}

/// A table inserted at the end of each fixture: its cells and escape
/// paragraph carry the cascade in the engine, are written byte for byte
/// as before #560 (no `<w:pPr>` at all — the docDefaults' spacing of the
/// `Normal`-empty fixture included) and re-read to the same properties.
#[test]
fn an_inserted_table_writes_no_inherited_ppr_and_rereads_the_same() {
    for (name, fixture, paragraphs) in [
        (
            "default_style_rtl",
            default_style_rtl_docx(),
            DEFAULT_STYLE_TEXTS.len(),
        ),
        (
            "default_char_style",
            default_char_style_docx(),
            DEFAULT_CHAR_TEXTS.len(),
        ),
    ] {
        let archive = read_docx(&fixture).expect("read");
        let at = BlockPath::top(paragraphs as u32);
        let doc = archive.document.insert_table(at.clone(), 2, 2);
        let cascade = doc.resolve_style_cascade(None);
        assert_ne!(cascade, ParaProperties::default(), "{name}: a cascade");
        let minted = doc
            .paragraph_at_path(&cell(paragraphs as u32, 1, 1))
            .expect("cell paragraph");
        assert_eq!(minted.props, cascade, "{name}: the cell takes the cascade");

        let xml = entry(
            &write_docx(&archive, &doc).expect("write"),
            "word/document.xml",
        );
        let before = entry(
            &write_docx(
                &archive,
                &uncascaded(&archive.document).insert_table(at, 2, 2),
            )
            .expect("write"),
            "word/document.xml",
        );
        assert_eq!(
            table_and_escape(&xml),
            table_and_escape(&before),
            "{name}: minted paragraphs are written as before #560"
        );
        assert!(!table_and_escape(&xml).contains("<w:pPr>"), "{name}: {xml}");

        let back = read_docx(&write_docx(&archive, &doc).expect("write"))
            .expect("re-read")
            .document;
        for row in 0..2 {
            for col in 0..2 {
                let path = cell(paragraphs as u32, row, col);
                assert_eq!(
                    back.paragraph_at_path(&path).map(|p| &p.props),
                    doc.paragraph_at_path(&path).map(|p| &p.props),
                    "{name}: cell ({row}, {col}) re-reads the same"
                );
            }
        }
        let escape = BlockPath::top(paragraphs as u32 + 1);
        assert_eq!(
            back.paragraph_at_path(&escape).map(|p| &p.props),
            doc.paragraph_at_path(&escape).map(|p| &p.props),
            "{name}: the escape paragraph re-reads the same"
        );
    }
}

/// A minted paragraph in a NAMED style (a note body's `FootnoteText`, a
/// paragraph switched to a style) writes only its `<w:pStyle>`: the
/// style's own properties stay on `styles.xml`.
#[test]
fn a_minted_styled_paragraph_writes_only_its_style() {
    let archive = read_docx(&default_style_rtl_docx()).expect("read");
    let mut doc = archive.document.clone();
    let plain = Paragraph {
        text: "minted in Plain".into(),
        ..doc.new_paragraph_in_style(Some("Plain".into()))
    };
    assert_eq!(plain.props, doc.resolve_style_cascade(Some("Plain")));
    assert_eq!(plain.props.spacing.after_twips, 160, "the docDefaults'");
    doc.blocks.push_back(Block::Paragraph(plain.clone()));
    let bytes = write_docx(&archive, &doc).expect("write");
    let xml = entry(&bytes, "word/document.xml");
    assert!(
        xml.contains(r#"<w:p><w:pPr><w:pStyle w:val="Plain"/></w:pPr><w:r><w:t xml:space="preserve">minted in Plain</w:t>"#),
        "{xml}"
    );
    let back = read_docx(&bytes).expect("re-read").document;
    let last = back
        .nth_paragraph(DEFAULT_STYLE_TEXTS.len() as u32)
        .unwrap();
    assert_eq!(last.props, plain.props);
}

/// A paragraph regenerated with no source `<w:pPr>` (pasted, rebuilt)
/// writes what it sets directly and nothing it inherits — the
/// docDefaults' spacing of a `Normal`-empty document included (#431).
#[test]
fn a_sourceless_paragraph_writes_only_its_direct_formatting() {
    let archive = read_docx(&default_char_style_docx()).expect("read");
    let mut doc = archive.document.clone();
    let mut pasted = doc.nth_paragraph(0).unwrap().clone();
    pasted.source_xml = None;
    pasted.source_markup = None;
    pasted.dirty = true;
    pasted.text = "pasted".into();
    pasted.direct_overrides.alignment = Some(engine::Alignment::Center);
    doc.style_cascade()
        .recompute(&mut pasted, &doc.style_defaults);
    assert_eq!(pasted.props.spacing.after_twips, 160, "inherited");
    doc.blocks.push_back(Block::Paragraph(pasted.clone()));
    let bytes = write_docx(&archive, &doc).expect("write");
    let xml = entry(&bytes, "word/document.xml");
    assert!(
        xml.contains(r#"<w:p><w:pPr><w:jc w:val="center"/></w:pPr><w:r><w:t xml:space="preserve">pasted</w:t>"#),
        "{xml}"
    );
    let back = read_docx(&bytes).expect("re-read").document;
    let last = back.nth_paragraph(DEFAULT_CHAR_TEXTS.len() as u32).unwrap();
    assert_eq!(last.props, pasted.props);
}
