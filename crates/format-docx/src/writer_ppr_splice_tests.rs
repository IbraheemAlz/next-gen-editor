//! Issue #419 — a regenerated `<w:pPr>` changes only the children whose
//! meaning changed: leader tabs, border `w:space` / `w:shadow`, a `pct25`
//! shading, autospacing and the source spelling of every other child
//! survive an indent change; a changed child keeps its unowned
//! attributes; nothing the cascade supplies is baked in.

use super::tests::document_xml_of;
use super::*;
use crate::opc::archive::read_docx;
use crate::test_fixtures::{PPR_PLAIN_PARAGRAPH, ppr_attributes_docx};
use engine::{BlockPath, LogicalPos, TabKind, TabLeader, TabStopPatch};

fn open() -> (String, DocxArchive) {
    let docx = ppr_attributes_docx();
    let archive = read_docx(&docx).expect("read fixture");
    (document_xml_of(&docx), archive)
}

fn save(archive: &DocxArchive, doc: &engine::DocumentTree) -> String {
    let bytes = write_docx(archive, doc).expect("write");
    crate::check_document_xml_well_formed(&bytes).expect("well-formed");
    document_xml_of(&bytes)
}

fn at(block: u32, offset: u32) -> LogicalPos {
    LogicalPos::new(BlockPath::top(block), offset)
}

/// The reader models the pattern and the border extras.
#[test]
fn pattern_shading_and_border_extras_are_modeled() {
    let (xml, archive) = open();
    assert_eq!(save(&archive, &archive.document), xml, "zero-edit identity");
    let p = archive.document.nth_paragraph(0).unwrap();
    let pattern = p.props.shading_pattern.as_ref().expect("pattern");
    assert_eq!(pattern.val, "pct25");
    assert_eq!(pattern.color, Some([0xFF, 0, 0, 0xFF]));
    assert_eq!(p.props.shading, Some([0, 0xFF, 0, 0xFF]));
    let b = p.props.borders.as_ref().expect("borders");
    let top = b.top.as_ref().unwrap();
    assert_eq!(
        (top.space_pt, top.shadow, top.frame),
        (Some(1), true, false)
    );
    assert_eq!(b.left.as_ref().unwrap().space_pt, Some(4));
    assert_eq!(p.props.tab_stops[1].leader, TabLeader::Dot);
}

/// Changing the indent rewrites exactly the `<w:ind>` element (in the
/// source's `w:left` spelling); every other child — tabs with leaders,
/// border edges with space / shadow / theme colour, the pattern shading,
/// autospacing, `jc="right"` — stays byte-identical.
#[test]
fn an_indent_change_rewrites_only_the_ind_child() {
    let (xml, archive) = open();
    let doc = archive
        .document
        .set_paragraph_indent(at(0, 0), at(0, 0), 72.0, 0.0, 18.0);
    let out = save(&archive, &doc);
    let expected = xml.replacen(
        r#"<w:ind w:left="720" w:firstLine="360"/>"#,
        r#"<w:ind w:left="1440" w:firstLine="360"/>"#,
        1,
    );
    assert_ne!(expected, xml);
    assert_eq!(out, expected);
}

/// A changed fill keeps the modeled pattern; a changed edge keeps its
/// `w:space` and drops the stale theme colour; the other edges stay.
#[test]
fn a_changed_child_keeps_what_the_model_does_not_change() {
    let (xml, archive) = open();
    let doc = archive
        .document
        .set_paragraph_shading(at(0, 0), at(0, 0), Some([0, 0, 0xFF, 0xFF]));
    let out = save(&archive, &doc);
    assert_eq!(
        out,
        xml.replacen(
            r#"<w:shd w:val="pct25" w:color="FF0000" w:fill="00FF00"/>"#,
            r#"<w:shd w:val="pct25" w:color="FF0000" w:fill="0000FF"/>"#,
            1
        )
    );

    let mut borders = archive
        .document
        .nth_paragraph(0)
        .unwrap()
        .props
        .borders
        .clone()
        .unwrap();
    borders.left.as_mut().unwrap().color = Some([0, 0, 0xFF, 0xFF]);
    let doc = archive
        .document
        .set_paragraph_borders(at(0, 0), at(0, 0), Some(borders));
    let out = save(&archive, &doc);
    assert_eq!(
        out,
        xml.replacen(
            r#"<w:left w:val="double" w:sz="6" w:space="4" w:color="FF0000" w:themeColor="accent2"/>"#,
            r#"<w:left w:val="double" w:sz="6" w:space="4" w:color="0000FF"/>"#,
            1
        )
    );
}

/// A new tab stop is inserted at its position; the existing stops keep
/// their source bytes (leaders included).
#[test]
fn a_new_tab_stop_keeps_the_others() {
    let (xml, archive) = open();
    let stop = |pt: f32, kind, leader| TabStopPatch {
        position_pt: pt,
        kind,
        leader,
    };
    let doc = archive.document.set_tab_stops(
        at(0, 0),
        at(0, 0),
        vec![
            stop(144.0, TabKind::Left, None),
            stop(288.0, TabKind::Center, Some(TabLeader::None)),
            /* `TabStopPatch` inherits a leader by index (#145): name it. */
            stop(467.5, TabKind::Right, Some(TabLeader::Dot)),
        ],
    );
    let out = save(&archive, &doc);
    assert_eq!(
        out,
        xml.replacen(
            r#"<w:tab w:val="right" w:leader="dot" w:pos="9350"/>"#,
            r#"<w:tab w:val="center" w:pos="5760"/><w:tab w:val="right" w:leader="dot" w:pos="9350"/>"#,
            1
        )
    );
}

/// Nothing the cascade supplies is baked in: typing into the section-mark
/// paragraph is a pure insertion (its pPr was regenerated with the
/// docDefaults spacing before), and aligning the pPr-less paragraph writes
/// `<w:jc>` alone.
#[test]
fn cascade_values_are_never_baked_in() {
    let (xml, archive) = open();
    let doc = archive.document.insert_text(at(1, 3), "X");
    let out = save(&archive, &doc);
    assert_eq!(out, xml.replacen("End of", "EndX of", 1));

    let doc = archive
        .document
        .set_alignment(at(2, 0), at(2, 0), engine::Alignment::Center);
    let out = save(&archive, &doc);
    let plain =
        PPR_PLAIN_PARAGRAPH.replacen("<w:r>", r#"<w:pPr><w:jc w:val="center"/></w:pPr><w:r>"#, 1);
    assert_eq!(out, xml.replacen(PPR_PLAIN_PARAGRAPH, &plain, 1));
}

/// A split of the section-mark paragraph writes its `<w:sectPr>` once, on
/// the right half (the mark's); the left half drops the recorded one.
#[test]
fn a_split_section_paragraph_keeps_one_section_marker() {
    let (_, archive) = open();
    let doc = archive.document.split_paragraph(at(1, 3));
    let out = save(&archive, &doc);
    assert_eq!(out.matches("<w:sectPr").count(), 2, "{out}");
    let left = out.find("<w:t>End</w:t>").expect("left half");
    let marker = out.find(r#"<w:sectPr w:rsidR="00B2">"#).expect("marker");
    let right = out.find(" of section one").expect("right half");
    assert!(left < marker && marker < right, "{out}");
    let back = read_docx(&write_docx(&archive, &doc).unwrap()).unwrap();
    assert!(
        back.document
            .nth_paragraph(1)
            .unwrap()
            .section_end
            .is_none()
    );
    assert!(
        back.document
            .nth_paragraph(2)
            .unwrap()
            .section_end
            .is_some()
    );
}
