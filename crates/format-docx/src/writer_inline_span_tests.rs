//! Positioned verbatim spans in `Paragraph::source_markup` — the Tier-3
//! ("never lose content") regeneration contracts of issues #244 (legacy
//! form fields), #245 (run-level content controls) and #246 (simple
//! fields): a regenerated paragraph keeps the unmodeled markup whole, at
//! its text offset, through edits before, after and inside it.

use super::tests::{build_docx_with_styles, document_xml_of};
use super::*;
use crate::opc::archive::read_docx;
use engine::{BlockPath, LogicalPos, PathStep};

const STYLES_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"/>"#;

fn document(body: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml" xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006" mc:Ignorable="w14"><w:body>{body}<w:sectPr/></w:body></w:document>"#
    )
}

fn open(body: &str) -> (String, DocxArchive) {
    let xml = document(body);
    let archive = read_docx(&build_docx_with_styles(STYLES_XML, &xml)).expect("read fixture");
    (xml, archive)
}

fn at(block: u32, offset: usize) -> LogicalPos {
    LogicalPos {
        path: BlockPath::top(block),
        offset: offset as u32,
    }
}

fn save(archive: &DocxArchive, doc: &engine::DocumentTree) -> String {
    let bytes = write_docx(archive, doc).expect("write");
    crate::check_document_xml_well_formed(&bytes).expect("well-formed");
    document_xml_of(&bytes)
}

/* ============================== issue #244 — legacy form fields ==== */

/// `checkboxes.docx`'s shape: a `FORMCHECKBOX` whose begin `fldChar`
/// carries `<w:ffData>`, the form field's name bookmark inside the field,
/// a text-less rPr-only run between the instruction and `separate`, run
/// rsids everywhere, the bookmark end after the field.
const CHECKBOX: &str = concat!(
    r#"<w:r w:rsidR="00414CBC" w:rsidRPr="00045869"><w:rPr><w:lang w:val="de-DE"/></w:rPr><w:fldChar w:fldCharType="begin"><w:ffData><w:name w:val="Check1"/><w:enabled/><w:calcOnExit w:val="0"/><w:checkBox><w:sizeAuto/><w:default w:val="0"/></w:checkBox></w:ffData></w:fldChar></w:r>"#,
    r#"<w:bookmarkStart w:id="0" w:name="Check1"/>"#,
    r#"<w:r w:rsidRPr="00D051C0"><w:instrText xml:space="preserve"> FORMCHECKBOX </w:instrText></w:r>"#,
    r#"<w:r w:rsidR="00414CBC"><w:rPr><w:lang w:val="de-DE"/></w:rPr></w:r>"#,
    r#"<w:r w:rsidR="00414CBC"><w:rPr><w:lang w:val="de-DE"/></w:rPr><w:fldChar w:fldCharType="separate"/></w:r>"#,
    r#"<w:r w:rsidR="00414CBC" w:rsidRPr="00045869"><w:rPr><w:lang w:val="de-DE"/></w:rPr><w:fldChar w:fldCharType="end"/></w:r>"#,
);

/// A `FORMDROPDOWN` without a `separate` (Word omits it for an empty
/// result).
const DROPDOWN: &str = concat!(
    r#"<w:r><w:fldChar w:fldCharType="begin"><w:ffData><w:name w:val="Drop1"/><w:enabled/><w:ddList><w:listEntry w:val="one"/><w:listEntry w:val="two"/></w:ddList></w:ffData></w:fldChar></w:r>"#,
    r#"<w:r><w:instrText xml:space="preserve"> FORMDROPDOWN </w:instrText></w:r>"#,
    r#"<w:r><w:fldChar w:fldCharType="end"/></w:r>"#,
);

fn checkbox_paragraph() -> String {
    format!(
        r#"<w:p w:rsidR="00D8217F" w:rsidRDefault="00D051C0"><w:r><w:t xml:space="preserve">unchecked: </w:t></w:r>{CHECKBOX}<w:bookmarkEnd w:id="0"/><w:r w:rsidRPr="00D051C0"><w:t xml:space="preserve"> after</w:t></w:r>{DROPDOWN}</w:p>"#
    )
}

/// The field is ONE content marker at its offset; the markers inside it
/// (the name bookmark, the text-less run) are part of its bytes, not
/// separate markers; nothing reaches the field model.
#[test]
fn form_field_reads_as_one_content_span() {
    let (_, archive) = open(&checkbox_paragraph());
    let p = archive.document.nth_paragraph(0).unwrap();
    assert_eq!(p.text, "unchecked:  after");
    assert!(p.fields.is_empty(), "{:?}", p.fields);
    let m = p.source_markup.as_deref().unwrap();
    let content: Vec<_> = m
        .markers
        .iter()
        .filter(|mk| mk.role == engine::MarkerRole::Content)
        .collect();
    assert_eq!(content.len(), 2, "{:?}", m.markers);
    assert_eq!(content[0].at, "unchecked: ".len() as u32);
    assert_eq!(content[0].xml, CHECKBOX.as_bytes());
    assert_eq!(content[1].at, p.text.len() as u32);
    assert_eq!(content[1].xml, DROPDOWN.as_bytes());
    assert!(
        !m.markers
            .iter()
            .any(|mk| mk.xml.starts_with(br#"<w:bookmarkStart w:id="0""#)),
        "the name bookmark rides inside the span, never twice"
    );
}

/// Edits before, at and after the field regenerate the paragraph as
/// exactly source + the inserted bytes: the `fldChar` / `ffData` bytes
/// survive untouched.
#[test]
fn form_field_survives_edits_around_it() {
    let (xml, archive) = open(&checkbox_paragraph());
    assert_eq!(save(&archive, &archive.document), xml, "zero-edit");

    let edited = archive.document.insert_text(at(0, "unch".len()), "INS");
    assert_eq!(
        save(&archive, &edited),
        xml.replacen("unchecked", "unchINSecked", 1)
    );
    /* At the field's offset: the text continues the run before it. */
    let edited = archive
        .document
        .insert_text(at(0, "unchecked: ".len()), "X");
    assert_eq!(
        save(&archive, &edited),
        xml.replacen("unchecked: <", "unchecked: X<", 1)
    );
    let edited = archive
        .document
        .insert_text(at(0, "unchecked:  aft".len()), "Y");
    assert_eq!(
        save(&archive, &edited),
        xml.replacen(" after", " aftYer", 1)
    );
    /* Deleting the text on both sides keeps both fields. */
    let len = archive.document.nth_paragraph(0).unwrap().text.len();
    let edited = archive.document.delete_range(at(0, 0), at(0, len));
    let out = save(&archive, &edited);
    assert!(out.contains(CHECKBOX) && out.contains(DROPDOWN), "{out}");
    let back = read_docx(&write_docx(&archive, &edited).unwrap()).unwrap();
    assert_eq!(back.document.paragraph_text(0), Some(""));
}

/// A form field inside a table cell survives a cell edit (cells parse
/// through the body run parser).
#[test]
fn form_field_in_a_table_cell_survives_a_cell_edit() {
    let body = format!(
        r#"<w:tbl><w:tblPr><w:tblW w:w="0" w:type="auto"/></w:tblPr><w:tblGrid><w:gridCol w:w="4000"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:tcW w:w="4000" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>cell</w:t></w:r>{CHECKBOX}</w:p></w:tc></w:tr></w:tbl><w:p/>"#
    );
    let (_, archive) = open(&body);
    let cell = LogicalPos {
        path: BlockPath::top(0)
            .push(PathStep::Cell { row: 0, col: 0 })
            .push(PathStep::Block(0)),
        offset: 0,
    };
    let edited = archive.document.insert_text(cell, "Z");
    let out = save(&archive, &edited);
    assert!(out.contains(CHECKBOX), "{out}");
    assert!(out.contains(">Zcell<"), "{out}");
}

/// Stale offsets (an edit path that does not remap the markup) never drop
/// a form field: it is written at its offset clamped to the text, and
/// the write records a note.
#[test]
fn stale_markup_keeps_the_form_field_and_notes_it() {
    let (_, archive) = open(&checkbox_paragraph());
    let mut doc = archive.document.clone();
    let Some(engine::Block::Paragraph(mut p)) = doc.blocks.get(0).cloned() else {
        panic!("paragraph");
    };
    p.text = "rewritten".into();
    p.spans.clear();
    p.dirty = true;
    p.source_xml = None;
    doc.blocks.set(0, engine::Block::Paragraph(p));
    let (bytes, notes) = write_docx_with_notes(&archive, &doc).expect("write");
    crate::check_document_xml_well_formed(&bytes).expect("well-formed");
    let out = document_xml_of(&bytes);
    assert!(out.contains(CHECKBOX) && out.contains(DROPDOWN), "{out}");
    assert!(
        !out.contains(r#"<w:bookmarkEnd w:id="0"/>"#),
        "verbatim markers stay dropped when stale"
    );
    assert_eq!(notes, vec![WriteNote::StaleMarkupClamped { markers: 2 }]);
    /* The plain write path records nothing and leaks nothing. */
    assert_eq!(write_docx(&archive, &doc).unwrap(), bytes);
}

/// A field with a result is still modeled (not swallowed as a span); a
/// zero-result field NESTED in another field's instruction is left to
/// the enclosing field.
#[test]
fn only_unmodeled_zero_result_fields_become_spans() {
    let body = concat!(
        r#"<w:p><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText> PAGE </w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>3</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p>"#,
        r#"<w:p><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText> IF </w:instrText></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText> MERGEFIELD x </w:instrText></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:instrText> = 1 </w:instrText></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p>"#,
    );
    let (_, archive) = open(body);
    let p0 = archive.document.nth_paragraph(0).unwrap();
    assert_eq!(p0.fields.len(), 1);
    let m0 = p0.source_markup.as_deref().unwrap();
    assert!(m0.markers.iter().all(|mk| mk.role.is_verbatim()));
    /* The outer IF has no result either: kept whole, the inner MERGEFIELD
    inside its bytes (one span, not two). */
    let p1 = archive.document.nth_paragraph(1).unwrap();
    let m1 = p1.source_markup.as_deref().unwrap();
    let spans: Vec<_> = m1
        .markers
        .iter()
        .filter(|mk| mk.role.must_survive())
        .collect();
    assert_eq!(spans.len(), 1, "{:?}", m1.markers);
    assert!(spans[0].xml.windows(10).any(|w| w == b"MERGEFIELD"));
    assert!(
        spans[0]
            .xml
            .ends_with(br#"<w:fldChar w:fldCharType="end"/></w:r>"#)
    );
}

/* ======================= issue #245 — run-level content controls ==== */

/// `Bug66263-paragraph.docx`'s shape (Apache POI, pretty-printed): text,
/// a run-level `<w:sdt>` whose run carries an rPr, text; then a
/// paragraph that starts and ends inside controls.
const SDT_P0: &str = r#"<w:p>
            <w:r><w:t>Before </w:t></w:r>
            <w:sdt>
                <w:sdtPr><w:id w:val="1001"/></w:sdtPr>
                <w:sdtContent>
                    <w:r>
                        <w:rPr><w:b w:val="on"/></w:rPr>
                        <w:t>SDT Run with RPr</w:t>
                    </w:r>
                </w:sdtContent>
            </w:sdt>
            <w:r><w:t> After</w:t></w:r>
        </w:p>"#;
const SDT_P1: &str = r#"<w:p>
            <w:sdt>
                <w:sdtPr><w:id w:val="1003"/></w:sdtPr>
                <w:sdtContent>
                    <w:r>
                        <w:rPr><w:b w:val="on"/></w:rPr>
                        <w:t>First</w:t>
                    </w:r>
                </w:sdtContent>
            </w:sdt>
            <w:r><w:t> Middle </w:t></w:r>
            <w:sdt>
                <w:sdtPr><w:id w:val="1004"/></w:sdtPr>
                <w:sdtContent>
                    <w:r>
                        <w:rPr><w:i w:val="on"/></w:rPr>
                        <w:t>Second</w:t>
                    </w:r>
                </w:sdtContent>
            </w:sdt>
        </w:p>"#;

/// `Bug64561.docx`'s shape (Word, tab-indented): nested controls around
/// one run, then a `_GoBack` bookmark.
const SDT_NESTED: &str = "<w:p w:rsidR=\"005828DB\">\n\t\t\t<w:sdt>\n\t\t\t\t<w:sdtPr>\n\t\t\t\t\t<w:alias w:val=\"subject[@list=1]\"/>\n\t\t\t\t\t<w:id w:val=\"1332796321\"/>\n\t\t\t\t</w:sdtPr>\n\t\t\t\t<w:sdtContent>\n\t\t\t\t\t<w:sdt>\n\t\t\t\t\t\t<w:sdtPr>\n\t\t\t\t\t\t\t<w:alias w:val=\"subjectline\"/>\n\t\t\t\t\t\t\t<w:id w:val=\"614486968\"/>\n\t\t\t\t\t\t</w:sdtPr>\n\t\t\t\t\t\t<w:sdtContent>\n\t\t\t\t\t\t\t<w:r>\n\t\t\t\t\t\t\t\t<w:t>Subject</w:t>\n\t\t\t\t\t\t\t</w:r>\n\t\t\t\t\t\t</w:sdtContent>\n\t\t\t\t\t</w:sdt>\n\t\t\t\t</w:sdtContent>\n\t\t\t</w:sdt>\n\t\t\t<w:bookmarkStart w:id=\"0\" w:name=\"_GoBack\"/>\n\t\t\t<w:bookmarkEnd w:id=\"0\"/>\n\t\t</w:p>";

fn sdt_roles(p: &engine::Paragraph) -> Vec<(u32, &'static str)> {
    p.source_markup
        .as_deref()
        .unwrap()
        .markers
        .iter()
        .filter_map(|mk| match mk.role {
            engine::MarkerRole::Open { .. } => Some((mk.at, "open")),
            engine::MarkerRole::Close { .. } => Some((mk.at, "close")),
            _ => None,
        })
        .collect()
}

/// The wrapper is an opener / closer pair around the control's text; the
/// text itself stays paragraph content.
#[test]
fn run_level_sdt_reads_as_a_positioned_wrapper() {
    let (xml, archive) = open(&format!("{SDT_P0}{SDT_P1}{SDT_NESTED}"));
    let p0 = archive.document.nth_paragraph(0).unwrap();
    assert_eq!(p0.text, "Before SDT Run with RPr After");
    assert_eq!(sdt_roles(p0), vec![(7, "open"), (23, "close")]);
    let p1 = archive.document.nth_paragraph(1).unwrap();
    assert_eq!(
        sdt_roles(p1),
        vec![(0, "open"), (5, "close"), (13, "open"), (19, "close")]
    );
    let p2 = archive.document.nth_paragraph(2).unwrap();
    assert_eq!(p2.text, "Subject");
    assert_eq!(
        sdt_roles(p2),
        vec![(0, "open"), (0, "open"), (7, "close"), (7, "close")]
    );
    assert_eq!(save(&archive, &archive.document), xml, "zero-edit");
}

/// Edits outside, inside and at both ends of a control regenerate the
/// (pretty-printed) paragraph as exactly source + the inserted bytes.
#[test]
fn run_level_sdt_survives_edits_outside_and_inside() {
    let (xml, archive) = open(&format!("{SDT_P0}{SDT_P1}{SDT_NESTED}"));
    let cases: [(u32, usize, &str, &str); 7] = [
        /* Outside, before the control. */
        (0, 3, ">Before <", ">BefINSore <"),
        /* Inside the control's run. */
        (0, 10, ">SDT Run with RPr<", ">SDTINS Run with RPr<"),
        /* At the control's end: inside the control. `insert_text` does
        not extend the bold span at its end, so the text is its own run
        (the source run's attributes and whitespace, no rPr). */
        (
            0,
            23,
            "RPr</w:t>\n                    </w:r>",
            "RPr</w:t>\n                    </w:r><w:r>\n                        <w:t>INS</w:t>\n                    </w:r>",
        ),
        /* Outside, after the control. */
        (0, 26, "> After<", "> AfINSter<"),
        /* Inside the nested controls. */
        (2, 3, ">Subject<", ">SubINSject<"),
        (2, 7, ">Subject<", ">SubjectINS<"),
        /* Between two controls (the plain run keeps its bare `<w:t>`). */
        (1, 8, "> Middle <", "> MiINSddle <"),
    ];
    for (block, offset, from, to) in cases {
        let edited = archive.document.insert_text(at(block, offset), "INS");
        assert_eq!(
            save(&archive, &edited),
            xml.replacen(from, to, 1),
            "insert at {block}:{offset}"
        );
    }
}

/// Deleting all of a control's text keeps the (now empty) control;
/// splitting inside a control keeps both halves well-formed (the left
/// half's control closes at its end; the orphaned closer is dropped).
#[test]
fn run_level_sdt_stays_well_formed_through_delete_and_split() {
    let (_, archive) = open(SDT_P0);
    let edited = archive.document.delete_range(at(0, 7), at(0, 23));
    let out = save(&archive, &edited);
    assert!(
        out.contains("<w:sdtPr><w:id w:val=\"1001\"/></w:sdtPr>"),
        "{out}"
    );
    assert_eq!(out.matches("</w:sdt>").count(), 1, "{out}");

    let split = archive.document.split_paragraph(at(0, 12));
    let out = save(&archive, &split);
    assert_eq!(out.matches("<w:sdt>").count(), 1, "{out}");
    assert_eq!(out.matches("</w:sdt>").count(), 1, "{out}");
    let back = read_docx(&write_docx(&archive, &split).unwrap()).unwrap();
    assert_eq!(back.document.paragraph_text(0), Some("Before SDT R"));
    assert_eq!(back.document.paragraph_text(1), Some("un with RPr After"));
}

/// A control that shares its start with a regenerated wrapper extending
/// past its end (here an internal hyperlink around the control and more
/// text) cannot be written in the natural order; it is widened to
/// enclose the wrapper, noted, and the part stays well-formed.
#[test]
fn run_level_sdt_crossing_a_wrapper_is_widened_and_noted() {
    let body = r#"<w:p><w:hyperlink w:anchor="target"><w:sdt><w:sdtPr><w:id w:val="9"/></w:sdtPr><w:sdtContent><w:r><w:t>inside</w:t></w:r></w:sdtContent></w:sdt><w:r><w:t>linked</w:t></w:r></w:hyperlink><w:r><w:t>tail</w:t></w:r></w:p>"#;
    let (_, archive) = open(body);
    let edited = archive.document.insert_text(at(0, 13), "X");
    let (bytes, notes) = write_docx_with_notes(&archive, &edited).expect("write");
    crate::check_document_xml_well_formed(&bytes).expect("well-formed");
    let out = document_xml_of(&bytes);
    assert!(
        out.contains(
            r#"<w:sdt><w:sdtPr><w:id w:val="9"/></w:sdtPr><w:sdtContent><w:hyperlink w:anchor="target""#
        ),
        "{out}"
    );
    assert!(
        out.contains("</w:hyperlink></w:sdtContent></w:sdt>"),
        "{out}"
    );
    assert!(
        matches!(notes.as_slice(), [WriteNote::InlineWrapperWidened { .. }]),
        "{notes:?}"
    );
    let back = read_docx(&bytes).unwrap();
    assert_eq!(back.document.paragraph_text(0), Some("insidelinkedtXail"));
}

/// Stale offsets keep the control (clamped) and note it.
#[test]
fn stale_markup_keeps_the_content_control() {
    let (_, archive) = open(SDT_P0);
    let mut doc = archive.document.clone();
    let Some(engine::Block::Paragraph(mut p)) = doc.blocks.get(0).cloned() else {
        panic!("paragraph");
    };
    p.text = "short".into();
    p.spans.clear();
    p.dirty = true;
    p.source_xml = None;
    doc.blocks.set(0, engine::Block::Paragraph(p));
    let (bytes, notes) = write_docx_with_notes(&archive, &doc).expect("write");
    crate::check_document_xml_well_formed(&bytes).expect("well-formed");
    let out = document_xml_of(&bytes);
    assert_eq!(out.matches("<w:sdt>").count(), 1, "{out}");
    assert_eq!(notes, vec![WriteNote::StaleMarkupClamped { markers: 2 }]);
}

/* ============================ issue #246 — simple fields ==== */

/// `FldSimple.docx`'s shape: a `<w:fldSimple>` whose instruction carries
/// its own spacing, a result run with an rPr, then a `_GoBack` bookmark.
const FLD_SIMPLE_P: &str = r#"<w:p w14:paraId="5E924D5F" w14:textId="5C251B6F" w:rsidR="00545A56" w:rsidRDefault="006B3937"><w:fldSimple w:instr=" FILENAME   \* MERGEFORMAT "><w:r><w:rPr><w:noProof/></w:rPr><w:t>FldSimple.docx</w:t></w:r></w:fldSimple><w:bookmarkStart w:id="0" w:name="_GoBack"/><w:bookmarkEnd w:id="0"/></w:p>"#;

/// The field remembers its simple form; edits after it, inside its
/// result and at its start are exactly source + the inserted bytes — the
/// `<w:fldSimple>` element and the `_GoBack` bookmark survive.
#[test]
fn simple_field_keeps_its_form_through_edits() {
    let (xml, archive) = open(FLD_SIMPLE_P);
    let p = archive.document.nth_paragraph(0).unwrap();
    assert_eq!(p.fields.len(), 1);
    let src = p.fields[0].source.as_deref().expect("source form");
    assert_eq!(
        src.open,
        br#"<w:fldSimple w:instr=" FILENAME   \* MERGEFORMAT ">"#
    );
    assert_eq!(src.close, b"</w:fldSimple>");
    assert_eq!(save(&archive, &archive.document), xml, "zero-edit");

    let end = "FldSimple.docx".len();
    let edited = archive.document.insert_text(at(0, end), " X");
    assert_eq!(
        save(&archive, &edited),
        xml.replacen(
            "</w:fldSimple>",
            /* `insert_text` does not extend the noProof span at its end:
            the typed text is plain, in its own run. */
            r#"</w:fldSimple><w:r><w:t xml:space="preserve"> X</w:t></w:r>"#,
            1
        )
    );
    let edited = archive.document.insert_text(at(0, 3), "INS");
    assert_eq!(
        save(&archive, &edited),
        xml.replacen(">FldSimple.docx<", ">FldINSSimple.docx<", 1)
    );
}

/// The #246 drop: a field restamp (the live editor's FILENAME / PAGE
/// resolution on F9 / save) used to leave the source markup stale, so
/// every positioned marker — the `_GoBack` bookmark after the field — was
/// dropped. The restamp now remaps the markup (issues #250 / #252), and
/// the restamped field keeps its simple form.
#[test]
fn restamped_simple_field_keeps_its_form_and_the_bookmark_after_it() {
    let (xml, archive) = open(FLD_SIMPLE_P);
    let doc = archive
        .document
        .restamp_fields(&mut |_| Some("Renamed.docx".to_string()));
    let p = doc.nth_paragraph(0).unwrap();
    assert_eq!(p.text, "Renamed.docx");
    assert!(
        p.source_markup
            .as_deref()
            .unwrap()
            .offsets_valid(p.text.len())
    );
    assert_eq!(
        save(&archive, &doc),
        xml.replacen(">FldSimple.docx<", ">Renamed.docx<", 1)
    );
}

/// A changed instruction regenerates the standard complex form (the
/// source bytes spell the old one).
#[test]
fn simple_field_with_a_new_instruction_regenerates() {
    let (_, archive) = open(FLD_SIMPLE_P);
    let mut doc = archive.document.clone();
    let Some(engine::Block::Paragraph(mut p)) = doc.blocks.get(0).cloned() else {
        panic!("paragraph");
    };
    p.fields[0].instruction = "PAGE".into();
    p.dirty = true;
    p.source_xml = None;
    doc.blocks.set(0, engine::Block::Paragraph(p));
    let out = save(&archive, &doc);
    assert!(!out.contains("<w:fldSimple"), "{out}");
    assert!(
        out.contains(r#"<w:fldChar w:fldCharType="begin"/>"#),
        "{out}"
    );
    assert!(out.contains(r#"w:name="_GoBack""#), "{out}");
}

/// A `<w:fldSimple>` that would cross a regenerated wrapper (a hyperlink
/// sharing its start but ending inside it) falls back to the complex
/// form, which has no element to unbalance.
#[test]
fn simple_field_crossing_a_wrapper_falls_back_to_the_complex_form() {
    let body = r#"<w:p><w:fldSimple w:instr="PAGE"><w:hyperlink w:anchor="a"><w:r><w:t>12</w:t></w:r></w:hyperlink><w:r><w:t>345</w:t></w:r></w:fldSimple></w:p>"#;
    let (_, archive) = open(body);
    let edited = archive.document.insert_text(at(0, 5), "X");
    let out = save(&archive, &edited);
    assert!(!out.contains("<w:fldSimple"), "{out}");
    assert!(
        out.contains(r#"<w:fldChar w:fldCharType="begin"/>"#),
        "{out}"
    );
}

/// A complex field WITH a result keeps its source prologue (the begin run
/// with its `<w:ffData>` — a `FORMTEXT` — the name bookmark, the
/// instruction runs) and its end run through an edit inside the result.
#[test]
fn complex_field_keeps_its_source_prologue_and_end_run() {
    let field_open = concat!(
        r#"<w:r w:rsidR="00A1"><w:fldChar w:fldCharType="begin"><w:ffData><w:name w:val="Text1"/><w:enabled/><w:textInput/></w:ffData></w:fldChar></w:r>"#,
        r#"<w:bookmarkStart w:id="4" w:name="Text1"/>"#,
        r#"<w:r w:rsidR="00A1"><w:instrText xml:space="preserve"> FORMTEXT </w:instrText></w:r>"#,
        r#"<w:r w:rsidR="00A1"><w:fldChar w:fldCharType="separate"/></w:r>"#,
    );
    let field_close = r#"<w:r w:rsidR="00A1"><w:fldChar w:fldCharType="end"/></w:r>"#;
    let body = format!(
        r#"<w:p><w:r><w:t xml:space="preserve">Name: </w:t></w:r>{field_open}<w:r w:rsidR="00B2"><w:t>typed</w:t></w:r>{field_close}<w:bookmarkEnd w:id="4"/></w:p>"#
    );
    let (xml, archive) = open(&body);
    let p = archive.document.nth_paragraph(0).unwrap();
    assert_eq!(p.fields.len(), 1);
    assert_eq!(p.fields[0].instruction, "FORMTEXT");
    let src = p.fields[0].source.as_deref().expect("source form");
    assert_eq!(src.open, field_open.as_bytes());
    assert_eq!(src.close, field_close.as_bytes());
    assert_eq!(save(&archive, &archive.document), xml, "zero-edit");
    let edited = archive.document.insert_text(at(0, "Name: ty".len()), "INS");
    assert_eq!(
        save(&archive, &edited),
        xml.replacen(">typed<", ">tyINSped<", 1)
    );
    let edited = archive.document.insert_text(at(0, 2), "Z");
    assert_eq!(
        save(&archive, &edited),
        xml.replacen("Name: ", "NaZme: ", 1)
    );
}
