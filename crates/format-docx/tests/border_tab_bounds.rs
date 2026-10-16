//! Issue #503 (residue) - border stroke widths and the tab-stop list length
//! read from a `.docx` stay inside Word's limits.
//!
//! Word draws a line 1/4 pt ..= 6 pt (`w:sz` 2..=48 eighths of a point; a
//! `none` / `nil` edge paints nothing at any size) and keeps 64 tab stops
//! per paragraph. The reader CLAMPS what a file spells (a file is not the
//! user's to re-type) and reports it as `DocxWarning::MeasureClamped`; the
//! source bytes are kept, so a zero-edit save is byte-identical and a
//! re-read re-derives the same clamped model.

use engine::{Block, BorderStroke, BorderStyle, DocumentTree};
use format_docx::test_fixtures::package_with_sect_extra;
use format_docx::{DocxWarning, read_docx, write_docx};
use std::io::Read;

fn part(docx: &[u8], name: &str) -> String {
    let mut a = zip::ZipArchive::new(std::io::Cursor::new(docx)).expect("zip");
    let mut s = String::new();
    a.by_name(name)
        .expect("entry")
        .read_to_string(&mut s)
        .expect("utf8");
    s
}

fn edge(border: &str, val: &str, sz: &str) -> String {
    format!(r#"<w:{border} w:val="{val}" w:sz="{sz}" w:space="1" w:color="auto"/>"#)
}

fn clamped(warnings: &[DocxWarning], attr: &str) -> Vec<(String, i64)> {
    warnings
        .iter()
        .filter_map(|w| match w {
            DocxWarning::MeasureClamped {
                attr: a,
                value,
                twips,
            } if a == attr => Some((value.clone(), *twips)),
            _ => None,
        })
        .collect()
}

fn size(s: &Option<BorderStroke>) -> u16 {
    s.as_ref().expect("edge").size_eighth_pt
}

fn body() -> String {
    let para = format!(
        "<w:p><w:pPr><w:pBdr>{}{}{}{}{}</w:pBdr></w:pPr><w:r><w:t>p</w:t></w:r></w:p>",
        edge("top", "single", "96"),
        edge("left", "single", "0"),
        edge("bottom", "dotted", "70000"),
        edge("right", "nil", "300"),
        edge("between", "single", "99999999999999999999"),
    );
    let table = format!(
        "<w:tbl><w:tblPr><w:tblBorders>{}{}{}</w:tblBorders></w:tblPr>\
         <w:tblGrid><w:gridCol w:w=\"2000\"/></w:tblGrid>\
         <w:tr><w:tc><w:tcPr><w:tcBorders>{}{}</w:tcBorders></w:tcPr>\
         <w:p><w:r><w:t>c</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p/>",
        edge("top", "single", "200"),
        edge("insideH", "double", "0"),
        edge("insideV", "single", "48"),
        edge("left", "single", "49"),
        edge("right", "single", "2"),
    );
    let tabs: String = (0..70)
        .map(|i| format!(r#"<w:tab w:val="left" w:pos="{}"/>"#, 100 + i * 10))
        .collect();
    let tabbed =
        format!("<w:p><w:pPr><w:tabs>{tabs}</w:tabs></w:pPr><w:r><w:t>tabs</w:t></w:r></w:p>");
    format!("{para}{table}{tabbed}")
}

fn fixture() -> Vec<u8> {
    package_with_sect_extra(
        &body(),
        &format!(
            r#"<w:pgBorders w:offsetFrom="page">{}{}</w:pgBorders>"#,
            edge("top", "single", "96"),
            edge("left", "single", "1"),
        ),
    )
}

fn table_of(doc: &DocumentTree) -> &engine::Table {
    doc.blocks
        .iter()
        .find_map(|b| match b {
            Block::Table(t) => Some(t),
            _ => None,
        })
        .expect("table")
}

#[test]
fn paragraph_borders_clamp_into_two_to_forty_eight() {
    let archive = read_docx(&fixture()).expect("read");
    let b = archive
        .document
        .nth_paragraph(0)
        .expect("paragraph")
        .props
        .borders
        .clone()
        .expect("borders");
    assert_eq!(size(&b.top), 48, "96 -> 6 pt");
    assert_eq!(size(&b.left), 2, "a drawn 0 -> 1/4 pt");
    assert_eq!(size(&b.bottom), 48, "70000 (past u16 / 4) -> 6 pt");
    assert_eq!(b.right.as_ref().unwrap().style, BorderStyle::None);
    assert_eq!(size(&b.right), 48, "a none edge keeps a width <= 48 too");
    assert_eq!(
        size(&b.inside_h),
        48,
        "`between`: a digit string past u64 clamps, not defaults"
    );

    let w = clamped(&archive.warnings, "w:top/@w:sz");
    assert!(w.contains(&("96".into(), 120)), "{w:?}");
    let z = clamped(&archive.warnings, "w:left/@w:sz");
    assert!(z.contains(&("0".into(), 5)), "{z:?}");
}

#[test]
fn table_and_cell_borders_clamp_too() {
    let archive = read_docx(&fixture()).expect("read");
    let t = table_of(&archive.document);
    let tb = t.props.borders.as_ref().expect("table borders");
    assert_eq!(size(&tb.top), 48);
    assert_eq!(size(&tb.inside_h), 2, "a drawn 0 -> 2");
    assert_eq!(size(&tb.inside_v), 48, "48 itself is untouched");
    let cb = t.rows[0].cells[0]
        .props
        .borders
        .as_ref()
        .expect("cell borders");
    assert_eq!(size(&cb.left), 48, "49 -> 48");
    assert_eq!(size(&cb.right), 2, "2 itself is untouched");
    /* Exactly the out-of-range widths are reported: top 200, insideH 0,
    left 49 (the two in-range ones are not). */
    let table_warnings: Vec<String> = archive
        .warnings
        .iter()
        .filter_map(|w| match w {
            DocxWarning::MeasureClamped { attr, value, .. } if attr.ends_with("@w:sz") => {
                Some(format!("{attr}={value}"))
            }
            _ => None,
        })
        .collect();
    for want in ["w:top/@w:sz=200", "w:insideH/@w:sz=0", "w:left/@w:sz=49"] {
        assert!(
            table_warnings.iter().any(|w| w == want),
            "{table_warnings:?}"
        );
    }
    assert!(
        !table_warnings.iter().any(|w| w == "w:insideV/@w:sz=48"),
        "{table_warnings:?}"
    );
}

#[test]
fn page_borders_clamp_too() {
    let archive = read_docx(&fixture()).expect("read");
    let pb = archive
        .document
        .body_section
        .page_borders
        .as_ref()
        .expect("page borders");
    assert_eq!(pb.top.as_ref().expect("top").size_eighth_pt, 48);
    assert_eq!(pb.left.as_ref().expect("left").size_eighth_pt, 2);
}

#[test]
fn a_tab_stop_list_keeps_the_first_64_and_says_so() {
    let archive = read_docx(&fixture()).expect("read");
    let tabbed = archive
        .document
        .blocks
        .iter()
        .filter_map(|b| b.as_paragraph())
        .find(|p| !p.props.tab_stops.is_empty())
        .expect("tabbed paragraph");
    assert_eq!(tabbed.props.tab_stops.len(), 64);
    /* The FIRST 64: pos 100 twips = 5 pt up to pos 730 twips. */
    assert_eq!(tabbed.props.tab_stops[0].position_pt, 5.0);
    assert_eq!(tabbed.props.tab_stops[63].position_pt, 36.5);
    let w = clamped(&archive.warnings, "w:tabs");
    assert_eq!(w, vec![("70 tab stops".to_string(), 64)], "{w:?}");
    let detail = archive
        .warnings
        .iter()
        .find(|w| matches!(w, DocxWarning::MeasureClamped { attr, .. } if attr == "w:tabs"))
        .expect("warning")
        .detail();
    assert!(
        detail.contains("70 tab stops") && detail.contains("64"),
        "{detail}"
    );
}

#[test]
fn an_exactly_64_stop_list_is_untouched() {
    let tabs: String = (0..64)
        .map(|i| format!(r#"<w:tab w:val="left" w:pos="{}"/>"#, 100 + i * 10))
        .collect();
    let docx = package_with_sect_extra(
        &format!("<w:p><w:pPr><w:tabs>{tabs}</w:tabs></w:pPr><w:r><w:t>t</w:t></w:r></w:p>"),
        "",
    );
    let archive = read_docx(&docx).expect("read");
    assert_eq!(
        archive
            .document
            .nth_paragraph(0)
            .unwrap()
            .props
            .tab_stops
            .len(),
        64
    );
    assert!(clamped(&archive.warnings, "w:tabs").is_empty());
}

#[test]
fn a_clamping_read_keeps_the_source_bytes_on_a_zero_edit_save() {
    let src = fixture();
    let archive = read_docx(&src).expect("read");
    let saved = write_docx(&archive, &archive.document).expect("write");
    assert_eq!(
        part(&saved, "word/document.xml"),
        part(&src, "word/document.xml")
    );
    /* ...and the re-read derives the same clamped model. */
    let again = read_docx(&saved).expect("re-read");
    assert_eq!(
        engine::first_point_field_beyond_bound(&again.document),
        None
    );
    assert_eq!(
        engine::first_point_field_beyond_bound(&archive.document),
        None
    );
}
