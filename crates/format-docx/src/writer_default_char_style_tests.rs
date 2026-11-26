//! Issue #558 — the DEFAULT CHARACTER style (`DefaultParagraphFont`,
//! here serif, bold, 16 pt) is mirrored into the engine
//! (`DocumentTree::default_char_style`) and formats every run naming no
//! character style at span-materialize time — never baked into a span, so
//! the writer stays byte-neutral: a zero-edit save is byte-identical, an
//! edited run regenerates to exactly source + insert, and a new run gains
//! no `<w:rPr>`.

use super::*;
use crate::opc::archive::read_docx;
use crate::test_fixtures::{
    DEFAULT_CHAR_BODY, DEFAULT_CHAR_STYLES_XML, DEFAULT_CHAR_TEXTS, default_char_style_docx,
};
use engine::{BlockPath, LogicalPos};

fn entry(bytes: &[u8], name: &str) -> String {
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).expect("zip");
    let mut f = zip.by_name(name).expect(name);
    let mut s = String::new();
    std::io::Read::read_to_string(&mut f, &mut s).expect("utf8");
    s
}

fn open() -> DocxArchive {
    read_docx(&default_char_style_docx()).expect("read fixture")
}

/// `(bold, italic, Latin size)` text in paragraph `i` resolves to at byte
/// `at` — the engine's one text-style fold.
fn effective(doc: &DocumentTree, i: u32, at: u32) -> (Option<bool>, Option<bool>, Option<f32>) {
    let p = doc.nth_paragraph(i).expect("paragraph");
    let own = p.spans.iter().find(|s| s.start <= at && at < s.end);
    let s = doc.resolve_text_style(p.style_id.as_deref(), own.map(|s| &s.style));
    (s.bold, s.italic, s.font_size)
}

#[test]
fn the_default_character_style_is_mirrored_never_baked() {
    let archive = open();
    let doc = &archive.document;
    let texts: Vec<&str> = (0..6)
        .map(|i| doc.nth_paragraph(i).unwrap().text.as_str())
        .collect();
    assert_eq!(texts, DEFAULT_CHAR_TEXTS);
    let dc = doc
        .default_char_style
        .as_ref()
        .expect("DefaultParagraphFont mirrored");
    assert_eq!((dc.bold, dc.font_size), (Some(true), Some(16.0)));
    assert!(
        dc.font_family.is_some() || dc.raw_font_family.is_some(),
        "the serif face: {dc:?}"
    );
    assert_eq!(dc.char_style, None);

    /* The plain run and the empty paragraph carry no span at all; the
    direct `<w:b w:val="0"/>` run carries only that. */
    assert!(doc.nth_paragraph(0).unwrap().spans.is_empty());
    let direct = &doc.nth_paragraph(3).unwrap().spans[0].style;
    assert_eq!(
        (direct.bold, direct.font_size, direct.char_style.as_deref()),
        (Some(false), None, None)
    );
    /* A named character style still folds its own chain (as before). */
    let quiet = &doc.nth_paragraph(1).unwrap().spans[0].style;
    assert_eq!(
        (quiet.char_style.as_deref(), quiet.italic, quiet.bold),
        (Some("Quiet"), Some(true), None)
    );
    let loud = &doc.nth_paragraph(2).unwrap().spans[0].style;
    assert_eq!(
        (loud.char_style.as_deref(), loud.bold, loud.font_size),
        (Some("Loud"), Some(true), Some(16.0)),
        "Loud is based on DefaultParagraphFont"
    );

    assert_eq!(effective(doc, 0, 0), (Some(true), None, Some(16.0)));
    assert_eq!(
        effective(doc, 1, 0),
        (None, Some(true), Some(11.0)),
        "a run naming another character style takes no default one"
    );
    assert_eq!(effective(doc, 2, 0), (Some(true), None, Some(16.0)));
    assert_eq!(effective(doc, 3, 0), (Some(false), None, Some(16.0)));
    assert_eq!(
        effective(doc, 5, 0),
        (Some(true), Some(true), Some(16.0)),
        "over the paragraph style's run formatting"
    );
}

#[test]
fn a_document_without_a_formatted_default_character_style_mirrors_none() {
    /* Word's own `DefaultParagraphFont` carries no `<w:rPr>`. */
    let archive = read_docx(&crate::test_fixtures::default_style_rtl_docx()).expect("read");
    assert_eq!(archive.document.default_char_style, None);
}

#[test]
fn a_zero_edit_save_is_byte_identical() {
    let source = default_char_style_docx();
    let archive = read_docx(&source).expect("read");
    let saved = write_docx(&archive, &archive.document).expect("write");
    assert_eq!(
        entry(&saved, "word/document.xml"),
        entry(&source, "word/document.xml")
    );
    assert_eq!(entry(&saved, "word/styles.xml"), DEFAULT_CHAR_STYLES_XML);
    let ui = save_docx(&archive.document).expect("save");
    assert_eq!(
        entry(&ui, "word/document.xml"),
        entry(&source, "word/document.xml")
    );
}

/// Typing into the plain run and into the direct-formatted one
/// regenerates each as exactly source + insert: no `<w:b/>`, `<w:sz>` or
/// serif `<w:rFonts>` from the default character style.
#[test]
fn edited_runs_gain_no_inherited_run_properties() {
    let archive = open();
    let at = |block: u32, offset: usize| LogicalPos {
        path: BlockPath::top(block),
        offset: offset as u32,
    };
    let edited = archive
        .document
        .insert_text(at(0, "Plain".len()), " text")
        .insert_text(at(3, "Direct".len()), " direct");
    let bytes = write_docx(&archive, &edited).expect("write");
    let xml = entry(&bytes, "word/document.xml");
    let source = entry(&default_char_style_docx(), "word/document.xml");
    assert_eq!(
        xml,
        source.replacen("Plain run", "Plain text run", 1).replacen(
            "Direct formatting",
            "Direct direct formatting",
            1
        ),
        "source + insert"
    );
    let back = read_docx(&bytes).expect("re-read").document;
    assert_eq!(back.default_char_style, archive.document.default_char_style);
    assert_eq!(effective(&back, 0, 0), (Some(true), None, Some(16.0)));
}

/// Enter at the end of the plain paragraph and typing into the new one:
/// the new run writes no `<w:rPr>`, and it re-reads in the default
/// character style.
#[test]
fn a_new_run_writes_no_run_properties() {
    let archive = open();
    let end = DEFAULT_CHAR_TEXTS[0].len() as u32;
    let split = archive.document.split_paragraph(LogicalPos {
        path: BlockPath::top(0),
        offset: end,
    });
    let typed = split.insert_text(
        LogicalPos {
            path: BlockPath::top(1),
            offset: 0,
        },
        "New",
    );
    assert_eq!(effective(&typed, 1, 0), (Some(true), None, Some(16.0)));
    let bytes = write_docx(&archive, &typed).expect("write");
    let xml = entry(&bytes, "word/document.xml");
    assert!(
        xml.contains(r#"<w:p><w:r><w:t xml:space="preserve">New</w:t></w:r></w:p>"#),
        "{xml}"
    );
    for leaked in ["<w:b/>", "<w:sz ", "Liberation Serif"] {
        assert!(!xml.contains(leaked), "`{leaked}` leaked: {xml}");
    }
    assert!(
        xml.contains(
            &DEFAULT_CHAR_BODY[DEFAULT_CHAR_BODY
                .find("<w:p><w:r><w:rPr><w:rStyle w:val=\"Quiet\"")
                .unwrap()..]
        )
    );
    let back = read_docx(&bytes).expect("re-read").document;
    assert_eq!(back.nth_paragraph(1).unwrap().text, "New");
    assert_eq!(effective(&back, 1, 0), (Some(true), None, Some(16.0)));
}
