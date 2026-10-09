//! Issue #282 — comments added to / deleted from paragraphs the writer
//! replays from their source bytes. A new comment's anchors are spliced
//! into the untouched paragraph (or table) as a pure insertion and its body
//! is appended to `comments.xml`; a deleted comment's anchors leave every
//! replayed byte (clean paragraphs, always-kept spans) and its body leaves
//! the comment parts, as a pure deletion.
//!
//! Issue #284 — comments anchored inside table cells are read with their
//! full cell paths and kept through edits inside the cell.

use super::tests::document_xml_of;
use super::*;
use crate::opc::archive::read_docx;
use engine::{BlockPath, LogicalPos, PathStep};
use std::io::{Cursor, Write};
use zip::ZipWriter;
use zip::write::SimpleFileOptions;

const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const CT_MAIN: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml";
const REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

/// Two comments: 0 on "comment " of paragraph 0 (reference run), 1 on all
/// of paragraph 1 ("second").
const P0: &str = concat!(
    r#"<w:p w:rsidR="00B561CA"><w:r><w:t xml:space="preserve">this is a </w:t></w:r>"#,
    r#"<w:commentRangeStart w:id="0"/><w:r><w:t xml:space="preserve">comment </w:t></w:r>"#,
    r#"<w:commentRangeEnd w:id="0"/><w:r w:rsidR="002903BF"><w:rPr><w:rStyle w:val="a5"/></w:rPr><w:commentReference w:id="0"/></w:r>"#,
    r#"<w:r><w:t>paragraph!</w:t></w:r></w:p>"#,
);
const P1: &str = concat!(
    r#"<w:p><w:commentRangeStart w:id="1"/><w:r w:rsidRPr="00AB12CD"><w:rPr><w:b/></w:rPr><w:t>second line</w:t></w:r>"#,
    r#"<w:commentRangeEnd w:id="1"/><w:r><w:rPr><w:rStyle w:val="a5"/></w:rPr><w:commentReference w:id="1"/></w:r></w:p>"#,
);
const P2: &str = r#"<w:p w:rsidR="00C0FFEE"><w:r w:rsidRPr="00AB12CD"><w:rPr><w:i/></w:rPr><w:t>alpha beta gamma</w:t></w:r></w:p>"#;

const COMMENTS: &str = concat!(
    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
    "\r\n",
    r#"<w:comments xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml">"#,
    r#"<w:comment w:id="0" w:author="A" w:date="2026-01-01T00:00:00Z" w:initials="A"><w:p w14:paraId="1A2B3C4D"><w:pPr><w:pStyle w:val="CommentText"/></w:pPr><w:r><w:t>first</w:t></w:r></w:p></w:comment>"#,
    r#"<w:comment w:id="1" w:author="B" w:date="2026-01-01T00:00:00Z"><w:p w14:paraId="2B3C4D5E"><w:r><w:t>second</w:t></w:r></w:p></w:comment>"#,
    r#"</w:comments>"#,
);
const COMMENTS_EXTENDED: &str = concat!(
    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
    r#"<w15:commentsEx xmlns:w15="http://schemas.microsoft.com/office/word/2012/wordml">"#,
    r#"<w15:commentEx w15:paraId="1A2B3C4D" w15:done="0"/><w15:commentEx w15:paraId="2B3C4D5E" w15:done="0"/>"#,
    r#"</w15:commentsEx>"#,
);
const COMMENTS_IDS: &str = concat!(
    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
    r#"<w16cid:commentsIds xmlns:w16cid="http://schemas.microsoft.com/office/word/2016/wordml/cid">"#,
    r#"<w16cid:commentId w16cid:paraId="1A2B3C4D" w16cid:durableId="11111111"/><w16cid:commentId w16cid:paraId="2B3C4D5E" w16cid:durableId="22222222"/>"#,
    r#"</w16cid:commentsIds>"#,
);

/// A package with `body` and (unless `None`) the comment parts.
fn package(body: &str, comments: Option<&str>) -> (String, DocxArchive) {
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W}" xmlns:r="{REL}" xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml" xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006"><w:body>{body}<w:sectPr/></w:body></w:document>"#
    );
    let mut parts: Vec<(String, String)> = vec![
        (
            "[Content_Types].xml".into(),
            format!(
                concat!(
                    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
                    r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">"#,
                    r#"<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>"#,
                    r#"<Default Extension="xml" ContentType="application/xml"/>"#,
                    r#"<Override PartName="/word/document.xml" ContentType="{ct}.document.main+xml"/>"#,
                    "{extra}",
                    r#"</Types>"#
                ),
                ct = CT_MAIN,
                extra = if comments.is_some() {
                    format!(
                        r#"<Override PartName="/word/comments.xml" ContentType="{CT_MAIN}.comments+xml"/>"#
                    )
                } else {
                    String::new()
                }
            ),
        ),
        ("_rels/.rels".into(), DOT_RELS_XML.into()),
        (
            "word/_rels/document.xml.rels".into(),
            format!(
                concat!(
                    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
                    r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#,
                    "{rows}",
                    r#"<Relationship Id="rId7" Type="{rel}/hyperlink" Target="http://x.example/" TargetMode="External"/>"#,
                    r#"</Relationships>"#
                ),
                rows = if comments.is_some() {
                    format!(
                        r#"<Relationship Id="rId1" Type="{REL}/comments" Target="comments.xml"/>"#
                    )
                } else {
                    String::new()
                },
                rel = REL,
            ),
        ),
    ];
    if let Some(c) = comments {
        parts.push(("word/comments.xml".into(), c.into()));
        parts.push(("word/commentsExtended.xml".into(), COMMENTS_EXTENDED.into()));
        parts.push(("word/commentsIds.xml".into(), COMMENTS_IDS.into()));
    }
    parts.push(("word/document.xml".into(), xml.clone()));
    let mut buf: Vec<u8> = Vec::new();
    {
        let mut zip = ZipWriter::new(Cursor::new(&mut buf));
        let opts =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (name, body) in &parts {
            zip.start_file(name.as_str(), opts).unwrap();
            zip.write_all(body.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    (xml, read_docx(&buf).expect("read fixture"))
}

fn part(bytes: &[u8], name: &str) -> Option<String> {
    let mut z = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
    let mut f = z.by_name(name).ok()?;
    let mut s = String::new();
    std::io::Read::read_to_string(&mut f, &mut s).unwrap();
    Some(s)
}

fn at(block: u32, offset: usize) -> LogicalPos {
    LogicalPos::new(BlockPath::top(block), offset as u32)
}

/// `edited` is `original` plus insertions only (byte-level).
fn pure_insertion(original: &str, edited: &str) -> bool {
    let (a, b) = (original.as_bytes(), edited.as_bytes());
    let Some(budget) = b.len().checked_sub(a.len()) else {
        return false;
    };
    crate::schema::anchor_patch::diff_by(a.len(), b.len(), |i, j| a[i] == b[j], budget).is_some_and(
        |ops| {
            !ops.iter()
                .any(|o| matches!(o, crate::schema::anchor_patch::Op::Delete(_)))
        },
    )
}

fn save(archive: &DocxArchive, doc: &engine::DocumentTree) -> Vec<u8> {
    let bytes = write_docx(archive, doc).expect("write");
    crate::check_document_xml_well_formed(&bytes).expect("well-formed");
    /* The UI save path (tree alone + its source package) writes the same
    file. */
    assert_eq!(save_docx(doc).expect("ui save"), bytes, "UI path differs");
    bytes
}

/// The text a comment's re-read range covers (single-paragraph ranges).
fn anchored_text(doc: &engine::DocumentTree, id: u32) -> String {
    let r = doc
        .comment_ranges
        .iter()
        .find(|r| r.id == id)
        .unwrap_or_else(|| panic!("comment {id} has no range: {:?}", doc.comment_ranges));
    assert_eq!(r.start.path, r.end.path, "single paragraph");
    let p = doc.paragraph_at_path(&r.start.path).expect("paragraph");
    p.text[r.start.offset as usize..r.end.offset as usize].to_string()
}

#[test]
fn new_comment_on_an_untouched_paragraph_is_spliced_into_its_source_bytes() {
    let (xml, archive) = package(&format!("{P0}{P1}{P2}"), Some(COMMENTS));
    let zero = write_docx(&archive, &archive.document).expect("zero-edit");
    assert_eq!(document_xml_of(&zero), xml);

    /* "beta" of the untouched third paragraph, mid-run on both ends. */
    let (doc, id) = archive.document.insert_comment(
        at(2, 6),
        at(2, 10),
        "look here".into(),
        "Me".into(),
        "2026-10-09T00:00:00Z".into(),
    );
    assert_eq!(id, 2);
    let bytes = save(&archive, &doc);
    let out = document_xml_of(&bytes);
    assert_eq!(
        out,
        xml.replacen(
            P2,
            concat!(
                r#"<w:p w:rsidR="00C0FFEE"><w:r w:rsidRPr="00AB12CD"><w:rPr><w:i/></w:rPr><w:t xml:space="preserve">alpha </w:t></w:r>"#,
                r#"<w:commentRangeStart w:id="2"/><w:r w:rsidRPr="00AB12CD"><w:rPr><w:i/></w:rPr><w:t>beta</w:t></w:r>"#,
                r#"<w:commentRangeEnd w:id="2"/><w:r><w:rPr><w:rStyle w:val="CommentReference"/></w:rPr><w:commentReference w:id="2"/></w:r>"#,
                r#"<w:r w:rsidRPr="00AB12CD"><w:rPr><w:i/></w:rPr><w:t xml:space="preserve"> gamma</w:t></w:r></w:p>"#,
            ),
            1
        )
    );
    assert!(pure_insertion(&xml, &out));
    /* The body reached `comments.xml` (appended — the rest untouched). */
    let comments = part(&bytes, "word/comments.xml").expect("comments part");
    assert!(pure_insertion(COMMENTS, &comments), "{comments}");
    let back = read_docx(&bytes).expect("re-read");
    assert_eq!(anchored_text(&back.document, 2), "beta");
    assert_eq!(back.document.comment_defs[&2].paragraphs, vec!["look here"]);
    assert_eq!(back.document.comment_defs[&2].author, "Me");
    assert_eq!(anchored_text(&back.document, 0), "comment ");
    /* Re-saving the re-read file changes nothing (nothing pending). */
    let again = write_docx(&back, &back.document).expect("resave");
    assert_eq!(document_xml_of(&again), out);
    assert_eq!(
        part(&again, "word/comments.xml").as_deref(),
        Some(comments.as_str())
    );
}

#[test]
fn a_reply_on_an_untouched_paragraph_gets_its_own_anchors_and_thread_row() {
    let (xml, archive) = package(&format!("{P0}{P1}{P2}"), Some(COMMENTS));
    let (doc, reply) = archive
        .document
        .reply_to_comment(
            1,
            "agreed".into(),
            "Me".into(),
            "2026-10-09T00:00:00Z".into(),
        )
        .expect("parent exists");
    let bytes = save(&archive, &doc);
    let out = document_xml_of(&bytes);
    assert!(pure_insertion(&xml, &out), "{out}");
    for piece in [
        format!(r#"<w:commentRangeStart w:id="{reply}"/>"#),
        format!(r#"<w:commentRangeEnd w:id="{reply}"/>"#),
        format!(r#"<w:commentReference w:id="{reply}"/>"#),
    ] {
        assert_eq!(out.matches(&piece).count(), 1, "{piece} in {out}");
    }
    let back = read_docx(&bytes).expect("re-read");
    assert_eq!(anchored_text(&back.document, reply), "second line");
    assert_eq!(back.document.comment_defs[&reply].parent_id, Some(1));
}

#[test]
fn a_deleted_comment_leaves_untouched_paragraphs_and_the_comment_parts() {
    let (xml, archive) = package(&format!("{P0}{P1}{P2}"), Some(COMMENTS));
    let doc = archive.document.delete_comment(1);
    let bytes = save(&archive, &doc);
    let out = document_xml_of(&bytes);
    assert_eq!(
        out,
        xml.replacen(
            P1,
            r#"<w:p><w:r w:rsidRPr="00AB12CD"><w:rPr><w:b/></w:rPr><w:t>second line</w:t></w:r></w:p>"#,
            1
        )
    );
    let comments = part(&bytes, "word/comments.xml").unwrap();
    assert!(
        pure_insertion(&comments, COMMENTS),
        "pure deletion: {comments}"
    );
    assert!(!comments.contains(r#"w:id="1""#));
    for (name, gone) in [
        ("word/commentsExtended.xml", "2B3C4D5E"),
        ("word/commentsIds.xml", "2B3C4D5E"),
    ] {
        let p = part(&bytes, name).unwrap();
        assert!(!p.contains(gone), "{name}: {p}");
        assert!(p.contains("1A2B3C4D"), "{name} kept the other row: {p}");
    }
    let back = read_docx(&bytes).expect("re-read");
    assert!(back.document.comment_ranges.iter().all(|r| r.id != 1));
    assert!(!back.document.comment_defs.contains_key(&1));
    assert_eq!(anchored_text(&back.document, 0), "comment ");

    /* A new comment after the delete never takes the deleted id. */
    let (doc, id) = doc.insert_comment(at(2, 0), at(2, 5), "n".into(), "Me".into(), String::new());
    assert_eq!(id, 2);
    let out = document_xml_of(&save(&archive, &doc));
    assert!(!out.contains(r#"w:id="1""#), "{out}");
}

/// Issues #244 / #245 — an anchor riding an always-kept span (a legacy
/// form field's content span) is stripped when its comment is deleted,
/// whether the paragraph replays or regenerates.
#[test]
fn a_deleted_comment_leaves_always_kept_spans() {
    let body = concat!(
        r#"<w:p><w:r><w:t xml:space="preserve">a </w:t></w:r>"#,
        r#"<w:r><w:fldChar w:fldCharType="begin"><w:ffData><w:name w:val="C1"/><w:checkBox><w:default w:val="0"/></w:checkBox></w:ffData></w:fldChar></w:r>"#,
        r#"<w:r><w:instrText xml:space="preserve"> FORMCHECKBOX </w:instrText></w:r>"#,
        r#"<w:commentRangeStart w:id="0"/>"#,
        r#"<w:r><w:fldChar w:fldCharType="end"/></w:r>"#,
        r#"<w:r><w:t>box</w:t></w:r><w:commentRangeEnd w:id="0"/>"#,
        r#"<w:r><w:rPr><w:rStyle w:val="CommentReference"/></w:rPr><w:commentReference w:id="0"/></w:r></w:p>"#,
    );
    let (xml, archive) = package(body, Some(COMMENTS));
    let p = archive.document.nth_paragraph(0).unwrap();
    assert!(
        p.source_markup
            .as_deref()
            .unwrap()
            .markers
            .iter()
            .any(|mk| mk.role == engine::MarkerRole::Content
                && String::from_utf8_lossy(&mk.xml).contains("commentRangeStart")),
        "the start rides the content span"
    );
    let deleted = archive.document.delete_comment(0);
    for (what, doc) in [
        ("replayed", deleted.clone()),
        ("regenerated", deleted.insert_text(at(0, 1), "X")),
    ] {
        let out = document_xml_of(&save(&archive, &doc));
        assert!(!out.contains(r#"w:id="0""#), "{what}: {out}");
        assert!(out.contains("FORMCHECKBOX"), "{what}: the field survives");
    }
    let out = document_xml_of(&save(&archive, &deleted));
    assert!(pure_insertion(&out, &xml), "replayed: a pure deletion");
}

/// A new comment inside an untouched table: the cell paragraph's bytes
/// are patched inside the table's source bytes.
#[test]
fn new_comment_in_an_untouched_table_cell_is_spliced_into_the_table_bytes() {
    let cell = |t: &str| {
        format!(
            r#"<w:tc><w:tcPr><w:tcW w:w="2000" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>{t}</w:t></w:r></w:p></w:tc>"#
        )
    };
    let body = format!(
        r#"<w:tbl><w:tblPr><w:tblW w:w="0" w:type="auto"/></w:tblPr><w:tblGrid><w:gridCol w:w="2000"/><w:gridCol w:w="2000"/></w:tblGrid><w:tr>{}{}</w:tr><w:tr>{}{}</w:tr></w:tbl>{P2}"#,
        cell("one"),
        cell("two"),
        cell("three four"),
        cell("five"),
    );
    let (xml, archive) = package(&body, Some(COMMENTS));
    let path = BlockPath {
        steps: vec![
            PathStep::Block(0),
            PathStep::Cell { row: 1, col: 0 },
            PathStep::Block(0),
        ],
    };
    let (doc, id) = archive.document.insert_comment(
        LogicalPos::new(path.clone(), 6),
        LogicalPos::new(path, 10),
        "cell note".into(),
        "Me".into(),
        String::new(),
    );
    let bytes = save(&archive, &doc);
    let out = document_xml_of(&bytes);
    assert!(pure_insertion(&xml, &out), "{out}");
    assert!(
        out.contains(&format!(
            r#"<w:t xml:space="preserve">three </w:t></w:r><w:commentRangeStart w:id="{id}"/><w:r><w:t>four</w:t></w:r><w:commentRangeEnd w:id="{id}"/>"#
        )),
        "{out}"
    );
    /* Issue #284 — the re-read comment carries its cell path. */
    let back = read_docx(&bytes).expect("re-read");
    let r = back
        .document
        .comment_ranges
        .iter()
        .find(|r| r.id == id)
        .expect("range");
    assert_eq!(r.start.path.steps[1], PathStep::Cell { row: 1, col: 0 });
    assert_eq!(anchored_text(&back.document, id), "four");
}

fn cell_path(table: u32, row: u32, col: u32, block: u32) -> BlockPath {
    BlockPath {
        steps: vec![
            PathStep::Block(table),
            PathStep::Cell { row, col },
            PathStep::Block(block),
        ],
    }
}

/// A table whose cell (1, 1) holds comment 0 on "beta", cell (0, 0) a
/// comment 1 spanning from body text into it, and cell (1, 0) a nested
/// table with comment 2 — plus a block-level range end between two cell
/// paragraphs.
fn cell_comment_body() -> String {
    let tc = |inner: &str| format!("<w:tc>{inner}</w:tc>");
    let p = |inner: &str| format!("<w:p>{inner}</w:p>");
    let run = |t: &str| format!(r#"<w:r><w:t xml:space="preserve">{t}</w:t></w:r>"#);
    let reference = |id: u32| {
        format!(
            r#"<w:r><w:rPr><w:rStyle w:val="CommentReference"/></w:rPr><w:commentReference w:id="{id}"/></w:r>"#
        )
    };
    let nested = format!(
        "<w:tbl><w:tblGrid><w:gridCol w:w=\"1000\"/></w:tblGrid><w:tr>{}</w:tr></w:tbl>{}",
        tc(&p(&format!(
            r#"{}<w:commentRangeStart w:id="2"/>{}<w:commentRangeEnd w:id="2"/>{}"#,
            run("in "),
            run("deep"),
            reference(2)
        ))),
        p("")
    );
    format!(
        "{}<w:tbl><w:tblGrid><w:gridCol w:w=\"2000\"/><w:gridCol w:w=\"2000\"/></w:tblGrid>\
         <w:tr>{}{}</w:tr><w:tr>{}{}</w:tr></w:tbl>{}",
        p(&format!(
            r#"{}<w:commentRangeStart w:id="1"/>{}"#,
            run("lead "),
            run("in")
        )),
        tc(&format!(
            r#"{}<w:commentRangeEnd w:id="1"/>{}"#,
            p(&run("to cell")),
            p(&reference(1))
        )),
        tc(&p(&run("b"))),
        tc(&nested),
        tc(&p(&format!(
            r#"{}<w:commentRangeStart w:id="0"/>{}<w:commentRangeEnd w:id="0"/>{}{}"#,
            run("alpha "),
            run("beta"),
            reference(0),
            run(" gamma")
        ))),
        p(&run("tail")),
    )
}

const CELL_COMMENTS: &str = concat!(
    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
    r#"<w:comments xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">"#,
    r#"<w:comment w:id="0" w:author="A"><w:p><w:r><w:t>zero</w:t></w:r></w:p></w:comment>"#,
    r#"<w:comment w:id="1" w:author="A"><w:p><w:r><w:t>one</w:t></w:r></w:p></w:comment>"#,
    r#"<w:comment w:id="2" w:author="A"><w:p><w:r><w:t>two</w:t></w:r></w:p></w:comment>"#,
    r#"</w:comments>"#,
);

/// Issue #284 — the reader records cell comment ranges with their full
/// paths: inside a cell paragraph, across body → cell (a block-level end
/// between two cell paragraphs marks the next one's start), and inside a
/// nested table.
#[test]
fn cell_comment_ranges_are_read_with_full_paths() {
    let (xml, archive) = package(&cell_comment_body(), Some(CELL_COMMENTS));
    let doc = &archive.document;
    let range = |id: u32| {
        doc.comment_ranges
            .iter()
            .find(|r| r.id == id)
            .cloned()
            .unwrap()
    };
    let r0 = range(0);
    assert_eq!(r0.start, LogicalPos::new(cell_path(1, 1, 1, 0), 6));
    assert_eq!(r0.end, LogicalPos::new(cell_path(1, 1, 1, 0), 10));
    assert_eq!(anchored_text(doc, 0), "beta");
    let r1 = range(1);
    assert_eq!(r1.start, LogicalPos::new(BlockPath::top(0), 5));
    assert_eq!(r1.end, LogicalPos::new(cell_path(1, 0, 0, 1), 0));
    let r2 = range(2);
    let deep = BlockPath {
        steps: vec![
            PathStep::Block(1),
            PathStep::Cell { row: 1, col: 0 },
            PathStep::Block(0),
            PathStep::Cell { row: 0, col: 0 },
            PathStep::Block(0),
        ],
    };
    assert_eq!(r2.start, LogicalPos::new(deep.clone(), 3));
    assert_eq!(r2.end, LogicalPos::new(deep, 7));
    assert_eq!(anchored_text(doc, 2), "deep");
    /* Zero-edit save stays byte-identical. */
    assert_eq!(document_xml_of(&save(&archive, doc)), xml);
}

/// Issue #284 — typing inside a commented cell (the table regenerates)
/// keeps every anchor: the save is the source plus the typed bytes, and
/// the re-read range covers the same, grown text.
#[test]
fn edits_inside_a_commented_cell_keep_its_anchors() {
    let (xml, archive) = package(&cell_comment_body(), Some(CELL_COMMENTS));
    for (offset, covered, exact) in [
        (2, "beta", Some((">alpha <", ">alXpha <"))),
        (8, "beXta", Some((">beta<", ">beXta<"))),
        /* Right after the comment: outside it (its own run between the
        range end and the reference run). */
        (10, "beta", None),
    ] {
        let doc = archive
            .document
            .insert_text(LogicalPos::new(cell_path(1, 1, 1, 0), offset), "X");
        let bytes = save(&archive, &doc);
        let out = document_xml_of(&bytes);
        assert!(pure_insertion(&xml, &out), "insert at {offset}: {out}");
        if let Some((from, to)) = exact {
            assert_eq!(out, xml.replacen(from, to, 1), "insert at {offset}");
        }
        let back = read_docx(&bytes).expect("re-read");
        assert_eq!(
            anchored_text(&back.document, 0),
            covered,
            "insert at {offset}"
        );
        assert_eq!(anchored_text(&back.document, 2), "deep");
        assert_eq!(back.document.comment_ranges.len(), 3);
    }
    /* Typing in the nested table's commented paragraph. */
    let deep = BlockPath {
        steps: vec![
            PathStep::Block(1),
            PathStep::Cell { row: 1, col: 0 },
            PathStep::Block(0),
            PathStep::Cell { row: 0, col: 0 },
            PathStep::Block(0),
        ],
    };
    let doc = archive.document.insert_text(LogicalPos::new(deep, 5), "X");
    let bytes = save(&archive, &doc);
    assert_eq!(
        document_xml_of(&bytes),
        xml.replacen(">deep<", ">deXep<", 1)
    );
    let back = read_docx(&bytes).expect("re-read");
    assert_eq!(anchored_text(&back.document, 2), "deXep");
}

/// Issue #284 — deleting a cell comment strips its anchors from the
/// untouched table's bytes (and from a regenerated one).
#[test]
fn deleting_a_cell_comment_strips_its_anchors() {
    let (xml, archive) = package(&cell_comment_body(), Some(CELL_COMMENTS));
    let deleted = archive.document.delete_comment(0);
    for (what, doc) in [
        ("untouched", deleted.clone()),
        (
            "edited",
            deleted.insert_text(LogicalPos::new(cell_path(1, 1, 1, 0), 0), "X"),
        ),
    ] {
        let out = document_xml_of(&save(&archive, &doc));
        assert!(!out.contains(r#"w:id="0""#), "{what}: {out}");
        assert!(
            out.contains(r#"<w:commentRangeStart w:id="2"/>"#),
            "{what}: {out}"
        );
    }
    let out = document_xml_of(&save(&archive, &deleted));
    assert!(pure_insertion(&out, &xml), "untouched: a pure deletion");
}

/// Pretty-printed / drifted source: the regenerated paragraph is not
/// byte-identical to its source (a `<w:smartTag>` the model drops), yet
/// the splice is still a pure insertion and the smart tag survives.
#[test]
fn a_splice_into_a_drifted_paragraph_is_still_a_pure_insertion() {
    let body = concat!(
        "<w:p>\n  <w:r>\n    <w:t xml:space=\"preserve\">by </w:t>\n  </w:r>\n",
        r#"  <w:smartTag w:uri="urn:schemas-microsoft-com:office:smarttags" w:element="metricconverter"><w:smartTagPr><w:attr w:name="ProductID" w:val="13 km"/></w:smartTagPr><w:r><w:t>13 km</w:t></w:r></w:smartTag>"#,
        "\n  <w:r>\n    <w:t xml:space=\"preserve\"> away</w:t>\n  </w:r>\n</w:p>",
    );
    let (xml, archive) = package(body, None);
    let (doc, id) = archive.document.insert_comment(
        at(0, 3),
        at(0, 8),
        "distance".into(),
        "Me".into(),
        String::new(),
    );
    let bytes = save(&archive, &doc);
    let out = document_xml_of(&bytes);
    assert!(pure_insertion(&xml, &out), "{out}");
    assert!(out.contains("<w:smartTag"), "{out}");
    let back = read_docx(&bytes).expect("re-read");
    assert_eq!(anchored_text(&back.document, id), "13 km");
    assert_eq!(back.document.comment_defs[&id].paragraphs, vec!["distance"]);
}

/// A document without `comments.xml` (and an engine-authored one) gets
/// the part synthesized for a plain new comment — before, only a resolved
/// comment or a reply triggered it and the anchors dangled.
#[test]
fn a_plain_new_comment_synthesizes_comments_xml() {
    let (_, archive) = package(P2, None);
    let (doc, id) = archive.document.insert_comment(
        at(0, 0),
        at(0, 5),
        "hi".into(),
        "Me".into(),
        "2026-10-09T00:00:00Z".into(),
    );
    let bytes = save(&archive, &doc);
    let back = read_docx(&bytes).expect("re-read");
    assert_eq!(anchored_text(&back.document, id), "alpha");
    assert_eq!(back.document.comment_defs[&id].paragraphs, vec!["hi"]);

    let fresh = engine::DocumentTree::from_text("hello world");
    let (fresh, id) =
        fresh.insert_comment(at(0, 6), at(0, 11), "w".into(), "Me".into(), String::new());
    let bytes = save_docx(&fresh).expect("minimal package");
    let back = read_docx(&bytes).expect("re-read");
    assert_eq!(anchored_text(&back.document, id), "world");
    assert_eq!(back.document.comment_defs[&id].paragraphs, vec!["w"]);
}

/// A clean paragraph regenerated only as the splice baseline keeps its
/// external link: the link is resolved like a dirty paragraph's, and the
/// splice leaves `<w:hyperlink r:id>` exactly as the source wrote it.
#[test]
fn a_splice_keeps_the_paragraphs_hyperlinks() {
    let body = concat!(
        r#"<w:p><w:r><w:t xml:space="preserve">see </w:t></w:r><w:hyperlink r:id="rId7" w:history="1">"#,
        r#"<w:r><w:rPr><w:rStyle w:val="Hyperlink"/></w:rPr><w:t>the site</w:t></w:r></w:hyperlink>"#,
        r#"<w:smartTag w:uri="urn:x" w:element="y"><w:r><w:t xml:space="preserve"> now</w:t></w:r></w:smartTag></w:p>"#,
    );
    let (xml, archive) = package(body, Some(COMMENTS));
    let (doc, id) = archive.document.insert_comment(
        at(0, 4),
        at(0, 12),
        "link".into(),
        "Me".into(),
        String::new(),
    );
    let bytes = save(&archive, &doc);
    let out = document_xml_of(&bytes);
    assert!(pure_insertion(&xml, &out), "{out}");
    assert!(
        out.contains(r#"<w:hyperlink r:id="rId7" w:history="1">"#),
        "{out}"
    );
    let back = read_docx(&bytes).expect("re-read");
    assert_eq!(anchored_text(&back.document, id), "the site");
    assert_eq!(back.document.nth_paragraph(0).unwrap().hyperlinks.len(), 1);
}

/// The `w:id`s of every tracked-change annotation in `xml`, in order.
fn annotation_ids(xml: &str) -> Vec<String> {
    let mut out = Vec::new();
    for el in ["<w:ins ", "<w:del ", "<w:rPrChange "] {
        let mut rest = xml;
        while let Some(at) = rest.find(el) {
            rest = &rest[at + el.len()..];
            if let Some(v) = rest
                .split("w:id=\"")
                .nth(1)
                .and_then(|t| t.split('"').next())
            {
                out.push(v.to_string());
            }
        }
    }
    out.sort();
    out
}

/// Issue #295 × #282 — a comment spliced into an untouched paragraph that
/// carries tracked changes (an `<w:ins>` wrapper, a run with a recorded
/// formatting change): the regenerated baselines carry annotation-id
/// tokens, the splice still lands as a pure insertion of the anchors (no
/// token, no id respelled), and every annotation id stays unique.
#[test]
fn a_splice_into_a_paragraph_with_tracked_changes_keeps_its_ids() {
    let body = concat!(
        r#"<w:p><w:r><w:t xml:space="preserve">keep </w:t></w:r>"#,
        r#"<w:ins w:id="5" w:author="A" w:date="2026-01-01T00:00:00Z"><w:r><w:t>alpha beta</w:t></w:r></w:ins>"#,
        r#"<w:r><w:rPr><w:b/><w:rPrChange w:id="7" w:author="A" w:date="2026-01-01T00:00:00Z"><w:rPr/></w:rPrChange></w:rPr><w:t xml:space="preserve"> gamma delta</w:t></w:r></w:p>"#,
        r#"<w:p><w:r><w:t>second</w:t></w:r></w:p>"#,
    );
    let (xml, archive) = package(body, Some(COMMENTS));
    for (lo, hi, covered) in [
        /* Inside the insertion. */
        (11usize, 15usize, "beta"),
        /* Across the insertion's end into the format-changed run. */
        (11, 21, "beta gamma"),
    ] {
        let (doc, id) = archive.document.insert_comment(
            at(0, lo),
            at(0, hi),
            "n".into(),
            "Me".into(),
            String::new(),
        );
        let bytes = save(&archive, &doc);
        let out = document_xml_of(&bytes);
        assert!(!out.contains('\u{E000}'), "an unresolved token: {out}");
        assert!(pure_insertion(&xml, &out), "{lo}..{hi}: {out}");
        let ids = annotation_ids(&out);
        let mut unique = ids.clone();
        unique.dedup();
        assert_eq!(ids, unique, "{lo}..{hi}: duplicate annotation ids {ids:?}");
        assert!(
            ids.contains(&"5".to_string()) && ids.contains(&"7".to_string()),
            "{ids:?}"
        );
        let back = read_docx(&bytes).expect("re-read");
        assert_eq!(anchored_text(&back.document, id), covered);
    }
}

/// Issue #351 × #282 / #284 — comment range markers inside an
/// `mc:AlternateContent`: the selected branch's markers anchor the
/// comment (once — the other branch is kept as bytes, not read), in a body
/// paragraph and in a cell paragraph alike; a new comment beside it is
/// still a pure insertion, and deleting the comment strips its markers
/// from EVERY branch.
#[test]
fn comment_markers_inside_alternate_content_come_from_the_selected_branch() {
    let ac = concat!(
        r#"<w:r><w:t xml:space="preserve">a </w:t></w:r>"#,
        r#"<mc:AlternateContent><mc:Choice Requires="w14"><w:commentRangeStart w:id="0"/><w:r><w:t>chosen</w:t></w:r><w:commentRangeEnd w:id="0"/></mc:Choice>"#,
        r#"<mc:Fallback><w:commentRangeStart w:id="0"/><w:r><w:t>fallback</w:t></w:r><w:commentRangeEnd w:id="0"/></mc:Fallback></mc:AlternateContent>"#,
        r#"<w:r><w:rPr><w:rStyle w:val="CommentReference"/></w:rPr><w:commentReference w:id="0"/></w:r>"#,
        r#"<w:r><w:t xml:space="preserve"> tail words</w:t></w:r>"#,
    );
    for in_cell in [false, true] {
        let body = if in_cell {
            format!(
                r#"<w:tbl><w:tblGrid><w:gridCol w:w="4000"/></w:tblGrid><w:tr><w:tc><w:p>{ac}</w:p></w:tc></w:tr></w:tbl><w:p/>"#
            )
        } else {
            format!("<w:p>{ac}</w:p>")
        };
        let (xml, archive) = package(&body, Some(COMMENTS));
        let doc = &archive.document;
        let ranges: Vec<_> = doc.comment_ranges.iter().filter(|r| r.id == 0).collect();
        assert_eq!(ranges.len(), 1, "cell={in_cell}: {:?}", doc.comment_ranges);
        assert_eq!(anchored_text(doc, 0), "chosen", "cell={in_cell}");
        let path = ranges[0].start.path.clone();
        assert_eq!(path.steps.len(), if in_cell { 3 } else { 1 });
        assert_eq!(document_xml_of(&save(&archive, doc)), xml, "zero-edit");

        /* A new comment on "tail" beside it: a pure insertion. */
        let (with_new, id) = doc.insert_comment(
            LogicalPos::new(path.clone(), 9),
            LogicalPos::new(path.clone(), 13),
            "n".into(),
            "Me".into(),
            String::new(),
        );
        let bytes = save(&archive, &with_new);
        let out = document_xml_of(&bytes);
        assert!(pure_insertion(&xml, &out), "cell={in_cell}: {out}");
        let back = read_docx(&bytes).expect("re-read");
        assert_eq!(anchored_text(&back.document, id), "tail");
        assert_eq!(anchored_text(&back.document, 0), "chosen");

        /* Deleting comment 0 strips it from both branches. */
        let out = document_xml_of(&save(&archive, &doc.delete_comment(0)));
        assert!(!out.contains(r#"w:id="0""#), "cell={in_cell}: {out}");
        assert!(out.contains("fallback") && out.contains("chosen"), "{out}");
        assert!(
            pure_insertion(&out, &xml),
            "cell={in_cell}: a pure deletion"
        );
    }
}
