//! Issues #100 / #101 — the two `.docx` save paths.
//!
//! The live editor saves through `build_minimal_docx(&DocumentTree)` (the
//! engine-wasm `SaveDocx` / `SaveDocument` handlers hold only the tree);
//! the round-trip harnesses save through `write_docx(&DocxArchive, …)`.
//! Both must produce namespace-well-formed parts for a Word-authored
//! document, and both must regenerate an edited table cell with its runs.

use engine::{BlockPath, InlineKind, LogicalPos, PathStep};
use format_docx::{
    build_minimal_docx, check_document_xml_well_formed, check_part_xml_well_formed, read_docx,
    write_docx,
};
use std::io::{Read, Write};

const W14_FIXTURE: &[u8] = include_bytes!("fixtures/w14_paraid_word.docx");
const CELL_RUNS_FIXTURE: &[u8] = include_bytes!("fixtures/table_cell_runs.docx");
const W14_NS: &str = "http://schemas.microsoft.com/office/word/2010/wordml";

fn part(docx: &[u8], name: &str) -> String {
    let mut a = zip::ZipArchive::new(std::io::Cursor::new(docx)).expect("zip");
    let mut f = a.by_name(name).expect("part present");
    let mut s = String::new();
    f.read_to_string(&mut s).expect("utf8 part");
    s
}

fn part_names(docx: &[u8]) -> Vec<String> {
    let a = zip::ZipArchive::new(std::io::Cursor::new(docx)).expect("zip");
    a.file_names().map(str::to_owned).collect()
}

/// Issue #100 — the reader hands the source root's attributes to the
/// tree, not only to the archive: the UI save path never sees the archive.
#[test]
fn read_docx_carries_root_attrs_on_the_tree() {
    let archive = read_docx(W14_FIXTURE).expect("read");
    assert!(
        archive
            .document
            .document_root_attrs
            .iter()
            .any(|(k, v)| k == "xmlns:w14" && v == W14_NS),
        "{:?}",
        archive.document.document_root_attrs
    );
    assert_eq!(
        archive.document.document_root_attrs,
        archive.document_root_attrs
    );
}

/// Issue #100 acceptance: open a Word-shaped document (`w14:paraId` on
/// every paragraph, bound only on the root), edit one paragraph, save
/// through the UI path — the part must be namespace-well-formed.
#[test]
fn ui_save_of_a_w14_document_is_namespace_well_formed() {
    let archive = read_docx(W14_FIXTURE).expect("read");
    let edited = archive.document.insert_text(
        LogicalPos {
            path: BlockPath::top(0),
            offset: 5,
        },
        " edited",
    );
    let bytes = build_minimal_docx(&edited).expect("UI save");
    check_document_xml_well_formed(&bytes).expect("UI save must be namespace-well-formed");
    let xml = part(&bytes, "word/document.xml");
    assert!(xml.contains(&format!(r#"xmlns:w14="{W14_NS}""#)), "{xml}");
    assert!(xml.contains(r#"mc:Ignorable="w14 w15""#), "{xml}");
    /* An engine-authored document keeps the minimal root. */
    let fresh = build_minimal_docx(&engine::DocumentTree::from_text("x")).expect("fresh");
    assert!(!part(&fresh, "word/document.xml").contains("w14"));
}

/// Issue #100 — the attrs ride the crash-recovery snapshot envelope, so a
/// recovered session still saves a well-formed file.
#[test]
fn root_attrs_survive_the_snapshot_envelope() {
    let archive = read_docx(W14_FIXTURE).expect("read");
    let bytes = engine::snapshot::encode(&archive.document).expect("encode");
    let back: engine::snapshot::Decoded<engine::DocumentTree> =
        engine::snapshot::decode(&bytes).expect("decode");
    assert_eq!(
        back.payload.document_root_attrs,
        archive.document_root_attrs
    );
}

/// A Word-shaped package with a header part whose paragraph carries
/// `w14:paraId` (Word binds the same prefix set on every part root).
fn w14_header_package() -> Vec<u8> {
    let root = concat!(
        r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" "#,
        r#"xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" "#,
        r#"xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml" "#,
        r#"xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006" "#,
        r#"mc:Ignorable="w14""#,
    );
    let document = format!(
        concat!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
            "<w:document {root}><w:body>",
            r#"<w:p w14:paraId="11111111"><w:r><w:t xml:space="preserve">body</w:t></w:r></w:p>"#,
            r#"<w:sectPr><w:headerReference w:type="default" r:id="rId7"/></w:sectPr>"#,
            "</w:body></w:document>",
        ),
        root = root
    );
    let header = format!(
        concat!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
            "<w:hdr {root}>",
            r#"<w:p w14:paraId="22222222"><w:r><w:t xml:space="preserve">head</w:t></w:r></w:p>"#,
            "</w:hdr>",
        ),
        root = root
    );
    let rels = concat!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
        r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#,
        r#"<Relationship Id="rId7" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/>"#,
        "</Relationships>",
    );
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, body) in [
            ("word/document.xml", document.as_str()),
            ("word/header1.xml", header.as_str()),
            ("word/_rels/document.xml.rels", rels),
        ] {
            zip.start_file(name, opts).unwrap();
            zip.write_all(body.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    buf
}

/// Issue #100 — a regenerated header part root re-declares the source
/// root's bindings on the UI save path too: the header's clean
/// `w14:paraId` paragraph rides the passthrough under a synthesized
/// `<w:hdr>`.
#[test]
fn ui_save_redeclares_root_bindings_on_regenerated_header_parts() {
    let archive = read_docx(&w14_header_package()).expect("read");
    let mut doc = archive.document.clone();
    assert!(doc.headers.contains_key("rId7"), "header parsed");
    doc.hf_dirty.headers.insert("rId7".into());
    let bytes = build_minimal_docx(&doc).expect("UI save");
    check_document_xml_well_formed(&bytes).expect("document.xml");
    let headers: Vec<String> = part_names(&bytes)
        .into_iter()
        .filter(|n| n.starts_with("word/header") && n.ends_with(".xml"))
        .collect();
    assert_eq!(headers.len(), 1, "{headers:?}");
    check_part_xml_well_formed(&bytes, &headers[0]).expect("regenerated header root must bind w14");
    assert!(part(&bytes, &headers[0]).contains(r#"w14:paraId="22222222""#));
}

/// Issue #101 — a cell paragraph reads through the body run parser:
/// styled spans, rPr grab bags and inline pictures, identical to what the
/// same `<w:p>` yields at body level.
#[test]
fn cell_paragraphs_read_runs_grab_bags_and_pictures() {
    let archive = read_docx(CELL_RUNS_FIXTURE).expect("read");
    let table = archive.document.blocks[1].as_table().expect("table");
    let c0 = table.rows[0].cells[0].blocks[0]
        .as_paragraph()
        .expect("cell paragraph");
    assert_eq!(c0.text, "Bold plain red italicserif");
    let bold = c0.spans.iter().find(|s| s.start == 0).expect("bold span");
    assert_eq!(bold.style.bold, Some(true));
    assert_eq!(bold.end, 4);
    let red = c0
        .spans
        .iter()
        .find(|s| s.style.color.is_some())
        .expect("red span");
    assert_eq!(red.style.color, Some([0xFF, 0, 0, 0xFF]));
    assert_eq!(red.style.italic, Some(true));
    assert_eq!(red.style.font_size, Some(14.0));
    let serif = c0.spans.iter().max_by_key(|s| s.start).expect("serif");
    assert!(
        serif.style.grab_bag.is_some(),
        "unmodeled <w:lang> must ride the run grab bag: {serif:?}"
    );
    let c1 = table.rows[0].cells[1].blocks[0]
        .as_paragraph()
        .expect("picture cell");
    assert_eq!(c1.text, "under\u{FFFC}pic");
    assert!(matches!(
        &c1.inline_objects[..],
        [o] if matches!(&o.kind, InlineKind::Image { rel_id, .. } if rel_id == "rId5")
    ));
    assert!(archive.document.media.contains_key("rId5"));
}

/// Issue #101 — editing one cell regenerates the table; the edited
/// paragraph keeps every other run's `<w:rPr>` byte-for-byte, and the
/// picture in the other (also edited) cell survives.
#[test]
fn editing_a_cell_keeps_its_run_properties_and_pictures() {
    let archive = read_docx(CELL_RUNS_FIXTURE).expect("read");
    let cell = |col| {
        BlockPath::top(1)
            .push(PathStep::Cell { row: 0, col })
            .push(PathStep::Block(0))
    };
    let edited = archive
        .document
        .insert_text(
            LogicalPos {
                path: cell(0),
                offset: 7,
            },
            "X",
        )
        .insert_text(
            LogicalPos {
                path: cell(1),
                offset: 9,
            },
            "Y",
        );
    for bytes in [
        write_docx(&archive, &edited).expect("archive save"),
        build_minimal_docx(&edited).expect("UI save"),
    ] {
        check_document_xml_well_formed(&bytes).expect("well-formed");
        let xml = part(&bytes, "word/document.xml");
        for frag in [
            r#"<w:r><w:rPr><w:b/></w:rPr><w:t xml:space="preserve">Bold</w:t></w:r>"#,
            r#"<w:r><w:t xml:space="preserve"> plXain </w:t></w:r>"#,
            r#"<w:r><w:rPr><w:i/><w:color w:val="FF0000"/><w:sz w:val="28"/><w:szCs w:val="28"/></w:rPr><w:t xml:space="preserve">red italic</w:t></w:r>"#,
            r#"<w:r><w:rPr><w:rFonts w:ascii="Georgia" w:hAnsi="Georgia" w:cs="Georgia"/><w:lang w:val="en-GB"/></w:rPr><w:t xml:space="preserve">serif</w:t></w:r>"#,
            r#"<w:r><w:rPr><w:u w:val="single"/></w:rPr><w:t xml:space="preserve">under</w:t></w:r>"#,
            r#"<a:blip r:embed="rId5"/>"#,
            r#"<w:t xml:space="preserve">pYic</w:t>"#,
        ] {
            assert!(xml.contains(frag), "missing `{frag}` in\n{xml}");
        }
        let back = read_docx(&bytes).expect("re-read");
        let t = back.document.blocks[1].as_table().expect("table");
        let c1 = t.rows[0].cells[1].blocks[0].as_paragraph().expect("cell");
        assert_eq!(c1.inline_objects.len(), 1, "cell picture survives");
        assert!(back.document.media.contains_key("rId5"));
    }
}

/// Issue #100 × #80 — a note part whose OWN root binds `w14` (the
/// document root does not) is regenerated by the UI save path from the
/// tree alone; the reader records the part's root bindings on the tree
/// (`part_root_attrs`) so the regenerated `<w:footnotes>` re-declares them.
#[test]
fn ui_save_of_note_parts_is_namespace_well_formed() {
    const NOTES: &[u8] = include_bytes!("fixtures/footnotes_endnotes.docx");
    let archive = read_docx(NOTES).expect("read");
    let edited = archive.document.insert_text(
        LogicalPos {
            path: BlockPath::top(0),
            offset: 0,
        },
        "X",
    );
    let bytes = build_minimal_docx(&edited).expect("UI save");
    check_document_xml_well_formed(&bytes).expect("document.xml");
    check_part_xml_well_formed(&bytes, "word/footnotes.xml")
        .expect("regenerated footnotes.xml must bind w14");
}
