//! Paragraph formatting through edits and saves: issue #292 (a paragraph
//! merge keeps the head's style and source identity and carries the
//! tail's hyperlinks / tracked changes into the saved file), issue #293
//! (the paragraph mark's run properties, `<w:pPr><w:rPr>`) and issue #297
//! (a regenerated `styles.xml` keeps every style's display name).

use super::tests::document_xml_of;
use super::*;
use crate::opc::archive::read_docx;
use engine::{Block, BlockPath, DocumentTree, LogicalPos};

const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

/// Heading1 (bold, keepNext, next = Normal), Normal, Hyperlink (character).
const STYLES_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/><w:qFormat/></w:style><w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/><w:basedOn w:val="Normal"/><w:next w:val="Normal"/><w:qFormat/><w:pPr><w:keepNext/><w:outlineLvl w:val="0"/></w:pPr><w:rPr><w:b/><w:sz w:val="32"/></w:rPr></w:style><w:style w:type="character" w:styleId="Hyperlink"><w:name w:val="Hyperlink"/><w:rPr><w:color w:val="0563C1"/><w:u w:val="single"/></w:rPr></w:style></w:styles>"#;

const DOC_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/><Relationship Id="rId9" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="https://example.com/" TargetMode="External"/></Relationships>"#;

/// `word/document.xml` around `body` (root binds `w`, `r`, `w14`).
fn document(body: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="{W}" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml"><w:body>{body}<w:sectPr/></w:body></w:document>"#
    )
}

/// A package with `styles.xml`, a document rels part (styles + one
/// external hyperlink, `rId9`) and `document_xml`.
pub(super) fn package(styles_xml: &str, document_xml: &str) -> Vec<u8> {
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/></Types>"#;
    let dot_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let mut buf: Vec<u8> = Vec::new();
    {
        let mut zip = ZipWriter::new(Cursor::new(&mut buf));
        let opts = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(0o644);
        for (name, body) in [
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", dot_rels),
            ("word/_rels/document.xml.rels", DOC_RELS),
            ("word/styles.xml", styles_xml),
            ("word/document.xml", document_xml),
        ] {
            zip.start_file(name, opts).unwrap();
            zip.write_all(body.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    buf
}

fn at(block: u32, offset: u32) -> LogicalPos {
    LogicalPos::new(BlockPath::top(block), offset)
}

/// A Heading1 paragraph, then a body paragraph holding a hyperlink and a
/// tracked insertion.
const MERGE_BODY: &str = concat!(
    r#"<w:p w14:paraId="11111111" w:rsidR="00AA0001"><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Title</w:t></w:r></w:p>"#,
    r#"<w:p w14:paraId="22222222" w:rsidR="00AA0002"><w:r><w:t xml:space="preserve">Body </w:t></w:r><w:hyperlink r:id="rId9"><w:r><w:rPr><w:rStyle w:val="Hyperlink"/></w:rPr><w:t>link</w:t></w:r></w:hyperlink><w:ins w:id="5" w:author="Rev" w:date="2026-01-01T00:00:00Z"><w:r><w:t>new</w:t></w:r></w:ins></w:p>"#,
);

/// Issue #292 — Backspace at the start of the body paragraph saves ONE
/// paragraph that is still a Heading1 (verified source `<w:pPr>` — the
/// merge used to drop the `<w:pStyle>` and bake the resolved props as
/// direct formatting), keeps the head's `w14:paraId` (left ids win) and
/// the tail's hyperlink (its source `r:id`) and tracked insertion.
#[test]
fn a_paragraph_merge_saves_the_heading_with_the_tails_overlays() {
    let parsed = read_docx(&package(STYLES_XML, &document(MERGE_BODY))).expect("read");
    let merged = parsed.document.delete_range(at(0, 5), at(1, 0));
    let bytes = write_docx(&parsed, &merged).expect("write");
    crate::check_document_xml_well_formed(&bytes).expect("well-formed");
    let out = document_xml_of(&bytes);
    assert_eq!(out.matches("<w:p ").count(), 1, "{out}");
    assert!(out.contains(r#"w14:paraId="11111111""#), "{out}");
    assert!(
        !out.contains("22222222"),
        "the tail's identity is gone: {out}"
    );
    assert!(
        out.contains(r#"<w:pPr><w:pStyle w:val="Heading1"/></w:pPr>"#),
        "{out}"
    );
    assert!(out.contains(r#"<w:hyperlink r:id="rId9""#), "{out}");
    assert!(out.contains(r#"<w:ins w:id="5" w:author="Rev""#), "{out}");

    let back = read_docx(&bytes).expect("re-read");
    let p = back.document.nth_paragraph(0).unwrap();
    assert_eq!(p.text, "TitleBody linknew");
    assert_eq!(p.style_id.as_deref(), Some("Heading1"));
    assert_eq!(p.hyperlinks.len(), 1);
    let h = &p.hyperlinks[0];
    assert_eq!(&p.text[h.start as usize..h.end as usize], "link");
    assert_eq!(p.revisions.len(), 1);
    let r = &p.revisions[0];
    assert_eq!(&p.text[r.start as usize..r.end as usize], "new");
}

/* ---- Issue #293 — the paragraph mark's run properties ---- */

/// A centred paragraph whose mark is bold + themed red + `en-GB` (its
/// text run is bold), then an EMPTY paragraph whose mark is italic.
const MARK_BODY: &str = concat!(
    r#"<w:p w14:paraId="33333333"><w:pPr><w:jc w:val="center"/><w:rPr><w:b/><w:color w:val="FF0000" w:themeColor="accent2"/><w:lang w:val="en-GB"/></w:rPr></w:pPr><w:r><w:rPr><w:b/></w:rPr><w:t>Bold</w:t></w:r></w:p>"#,
    r#"<w:p w14:paraId="44444444"><w:pPr><w:rPr><w:i/></w:rPr></w:pPr></w:p>"#,
);

fn mark_of(doc: &DocumentTree, i: u32) -> Option<SpanStyle> {
    doc.nth_paragraph(i)
        .and_then(|p| p.mark_style.as_deref().cloned())
}

fn with_mark(doc: &DocumentTree, i: u32, mark: SpanStyle) -> DocumentTree {
    let mut out = doc.clone();
    let mut blocks = out.blocks.clone();
    if let Some(Block::Paragraph(p)) = blocks.get_mut(i as usize) {
        p.mark_style = Some(Box::new(mark));
        p.dirty = true;
        p.source_xml = None;
    }
    out.blocks = blocks;
    out
}

/// The reader models the mark (modeled children only); a zero-edit save
/// is byte-identical, an edit of the paragraph's text keeps the verified
/// source `<w:pPr>` (mark included) byte for byte, and typing into the
/// empty paragraph comes out italic — through a save and a re-read.
#[test]
fn the_paragraph_mark_rpr_is_modeled_and_round_trips() {
    let xml = document(MARK_BODY);
    let parsed = read_docx(&package(STYLES_XML, &xml)).expect("read");
    let doc = &parsed.document;
    let m0 = mark_of(doc, 0).expect("mark read");
    assert_eq!(m0.bold, Some(true));
    assert_eq!(m0.color, Some([0xFF, 0, 0, 255]));
    assert!(m0.grab_bag.is_none(), "<w:lang> stays in the pPr bag");
    assert_eq!(mark_of(doc, 1).map(|m| m.italic), Some(Some(true)));

    let bytes = write_docx(&parsed, doc).expect("write");
    assert_eq!(document_xml_of(&bytes), xml, "zero-edit save");

    let edited = doc.insert_text(at(0, 4), "er");
    let bytes = write_docx(&parsed, &edited).expect("write edit");
    let out = document_xml_of(&bytes);
    assert_eq!(out, xml.replacen(">Bold<", ">Bolder<", 1), "pure insertion");

    let typed = doc.insert_text(at(1, 0), "slanted");
    let p = typed.nth_paragraph(1).unwrap();
    assert_eq!(p.style_at(0).italic, Some(true), "typing inherits the mark");
    let bytes = write_docx(&parsed, &typed).expect("write typed");
    let back = read_docx(&bytes).expect("re-read");
    let p = back.document.nth_paragraph(1).unwrap();
    assert_eq!(p.text, "slanted");
    assert_eq!(p.style_at(0).italic, Some(true));
    assert_eq!(
        mark_of(&back.document, 1).and_then(|m| m.italic),
        Some(true)
    );
}

/// A changed mark regenerates ONLY the mark `<w:rPr>`: the new modeled
/// children, the unmodeled ones (`<w:lang>`) kept, an unchanged child in
/// its source spelling (`w:themeColor` on `<w:color>`); the re-read mark
/// is the new one. A mark with nothing left to say keeps only the
/// unmodeled children.
#[test]
fn a_changed_mark_regenerates_the_mark_rpr_keeping_its_unmodeled_children() {
    let parsed = read_docx(&package(STYLES_XML, &document(MARK_BODY))).expect("read");
    let mut mark = mark_of(&parsed.document, 0).unwrap();
    mark.italic = Some(true);
    let changed = with_mark(&parsed.document, 0, mark);
    let bytes = write_docx(&parsed, &changed).expect("write");
    crate::check_document_xml_well_formed(&bytes).expect("well-formed");
    let out = document_xml_of(&bytes);
    assert!(
        out.contains(concat!(
            r#"<w:pPr><w:jc w:val="center"/><w:rPr><w:b/><w:i/>"#,
            r#"<w:color w:val="FF0000" w:themeColor="accent2"/><w:lang w:val="en-GB"/></w:rPr></w:pPr>"#
        )),
        "{out}"
    );
    let back = read_docx(&bytes).expect("re-read");
    let m = mark_of(&back.document, 0).unwrap();
    assert_eq!((m.bold, m.italic), (Some(true), Some(true)));

    let plain = with_mark(&parsed.document, 0, SpanStyle::default());
    let out = document_xml_of(&write_docx(&parsed, &plain).expect("write plain"));
    assert!(
        out.contains(
            r#"<w:pPr><w:jc w:val="center"/><w:rPr><w:lang w:val="en-GB"/></w:rPr></w:pPr>"#
        ),
        "{out}"
    );
}

/// Issue #293's headline case end to end: Enter at the end of a bold run in
/// a paragraph WITHOUT a mark rPr gives the new paragraph a bold mark;
/// the save spells it (`<w:pPr><w:rPr><w:b/></w:rPr></w:pPr>`) while the
/// original paragraph keeps its bytes; the re-read types bold there.
#[test]
fn enter_after_a_bold_run_saves_a_bold_mark_on_the_new_paragraph() {
    let body = r#"<w:p w14:paraId="55555555"><w:r><w:t xml:space="preserve">Hello </w:t></w:r><w:r><w:rPr><w:b/></w:rPr><w:t>world</w:t></w:r></w:p>"#;
    let parsed = read_docx(&package(STYLES_XML, &document(body))).expect("read");
    assert_eq!(
        mark_of(&parsed.document, 0),
        None,
        "no mark rPr in the source"
    );
    let split = parsed.document.split_paragraph(at(0, 11));
    let bytes = write_docx(&parsed, &split).expect("write");
    let out = document_xml_of(&bytes);
    assert!(
        out.contains(body),
        "the original paragraph is untouched: {out}"
    );
    assert!(
        out.contains("<w:p><w:pPr><w:rPr><w:b/></w:rPr></w:pPr>"),
        "{out}"
    );
    let back = read_docx(&bytes).expect("re-read");
    assert_eq!(mark_of(&back.document, 1).and_then(|m| m.bold), Some(true));
    let typed = back.document.insert_text(at(1, 0), "next");
    assert_eq!(typed.nth_paragraph(1).unwrap().style_at(0).bold, Some(true));
}

/// Issue #295 × #293 — a REGENERATED mark `<w:rPr>` keeps its unmodeled
/// `<w:rPrChange>` history, and its annotation `w:id` goes through the
/// package-wide id pass like every other: a paragraph split in two whose
/// halves both regenerate the mark writes the source id once and a fresh
/// one on the other half — never the same `w:id` twice.
#[test]
fn a_regenerated_mark_rpr_gets_package_unique_annotation_ids() {
    let body = concat!(
        r#"<w:p><w:pPr><w:rPr><w:b/><w:rPrChange w:id="7" w:author="A" w:date="2026-01-01T00:00:00Z"><w:rPr/></w:rPrChange></w:rPr></w:pPr>"#,
        r#"<w:r><w:rPr><w:b/></w:rPr><w:t xml:space="preserve">ab cd</w:t></w:r></w:p>"#,
    );
    let parsed = read_docx(&package(STYLES_XML, &document(body))).expect("read");
    let italic = SpanStyle {
        bold: Some(true),
        italic: Some(true),
        ..Default::default()
    };
    let split = parsed.document.split_paragraph(at(0, 2));
    let both = with_mark(&with_mark(&split, 0, italic.clone()), 1, italic);
    let bytes = write_docx(&parsed, &both).expect("write");
    crate::check_document_xml_well_formed(&bytes).expect("well-formed");
    let out = document_xml_of(&bytes);
    assert_eq!(out.matches("<w:rPrChange ").count(), 2, "{out}");
    assert_eq!(out.matches(r#"<w:rPrChange w:id="7""#).count(), 1, "{out}");
    assert_eq!(
        out.matches("<w:i/>").count(),
        2,
        "both marks regenerated: {out}"
    );
    let back = read_docx(&bytes).expect("re-read");
    for i in 0..2 {
        assert_eq!(
            mark_of(&back.document, i).and_then(|m| m.italic),
            Some(true)
        );
    }
}

/// A tracked paragraph mark (#262) and a changed mark style regenerate
/// together: the revision stays the rPr's first child.
#[test]
fn a_changed_mark_keeps_the_mark_revision_first() {
    let body = concat!(
        r#"<w:p><w:pPr><w:rPr><w:ins w:id="9" w:author="A" w:date="2026-01-01T00:00:00Z"/><w:b/></w:rPr></w:pPr><w:r><w:t>x</w:t></w:r></w:p>"#,
        r#"<w:p><w:r><w:t>y</w:t></w:r></w:p>"#,
    );
    let parsed = read_docx(&package(STYLES_XML, &document(body))).expect("read");
    let p0 = parsed.document.nth_paragraph(0).unwrap();
    assert!(p0.mark_revision().is_some());
    assert_eq!(
        mark_of(&parsed.document, 0).and_then(|m| m.bold),
        Some(true)
    );
    let italic = SpanStyle {
        italic: Some(true),
        ..Default::default()
    };
    let changed = with_mark(&parsed.document, 0, italic);
    let out = document_xml_of(&write_docx(&parsed, &changed).expect("write"));
    assert!(
        out.contains(concat!(
            r#"<w:pPr><w:rPr><w:ins w:id="9" w:author="A" w:date="2026-01-01T00:00:00Z"/>"#,
            r#"<w:i/></w:rPr></w:pPr>"#
        )),
        "{out}"
    );
}

/// An engine-authored tree (no source package: `build_minimal_docx`)
/// writes the mark too, and an empty `<w:rPr/>` mark reads as an empty
/// (`Some(default)`) mark that keeps its bytes through an edit.
#[test]
fn minimal_package_writes_the_mark_and_an_empty_mark_rpr_round_trips() {
    let bold = SpanStyle {
        bold: Some(true),
        ..Default::default()
    };
    let d = DocumentTree::from_text("Hello world")
        .apply_style(at(0, 6), at(0, 11), bold)
        .split_paragraph(at(0, 11));
    let bytes = build_minimal_docx(&d).expect("minimal");
    let back = read_docx(&bytes).expect("re-read");
    assert_eq!(mark_of(&back.document, 1).and_then(|m| m.bold), Some(true));

    let xml = document(r#"<w:p><w:pPr><w:rPr/></w:pPr><w:r><w:t>z</w:t></w:r></w:p>"#);
    let parsed = read_docx(&package(STYLES_XML, &xml)).expect("read");
    assert_eq!(mark_of(&parsed.document, 0), Some(SpanStyle::default()));
    let edited = parsed.document.insert_text(at(0, 1), "z");
    let out = document_xml_of(&write_docx(&parsed, &edited).expect("write"));
    assert_eq!(out, xml.replacen(">z<", ">zz<", 1));
}

/* ---- Issue #297 — style ids and display names are distinct ---- */

fn styles_xml_of(bytes: &[u8]) -> String {
    let mut z = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
    let mut f = z.by_name("word/styles.xml").unwrap();
    let mut s = String::new();
    std::io::Read::read_to_string(&mut f, &mut s).unwrap();
    s
}

/// `Heading1` named `heading 1`, a localized custom name, and a style
/// with no `<w:name>` at all.
const NAMED_STYLES_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/></w:style><w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/><w:basedOn w:val="Normal"/><w:next w:val="Normal"/><w:rPr><w:b/></w:rPr></w:style><w:style w:type="paragraph" w:customStyle="1" w:styleId="Zitat2"><w:name w:val="Zitat &amp; Quelle"/></w:style><w:style w:type="paragraph" w:customStyle="1" w:styleId="Nameless"><w:rPr><w:i/></w:rPr></w:style></w:styles>"#;

/// The reader keeps `w:styleId` and `<w:name>` apart, and a `ModifyStyle`
/// (which regenerates `styles.xml`) writes every name back exactly as it
/// was read — `<w:name w:val="heading 1"/>` stays, it used to become
/// `Heading1` — and invents none for a style that had none.
#[test]
fn modify_style_keeps_every_style_name() {
    let xml = document(
        r#"<w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>T</w:t></w:r></w:p>"#,
    );
    let parsed = read_docx(&package(NAMED_STYLES_XML, &xml)).expect("read");
    let styles = &parsed.document.styles;
    assert_eq!(styles["Heading1"].id, "Heading1");
    assert_eq!(styles["Heading1"].name, "heading 1");
    assert_eq!(styles["Zitat2"].name, "Zitat & Quelle");
    assert_eq!(styles["Nameless"].name, "", "no <w:name> in the source");

    let bigger = SpanStyle {
        font_size: Some(20.0),
        ..Default::default()
    };
    let modified = parsed
        .document
        .modify_style("Heading1", None, Some(bigger), None, None);
    assert!(modified.styles_dirty);
    let bytes = write_docx(&parsed, &modified).expect("write");
    let out = styles_xml_of(&bytes);
    assert!(
        out.contains(
            r#"<w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/>"#
        ),
        "{out}"
    );
    assert!(
        out.contains(r#"<w:name w:val="Zitat &amp; Quelle"/>"#),
        "{out}"
    );
    assert!(
        out.contains(
            r#"<w:style w:type="paragraph" w:customStyle="1" w:styleId="Nameless"><w:rPr><w:i/></w:rPr></w:style>"#
        ),
        "a nameless style stays nameless: {out}"
    );
    assert!(!out.contains(r#"<w:name w:val="Heading1"/>"#), "{out}");
    /* Issue #371 — the part is patched, not regenerated: only the edited
    style's `<w:rPr>` changed. */
    assert_eq!(
        out,
        NAMED_STYLES_XML.replacen(
            r#"<w:rPr><w:b/></w:rPr>"#,
            r#"<w:rPr><w:b/><w:sz w:val="40"/></w:rPr>"#,
            1
        )
    );

    let back = read_docx(&bytes).expect("re-read");
    assert_eq!(back.document.styles["Heading1"].name, "heading 1");
    assert_eq!(back.document.styles["Zitat2"].name, "Zitat & Quelle");
    assert_eq!(
        back.document
            .resolve_style_run_cascade(Some("Heading1"))
            .font_size,
        Some(20.0)
    );
    /* An explicit rename (`ModifyStyle.display_name`) is still a rename. */
    let renamed =
        parsed
            .document
            .modify_style("Heading1", None, None, None, Some("Chapter title".into()));
    let out = styles_xml_of(&write_docx(&parsed, &renamed).expect("write renamed"));
    assert!(out.contains(r#"<w:name w:val="Chapter title"/>"#), "{out}");
}
