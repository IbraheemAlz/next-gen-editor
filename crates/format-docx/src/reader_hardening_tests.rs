//! Reader hardening: hostile input (issue #349 — numbers) must read into
//! a sane model and still round-trip byte-identical on a zero-edit save.

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
