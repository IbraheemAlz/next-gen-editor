//! Reader hardening: hostile input (issue #349 — numbers, #350 — field
//! markers) must read into a sane model and still round-trip
//! byte-identical on a zero-edit save.

use crate::error::DocxWarning;
use crate::opc::archive::read_docx;
use crate::test_fixtures::package_with_document_xml;
use crate::writer::write_docx;
use std::io::{Cursor, Read};

const ROOT: &str = concat!(
    r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" "#,
    r#"xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" "#,
    r#"xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006" "#,
    r#"xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" "#,
    r#"xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" "#,
    r#"xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape" "#,
    r#"xmlns:v="urn:schemas-microsoft-com:vml" "#,
    r#"xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml" "#,
    r#"xmlns:w99="urn:example:future" mc:Ignorable="w14 w99">"#,
);

/// A full `word/document.xml`: `body` then `sect` (a whole `<w:sectPr>`).
fn document(body: &str, sect: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n{ROOT}<w:body>{body}{sect}</w:body></w:document>"
    )
}

fn document_xml_of(bytes: &[u8]) -> String {
    let mut z = zip::ZipArchive::new(Cursor::new(bytes)).expect("zip");
    let mut f = z.by_name("word/document.xml").expect("document part");
    let mut s = String::new();
    f.read_to_string(&mut s).expect("utf-8");
    s
}

/// Read, then save with no edit: the part must come back byte-identical.
fn assert_zero_edit_identity(xml: &str) -> crate::DocxArchive {
    let docx = package_with_document_xml(xml, &[]);
    let archive = read_docx(&docx).expect("read");
    let saved = write_docx(&archive, &archive.document).expect("write");
    assert_eq!(document_xml_of(&saved), xml, "zero-edit save drifted");
    archive
}

/* ------------------------------------------------------------------ */
/* Issue #349 — measures                                               */
/* ------------------------------------------------------------------ */

const HOSTILE_NUMBERS: &[&str] = &[
    "NaN",
    "inf",
    "-inf",
    "1e30",
    "-720",
    "99999999999",
    "abc",
    "",
];

fn hostile_measure_document(v: &str) -> String {
    let para = format!(
        r#"<w:p><w:pPr><w:ind w:left="{v}" w:right="{v}" w:hanging="{v}"/><w:spacing w:before="{v}" w:after="{v}" w:line="{v}"/></w:pPr><w:r><w:t>para</w:t></w:r></w:p>"#
    );
    let table = format!(
        r#"<w:tbl><w:tblPr><w:tblW w:w="{v}" w:type="dxa"/><w:tblInd w:w="{v}" w:type="dxa"/></w:tblPr><w:tblGrid><w:gridCol w:w="{v}"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:tcW w:w="{v}" w:type="pct"/></w:tcPr><w:p><w:r><w:t>cell</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#
    );
    let sect = format!(
        r#"<w:sectPr><w:pgSz w:w="{v}" w:h="{v}"/><w:pgMar w:top="{v}" w:right="{v}" w:bottom="{v}" w:left="{v}" w:header="{v}" w:footer="{v}" w:gutter="0"/></w:sectPr>"#
    );
    document(
        &format!("{para}{table}<w:p><w:r><w:t>tail</w:t></w:r></w:p>"),
        &sect,
    )
}

/// NaN / infinite / huge / negative / garbage page geometry, indents,
/// spacing and table widths: the model only ever holds finite in-range
/// values, every bad value is reported, and the zero-edit save is
/// byte-identical (a NaN page width used to defeat the verified-sectPr
/// equality and regenerate the section on every save).
#[test]
fn hostile_measures_read_finite_and_round_trip_byte_identical() {
    for v in HOSTILE_NUMBERS {
        let xml = hostile_measure_document(v);
        let archive = assert_zero_edit_identity(&xml);
        let doc = &archive.document;

        let g = doc.body_section.geometry;
        for (what, x) in [
            ("width", g.width),
            ("height", g.height),
            ("margin_top", g.margin_top),
            ("margin_right", g.margin_right),
            ("margin_bottom", g.margin_bottom),
            ("margin_left", g.margin_left),
            ("header_offset", g.header_offset),
            ("footer_offset", g.footer_offset),
        ] {
            assert!(x.is_finite(), "{v}: {what} = {x}");
            assert!(x.abs() <= 1584.0, "{v}: {what} = {x}");
        }
        assert!(g.width >= 7.2 && g.height >= 7.2, "{v}: {g:?}");

        let p = doc.blocks[0].as_paragraph().expect("paragraph");
        let ind = &p.props.indent;
        for x in [ind.start_twips, ind.end_twips, ind.hanging_twips] {
            assert!(x.abs() <= 31_680, "{v}: indent {x}");
        }
        assert!(ind.hanging_twips >= 0, "{v}: hanging is unsigned");
        assert!(p.props.spacing.before_twips >= 0 && p.props.spacing.before_twips <= 31_680);

        let t = doc.blocks[1].as_table().expect("table");
        assert!(t.props.indent_twips.abs() <= 31_680, "{v}");
        if let Some(w) = t.grid.first() {
            assert!((0..=31_680).contains(w), "{v}: gridCol {w}");
        }
        match t.props.width {
            Some(engine::CellWidth::Dxa(w)) => assert!((0..=31_680).contains(&w), "{v}: {w}"),
            Some(engine::CellWidth::Pct(_)) | Some(engine::CellWidth::Auto) | None => {}
            Some(other) => panic!("{v}: {other:?}"),
        }

        if !v.is_empty() {
            assert!(
                archive.warnings.iter().any(|w| matches!(
                    w,
                    DocxWarning::InvalidMeasure { .. } | DocxWarning::MeasureClamped { .. }
                )),
                "{v}: no reader warning in {:?}",
                archive.warnings
            );
        }
    }
}

/// An edit elsewhere in the document still re-emits the hostile
/// `<w:sectPr>` bytes verbatim (the verified passthrough holds), and the
/// edited paragraph keeps its source `<w:pPr>` (its model is unchanged).
#[test]
fn hostile_sect_pr_bytes_survive_an_edit() {
    for v in ["NaN", "1e30", "-5"] {
        let xml = hostile_measure_document(v);
        let docx = package_with_document_xml(&xml, &[]);
        let archive = read_docx(&docx).expect("read");
        let end = archive.document.end_of_document();
        let edited = archive.document.insert_text(end, "X");
        let saved = document_xml_of(&write_docx(&archive, &edited).expect("write"));
        assert_eq!(saved, xml.replacen(">tail<", ">tailX<", 1), "{v}");
    }
}

/// Universal-measure units ECMA-376 allows are honoured (they used to be
/// dropped by the integer parses).
#[test]
fn universal_measure_units_are_honoured() {
    let xml = document(
        r#"<w:p><w:pPr><w:ind w:left="0.5in" w:firstLine="18pt"/></w:pPr><w:r><w:t>x</w:t></w:r></w:p>"#,
        r#"<w:sectPr><w:pgSz w:w="8.5in" w:h="11in"/><w:pgMar w:top="2.54cm" w:right="1in" w:bottom="25.4mm" w:left="6pc" w:header="720" w:footer="720"/></w:sectPr>"#,
    );
    let archive = assert_zero_edit_identity(&xml);
    let g = archive.document.body_section.geometry;
    assert_eq!((g.width, g.height), (612.0, 792.0));
    assert!((g.margin_top - 72.0).abs() < 1e-3, "{g:?}");
    assert!((g.margin_bottom - 72.0).abs() < 1e-3, "{g:?}");
    assert_eq!((g.margin_right, g.margin_left), (72.0, 72.0));
    let p = archive.document.blocks[0].as_paragraph().expect("p");
    assert_eq!(p.props.indent.start_twips, 720);
    assert_eq!(p.props.indent.first_line_twips, 360);
    assert!(archive.warnings.is_empty(), "{:?}", archive.warnings);
}

/* ------------------------------------------------------------------ */
/* Issue #350 — field phases                                           */
/* ------------------------------------------------------------------ */

const SECT: &str = r#"<w:sectPr><w:pgSz w:w="11906" w:h="16838"/></w:sectPr>"#;

fn fld(kind: &str) -> String {
    format!(r#"<w:r><w:fldChar w:fldCharType="{kind}"/></w:r>"#)
}

fn instr(code: &str) -> String {
    format!(r#"<w:r><w:instrText xml:space="preserve">{code}</w:instrText></w:r>"#)
}

fn text(t: &str) -> String {
    format!(r#"<w:r><w:t xml:space="preserve">{t}</w:t></w:r>"#)
}

/// `Pre { IF { MERGEFIELD x } = "a" "yes" "no" } post` with the cached
/// inner result `«x»` and outer result `no`.
fn nested_if_paragraph() -> String {
    [
        text("Pre "),
        fld("begin"),
        instr(" IF "),
        fld("begin"),
        instr(" MERGEFIELD x "),
        fld("separate"),
        text("«x»"),
        fld("end"),
        instr(r#" = "a" "yes" "no" "#),
        fld("separate"),
        text("no"),
        fld("end"),
        text(" post"),
    ]
    .concat()
}

/// Byte count of the ORIGINAL `a` an edited `b` rewrote (0 = pure
/// insertion), the issue #251 metric.
fn source_bytes_rewritten(a: &str, b: &str) -> usize {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let prefix = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    let max_suffix = a.len().min(b.len()) - prefix;
    let suffix = a
        .iter()
        .rev()
        .zip(b.iter().rev())
        .take(max_suffix)
        .take_while(|(x, y)| x == y)
        .count();
    a.len() - prefix - suffix
}

/// The inner field's result sits inside the outer field's CODE: it is
/// never visible text and no overlay of its own. Only the outer result
/// renders; the outer instruction spells the nested code the way Word's
/// field-code view does; the source prologue (begin … separate, the inner
/// field included) is kept, so an edit is a pure insertion.
#[test]
fn nested_field_result_inside_an_instruction_is_hidden() {
    let xml = document(&format!("<w:p>{}</w:p>", nested_if_paragraph()), SECT);
    let archive = assert_zero_edit_identity(&xml);
    let p = archive.document.blocks[0].as_paragraph().expect("p");
    assert_eq!(p.text, "Pre no post");
    assert_eq!(p.fields.len(), 1, "{:?}", p.fields);
    let f = &p.fields[0];
    assert_eq!((f.start, f.end), (4, 6));
    assert_eq!(f.instruction, r#"IF { MERGEFIELD x } = "a" "yes" "no""#);
    assert_eq!(f.code_text(), r#"{ IF { MERGEFIELD x } = "a" "yes" "no" }"#);
    let src = f.source.as_deref().expect("source prologue kept");
    assert!(
        String::from_utf8_lossy(&src.open).contains("MERGEFIELD x"),
        "the prologue carries the nested field"
    );
    assert!(archive.warnings.is_empty(), "{:?}", archive.warnings);

    /* An edit regenerates the paragraph: still a pure insertion. */
    let end = archive.document.end_of_document();
    let edited = archive.document.insert_text(end, "X");
    let saved = document_xml_of(&write_docx(&archive, &edited).expect("write"));
    assert_eq!(saved, xml.replacen("> post<", "> postX<", 1));

    /* F9 (#77): restamping evaluates what it can (a FILENAME elsewhere in
    the paragraph) and leaves the unevaluated IF's cached result alone. */
    let with_filename = document(
        &format!(
            "<w:p>{}{}{}{}{}{}</w:p>",
            nested_if_paragraph(),
            fld("begin"),
            instr(" FILENAME "),
            fld("separate"),
            text("old.docx"),
            fld("end"),
        ),
        SECT,
    );
    let archive = assert_zero_edit_identity(&with_filename);
    let env = engine::FieldEnv {
        document_name: Some("new.docx".into()),
        ..Default::default()
    };
    let restamped = archive.document.restamp_fields_with_env(&env);
    let p = restamped.blocks[0].as_paragraph().expect("p");
    assert_eq!(p.text, "Pre no postnew.docx");
    let saved = document_xml_of(&write_docx(&archive, &restamped).expect("write"));
    assert!(
        saved.contains("MERGEFIELD x"),
        "nested code survives a restamp"
    );
    assert!(saved.contains(">new.docx<"), "{saved}");
}

/// `end` / `separate` before any `begin`: ignored (reported), the text
/// stays visible, a later field still parses, and the stray runs survive
/// a regeneration verbatim.
#[test]
fn stray_field_characters_before_a_begin_are_ignored() {
    let para = [
        fld("end"),
        fld("separate"),
        text("visible "),
        fld("begin"),
        instr(" PAGE "),
        fld("separate"),
        text("1"),
        fld("end"),
    ]
    .concat();
    let xml = document(&format!("<w:p>{para}</w:p>"), SECT);
    let archive = assert_zero_edit_identity(&xml);
    let p = archive.document.blocks[0].as_paragraph().expect("p");
    assert_eq!(p.text, "visible 1");
    assert_eq!(p.fields.len(), 1);
    assert_eq!(p.fields[0].instruction, "PAGE");
    let strays = archive
        .warnings
        .iter()
        .filter(|w| matches!(w, DocxWarning::StrayFieldChar { .. }))
        .count();
    assert_eq!(strays, 2, "{:?}", archive.warnings);

    let edited = archive
        .document
        .insert_text(archive.document.end_of_document(), "X");
    let saved = document_xml_of(&write_docx(&archive, &edited).expect("write"));
    assert_eq!(source_bytes_rewritten(&xml, &saved), 0, "{saved}");
    assert_eq!(saved.matches(r#"w:fldCharType="end""#).count(), 2);
}

/// 200 `begin`s that never separate: the stack is capped (32), the
/// unclosed code closes at its paragraph's end — the next paragraph is
/// ordinary visible text — and the broken field code survives an edit of
/// either paragraph.
#[test]
fn unclosed_begins_close_at_the_paragraph_end() {
    let mut broken = fld("begin").repeat(200);
    broken.push_str(&instr(" PAGE "));
    broken.push_str(&text("code"));
    let xml = document(
        &format!("<w:p>{broken}</w:p><w:p>{}</w:p>", text("normal")),
        SECT,
    );
    let archive = assert_zero_edit_identity(&xml);
    let blocks: Vec<_> = archive.document.blocks.iter().collect();
    assert_eq!(blocks.len(), 2);
    assert_eq!(blocks[0].as_paragraph().unwrap().text, "");
    assert_eq!(blocks[1].as_paragraph().unwrap().text, "normal");
    assert!(
        archive
            .warnings
            .contains(&DocxWarning::FieldNestingTooDeep { limit: 32 }),
        "{:?}",
        archive.warnings
    );
    assert!(
        archive
            .warnings
            .contains(&DocxWarning::UnclosedField { count: 200 }),
        "{:?}",
        archive.warnings
    );

    /* Edit the normal paragraph. */
    let edited = archive
        .document
        .insert_text(archive.document.end_of_document(), "X");
    let saved = document_xml_of(&write_docx(&archive, &edited).expect("write"));
    assert_eq!(saved, xml.replacen(">normal<", ">normalX<", 1));

    /* Edit the broken paragraph itself: its field code is kept whole. */
    let at = engine::LogicalPos {
        path: engine::BlockPath::top(0),
        offset: 0,
    };
    let edited = archive.document.insert_text(at, "Y");
    let saved = document_xml_of(&write_docx(&archive, &edited).expect("write"));
    assert_eq!(source_bytes_rewritten(&xml, &saved), 0, "{saved}");
    assert_eq!(saved.matches(r#"w:fldCharType="begin""#).count(), 200);
    assert!(saved.contains(">code<"));
}
