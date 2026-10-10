//! Reader hardening: hostile input (issue #349 — numbers, #350 — field
//! markers, #351 — markup-compatibility branches) must read into a sane
//! model and still round-trip byte-identical on a zero-edit save.

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
/* Issue #407 — the attributes #349 left on ad hoc parses              */
/* ------------------------------------------------------------------ */

/// Tab stops, `w:cols w:space`, cell margins, row heights, a DrawingML
/// extent and a VML `style` size, every one carrying the hostile `v`.
fn hostile_attribute_document(v: &str) -> String {
    let tabs = format!(
        r#"<w:p><w:pPr><w:tabs><w:tab w:val="left" w:pos="{v}"/><w:tab w:val="right" w:pos="720"/></w:tabs></w:pPr><w:r><w:t>tabs</w:t></w:r></w:p>"#
    );
    let table = format!(
        r#"<w:tbl><w:tblPr><w:tblCellMar><w:top w:w="{v}" w:type="dxa"/><w:left w:w="{v}" w:type="dxa"/></w:tblCellMar></w:tblPr><w:tblGrid><w:gridCol w:w="2000"/></w:tblGrid><w:tr><w:trPr><w:trHeight w:val="{v}" w:hRule="exact"/></w:trPr><w:tc><w:tcPr><w:tcMar><w:bottom w:w="{v}" w:type="dxa"/></w:tcMar></w:tcPr><w:p><w:r><w:t>cell</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#
    );
    let drawing = format!(
        r#"<w:p><w:r><w:drawing><wp:inline distT="0" distB="0" distL="0" distR="0"><wp:extent cx="{v}" cy="{v}"/><wp:docPr id="1" name="Shape 1"/><a:graphic><a:graphicData uri="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"/></a:graphic></wp:inline></w:drawing></w:r><w:r><w:pict><v:rect style="width:{v};height:{v}pt" stroked="f"/></w:pict></w:r></w:p>"#
    );
    let sect = format!(
        r#"<w:sectPr><w:pgSz w:w="11906" w:h="16838"/><w:cols w:num="2" w:space="{v}"/></w:sectPr>"#
    );
    document(
        &format!("{tabs}{table}{drawing}<w:p><w:r><w:t>tail</w:t></w:r></w:p>"),
        &sect,
    )
}

/// Issue #407 — the remaining numeric attributes read into finite,
/// in-range model values (never `NaN as i64` = 0 or `inf as i64` =
/// `i64::MAX`), every bad value is reported, and the zero-edit save stays
/// byte-identical.
#[test]
fn hostile_remaining_attributes_read_finite_and_round_trip_byte_identical() {
    for v in HOSTILE_NUMBERS {
        let xml = hostile_attribute_document(v);
        let archive = assert_zero_edit_identity(&xml);
        let doc = &archive.document;

        let p = doc.blocks[0].as_paragraph().expect("paragraph");
        for stop in &p.props.tab_stops {
            assert!(stop.position_pt.is_finite(), "{v}: {stop:?}");
            assert!(stop.position_pt.abs() <= 1584.0, "{v}: {stop:?}");
        }
        assert!(
            p.props.tab_stops.iter().any(|t| t.position_pt == 36.0),
            "{v}: the good stop survives"
        );

        let t = doc.blocks[1].as_table().expect("table");
        let m = &t.props.cell_margins;
        for x in [m.top_twips, m.left_twips].into_iter().flatten() {
            assert!((0..=31_680).contains(&x), "{v}: tblCellMar {x}");
        }
        let row = &t.rows[0];
        match row.props.height {
            Some(engine::RowHeight::Exact { twips } | engine::RowHeight::AtLeast { twips }) => {
                assert!((0..=31_680).contains(&twips), "{v}: trHeight {twips}")
            }
            Some(engine::RowHeight::Auto) | None => {}
        }
        if let Some(cm) = &row.cells[0].props.cell_margins {
            for x in [cm.top_twips, cm.bottom_twips].into_iter().flatten() {
                assert!((0..=31_680).contains(&x), "{v}: tcMar {x}");
            }
        }

        let p = doc.blocks[2].as_paragraph().expect("drawing paragraph");
        for obj in &p.inline_objects {
            if let engine::InlineKind::Image {
                width_emu,
                height_emu,
                ..
            } = &obj.kind
            {
                assert!((0..=20_116_800).contains(width_emu), "{v}: cx {width_emu}");
                assert!(
                    (0..=20_116_800).contains(height_emu),
                    "{v}: cy {height_emu}"
                );
            }
        }

        let cols = doc.body_section.columns;
        assert!(
            cols.gutter_pt.is_finite() && (0.0..=1584.0).contains(&cols.gutter_pt),
            "{v}: {cols:?}"
        );

        if !v.is_empty() {
            assert!(
                archive.warnings.iter().any(|w| matches!(
                    w,
                    DocxWarning::InvalidMeasure { .. }
                        | DocxWarning::MeasureClamped { .. }
                        | DocxWarning::EmuClamped { .. }
                )),
                "{v}: no reader warning in {:?}",
                archive.warnings
            );
        }
    }
}

/// Issue #407 — each attribute names itself in the report.
#[test]
fn hostile_remaining_attributes_are_reported_by_name() {
    let archive = assert_zero_edit_identity(&hostile_attribute_document("NaN"));
    let details: Vec<String> = archive.warnings.iter().map(DocxWarning::detail).collect();
    for want in [
        "w:tab/@w:pos = \"NaN\"",
        "w:top/@w:w = \"NaN\"",
        "w:left/@w:w = \"NaN\"",
        "w:trHeight/@w:val = \"NaN\"",
        "w:bottom/@w:w = \"NaN\"",
        "wp:extent/@cx = \"NaN\"",
        "wp:extent/@cy = \"NaN\"",
        "v:rect/@style width = \"NaN\"",
        "v:rect/@style height = \"NaNpt\"",
        "w:cols/@w:space = \"NaN\"",
    ] {
        assert!(
            details.iter().any(|d| d == want),
            "missing {want:?} in {details:#?}"
        );
    }
    let clamped = assert_zero_edit_identity(&hostile_attribute_document("1e30"));
    let details: Vec<String> = clamped.warnings.iter().map(DocxWarning::detail).collect();
    assert!(
        details
            .iter()
            .any(|d| d == "w:tab/@w:pos = \"1e30\" → 31680 twips"),
        "{details:#?}"
    );
    assert!(
        details
            .iter()
            .any(|d| d == "wp:extent/@cx = \"1e30\" → 20116800 EMU"),
        "{details:#?}"
    );
}

/// Issue #407 — `<w:defaultTabStop>` and a numbering level's `<w:ind>`
/// (read from their own parts) go through the measure reader too.
#[test]
fn default_tab_stop_and_numbering_indents_are_validated() {
    let mut warnings = Vec::new();
    let settings = crate::error::collect_read_warnings(&mut warnings, |_| {
        crate::parts::settings::parse_settings_xml(
            br#"<w:settings xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:defaultTabStop w:val="NaN"/></w:settings>"#,
        )
    })
    .expect("settings");
    assert_eq!(settings.default_tab_stop_twips, None);
    let settings = crate::parts::settings::parse_settings_xml(
        br#"<w:settings xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:defaultTabStop w:val="0.5in"/></w:settings>"#,
    )
    .expect("settings");
    assert_eq!(settings.default_tab_stop_twips, Some(720));

    let numbering = crate::error::collect_read_warnings(&mut warnings, |_| {
        crate::parts::numbering::parse_numbering_xml(
            br#"<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:numFmt w:val="decimal"/><w:pPr><w:ind w:left="inf" w:hanging="1e30"/></w:pPr></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num></w:numbering>"#,
        )
    })
    .expect("numbering");
    let ind = numbering.abstract_nums[&0].levels[0].indent;
    assert_eq!(ind.start_twips, 0, "an unusable left keeps the default");
    assert_eq!(ind.hanging_twips, 31_680, "clamped to 22 in");
    let details: Vec<String> = warnings.iter().map(DocxWarning::detail).collect();
    assert_eq!(
        details,
        [
            "w:defaultTabStop/@w:val = \"NaN\"",
            "w:ind/@w:left = \"inf\"",
            "w:ind/@w:hanging = \"1e30\" → 31680 twips",
        ]
    );
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

/* ------------------------------------------------------------------ */
/* Issue #351 — markup compatibility                                   */
/* ------------------------------------------------------------------ */

use crate::test_fixtures::alternate_content_text_box;

/// The single text box of paragraph `p` and its story's text.
fn text_box_story(p: &engine::Paragraph) -> String {
    let boxes: Vec<_> = p
        .inline_objects
        .iter()
        .filter_map(|o| match &o.kind {
            engine::InlineKind::TextBox { story, .. } => Some(story),
            _ => None,
        })
        .collect();
    assert_eq!(
        boxes.len(),
        1,
        "exactly one text box: {:?}",
        p.inline_objects
    );
    boxes[0]
        .body
        .iter()
        .filter_map(engine::Block::as_paragraph)
        .map(|q| q.text.clone())
        .collect::<Vec<_>>()
        .join("|")
}

/// A paragraph-level `mc:AlternateContent` (`wps` text box choice, VML
/// fallback): the text box appears ONCE, its story from the choice; the
/// zero-edit save is byte-identical and an edit keeps both branches.
#[test]
fn paragraph_level_alternate_content_reads_its_choice_once() {
    let ac = alternate_content_text_box("wps");
    let para = format!(
        "<w:p>{}{}{}</w:p>",
        text("before "),
        ac.replace("<w:drawing>", "<w:r><w:drawing>")
            .replace("</w:drawing>", "</w:drawing></w:r>")
            .replace("<w:pict>", "<w:r><w:pict>")
            .replace("</w:pict>", "</w:pict></w:r>"),
        text(" after"),
    );
    let xml = document(&para, SECT);
    let archive = assert_zero_edit_identity(&xml);
    let p = archive.document.blocks[0].as_paragraph().expect("p");
    assert_eq!(p.text, "before \u{FFFC} after");
    assert_eq!(text_box_story(p), "choice story");

    let edited = archive
        .document
        .insert_text(archive.document.end_of_document(), "X");
    let saved = document_xml_of(&write_docx(&archive, &edited).expect("write"));
    assert_eq!(saved, xml.replacen("> after<", "> afterX<", 1));
    let edited = archive.document.insert_text(
        engine::LogicalPos {
            path: engine::BlockPath::top(0),
            offset: 0,
        },
        "Y",
    );
    let saved = document_xml_of(&write_docx(&archive, &edited).expect("write"));
    assert_eq!(source_bytes_rewritten(&xml, &saved), 0, "{saved}");

    /* `Requires="w99"` (unknown): the VML fallback is taken. */
    let xml = xml.replace(r#"Requires="wps""#, r#"Requires="w99""#);
    let archive = assert_zero_edit_identity(&xml);
    let p = archive.document.blocks[0].as_paragraph().expect("p");
    assert_eq!(p.text, "before \u{FFFC} after");
    assert_eq!(text_box_story(p), "fallback story");
}

/// A run-level `mc:AlternateContent` honours `Requires` too.
#[test]
fn run_level_alternate_content_evaluates_requires() {
    for (requires, story) in [("wps", "choice story"), ("w99", "fallback story")] {
        let para = format!(
            "<w:p>{}<w:r>{}</w:r></w:p>",
            text("x"),
            alternate_content_text_box(requires)
        );
        let xml = document(&para, SECT);
        let archive = assert_zero_edit_identity(&xml);
        let p = archive.document.blocks[0].as_paragraph().expect("p");
        assert_eq!(p.text, "x\u{FFFC}");
        assert_eq!(text_box_story(p), story, "Requires={requires}");
        let edited = archive
            .document
            .insert_text(archive.document.end_of_document(), "X");
        let saved = document_xml_of(&write_docx(&archive, &edited).expect("write"));
        assert_eq!(source_bytes_rewritten(&xml, &saved), 0, "{saved}");
    }
}

/// A block-level `mc:AlternateContent`: only the selected branch's
/// paragraph is a body block; the envelope keeps every branch.
#[test]
fn block_level_alternate_content_selects_one_branch() {
    for (requires, expect) in [("w14", "choice"), ("w99", "fallback")] {
        let body = format!(
            concat!(
                r#"<w:p><w:r><w:t>first</w:t></w:r></w:p>"#,
                r#"<mc:AlternateContent><mc:Choice Requires="{r}"><w:p><w:r><w:t>choice</w:t></w:r></w:p></mc:Choice>"#,
                r#"<mc:Fallback><w:p><w:r><w:t>fallback</w:t></w:r></w:p></mc:Fallback></mc:AlternateContent>"#,
                r#"<w:p><w:r><w:t>last</w:t></w:r></w:p>"#,
            ),
            r = requires
        );
        let xml = document(&body, SECT);
        let archive = assert_zero_edit_identity(&xml);
        let texts: Vec<_> = archive
            .document
            .blocks
            .iter()
            .filter_map(engine::Block::as_paragraph)
            .map(|p| p.text.clone())
            .collect();
        assert_eq!(texts, ["first", expect, "last"], "Requires={requires}");
        /* Edit the selected paragraph: the envelope survives. */
        let edited = archive.document.insert_text(
            engine::LogicalPos {
                path: engine::BlockPath::top(1),
                offset: expect.len() as u32,
            },
            "X",
        );
        let saved = document_xml_of(&write_docx(&archive, &edited).expect("write"));
        assert_eq!(
            saved,
            xml.replacen(&format!(">{expect}<"), &format!(">{expect}X<"), 1),
            "Requires={requires}"
        );
    }
}

/// `mc:AlternateContent` between table rows (issue #290's shape): one
/// branch's rows are the table's rows, and a cell edit — which
/// regenerates the table — keeps the whole AlternateContent around them.
#[test]
fn row_level_alternate_content_survives_table_regeneration() {
    let body = concat!(
        r#"<w:tbl><w:tblGrid><w:gridCol w:w="2000"/></w:tblGrid>"#,
        r#"<w:tr><w:tc><w:p><w:r><w:t>r1</w:t></w:r></w:p></w:tc></w:tr>"#,
        r#"<mc:AlternateContent><mc:Choice Requires="w14">"#,
        r#"<w:tr><w:tc><w:p><w:r><w:t>r2</w:t></w:r></w:p></w:tc></w:tr></mc:Choice>"#,
        r#"<mc:Fallback><w:tr><w:tc><w:p><w:r><w:t>r2</w:t></w:r></w:p></w:tc></w:tr></mc:Fallback>"#,
        r#"</mc:AlternateContent></w:tbl><w:p/>"#,
    );
    let xml = document(body, SECT);
    let archive = assert_zero_edit_identity(&xml);
    let t = archive.document.blocks[0].as_table().expect("table");
    assert_eq!(t.rows.len(), 2, "one branch's row");
    let at = engine::LogicalPos {
        path: engine::BlockPath {
            steps: vec![
                engine::PathStep::Block(0),
                engine::PathStep::Cell { row: 0, col: 0 },
                engine::PathStep::Block(0),
            ],
        },
        offset: 2,
    };
    let edited = archive.document.insert_text(at, "X");
    let saved = document_xml_of(&write_docx(&archive, &edited).expect("write"));
    assert_eq!(saved, xml.replacen(">r1<", ">r1X<", 1));
}

/// `mc:AlternateContent` between the paragraphs of a table cell used to be
/// walked branch by branch — the cell's text came out once per branch.
#[test]
fn cell_level_alternate_content_reads_one_branch() {
    let body = concat!(
        r#"<w:tbl><w:tblGrid><w:gridCol w:w="2000"/></w:tblGrid><w:tr><w:tc>"#,
        r#"<mc:AlternateContent><mc:Choice Requires="w14"><w:p><w:r><w:t>cell</w:t></w:r></w:p></mc:Choice>"#,
        r#"<mc:Fallback><w:p><w:r><w:t>cell</w:t></w:r></w:p></mc:Fallback></mc:AlternateContent>"#,
        r#"</w:tc></w:tr></w:tbl><w:p/>"#,
    );
    let xml = document(body, SECT);
    let archive = assert_zero_edit_identity(&xml);
    let t = archive.document.blocks[0].as_table().expect("table");
    let cell = &t.rows[0].cells[0];
    assert_eq!(cell.blocks.len(), 1, "{:?}", cell.blocks);
    assert_eq!(cell.blocks[0].as_paragraph().unwrap().text, "cell");
}

/// A text box inside a table cell paragraph: the table walker no longer
/// takes the box's own `<w:p>` (in either branch) for a cell block.
#[test]
fn text_box_in_a_table_cell_stays_inside_its_paragraph() {
    let body = format!(
        concat!(
            r#"<w:tbl><w:tblGrid><w:gridCol w:w="2000"/></w:tblGrid><w:tr><w:tc>"#,
            r#"<w:p><w:r><w:t>in cell</w:t></w:r><w:r>{}</w:r></w:p>"#,
            r#"</w:tc></w:tr></w:tbl><w:p/>"#,
        ),
        alternate_content_text_box("wps")
    );
    let xml = document(&body, SECT);
    let archive = assert_zero_edit_identity(&xml);
    let t = archive.document.blocks[0].as_table().expect("table");
    let cell = &t.rows[0].cells[0];
    assert_eq!(cell.blocks.len(), 1, "{:?}", cell.blocks);
    let p = cell.blocks[0].as_paragraph().unwrap();
    assert_eq!(p.text, "in cell\u{FFFC}");
    assert_eq!(text_box_story(p), "choice story");
}

/// `mc:Ignorable`: an element in an ignorable namespace the reader does
/// not understand is ignored — its text never shows — and kept verbatim.
#[test]
fn ignorable_unknown_elements_are_not_walked() {
    let para = format!(
        r#"<w:p><w99:ext w99:v="1">{}</w99:ext>{}</w:p><w99:block><w:p><w:r><w:t>also hidden</w:t></w:r></w:p></w99:block><w:p>{}</w:p>"#,
        text("hidden"),
        text("shown"),
        text("tail")
    );
    let xml = document(&para, SECT);
    let archive = assert_zero_edit_identity(&xml);
    let texts: Vec<_> = archive
        .document
        .blocks
        .iter()
        .filter_map(engine::Block::as_paragraph)
        .map(|p| p.text.clone())
        .collect();
    assert_eq!(texts, ["shown", "tail"]);
    let edited = archive.document.insert_text(
        engine::LogicalPos {
            path: engine::BlockPath::top(0),
            offset: 5,
        },
        "X",
    );
    let saved = document_xml_of(&write_docx(&archive, &edited).expect("write"));
    assert_eq!(saved, xml.replacen(">shown<", ">shownX<", 1));
}

/* ------------------------------------------------------------------ */
/* Issue #358 — the widened fuzz generator's intents, pinned            */
/* ------------------------------------------------------------------ */

/// Read + zero-edit save + re-read: the text survives (the
/// `docx_roundtrip` fuzz invariant), returning the first archive.
fn assert_text_round_trips(xml: &str) -> crate::DocxArchive {
    let docx = package_with_document_xml(xml, &[]);
    let a = read_docx(&docx).expect("read");
    let saved = write_docx(&a, &a.document).expect("write");
    let b = read_docx(&saved).expect("re-read");
    assert_eq!(a.document.to_plain_text(), b.document.to_plain_text());
    a
}

/// Issue #358 — a `begin` field character outside every paragraph
/// (before `<w:body>`, or between two blocks) used to stay in its
/// instruction phase into the next paragraph and hide all of its text:
/// #350's `</w:p>` close cannot reach a field that opened before the
/// paragraph did. Outside a paragraph a field character is not modeled.
#[test]
fn a_field_character_outside_every_paragraph_hides_nothing() {
    let visible = text("visible");
    for junk in [fld("begin"), fld("separate"), instr(" PAGE ")] {
        let xml = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n{ROOT}{junk}<w:body><w:p>{visible}</w:p>{SECT}</w:body></w:document>"
        );
        let a = assert_text_round_trips(&xml);
        assert_eq!(a.document.to_plain_text(), "visible", "{junk}");
    }
    let between = document(
        &format!(
            "<w:p>{}</w:p>{}<w:p>{}</w:p>",
            text("one"),
            fld("begin"),
            text("two")
        ),
        SECT,
    );
    let a = assert_text_round_trips(&between);
    assert_eq!(a.document.to_plain_text(), "one\ntwo");
    assert!(
        a.warnings
            .iter()
            .any(|w| matches!(w, DocxWarning::StrayFieldChar { kind } if kind == "begin")),
        "{:?}",
        a.warnings
    );
}

/// Issue #358 — a 40-deep balanced chain (one `separate` for the
/// innermost field, so every outer field is still in its instruction when
/// the result appears) and one past the reader's nesting cap both read,
/// hide the nested result and save byte-identical.
#[test]
fn deep_field_chains_read_and_round_trip() {
    for depth in [40usize, 41] {
        let mut runs = text("a");
        for _ in 0..depth {
            runs.push_str(&fld("begin"));
            runs.push_str(&instr(" PAGE "));
        }
        runs.push_str(&fld("separate"));
        runs.push_str(&text("deep"));
        for _ in 0..depth {
            runs.push_str(&fld("end"));
        }
        let a = assert_zero_edit_identity(&document(&format!("<w:p>{runs}</w:p>"), SECT));
        assert_eq!(a.document.to_plain_text(), "a", "depth {depth}");
    }
}

/// Issue #358 — surplus `separate` / `end` markers after a closed field
/// are ignored (warned), and a `fldSimple` with an empty, quote-only or
/// unknown instruction keeps its cached result.
#[test]
fn surplus_markers_and_hostile_simple_fields_keep_their_text() {
    let surplus = [
        fld("begin"),
        instr(" PAGE "),
        fld("separate"),
        text("1"),
        fld("end"),
        fld("separate"),
        fld("end"),
        text("after"),
    ]
    .concat();
    let a = assert_zero_edit_identity(&document(&format!("<w:p>{surplus}</w:p>"), SECT));
    assert_eq!(a.document.to_plain_text(), "1after");
    for instr in ["", "&quot;", " ", "BOGUS", " PAGE \\* ROMAN "] {
        let p = format!(
            r#"<w:p><w:fldSimple w:instr="{instr}">{}</w:fldSimple></w:p>"#,
            text("r")
        );
        let a = assert_zero_edit_identity(&document(&p, SECT));
        assert_eq!(a.document.to_plain_text(), "r", "{instr:?}");
    }
}

/// Issue #358 — drawings whose `r:embed` resolves to nothing (no such
/// relationship, an empty id) or that are empty (`<wp:inline/>`), inline
/// or anchored, with hostile extents and offsets: each reads as one
/// object placeholder with FINITE geometry and saves byte-identical.
#[test]
fn unresolvable_drawings_read_as_placeholders_and_round_trip() {
    let graphic = |rid: &str| {
        format!(
            r#"<a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:pic xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:blipFill><a:blip r:embed="{rid}"/></pic:blipFill></pic:pic></a:graphicData></a:graphic>"#
        )
    };
    let mut drawings = vec!["<wp:inline/>".to_string()];
    for (rid, cx, cy) in [
        ("rIdNowhere", "NaN", "-1e30"),
        ("", "inf", "1in"),
        ("rIdNowhere", "50%", "0x20"),
        ("rIdNowhere", "99999999999999999999", "-5"),
    ] {
        drawings.push(format!(
            r#"<wp:inline><wp:extent cx="{cx}" cy="{cy}"/><wp:docPr id="1" name="p"/>{}</wp:inline>"#,
            graphic(rid)
        ));
        drawings.push(format!(
            r#"<wp:anchor behindDoc="bogus" relativeHeight="{cx}"><wp:simplePos x="0" y="0"/><wp:positionH relativeFrom="column"><wp:posOffset>{cy}</wp:posOffset></wp:positionH><wp:positionV relativeFrom="paragraph"><wp:posOffset>{cx}</wp:posOffset></wp:positionV><wp:extent cx="{cx}" cy="{cy}"/><wp:wrapSquare wrapText="bothSides"/><wp:docPr id="2" name="p"/>{}</wp:anchor>"#,
            graphic(rid)
        ));
    }
    for d in drawings {
        let p = format!(
            "<w:p>{}<w:r><w:drawing>{d}</w:drawing></w:r>{}</w:p>",
            text("a"),
            text("b")
        );
        let a = assert_zero_edit_identity(&document(&p, SECT));
        assert_eq!(a.document.to_plain_text(), "a[image]b", "{d}");
        let para = a.document.blocks[0].as_paragraph().expect("paragraph");
        for o in &para.inline_objects {
            if let engine::InlineKind::Image {
                width_emu,
                height_emu,
                ..
            } = &o.kind
            {
                assert!(
                    *width_emu >= 0 && *height_emu >= 0,
                    "{d}: {width_emu}×{height_emu}"
                );
            }
        }
    }
}

/// Issue #358 — inline content controls nested 100 deep (200 XML levels,
/// under the 256 cap) read their innermost text and save byte-identical;
/// past the cap the package is a typed refusal (the 5000-deep block-level
/// case is `opc::archive`'s).
#[test]
fn deep_inline_content_controls_read_or_refuse_typed() {
    let nest = |depth: usize| {
        let mut s = text("inner");
        for _ in 0..depth {
            s = format!("<w:sdt><w:sdtContent>{s}</w:sdtContent></w:sdt>");
        }
        document(&format!("<w:p>{s}</w:p>"), SECT)
    };
    let a = assert_zero_edit_identity(&nest(100));
    assert_eq!(a.document.to_plain_text(), "inner");
    let err = read_docx(&package_with_document_xml(&nest(200), &[])).expect_err("too deep");
    assert!(err.to_string().contains("nesting depth"), "{err}");
}

/// Issue #358 — the splice snippets: a NUL character reference (XML 1.0
/// forbids it) is repaired up front into U+FFFD and reported (issue #434 —
/// it used to refuse the whole part), and a literal U+FFFC — the engine's
/// own inline-object placeholder — stays TEXT through read → save → read,
/// never becoming an object.
#[test]
fn nul_references_are_repaired_and_object_replacement_characters_stay_text() {
    let nul = document(r#"<w:p><w:r><w:t>a&#0;b</w:t></w:r></w:p>"#, SECT);
    let a = assert_text_round_trips(&nul);
    assert_eq!(a.document.to_plain_text(), "a\u{FFFD}b");
    assert!(
        a.warnings
            .iter()
            .any(|w| matches!(w, DocxWarning::MalformedPart { repaired: true, .. })),
        "{:?}",
        a.warnings
    );
    for t in ["a\u{FFFC}b", "a&#xFFFC;b"] {
        let xml = document(&format!("<w:p><w:r><w:t>{t}</w:t></w:r></w:p>"), SECT);
        let a = assert_text_round_trips(&xml);
        assert_eq!(a.document.to_plain_text(), "a\u{FFFC}b");
        let para = a.document.blocks[0].as_paragraph().expect("paragraph");
        assert!(para.inline_objects.is_empty(), "{:?}", para.inline_objects);
    }
}

/// Issue #358 — `mc:AlternateContent` with hostile `Requires` values,
/// around runs inside a paragraph and around whole paragraphs: an empty
/// or blank value requires nothing (the choice), an unknown or malformed
/// prefix selects the fallback, the first satisfiable of several choices
/// wins, an unsatisfied choice with no fallback and an empty envelope
/// contribute nothing — never both branches. Every shape reads, saves
/// byte-identical and keeps its text through read => write => read.
#[test]
fn hostile_requires_values_select_exactly_one_branch() {
    let choice =
        |req: &str, body: &str| format!(r#"<mc:Choice Requires="{req}">{body}</mc:Choice>"#);
    let fallback = |body: &str| format!("<mc:Fallback>{body}</mc:Fallback>");
    // (envelope children built from a branch body, expected branch text)
    let shapes = |c: &dyn Fn(&str) -> String| -> Vec<(String, &'static str)> {
        let (cx, fb, c2) = (c("CHOICE"), c("FALLBACK"), c("SECOND"));
        let mut v: Vec<(String, &'static str)> = Vec::new();
        for (req, expect) in [
            ("", "CHOICE"),
            ("  ", "CHOICE"),
            ("wps", "CHOICE"),
            ("a:b", "FALLBACK"),
            ("bogus", "FALLBACK"),
            ("wps w99", "FALLBACK"),
            ("w99 ", "FALLBACK"),
        ] {
            v.push((format!("{}{}", choice(req, &cx), fallback(&fb)), expect));
        }
        v.push((
            format!(
                "{}{}{}",
                choice("bogus", &cx),
                choice("", &c2),
                fallback(&fb)
            ),
            "SECOND",
        ));
        v.push((choice("w99", &cx), ""));
        v.push((fallback(&fb), "FALLBACK"));
        v.push((String::new(), ""));
        v
    };
    // Around runs, inside one paragraph.
    for (children, expect) in shapes(&|t| text(t)) {
        let ac = if children.is_empty() {
            "<mc:AlternateContent/>".to_string()
        } else {
            format!("<mc:AlternateContent>{children}</mc:AlternateContent>")
        };
        let xml = document(&format!("<w:p>{}{ac}{}</w:p>", text("x"), text("y")), SECT);
        let a = assert_zero_edit_identity(&xml);
        assert_eq!(a.document.to_plain_text(), format!("x{expect}y"), "{ac}");
        assert_text_round_trips(&xml);
    }
    // Around whole paragraphs, between two body paragraphs.
    for (children, expect) in shapes(&|t| format!("<w:p>{}</w:p>", text(t))) {
        let ac = if children.is_empty() {
            "<mc:AlternateContent/>".to_string()
        } else {
            format!("<mc:AlternateContent>{children}</mc:AlternateContent>")
        };
        let body = format!("<w:p>{}</w:p>{ac}<w:p>{}</w:p>", text("x"), text("y"));
        let xml = document(&body, SECT);
        let a = assert_zero_edit_identity(&xml);
        let plain = a.document.to_plain_text();
        for other in ["CHOICE", "FALLBACK", "SECOND"] {
            assert_eq!(plain.contains(other), other == expect, "{ac}: {plain:?}");
        }
        assert!(
            plain.starts_with('x') && plain.trim_end().ends_with('y'),
            "{plain:?}"
        );
        assert_text_round_trips(&xml);
    }
    // An empty envelope between table rows and between a cell's blocks.
    for (row_gap, cell_gap) in [
        ("<mc:AlternateContent/>", ""),
        ("", "<mc:AlternateContent/>"),
    ] {
        let row = |t: &str| {
            format!(
                "<w:tr><w:tc><w:p>{}</w:p>{cell_gap}<w:p/></w:tc></w:tr>",
                text(t)
            )
        };
        let body = format!(
            r#"<w:tbl><w:tblPr/><w:tblGrid><w:gridCol w:w="2000"/></w:tblGrid>{}{row_gap}{}</w:tbl><w:p/>"#,
            row("r1"),
            row("r2")
        );
        let xml = document(&body, SECT);
        let a = assert_zero_edit_identity(&xml);
        let plain = a.document.to_plain_text();
        assert!(plain.contains("r1") && plain.contains("r2"), "{plain:?}");
        assert_text_round_trips(&xml);
    }
}

/// Issue #358 — found by the splice mode's raw-input splice: junk bytes
/// that are not UTF-8 (`\xae\x95`, control characters) as character data
/// inside an UNSELECTED `mc:Fallback` (paragraph level and block level).
/// The reader never decodes them, so the package opens; the writer used to
/// skip any captured fragment that was not UTF-8, dropping the
/// `mc:AlternateContent` closer and writing a part its own reader refused.
/// The structure now survives a save: the re-read succeeds with the text.
#[test]
fn junk_bytes_in_an_unselected_branch_keep_the_part_well_formed() {
    let junk: &[u8] = b"j\xae\x9512\x1cc?\x02\xa0\xd52\xd8\xfd";
    let fallback = |inner: &[u8]| {
        let mut v = b"<mc:Fallback>".to_vec();
        v.extend_from_slice(junk);
        v.extend_from_slice(inner);
        v.extend_from_slice(b"</mc:Fallback>");
        v
    };
    let run = text("fallback");
    let para = format!("<w:p>{run}</w:p>");
    let shapes: Vec<Vec<u8>> = vec![
        // Paragraph level: around runs.
        [
            format!("<w:p>{}<mc:AlternateContent>", text("x")).as_bytes(),
            format!(
                r#"<mc:Choice Requires="wps">{}</mc:Choice>"#,
                text("choice")
            )
            .as_bytes(),
            &fallback(run.as_bytes()),
            format!("</mc:AlternateContent>{}</w:p>", text("y")).as_bytes(),
        ]
        .concat(),
        // Block level: around paragraphs.
        [
            format!("<w:p>{}</w:p><mc:AlternateContent>", text("x")).as_bytes(),
            format!(
                r#"<mc:Choice Requires="wps"><w:p>{}</w:p></mc:Choice>"#,
                text("choice")
            )
            .as_bytes(),
            &fallback(para.as_bytes()),
            format!("</mc:AlternateContent><w:p>{}</w:p>", text("y")).as_bytes(),
        ]
        .concat(),
    ];
    for body in shapes {
        let xml = [
            document("", SECT)
                .split("<w:body>")
                .next()
                .unwrap()
                .as_bytes(),
            b"<w:body>",
            &body,
            SECT.as_bytes(),
            b"</w:body></w:document>",
        ]
        .concat();
        let docx = crate::test_fixtures::package_with_document_xml_bytes(&xml, &[]);
        let a = read_docx(&docx).expect("the source opens");
        let plain = a.document.to_plain_text();
        assert!(
            plain.contains("choice") && !plain.contains("fallback"),
            "{plain:?}"
        );
        let saved = write_docx(&a, &a.document).expect("write");
        let b = read_docx(&saved).expect("the writer's own output re-reads");
        assert_eq!(b.document.to_plain_text(), plain);
    }
}

/// Issue #358 — a picture-less drawing (`r:embed=""`) whose bytes hold a
/// byte that is not UTF-8 (a flipped attribute-name byte the reader never
/// decodes) is written from its bytes — lossily — instead of being
/// dropped: the `[image]` placeholder survives read => write => read.
#[test]
fn a_drawing_with_non_utf8_bytes_keeps_its_placeholder() {
    let drawing: &[u8] = b"<w:r><w:drawing><wp:inline><wp:extent cx=\"914400\" cy=\"914400\"/><wp:docPr id=\"1\" name=\"p\"/><a:graphic><a:graphicData uri=\"http://schemas.openxmlformats.org/drawingml/2006/picture\"><pic:pic xmlns:pic=\"http://schemas.openxmlformats.org/drawingml/2006/picture\"><pic:blipFill><a:blip r:embed=\"\"/></pic:blipFill><pic:spPr><a:xfrm><a:ext \x9cx=\"914400\" cy=\"914400\"/></a:xfrm></pic:spPr></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r>";
    let xml = [
        document("", SECT)
            .split("<w:body>")
            .next()
            .unwrap()
            .as_bytes(),
        b"<w:body><w:p>",
        text("a").as_bytes(),
        drawing,
        text("b").as_bytes(),
        b"</w:p>",
        SECT.as_bytes(),
        b"</w:body></w:document>",
    ]
    .concat();
    let docx = crate::test_fixtures::package_with_document_xml_bytes(&xml, &[]);
    let a = read_docx(&docx).expect("the source opens");
    assert_eq!(a.document.to_plain_text(), "a[image]b");
    let saved = write_docx(&a, &a.document).expect("write");
    let b = read_docx(&saved).expect("re-read");
    assert_eq!(b.document.to_plain_text(), "a[image]b");
}

/// Issue #358 — a `<w:tab/>` / `<w:br/>` spliced into a run's `<w:rPr>`
/// is not content: it rides the rPr grab bag and adds no text, so a save
/// cannot double it (the tab used to count as text AND ride the bag, and
/// every resave added one more).
#[test]
fn a_tab_inside_run_properties_is_not_text() {
    let para = r#"<w:p><w:r><w:rPr><w:i/><w:tab/><w:br/></w:rPr><w:t xml:space="preserve">a</w:t><w:tab/><w:t xml:space="preserve">b</w:t></w:r></w:p>"#;
    let xml = document(para, SECT);
    let a = assert_zero_edit_identity(&xml);
    assert_eq!(a.document.to_plain_text(), "a\tb");
    assert_text_round_trips(&xml);
    // A regenerated paragraph (an edit) keeps the rPr children verbatim
    // and still reads back the same text plus the insertion.
    let edited = a.document.insert_text(a.document.end_of_document(), "c");
    let saved = write_docx(&a, &edited).expect("write");
    let b = read_docx(&saved).expect("re-read");
    assert_eq!(b.document.to_plain_text(), "a\tbc");
}

/// Issue #358 — a 60-deep chain of 1×1 tables (deeper than layout's
/// 32-level flattening, under the reader's typed-table cap) around one
/// paragraph: it reads, the innermost text survives, and a zero-edit save
/// is byte-identical and re-reads to the same text.
#[test]
fn sixty_deep_table_chains_read_and_round_trip() {
    let mut inner = format!("<w:p>{}</w:p>", text("deep"));
    for level in 0..60 {
        inner = format!(
            r#"<w:tbl><w:tblPr/><w:tblGrid><w:gridCol w:w="{}"/></w:tblGrid><w:tr><w:tc>{inner}<w:p/></w:tc></w:tr></w:tbl>"#,
            9000 - level * 100
        );
    }
    let xml = document(&format!("{inner}<w:p>{}</w:p>", text("after")), SECT);
    let a = assert_zero_edit_identity(&xml);
    let plain = a.document.to_plain_text();
    assert!(
        plain.contains("deep") && plain.contains("after"),
        "{plain:?}"
    );
    assert_text_round_trips(&xml);
}

/// Issue #422 — `<w:start w:val="2147483647"/>` on a letter level: the
/// counter saturates instead of overflowing on the next item, and every
/// marker stays short (the repeated-letter form used to spell 82 MB).
#[test]
fn a_hostile_numbering_start_keeps_markers_bounded() {
    let numbering = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:start w:val="2147483647"/><w:numFmt w:val="lowerLetter"/><w:lvlText w:val="%1."/></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num></w:numbering>"#;
    let item = r#"<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>item</w:t></w:r></w:p>"#;
    let xml = document(&item.repeat(3), SECT);
    let docx = package_with_document_xml(&xml, &[("word/numbering.xml", numbering.as_bytes())]);
    let a = read_docx(&docx).expect("read");
    let markers: Vec<_> = a
        .document
        .blocks
        .iter()
        .filter_map(|b| b.as_paragraph()?.resolved_marker.clone())
        .collect();
    for m in &markers {
        assert!(m.len() <= 16, "marker of {} bytes", m.len());
    }
}
