//! Issues #536 / #530 (and the closed #491, #492, #511) — a block the body
//! walk meets where no block may stand: a `<w:p>` before or after
//! `<w:body>`, an UNCLOSED `<w:p>` before it (the EOF repair makes it enclose
//! the body), a `<w:p>` nested in another paragraph (the same repair nests the
//! paragraphs that follow an unclosed one), a `<w:tbl>` in those places. The
//! walk used to take such an element for a block while its bytes ALSO rode the
//! part envelope or the enclosing paragraph's verbatim capture, so a zero-edit
//! save wrote the content twice (#536 cluster) or wrote the inner paragraph's
//! bytes for the outer paragraph's text (#530). The ONE placement rule
//! (`parts::document::misplaced_block`) refuses the part instead.
//!
//! Every test runs the `docx_roundtrip` fuzz invariant itself
//! ([`zero_edit_round_trip`]): an open that succeeds is saved with no edit and
//! read back, and the text after the round trip must equal the text after the
//! read; an open that fails is a refusal, and there is no save to drift.

use crate::error::DocxError;
use crate::opc::archive::read_docx;
use crate::style_resolver::StyleResolver;
use crate::test_fixtures::{alternate_content_text_box, package_with_document_xml_bytes};
use crate::writer::write_docx;

/// The fuzz reproducers' packages, exactly as the `docx_roundtrip` generator
/// built them from the nightly inputs (`fuzz/corpus/docx_roundtrip/repro_*`
/// are those inputs) — pinned here so the shapes outlive generator changes.
const ISSUE_491: &[u8] = include_bytes!("../tests/fixtures/malformed/issue_491.docx");
const ISSUE_492: &[u8] = include_bytes!("../tests/fixtures/malformed/issue_492.docx");
const ISSUE_511: &[u8] = include_bytes!("../tests/fixtures/malformed/issue_511.docx");
const ISSUE_530: &[u8] = include_bytes!("../tests/fixtures/malformed/issue_530.docx");
const ISSUE_536: &[u8] = include_bytes!("../tests/fixtures/malformed/issue_536.docx");

const ROOT: &str = concat!(
    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
    r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" "#,
    r#"xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" "#,
    r#"xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006" "#,
    r#"xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" "#,
    r#"xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" "#,
    r#"xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape" "#,
    r#"xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml" "#,
    r#"xmlns:v="urn:schemas-microsoft-com:vml" xmlns:x="urn:example:unmodeled">"#,
);

const SECRET: &str = "zzsecret";

fn para(text: &str) -> String {
    format!("<w:p><w:r><w:t>{text}</w:t></w:r></w:p>")
}

const TABLE: &str = concat!(
    r#"<w:tbl><w:tblPr/><w:tblGrid><w:gridCol w:w="2000"/></w:tblGrid>"#,
    r#"<w:tr><w:tc><w:p><w:r><w:t>cell</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#,
);

/// A package whose `word/document.xml` is `ROOT` + `inner` (the caller spells
/// `<w:body>` and the end tags — or leaves them off for a truncated part).
fn package(inner: &str) -> Vec<u8> {
    package_with_document_xml_bytes(format!("{ROOT}{inner}").as_bytes(), &[])
}

#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    /// The open succeeded and read => write => read kept this text.
    Kept(String),
    /// The open was refused as damaged (`DocxError::MalformedXml`).
    Refused(String),
}

/// The `docx_roundtrip` invariant (`fuzz/src/lib.rs` `check_roundtrip`) over
/// one package.
fn zero_edit_round_trip(docx: &[u8]) -> Outcome {
    let a = match read_docx(docx) {
        Ok(a) => a,
        Err(DocxError::MalformedXml(detail)) => return Outcome::Refused(detail),
        Err(e) => panic!("expected an open or a MalformedXml refusal, got {e:?}"),
    };
    let text = a.document.to_plain_text();
    let saved = write_docx(&a, &a.document).expect("a zero-edit save");
    crate::check_document_xml_well_formed(&saved).expect("the save is well-formed");
    let b = read_docx(&saved).expect("the writer's own output re-reads");
    assert_eq!(
        b.document.to_plain_text(),
        text,
        "text after the round trip != text after the read"
    );
    Outcome::Kept(text)
}

/// `docx` is refused with the placement rule's detail for a `block` that
/// stands `place`; the detail names the part and never the document's text.
fn assert_misplaced(docx: &[u8], block: &str, place: &str) {
    let Outcome::Refused(detail) = zero_edit_round_trip(docx) else {
        panic!("expected a refusal for a {block} {place}");
    };
    assert!(
        detail.starts_with(&format!("word/document.xml: a {block} at byte ")),
        "{detail}"
    );
    assert!(
        detail.contains(&format!("stands {place}, where no block may stand")),
        "{detail}"
    );
    assert!(!detail.contains(SECRET), "{detail}");
}

/// The nightly reproducers: before the rule each opened and its zero-edit save
/// read back different text (#492: the stray empty paragraph twice; #491 /
/// #511 / #536: the tables twice; #530: "helloworld" lost). Each is now
/// refused, so the invariant holds with no save at all.
#[test]
fn the_fuzz_reproducers_are_refused_not_drifted() {
    for (issue, docx, place) in [
        (491, ISSUE_491, "outside the document body"),
        (492, ISSUE_492, "outside the document body"),
        (511, ISSUE_511, "outside the document body"),
        (536, ISSUE_536, "outside the document body"),
        (530, ISSUE_530, "inside a paragraph"),
    ] {
        let Outcome::Refused(detail) = zero_edit_round_trip(docx) else {
            panic!("#{issue}: expected the open to be refused");
        };
        assert!(
            detail.starts_with("word/document.xml: a paragraph at byte ") && detail.contains(place),
            "#{issue}: {detail}"
        );
    }
}

/// #492 — a CLOSED paragraph before `<w:body>`: it was block 0 AND the
/// envelope's `root_to_body` bytes, so the save emitted it twice.
#[test]
fn a_paragraph_before_the_body_is_refused() {
    let body = format!("<w:body>{}<w:sectPr/></w:body></w:document>", para("a"));
    assert_misplaced(
        &package(&format!("{}{body}", para(SECRET))),
        "paragraph",
        "outside the document body",
    );
    assert_misplaced(
        &package(&format!("<w:p/>{body}")),
        "paragraph",
        "outside the document body",
    );
    /* After the body as well. */
    assert_misplaced(
        &package(&format!(
            "<w:body>{}</w:body>{}</w:document>",
            para("a"),
            para(SECRET)
        )),
        "paragraph",
        "outside the document body",
    );
    /* The same document without the stray paragraph opens and round-trips. */
    assert_eq!(
        zero_edit_round_trip(&package(&body)),
        Outcome::Kept("a".into())
    );
}

/// #536 / #491 / #511 — an UNCLOSED paragraph before `<w:body>` in a part that
/// stops early: the EOF repair closes it after `</w:body>`, so it ENCLOSES the
/// body. Its verbatim bytes (the whole body) were replayed for a clean
/// paragraph while the body's blocks were ALSO read: the tables twice.
#[test]
fn an_unclosed_paragraph_enclosing_the_body_is_refused() {
    assert_misplaced(
        &package(&format!(
            "<w:p><w:r><w:t>{SECRET}</w:t></w:r><w:body>{TABLE}{}",
            para("after")
        )),
        "paragraph",
        "outside the document body",
    );
    /* The same truncated part without the stray start tag is repaired and
    opens: the EOF repair alone is not what is refused. */
    assert_eq!(
        zero_edit_round_trip(&package(&format!("<w:body>{TABLE}{}", para("after")))),
        Outcome::Kept("cell\nafter".into())
    );
}

/// #530 — the EOF repair NESTS the paragraphs that follow an unclosed one. The
/// inner `<w:p>` start overwrote the outer paragraph's start offset, so the
/// outer paragraph's text was saved with the inner paragraph's bytes (and
/// lost). A nested paragraph a well-formed part spells through an element the
/// reader does not model is the same defect.
#[test]
fn a_paragraph_inside_a_paragraph_is_refused() {
    assert_misplaced(
        &package(&format!(
            "<w:body><w:p><w:r><w:t>hello</w:t></w:r><w:p><w:r><w:t>{SECRET}</w:t></w:r>"
        )),
        "paragraph",
        "inside a paragraph",
    );
    assert_misplaced(
        &package(&format!(
            "<w:body><w:p><w:r><w:t>hello</w:t></w:r><x:junk>{}</x:junk></w:p><w:sectPr/></w:body></w:document>",
            para(SECRET)
        )),
        "paragraph",
        "inside a paragraph",
    );
    assert_misplaced(
        &package(
            "<w:body><w:p><w:r><w:t>hello</w:t></w:r><w:p/></w:p><w:sectPr/></w:body></w:document>",
        ),
        "paragraph",
        "inside a paragraph",
    );
}

/// The same rule for a table (it was a block wherever the walk met it), and
/// for a paragraph inside the body's section properties.
#[test]
fn a_table_or_paragraph_where_no_block_may_stand_is_refused() {
    let body = format!("<w:body>{}<w:sectPr/></w:body></w:document>", para("a"));
    assert_misplaced(
        &package(&format!("{TABLE}{body}")),
        "table",
        "outside the document body",
    );
    assert_misplaced(
        &package(&format!(
            "<w:body><w:p><w:r><w:t>{SECRET}</w:t></w:r>{TABLE}</w:p><w:sectPr/></w:body></w:document>"
        )),
        "table",
        "inside a paragraph",
    );
    assert_misplaced(
        &package(&format!(
            "<w:body>{}<w:sectPr>{}</w:sectPr></w:body></w:document>",
            para("a"),
            para(SECRET)
        )),
        "paragraph",
        "inside section properties",
    );
}

/// Every place a block legitimately stands — and every story whose
/// paragraphs live inside a run — still opens, and keeps its text through a
/// zero-edit save: block-level content controls, custom XML and
/// `mc:AlternateContent`, a paragraph-level `mc:AlternateContent`, text-box
/// stories (DrawingML and VML), nested tables, `<w:background>` before the
/// body.
#[test]
fn blocks_where_blocks_stand_are_untouched() {
    let cases = [
        format!(
            "<w:background w:color=\"FFFFFF\"/><w:body>{}<w:sectPr/></w:body></w:document>",
            para("a")
        ),
        format!(
            "<w:body><w:sdt><w:sdtPr/><w:sdtContent>{}{TABLE}</w:sdtContent></w:sdt>{}<w:sectPr/></w:body></w:document>",
            para("in sdt"),
            para("b")
        ),
        format!(
            "<w:body><w:customXml w:element=\"e\">{}</w:customXml><w:sectPr/></w:body></w:document>",
            para("in custom xml")
        ),
        format!(
            "<w:body><mc:AlternateContent><mc:Choice Requires=\"w14\">{}</mc:Choice><mc:Fallback>{}</mc:Fallback></mc:AlternateContent><w:sectPr/></w:body></w:document>",
            para("choice"),
            para("fallback")
        ),
        "<w:body><w:p><w:r><w:t>x</w:t></w:r><mc:AlternateContent><mc:Choice Requires=\"w14\"><w:r><w:t>y</w:t></w:r></mc:Choice><mc:Fallback><w:r><w:t>z</w:t></w:r></mc:Fallback></mc:AlternateContent></w:p><w:sectPr/></w:body></w:document>"
            .to_string(),
        format!(
            "<w:body><w:p><w:r><w:t>host</w:t></w:r><w:r>{}</w:r></w:p><w:sectPr/></w:body></w:document>",
            alternate_content_text_box("wps")
        ),
        format!(
            "<w:body><w:tbl><w:tblGrid><w:gridCol w:w=\"2000\"/></w:tblGrid><w:tr><w:tc>{TABLE}{}</w:tc></w:tr></w:tbl>{}<w:sectPr/></w:body></w:document>",
            para("outer cell"),
            para("end")
        ),
    ];
    for inner in &cases {
        match zero_edit_round_trip(&package(inner)) {
            Outcome::Kept(text) => assert!(!text.is_empty(), "{inner}"),
            Outcome::Refused(detail) => panic!("{inner}\nwas refused: {detail}"),
        }
    }
}

/// A header / footer part is walked by the same rule: a misplaced paragraph
/// fails the part's walk (the package read then treats the part as absent,
/// like any header that does not parse), a well-placed one reads.
#[test]
fn a_header_part_obeys_the_same_rule() {
    let table = crate::parts::styles::StyleTable::default();
    let resolver = StyleResolver::new(&table);
    let hdr = |inner: &str| {
        format!(
            "<w:hdr xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">{inner}</w:hdr>"
        )
    };
    let ok = crate::parts::header::parse_header_xml(hdr(&para("head")).as_bytes(), &resolver)
        .expect("a well-placed header paragraph reads");
    assert_eq!(ok.blocks.len(), 1);
    let nested = format!("<w:p><w:r><w:t>a</w:t></w:r>{}</w:p>", para("b"));
    let err = crate::parts::header::parse_header_xml(hdr(&nested).as_bytes(), &resolver)
        .expect_err("a nested header paragraph is refused");
    assert!(
        matches!(&err, DocxError::MalformedXml(d) if d.contains("inside a paragraph")),
        "{err:?}"
    );
}

/* ---- Issue #564: the table walker asks the same rule for a cell's blocks ---- */

/// A table with one cell holding `cell` (the caller spells the cell's
/// blocks), as the body's only block.
fn table_in_body(cell: &str) -> String {
    format!(
        "<w:body><w:tbl><w:tblPr/><w:tblGrid><w:gridCol w:w=\"2000\"/></w:tblGrid>\
         <w:tr><w:tc>{cell}</w:tc></w:tr></w:tbl><w:sectPr/></w:body></w:document>"
    )
}

/// `inner` is refused as a `block` standing `place`, and the offset the
/// refusal names is where `needle` stands in the part (`ROOT` + `inner`),
/// not a position in the table's own bytes.
fn assert_refused_at(inner: &str, needle: &str, block: &str, place: &str) {
    let part = format!("{ROOT}{inner}");
    let at = part.find(needle).expect("the needle is in the part");
    let Outcome::Refused(detail) = zero_edit_round_trip(&package(inner)) else {
        panic!("expected a refusal for a {block} {place}: {inner}");
    };
    assert_eq!(
        detail,
        format!(
            "word/document.xml: a {block} at byte {at} stands {place}, where no block may \
             stand (no faithful repair)"
        )
    );
    assert!(!detail.contains(SECRET), "{detail}");
}

/// #564 — a `<w:p>` nested in a CELL paragraph (through an element the reader
/// does not model, or the EOF repair's nesting): the cell walker's start arm
/// overwrote the paragraph's start offset, so the cell read as the INNER
/// paragraph only and `outer` / `tail` were dropped, silently. The same shape
/// in the body is refused (#530); the cell now is too.
#[test]
fn a_paragraph_inside_a_cell_paragraph_is_refused() {
    let inner = table_in_body(&format!(
        "<w:p><w:r><w:t>outer</w:t></w:r><x:junk>{}</x:junk><w:r><w:t>tail</w:t></w:r></w:p>",
        para(SECRET)
    ));
    assert_refused_at(
        &inner,
        &format!("<w:p><w:r><w:t>{SECRET}"),
        "paragraph",
        "inside a paragraph",
    );
    /* A self-closing nested paragraph, and the EOF repair's nesting (an
    unclosed cell paragraph followed by the next one). */
    assert_refused_at(
        &table_in_body("<w:p><w:r><w:t>outer</w:t></w:r><w:p/></w:p>"),
        "<w:p/>",
        "paragraph",
        "inside a paragraph",
    );
    assert_refused_at(
        &format!(
            "<w:body><w:tbl><w:tblPr/><w:tblGrid><w:gridCol w:w=\"2000\"/></w:tblGrid>\
             <w:tr><w:tc><w:p><w:r><w:t>outer</w:t></w:r><w:p><w:r><w:t>{SECRET}</w:t></w:r>"
        ),
        &format!("<w:p><w:r><w:t>{SECRET}"),
        "paragraph",
        "inside a paragraph",
    );
    /* The same cell without the nested paragraph opens and keeps its text. */
    assert_eq!(
        zero_edit_round_trip(&package(&table_in_body(
            "<w:p><w:r><w:t>outer</w:t></w:r><w:r><w:t>tail</w:t></w:r></w:p>"
        ))),
        Outcome::Kept("outertail".into())
    );
}

/// #564 — a `<w:tbl>` in a cell paragraph was a cell block while its bytes
/// ALSO rode the paragraph's capture.
#[test]
fn a_table_inside_a_cell_paragraph_is_refused() {
    let inner = table_in_body(&format!("<w:p><w:r><w:t>{SECRET}</w:t></w:r>{TABLE}</w:p>"));
    assert_refused_at(&inner, TABLE, "table", "inside a paragraph");
}

/// #564 — blocks of a table that stand in no cell (a `<w:p>` directly in the
/// `<w:tbl>` or a `<w:tr>`, a `<w:tbl>` there) or in a cell's `<w:tcPr>`.
#[test]
fn a_block_in_a_table_outside_every_cell_is_refused() {
    let grid = "<w:tblPr/><w:tblGrid><w:gridCol w:w=\"2000\"/></w:tblGrid>";
    let cell = "<w:tc><w:p><w:r><w:t>cell</w:t></w:r></w:p></w:tc>";
    let tail = "<w:sectPr/></w:body></w:document>";
    let secret_p = format!("<w:p><w:r><w:t>{SECRET}");
    /* (the table's inner xml, the needle the refusal names, block, place) */
    let cases = [
        (
            format!("{grid}{}<w:tr>{cell}</w:tr>", para(SECRET)),
            secret_p.clone(),
            "paragraph",
            "outside a table cell",
        ),
        (
            format!("{grid}<w:tr>{}{cell}</w:tr>", para(SECRET)),
            secret_p.clone(),
            "paragraph",
            "outside a table cell",
        ),
        (
            format!("{grid}<w:tr><w:p/>{cell}</w:tr>"),
            "<w:p/>".to_string(),
            "paragraph",
            "outside a table cell",
        ),
        (
            format!("{grid}<w:tr><w:trPr>{}</w:trPr>{cell}</w:tr>", para(SECRET)),
            secret_p.clone(),
            "paragraph",
            "outside a table cell",
        ),
        (
            format!("{grid}<w:tr>{TABLE}{cell}</w:tr>"),
            TABLE.to_string(),
            "table",
            "outside a table cell",
        ),
        (
            format!(
                "{grid}<w:tr><w:tc><w:tcPr><w:tcW w:w=\"2000\" w:type=\"dxa\"/>{}</w:tcPr>\
                 <w:p><w:r><w:t>cell</w:t></w:r></w:p></w:tc></w:tr>",
                para(SECRET)
            ),
            secret_p,
            "paragraph",
            "inside cell properties",
        ),
        (
            format!("{grid}<w:tr><w:tc><w:tcPr><w:p/></w:tcPr><w:p/></w:tc></w:tr>"),
            "<w:p/>".to_string(),
            "paragraph",
            "inside cell properties",
        ),
    ];
    for (table, needle, block, place) in cases {
        let inner = format!("<w:body><w:tbl>{table}</w:tbl>{tail}");
        assert_refused_at(&inner, &needle, block, place);
    }
}

/// #564 — a nested table is walked by its own recursion, and its refusal is
/// the part's (the second swallow point), with the offset counted from the
/// part's start through both tables' bases.
#[test]
fn a_misplaced_block_in_a_nested_table_is_refused_with_a_part_relative_offset() {
    let nested = format!(
        "<w:tbl><w:tblPr/><w:tblGrid><w:gridCol w:w=\"1000\"/></w:tblGrid><w:tr><w:tc>\
         <w:p><w:r><w:t>a</w:t></w:r><x:junk>{}</x:junk></w:p></w:tc></w:tr></w:tbl>",
        para(SECRET)
    );
    let inner = table_in_body(&format!("<w:p><w:r><w:t>lead</w:t></w:r></w:p>{nested}"));
    assert_refused_at(
        &inner,
        &format!("<w:p><w:r><w:t>{SECRET}"),
        "paragraph",
        "inside a paragraph",
    );
}

/// #564 — the third swallow point: a paragraph-level `mc:AlternateContent`
/// the table walk skips whole is parsed by the cell paragraph's own body
/// walk, whose placement refusal used to fold into an opaque paragraph (the
/// text kept in the table's bytes, the paragraph unread). It refuses the
/// table now, naming the cell paragraph's part-relative offset.
#[test]
fn a_block_in_a_cell_paragraphs_alternate_content_is_refused() {
    let cell_p = format!(
        "<w:p><w:r><w:t>outer</w:t></w:r><mc:AlternateContent><mc:Choice Requires=\"w14\">{}\
         </mc:Choice><mc:Fallback><w:r><w:t>fb</w:t></w:r></mc:Fallback></mc:AlternateContent></w:p>",
        para(SECRET)
    );
    let inner = table_in_body(&cell_p);
    let part = format!("{ROOT}{inner}");
    let at = part
        .find("<w:p><w:r><w:t>outer")
        .expect("the cell paragraph");
    let Outcome::Refused(detail) = zero_edit_round_trip(&package(&inner)) else {
        panic!("expected a refusal");
    };
    assert_eq!(
        detail,
        format!(
            "word/document.xml: a block inside the paragraph at byte {at} stands where no \
             block may stand (no faithful repair)"
        )
    );
    assert!(!detail.contains(SECRET), "{detail}");
}

/// #564 — the same rule in a header part's table (the header then reads as
/// absent, like any header that does not parse).
#[test]
fn a_header_tables_cell_obeys_the_same_rule() {
    let table = crate::parts::styles::StyleTable::default();
    let resolver = StyleResolver::new(&table);
    let hdr = |inner: &str| {
        format!(
            "<w:hdr xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">{inner}</w:hdr>"
        )
    };
    let cell = |c: &str| {
        format!(
            "<w:tbl><w:tblPr/><w:tblGrid><w:gridCol w:w=\"2000\"/></w:tblGrid><w:tr><w:tc>{c}</w:tc></w:tr></w:tbl>"
        )
    };
    crate::parts::header::parse_header_xml(hdr(&cell(&para("head"))).as_bytes(), &resolver)
        .expect("a well-placed header cell paragraph reads");
    let nested = cell(&format!("<w:p><w:r><w:t>a</w:t></w:r>{}</w:p>", para("b")));
    let err = crate::parts::header::parse_header_xml(hdr(&nested).as_bytes(), &resolver)
        .expect_err("a nested cell paragraph in a header is refused");
    assert!(
        matches!(&err, DocxError::MalformedXml(d) if d.contains("inside a paragraph")),
        "{err:?}"
    );
}

/// #564 — the placement rule must not touch what Word writes: a cell's own
/// blocks (paragraphs, a nested table, a content control around them, an
/// `mc:AlternateContent` between them, a text box's own paragraphs inside a
/// cell paragraph) and a `<w:tcPr>` before them.
#[test]
fn blocks_where_cell_blocks_stand_are_untouched() {
    let tc_pr = "<w:tcPr><w:tcW w:w=\"2000\" w:type=\"dxa\"/></w:tcPr>";
    let cells = [
        format!("{tc_pr}{}{}", para("one"), para("two")),
        format!("{tc_pr}{TABLE}{}", para("after nested")),
        format!(
            "{tc_pr}<w:sdt><w:sdtPr/><w:sdtContent>{}{TABLE}</w:sdtContent></w:sdt>{}",
            para("in sdt"),
            para("end")
        ),
        format!(
            "{tc_pr}<mc:AlternateContent><mc:Choice Requires=\"w14\">{}</mc:Choice>\
             <mc:Fallback>{}</mc:Fallback></mc:AlternateContent>",
            para("choice"),
            para("fallback")
        ),
        format!(
            "{tc_pr}<w:p><w:r><w:t>host</w:t></w:r><w:r>{}</w:r></w:p>",
            alternate_content_text_box("wps")
        ),
        format!("{tc_pr}<w:p/><w:p w:rsidR=\"00A1\"/>"),
    ];
    for cell in &cells {
        let inner = table_in_body(cell);
        match zero_edit_round_trip(&package(&inner)) {
            Outcome::Kept(text) => assert!(!text.is_empty() || cell.contains("<w:p/>"), "{cell}"),
            Outcome::Refused(detail) => panic!("{cell}\nwas refused: {detail}"),
        }
    }
}
