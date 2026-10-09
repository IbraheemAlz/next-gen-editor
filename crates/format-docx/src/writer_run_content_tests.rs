//! Issue #335 — run-content elements with no text of their own: the
//! reader maps `<w:softHyphen/>` / `<w:noBreakHyphen/>` onto U+00AD /
//! U+2011 in the paragraph text, an untouched paragraph saves
//! byte-identical, and a regenerated paragraph re-emits the ELEMENTS
//! (never the raw characters, which Word treats differently).

use super::tests::{build_docx_with_styles, document_xml_of};
use super::*;
use crate::opc::archive::read_docx;
use engine::run_content::{NON_BREAKING_HYPHEN, SOFT_HYPHEN};
use engine::{BlockPath, LogicalPos};

const STYLES_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:docDefaults><w:rPrDefault><w:rPr><w:sz w:val="22"/></w:rPr></w:rPrDefault></w:docDefaults></w:styles>"#;

fn document(body: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}<w:sectPr/></w:body></w:document>"#
    )
}

fn at(offset: usize) -> LogicalPos {
    LogicalPos::new(BlockPath::top(0), offset as u32)
}

/// Word's shape: the soft hyphen inside the run that carries the word,
/// the non-breaking hyphen in a run of its own between two runs.
const HYPHEN_BODY: &str = concat!(
    r#"<w:p><w:r w:rsidR="00A1"><w:t>Extra</w:t><w:softHyphen/>"#,
    r#"<w:t>ordinary e</w:t></w:r><w:r><w:noBreakHyphen/></w:r>"#,
    r#"<w:r><w:t>mail</w:t></w:r></w:p>"#,
);

fn open(body: &str) -> (String, DocxArchive) {
    let xml = document(body);
    let archive = read_docx(&build_docx_with_styles(STYLES_XML, &xml)).expect("read");
    (xml, archive)
}

#[test]
fn the_reader_maps_both_hyphen_elements_into_the_text() {
    let (_, archive) = open(HYPHEN_BODY);
    let p = archive.document.nth_paragraph(0).unwrap();
    assert_eq!(
        p.text,
        format!("Extra{SOFT_HYPHEN}ordinary e{NON_BREAKING_HYPHEN}mail")
    );
}

#[test]
fn a_zero_edit_save_is_byte_identical() {
    let (xml, archive) = open(HYPHEN_BODY);
    let out = document_xml_of(&write_docx(&archive, &archive.document).expect("write"));
    assert_eq!(out, xml);
}

/// Typing at the end regenerates the paragraph: both elements come back
/// as elements in their source runs, the raw characters never reach a
/// `<w:t>`, and the source bytes survive as a pure insertion.
#[test]
fn a_regenerated_paragraph_re_emits_the_elements() {
    let (xml, archive) = open(HYPHEN_BODY);
    let len = archive.document.nth_paragraph(0).unwrap().text.len();
    let edited = archive.document.insert_text(at(len), " today");
    let out = document_xml_of(&write_docx(&archive, &edited).expect("write"));
    assert!(!out.contains(SOFT_HYPHEN), "raw U+00AD written:\n{out}");
    assert!(
        !out.contains(NON_BREAKING_HYPHEN),
        "raw U+2011 written:\n{out}"
    );
    let want = concat!(
        r#"<w:r w:rsidR="00A1"><w:t>Extra</w:t><w:softHyphen/>"#,
        r#"<w:t>ordinary e</w:t></w:r><w:r><w:noBreakHyphen/></w:r>"#,
        r#"<w:r><w:t>mail today</w:t></w:r>"#,
    );
    assert!(out.contains(want), "elements in their source runs:\n{out}");
    let back = read_docx(&write_docx(&archive, &edited).expect("write")).expect("reread");
    assert_eq!(
        back.document.nth_paragraph(0).unwrap().text,
        edited.nth_paragraph(0).unwrap().text
    );
    assert_ne!(out, xml);
}

/// A run holding nothing but a soft hyphen is a text run now (it used to
/// be kept as an opaque marker): typing elsewhere still re-emits it.
#[test]
fn a_run_of_only_a_soft_hyphen_regenerates_as_the_element() {
    let (_, archive) = open(
        r#"<w:p><w:r><w:t>co</w:t></w:r><w:r><w:softHyphen/></w:r><w:r><w:t>operate</w:t></w:r></w:p>"#,
    );
    let p = archive.document.nth_paragraph(0).unwrap();
    assert_eq!(p.text, format!("co{SOFT_HYPHEN}operate"));
    let edited = archive.document.insert_text(at(0), "Re-");
    let out = document_xml_of(&write_docx(&archive, &edited).expect("write"));
    assert!(
        out.contains("<w:r><w:softHyphen/></w:r>"),
        "the element's own run:\n{out}"
    );
}

/// Engine-authored text (a paste, the minimal writer) spells the
/// characters as the elements too.
#[test]
fn engine_authored_hyphen_characters_write_as_elements() {
    let doc = engine::DocumentTree::from_text(&format!("a{SOFT_HYPHEN}b{NON_BREAKING_HYPHEN}c"));
    let out = document_xml_of(&build_minimal_docx(&doc).expect("write"));
    assert!(out.contains("<w:softHyphen/>"), "{out}");
    assert!(out.contains("<w:noBreakHyphen/>"), "{out}");
    assert!(!out.contains(SOFT_HYPHEN) && !out.contains(NON_BREAKING_HYPHEN));
    let back = read_docx(&build_minimal_docx(&doc).expect("write")).expect("reread");
    assert_eq!(
        back.document.nth_paragraph(0).unwrap().text,
        doc.nth_paragraph(0).unwrap().text
    );
}

/// Issue #350 × #335 — inside a field's instruction a hyphen element is
/// code, never visible text.
#[test]
fn a_hyphen_element_in_field_code_stays_hidden() {
    let (_, archive) = open(concat!(
        r#"<w:p><w:r><w:t>a</w:t></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r>"#,
        r#"<w:r><w:instrText xml:space="preserve"> MERGEFIELD x</w:instrText><w:softHyphen/>"#,
        r#"<w:noBreakHyphen/></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r>"#,
        r#"<w:r><w:t>v</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p>"#,
    ));
    assert_eq!(archive.document.nth_paragraph(0).unwrap().text, "av");
}

/* ---- issue #357: w:sym, w:cr, w:ptab, w:bdo / w:dir ---------------- */

/// Word's shapes: a Symbol-font α in a run of its own, a Wingdings check
/// in a run carrying the symbol font, a `<w:cr/>` inside a text run.
const SYM_CR_P: &str = concat!(
    r#"<w:p><w:r><w:t xml:space="preserve">Greek </w:t></w:r>"#,
    r#"<w:r><w:sym w:font="Symbol" w:char="F061"/></w:r>"#,
    r#"<w:r><w:t xml:space="preserve"> and a check </w:t></w:r>"#,
    r#"<w:r><w:rPr><w:rFonts w:ascii="Wingdings" w:hAnsi="Wingdings"/></w:rPr><w:sym w:font="Wingdings" w:char="F0FC"/></w:r>"#,
    r#"<w:r><w:t>line one</w:t><w:cr/><w:t>line two</w:t></w:r></w:p>"#,
);

/// A header-style three-column line: two positional tabs.
const PTAB_P: &str = concat!(
    r#"<w:p><w:r><w:t>Left</w:t></w:r>"#,
    r#"<w:r><w:ptab w:relativeTo="margin" w:alignment="center" w:leader="none"/></w:r>"#,
    r#"<w:r><w:t>Middle</w:t></w:r>"#,
    r#"<w:r><w:ptab w:relativeTo="margin" w:alignment="right" w:leader="dot"/></w:r>"#,
    r#"<w:r><w:t>Right</w:t></w:r></w:p>"#,
);

/// An override and an embedding.
const BDO_DIR_P: &str = concat!(
    r#"<w:p><w:r><w:t xml:space="preserve">Forced: </w:t></w:r>"#,
    r#"<w:bdo w:val="rtl"><w:r><w:t>abc</w:t></w:r></w:bdo>"#,
    r#"<w:r><w:t xml:space="preserve"> embedded: </w:t></w:r>"#,
    r#"<w:dir w:val="rtl"><w:r><w:t>XYZ 123</w:t></w:r></w:dir></w:p>"#,
);

#[test]
fn the_reader_models_sym_cr_ptab_bdo_and_dir() {
    use engine::run_content::{
        CARRIAGE_RETURN, POP_DIRECTIONAL, PTabAlignment, PTabLeader, PTabRelativeTo, RLE, RLO,
    };
    let (_, archive) = open(&format!("{SYM_CR_P}{PTAB_P}{BDO_DIR_P}"));
    let doc = &archive.document;
    let p0 = doc.nth_paragraph(0).unwrap();
    assert_eq!(
        p0.text,
        format!("Greek \u{FFFC} and a check \u{FFFC}line one{CARRIAGE_RETURN}line two")
    );
    let kinds: Vec<_> = p0.inline_objects.iter().map(|o| o.kind.clone()).collect();
    assert_eq!(
        kinds,
        vec![
            engine::InlineKind::Symbol {
                font: "Symbol".into(),
                char: "F061".into()
            },
            engine::InlineKind::Symbol {
                font: "Wingdings".into(),
                char: "F0FC".into()
            },
        ]
    );
    let p1 = doc.nth_paragraph(1).unwrap();
    assert_eq!(p1.text, "Left\u{FFFC}Middle\u{FFFC}Right");
    assert_eq!(
        p1.inline_objects[1].kind,
        engine::InlineKind::PositionalTab {
            alignment: PTabAlignment::Right,
            relative_to: PTabRelativeTo::Margin,
            leader: PTabLeader::Dot,
        }
    );
    let p2 = doc.nth_paragraph(2).unwrap();
    assert_eq!(
        p2.text,
        format!("Forced: {RLO}abc{POP_DIRECTIONAL} embedded: {RLE}XYZ 123{POP_DIRECTIONAL}")
    );
}

#[test]
fn a_zero_edit_save_of_every_element_is_byte_identical() {
    let (xml, archive) = open(&format!("{SYM_CR_P}{PTAB_P}{BDO_DIR_P}"));
    let out = document_xml_of(&write_docx(&archive, &archive.document).expect("write"));
    assert_eq!(out, xml);
}

/// Typing at the end of each paragraph regenerates it: every element
/// comes back as itself (never a U+FFFC, a raw CR or a bidi control), in
/// its source run, as a pure insertion of the typed text.
#[test]
fn regenerated_paragraphs_re_emit_every_element() {
    for (i, p) in [SYM_CR_P, PTAB_P, BDO_DIR_P].into_iter().enumerate() {
        let (xml, archive) = open(p);
        let len = archive.document.nth_paragraph(0).unwrap().text.len();
        let edited = archive.document.insert_text(at(len), "!");
        let out = document_xml_of(&write_docx(&archive, &edited).expect("write"));
        for raw in [
            '\u{FFFC}', '\r', '\u{202A}', '\u{202B}', '\u{202C}', '\u{202D}', '\u{202E}',
        ] {
            assert!(
                !out.contains(raw),
                "paragraph {i}: raw {raw:?} written:\n{out}"
            );
        }
        /* A pure insertion: the source bytes survive around one inserted
        region (the typed `!` — in the last `<w:t>`, or, after the
        `<w:dir>`'s pop, in a run of its own outside the wrapper). */
        let prefix = xml
            .bytes()
            .zip(out.bytes())
            .take_while(|(a, b)| a == b)
            .count();
        let suffix = xml
            .bytes()
            .rev()
            .zip(out.bytes().rev())
            .take(xml.len().min(out.len()) - prefix)
            .take_while(|(a, b)| a == b)
            .count();
        assert_eq!(
            prefix + suffix,
            xml.len(),
            "paragraph {i}: source bytes rewritten:\n{out}"
        );
        assert!(out[prefix..out.len() - suffix].contains('!'));
        let back = read_docx(&write_docx(&archive, &edited).expect("write")).expect("reread");
        let b = back.document.nth_paragraph(0).unwrap();
        let e = edited.nth_paragraph(0).unwrap();
        assert_eq!(b.text, e.text, "paragraph {i}");
        assert_eq!(b.inline_objects.len(), e.inline_objects.len());
    }
}

/// Engine-authored bidi controls (a paste) write as the wrappers; an
/// orphaned pop is dropped and an unclosed opener closes at the
/// paragraph end — the output is always balanced.
#[test]
fn bidi_controls_write_as_balanced_wrappers() {
    for (text, want) in [
        (
            "a\u{202E}bc\u{202C}d",
            r#"<w:bdo w:val="rtl"><w:r><w:t xml:space="preserve">bc</w:t></w:r></w:bdo>"#,
        ),
        (
            "a\u{202B}b\u{202D}c\u{202C}d\u{202C}",
            r#"<w:dir w:val="rtl"><w:r><w:t xml:space="preserve">b</w:t></w:r><w:bdo w:val="ltr"><w:r><w:t xml:space="preserve">c</w:t></w:r></w:bdo><w:r><w:t xml:space="preserve">d</w:t></w:r></w:dir>"#,
        ),
        (
            "x\u{202C}y",
            r#"<w:r><w:t xml:space="preserve">y</w:t></w:r>"#,
        ),
        (
            "x\u{202A}y",
            r#"<w:dir w:val="ltr"><w:r><w:t xml:space="preserve">y</w:t></w:r></w:dir>"#,
        ),
    ] {
        let doc = engine::DocumentTree::from_text(text);
        let bytes = build_minimal_docx(&doc).expect("write");
        let out = document_xml_of(&bytes);
        assert!(out.contains(want), "{text:?}:\n{out}");
        crate::check_document_xml_well_formed(&bytes).expect("balanced");
        assert_eq!(
            out.matches("<w:dir").count(),
            out.matches("</w:dir>").count()
        );
        assert_eq!(
            out.matches("<w:bdo").count(),
            out.matches("</w:bdo>").count()
        );
    }
}

/* ---- issue #326: read-only hyphenation properties ------------------ */

/// `<w:suppressAutoHyphens>` follows the `widowControl` contract: a
/// regenerated `styles.xml` writes a style's value, a paragraph never
/// bakes a (style-inherited) value in — its own element rides the bag.
#[test]
fn suppress_auto_hyphens_emits_on_styles_not_on_paragraphs() {
    let mut doc = engine::DocumentTree::default();
    doc.styles.insert(
        "Body".into(),
        engine::ParagraphStyle {
            id: "Body".into(),
            name: "Body".into(),
            para: ParaProperties {
                suppress_auto_hyphens: Some(true),
                ..Default::default()
            },
            ..Default::default()
        },
    );
    let xml = String::from_utf8(build_styles_xml(&doc)).expect("utf8");
    assert!(xml.contains("<w:suppressAutoHyphens/>"), "{xml}");
    let para = Paragraph {
        text: "x".into(),
        props: ParaProperties {
            suppress_auto_hyphens: Some(true),
            ..Default::default()
        },
        ..Default::default()
    };
    let mut out = String::new();
    serialize_paragraph(&para, &mut out, &HashMap::new());
    assert!(!out.contains("suppressAutoHyphens"), "{out}");
    assert!(!out.contains("<w:pPr>"), "no empty pPr: {out}");
}

/// A run style that differs from the default only in its (read-only)
/// language writes no `<w:rPr>` at all — text typed into an empty
/// paragraph inherits the mark's `<w:lang>` but never an empty element.
#[test]
fn a_language_only_style_writes_no_run_properties() {
    let style = SpanStyle {
        lang: Some(Box::new(engine::Lang {
            val: Some("es-ES".into()),
            bidi: None,
        })),
        ..Default::default()
    };
    let mut out = String::new();
    emit_rpr(&style, &mut out);
    assert_eq!(out, "");
    /* The reader models `<w:lang>` typed AND keeps its bytes in the bag. */
    let (_, archive) = open(
        r#"<w:p><w:r><w:rPr><w:lang w:val="fr-FR" w:bidi="ar-SA"/></w:rPr><w:t>x</w:t></w:r></w:p>"#,
    );
    let s = archive.document.nth_paragraph(0).unwrap().style_at(0);
    assert_eq!(
        s.lang.as_deref(),
        Some(&engine::Lang {
            val: Some("fr-FR".into()),
            bidi: Some("ar-SA".into()),
        })
    );
    assert!(
        engine::GrabBag::fragments_of(&s.grab_bag)
            .iter()
            .any(|f| f.starts_with(b"<w:lang"))
    );
}
