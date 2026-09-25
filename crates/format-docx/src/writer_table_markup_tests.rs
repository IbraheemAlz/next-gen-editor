//! Issue #248 — attribute-level + whitespace source markup of tables. A
//! regenerated table (any cell edit or table command) re-emits the
//! `<w:tbl>` / `<w:tr>` / `<w:tc>` attributes, the pretty-print whitespace,
//! the verified `<w:tblPr>` / `<w:tblGrid>` / `<w:trPr>` / `<w:tcPr>`
//! bytes, `<w:tblPrEx>` (#103) and the markup between rows and cells —
//! including a `<w:sdt>` wrapping a row or a cell (#245's
//! `Bug66263-table.docx`) — so an edit inside a table is a pure insertion.

use super::tests::{build_docx_with_styles, document_xml_of};
use super::*;
use crate::opc::archive::read_docx;
use engine::{BlockPath, LogicalPos, PathStep};

const STYLES_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"/>"#;

/// A Word-shaped, pretty-printed table exercising every piece of table
/// source markup.
const TABLE: &str = r#"
  <w:tbl>
    <w:tblPr>
      <w:tblStyle w:val="TableGrid"/>
      <w:tblW w:w="0" w:type="auto"/>
      <w:tblBorders><w:top w:val="single" w:sz="4" w:space="0" w:color="auto"/></w:tblBorders>
      <w:tblLook w:val="04A0" w:firstRow="1" w:lastRow="0" w:firstColumn="1" w:lastColumn="0" w:noHBand="0" w:noVBand="1"/>
    </w:tblPr>
    <w:tblGrid>
      <w:gridCol w:w="2400"/>
      <w:gridCol w:w="2400"/>
      <w:tblGridChange w:id="7"><w:tblGrid><w:gridCol w:w="4800"/></w:tblGrid></w:tblGridChange>
    </w:tblGrid>
    <w:tr w:rsidR="00AA11BB" w14:paraId="1A2B3C4D" w14:textId="77777777">
      <w:tblPrEx><w:tblBorders><w:top w:val="double" w:sz="12" w:space="0" w:color="FF0000"/></w:tblBorders></w:tblPrEx>
      <w:trPr><w:cnfStyle w:val="100000000000"/><w:trHeight w:val="400"/></w:trPr>
      <w:tc>
        <w:tcPr><w:tcW w:w="2400" w:type="dxa"/><w:shd w:val="clear" w:color="auto" w:fill="FFEB78" w:themeFill="accent1"/></w:tcPr>
        <w:p w:rsidR="00AA11BB"
             w:rsidRDefault="00CC22DD">
          <w:r><w:t>A1</w:t></w:r>
        </w:p>
      </w:tc>
      <w:sdt><w:sdtPr><w:tag w:val="cell"/></w:sdtPr><w:sdtContent>
      <w:tc>
        <w:tcPr><w:tcW w:w="2400" w:type="dxa"/></w:tcPr>
        <w:p><w:r><w:t>B1</w:t></w:r></w:p>
      </w:tc>
      </w:sdtContent></w:sdt>
    </w:tr>
    <w:bookmarkStart w:id="5" w:name="between"/>
    <w:sdt>
      <w:sdtPr><w:id w:val="1001"/></w:sdtPr>
      <w:sdtContent>
        <w:tr w:rsidR="00EE33FF">
          <w:tc><w:p><w:r><w:t>A2</w:t></w:r></w:p></w:tc>
          <w:tc><w:p><w:r><w:t>B2</w:t></w:r></w:p></w:tc>
        </w:tr>
      </w:sdtContent>
    </w:sdt>
    <w:bookmarkEnd w:id="5"/>
  </w:tbl>
  <w:p><w:r><w:t>after</w:t></w:r></w:p>
"#;

fn document(body: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml" xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006" mc:Ignorable="w14"><w:body>{body}<w:sectPr/></w:body></w:document>"#
    )
}

fn open() -> (String, DocxArchive) {
    let xml = document(TABLE);
    let archive = read_docx(&build_docx_with_styles(STYLES_XML, &xml)).expect("read fixture");
    (xml, archive)
}

fn table_path() -> BlockPath {
    BlockPath::top(0)
}

fn cell(row: u32, col: u32) -> BlockPath {
    BlockPath {
        steps: vec![
            PathStep::Block(0),
            PathStep::Cell { row, col },
            PathStep::Block(0),
        ],
    }
}

fn save(archive: &DocxArchive, doc: &engine::DocumentTree) -> String {
    let bytes = write_docx(archive, doc).expect("write");
    crate::check_document_xml_well_formed(&bytes).expect("well-formed");
    document_xml_of(&bytes)
}

/// `(common prefix, original span, edited span)` of two parts — the
/// corpus tool's `source_bytes_rewritten` is the original span.
fn rewritten(orig: &str, edited: &str) -> (usize, usize, usize) {
    let (a, b) = (orig.as_bytes(), edited.as_bytes());
    let prefix = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    let max = a.len().min(b.len()) - prefix;
    let suffix = a
        .iter()
        .rev()
        .zip(b.iter().rev())
        .take(max)
        .take_while(|(x, y)| x == y)
        .count();
    (prefix, a.len() - prefix - suffix, b.len() - prefix - suffix)
}

#[test]
fn zero_edit_resave_is_byte_identical() {
    let (xml, archive) = open();
    assert_eq!(save(&archive, &archive.document), xml);
}

/// Typing in any cell — a plain cell, a cell inside a cell-level content
/// control, a cell of a row wrapped in a row-level content control — is
/// EXACTLY the source plus the inserted text, on both save paths: no
/// reflowed whitespace, no lost attribute, `<w:tblPrEx>`, `<w:tblGridChange>`
/// and the unread attributes of `<w:tblLook>` / `<w:shd>` / `<w:tblBorders>`
/// all intact.
#[test]
fn typing_in_any_cell_is_a_pure_insertion() {
    let (xml, archive) = open();
    for (row, col, text) in [(0, 0, "A1"), (0, 1, "B1"), (1, 0, "A2"), (1, 1, "B2")] {
        let edited = archive
            .document
            .insert_text(LogicalPos::new(cell(row, col), 2), "!");
        assert!(edited.blocks[0].as_table().unwrap().dirty);
        let expected = xml.replacen(&format!(">{text}<"), &format!(">{text}!<"), 1);
        assert_eq!(save(&archive, &edited), expected, "cell ({row}, {col})");
        let ui = format_ui(&edited);
        assert_eq!(ui, expected, "UI save path, cell ({row}, {col})");
    }
}

fn format_ui(doc: &engine::DocumentTree) -> String {
    let bytes = crate::save_docx(doc).expect("ui save");
    crate::check_document_xml_well_formed(&bytes).expect("well-formed");
    document_xml_of(&bytes)
}

/// The reader side: `<w:tblPrEx>` is captured whole (its borders no longer
/// overwrite the table's own), a `<w:tblGridChange>` adds no live grid
/// column, and every level carries its markup.
#[test]
fn reader_captures_table_row_and_cell_markup() {
    let (_, archive) = open();
    let t = archive.document.blocks[0].as_table().unwrap();
    assert_eq!(t.grid, vec![2400, 2400], "tblGridChange is history");
    let top = t
        .props
        .borders
        .as_ref()
        .and_then(|b| b.top.as_ref())
        .unwrap();
    assert_eq!(
        top.style,
        engine::BorderStyle::Single,
        "tblPrEx must not win"
    );
    let m = t.source_markup.as_deref().unwrap();
    assert!(m.tbl_pr.as_ref().unwrap().lead.starts_with(b"\n    "));
    assert!(
        m.grid
            .as_ref()
            .unwrap()
            .xml
            .windows(b"tblGridChange".len())
            .any(|w| w == b"tblGridChange")
    );
    let r0 = t.rows[0].source_markup.as_deref().unwrap();
    assert_eq!(r0.attrs.len(), 3);
    assert!(
        r0.tbl_pr_ex
            .as_ref()
            .unwrap()
            .xml
            .starts_with(b"<w:tblPrEx>")
    );
    assert_eq!(r0.tr_pr.as_ref().unwrap().model, t.rows[0].props);
    let c1 = t.rows[0].cells[1].source_markup.as_deref().unwrap();
    let before = &c1.body_xml.as_deref().unwrap().before;
    assert!(
        before
            .iter()
            .any(|f| matches!(f, engine::BodyFragment::Open { open_xml, .. } if open_xml.starts_with(b"<w:sdt>"))),
        "{before:?}"
    );
    let r1 = t.rows[1].source_markup.as_deref().unwrap();
    let bx = r1.body_xml.as_deref().unwrap();
    assert!(
        bx.before
            .iter()
            .any(|f| matches!(f, engine::BodyFragment::Open { .. }))
    );
    assert!(
        bx.after
            .iter()
            .any(|f| matches!(f, engine::BodyFragment::Verbatim { xml } if xml.starts_with(b"<w:bookmarkEnd")))
    );
    /* The multi-line `<w:p>` start tag keeps its attribute whitespace. */
    let p = t.rows[0].cells[0].blocks[0].as_paragraph().unwrap();
    let pm = p.source_markup.as_deref().unwrap();
    assert_eq!(pm.attrs[0].ws, None);
    assert_eq!(pm.attrs[1].ws.as_deref(), Some("\n             "));
}

/// A row inserted between the two source rows is a pure insertion: every
/// source byte (both rows' markup, the row-level content control, the
/// bookmarks) survives around the fresh, plainly written row.
#[test]
fn inserted_row_is_a_pure_insertion() {
    let (xml, archive) = open();
    for at in [0usize, 1, 2] {
        let edited = archive.document.insert_row(table_path(), at);
        let out = save(&archive, &edited);
        let (_, orig_span, new_span) = rewritten(&xml, &out);
        assert_eq!(
            orig_span, 0,
            "insert_row({at}) rewrote source bytes:\n{out}"
        );
        assert!(new_span > 0);
        let reread = read_docx(&write_docx(&archive, &edited).unwrap()).unwrap();
        assert_eq!(reread.document.blocks[0].as_table().unwrap().rows.len(), 3);
    }
}

/// Deleting the content-controlled row removes exactly its bytes (and the
/// markup riding it): a pure deletion, the part stays well-formed.
#[test]
fn deleted_row_is_a_pure_deletion() {
    let (xml, archive) = open();
    let edited = archive.document.delete_row(table_path(), 1);
    let out = save(&archive, &edited);
    let (_, orig_span, new_span) = rewritten(&xml, &out);
    assert_eq!(new_span, 0, "delete_row wrote new bytes:\n{out}");
    assert!(orig_span > 0);
    assert!(
        !out.contains("1001"),
        "the row's content control goes with it"
    );
    /* Deleting the first row keeps the second row's wrapper whole. */
    let edited = archive.document.delete_row(table_path(), 0);
    let out = save(&archive, &edited);
    assert!(
        out.contains(r#"<w:sdtPr><w:id w:val="1001"/></w:sdtPr>"#),
        "{out}"
    );
    let (_, _, new_span) = rewritten(&xml, &out);
    assert_eq!(new_span, 0, "{out}");
}

/// A merge regenerates only the owner's `<w:tcPr>` (now carrying the
/// `gridSpan`) — its unchanged children adopt their source spelling, the
/// `w:themeFill` included — and drops the merged-away cell; `<w:tblPr>`,
/// `<w:tblPrEx>` and `<w:trPr>` stay verbatim.
#[test]
fn merge_regenerates_the_owner_tcpr_only() {
    let (xml, archive) = open();
    let edited = archive.document.merge_cells(table_path(), 0, 0, 0, 1);
    let out = save(&archive, &edited);
    assert!(
        out.contains(r#"<w:tcPr><w:tcW w:w="2400" w:type="dxa"/><w:gridSpan w:val="2"/><w:shd w:val="clear" w:color="auto" w:fill="FFEB78" w:themeFill="accent1"/></w:tcPr>"#),
        "{out}"
    );
    for kept in [
        r#"<w:trPr><w:cnfStyle w:val="100000000000"/><w:trHeight w:val="400"/></w:trPr>"#,
        r#"<w:tblPrEx><w:tblBorders><w:top w:val="double" w:sz="12" w:space="0" w:color="FF0000"/></w:tblBorders></w:tblPrEx>"#,
        r#"<w:tblLook w:val="04A0" w:firstRow="1" w:lastRow="0" w:firstColumn="1" w:lastColumn="0" w:noHBand="0" w:noVBand="1"/>"#,
    ] {
        assert!(out.contains(kept), "{kept} lost:\n{out}");
    }
    assert!(!out.contains(">B1<"), "the merged-away cell is gone");
    assert!(xml.contains(">B1<"));
}

/// A cell property edit regenerates that cell's `<w:tcPr>` alone; a split
/// brings the source bytes back once the model matches again.
#[test]
fn stale_properties_regenerate_and_matching_ones_reuse() {
    let (xml, archive) = open();
    let shaded =
        archive
            .document
            .set_cell_shading(table_path(), 0, 1, Some([0x11, 0x22, 0x33, 0xFF]));
    let out = save(&archive, &shaded);
    assert!(
        out.contains(r#"<w:tcPr><w:tcW w:w="2400" w:type="dxa"/><w:shd w:val="clear" w:color="auto" w:fill="112233"/></w:tcPr>"#),
        "{out}"
    );
    let (_, orig_span, _) = rewritten(&xml, &out);
    assert!(
        orig_span < 40,
        "only the tcPr tail was rewritten ({orig_span} B)"
    );
    /* Undoing the property by value is a byte-identical save again. */
    let back = shaded.set_cell_shading(table_path(), 0, 1, None);
    assert_eq!(save(&archive, &back), xml);
}

/// Inserting a column changes the grid (regenerated) but no row, cell or
/// property markup is lost, and the fresh cells come in plain.
#[test]
fn inserted_column_keeps_every_row_and_cell_markup() {
    let (_, archive) = open();
    let edited = archive.document.insert_column(table_path(), 1);
    let out = save(&archive, &edited);
    for kept in [
        r#"<w:tr w:rsidR="00AA11BB" w14:paraId="1A2B3C4D" w14:textId="77777777">"#,
        r#"<w:tr w:rsidR="00EE33FF">"#,
        "<w:tblPrEx>",
        r#"<w:sdtPr><w:tag w:val="cell"/></w:sdtPr>"#,
        r#"<w:sdtPr><w:id w:val="1001"/></w:sdtPr>"#,
        r#"w:themeFill="accent1""#,
        r#"<w:bookmarkEnd w:id="5"/>"#,
    ] {
        assert!(out.contains(kept), "{kept} lost:\n{out}");
    }
    assert_eq!(out.matches("<w:tblPrEx>").count(), 1);
    let reread = read_docx(&write_docx(&archive, &edited).unwrap()).unwrap();
    let t = reread.document.blocks[0].as_table().unwrap();
    assert_eq!(t.grid.len(), 3);
    assert!(t.rows.iter().all(|r| r.cells.len() == 3));
}

/// The minimal-package save path (no source `styles.xml`) never trusts
/// the recorded property bytes — they regenerate — but attributes,
/// whitespace and wrappers still ride.
#[test]
fn minimal_package_regenerates_properties_but_keeps_markup() {
    let (_, archive) = open();
    let edited = archive
        .document
        .insert_text(LogicalPos::new(cell(0, 0), 2), "!");
    let out = document_xml_of(&build_minimal_docx(&edited).expect("minimal"));
    assert!(
        out.contains(r#"<w:trHeight w:val="400" w:hRule="atLeast"/>"#),
        "{out}"
    );
    assert!(out.contains(r#"w14:paraId="1A2B3C4D""#), "{out}");
    assert!(
        out.contains(r#"<w:sdtPr><w:id w:val="1001"/></w:sdtPr>"#),
        "{out}"
    );
    assert!(out.contains("<w:tblPrEx>"), "{out}");
}

/// Crash-recovery snapshots carry the table markup, byte-stable.
#[test]
fn snapshot_round_trip_is_byte_stable() {
    let (_, archive) = open();
    let doc = archive
        .document
        .insert_text(LogicalPos::new(cell(1, 1), 2), "!");
    let a = engine::snapshot::encode(&doc).expect("encode");
    let back: engine::DocumentTree = engine::snapshot::decode(&a).expect("decode").payload;
    let b = engine::snapshot::encode(&back).expect("re-encode");
    assert_eq!(a, b);
    assert_eq!(
        back.blocks[0].as_table().unwrap().source_markup,
        doc.blocks[0].as_table().unwrap().source_markup
    );
}
