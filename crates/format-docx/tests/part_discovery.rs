//! Issue #353 — the main part and its siblings are found through the
//! package's relationships (`_rels/.rels` → `officeDocument`; the main
//! part's rels by `Type`), with percent-encoded / `..` / `/`-absolute
//! targets normalised, the fixed `word/…` names only as a fallback, and a
//! target that escapes the package refused with a typed error.

use engine::{BlockPath, DocumentTree, ImageBlob, LogicalPos};
use format_docx::test_fixtures::inline_picture_run;
use format_docx::{
    DocxError, DocxWarning, check_document_xml_well_formed, read_docx, save_docx, write_docx,
};
use std::io::{Read, Write};

const REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/";
const NS: &str = "xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" \
    xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" \
    xmlns:wp=\"http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing\" \
    xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" \
    xmlns:pic=\"http://schemas.openxmlformats.org/drawingml/2006/picture\"";

fn rels(rows: &[(&str, String, &str)]) -> String {
    let mut s = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
         <Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">",
    );
    for (id, ty, target) in rows {
        s.push_str(&format!(
            "<Relationship Id=\"{id}\" Type=\"{ty}\" Target=\"{target}\"/>"
        ));
    }
    s.push_str("</Relationships>");
    s
}

fn zip_of(parts: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut buf = Vec::new();
    {
        let mut z = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, bytes) in parts {
            z.start_file(*name, opts).expect("start");
            z.write_all(bytes).expect("write");
        }
        z.finish().expect("finish");
    }
    buf
}

fn entries(docx: &[u8]) -> Vec<(String, Vec<u8>)> {
    let mut a = zip::ZipArchive::new(std::io::Cursor::new(docx)).expect("zip");
    (0..a.len())
        .map(|i| {
            let mut f = a.by_index(i).expect("entry");
            let mut b = Vec::new();
            f.read_to_end(&mut b).expect("read");
            (f.name().to_owned(), b)
        })
        .collect()
}

fn entry<'a>(all: &'a [(String, Vec<u8>)], name: &str) -> &'a [u8] {
    all.iter()
        .find(|(n, _)| n == name)
        .map(|(_, b)| b.as_slice())
        .unwrap_or_else(|| panic!("missing entry {name}"))
}

const PNG: &[u8] = &[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 7];

/// A package whose main part is `word/sub/main doc.xml` (renamed, in a
/// subdirectory, with a space), referenced percent-encoded and
/// package-absolute from `_rels/.rels`; its styles via a `..`-relative
/// target, its picture via `../media/image1.png`.
fn renamed_package() -> Vec<u8> {
    let pic = inline_picture_run("rId5");
    let document = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
         <w:document {NS}><w:body>\
         <w:p><w:r><w:t>Renamed main</w:t></w:r>{pic}</w:p>\
         <w:p><w:pPr><w:pStyle w:val=\"Big\"/></w:pPr><w:r><w:t>styled</w:t></w:r></w:p>\
         <w:sectPr><w:pgSz w:w=\"11906\" w:h=\"16838\"/>\
         <w:pgMar w:top=\"1440\" w:right=\"1440\" w:bottom=\"1440\" w:left=\"1440\" w:header=\"708\" w:footer=\"708\" w:gutter=\"0\"/>\
         </w:sectPr></w:body></w:document>"
    );
    let styles = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
        <w:styles xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\
        <w:style w:type=\"paragraph\" w:styleId=\"Big\"><w:name w:val=\"Big\"/>\
        <w:pPr><w:jc w:val=\"center\"/></w:pPr></w:style></w:styles>";
    let dot_rels = rels(&[(
        "rId1",
        format!("{REL}officeDocument"),
        "/word/sub/main%20doc.xml",
    )]);
    let main_rels = rels(&[
        ("rId2", format!("{REL}styles"), "../my%20styles.xml"),
        ("rId5", format!("{REL}image"), "../media/image1.png"),
    ]);
    zip_of(&[
        (
            "[Content_Types].xml",
            br#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="xml" ContentType="application/xml"/><Default Extension="png" ContentType="image/png"/></Types>"#.to_vec(),
        ),
        ("_rels/.rels", dot_rels.into_bytes()),
        ("word/sub/main doc.xml", document.into_bytes()),
        (
            "word/sub/_rels/main doc.xml.rels",
            main_rels.into_bytes(),
        ),
        ("word/my styles.xml", styles.as_bytes().to_vec()),
        ("word/media/image1.png", PNG.to_vec()),
    ])
}

#[test]
fn renamed_main_part_reads_with_its_siblings_found_by_type() {
    let archive = read_docx(&renamed_package()).expect("read");
    assert!(archive.warnings.is_empty(), "{:?}", archive.warnings);
    assert_eq!(archive.part_names.main, "word/sub/main doc.xml");
    assert_eq!(archive.part_names.styles, "word/my styles.xml");
    let doc = &archive.document;
    assert_eq!(doc.nth_paragraph(0).unwrap().text, "Renamed main\u{FFFC}");
    /* The style resolved through the `..` + `%20` target: centred. */
    assert_eq!(
        doc.nth_paragraph(1).unwrap().props.alignment,
        Some(engine::Alignment::Center),
        "styles.xml found by relationship type"
    );
    /* `../media/image1.png` registered as the picture's media key. */
    let key = doc
        .nth_paragraph(0)
        .unwrap()
        .inline_objects
        .iter()
        .find_map(|io| io.kind.image_media_key())
        .expect("image media key");
    assert_eq!(key, "word/media/image1.png");
    assert_eq!(doc.media[key].data, PNG);
}

#[test]
fn renamed_main_part_round_trips_zero_edit_and_dirty() {
    let src = renamed_package();
    let archive = read_docx(&src).expect("read");
    let saved = write_docx(&archive, &archive.document).expect("zero-edit");
    let (out, orig) = (entries(&saved), entries(&src));
    let sorted = |e: &[(String, Vec<u8>)]| {
        let mut v: Vec<String> = e.iter().map(|(n, _)| n.clone()).collect();
        v.sort();
        v
    };
    assert_eq!(sorted(&out), sorted(&orig), "no stray word/document.xml");
    for (name, bytes) in &orig {
        assert!(entry(&out, name) == bytes.as_slice(), "{name} drifted");
    }

    /* Dirty save through BOTH save paths: the edit lands in the renamed
    part, no `word/document.xml` appears, and the result re-reads. */
    let edited = archive.document.insert_text(
        LogicalPos {
            path: BlockPath::top(1),
            offset: 0,
        },
        "NEW ",
    );
    for saved in [
        write_docx(&archive, &edited).expect("write"),
        save_docx(&edited).expect("save_docx via source package"),
    ] {
        let out = entries(&saved);
        assert!(!out.iter().any(|(n, _)| n == "word/document.xml"));
        let main = String::from_utf8_lossy(entry(&out, "word/sub/main doc.xml")).into_owned();
        assert!(main.contains("NEW "), "{main}");
        check_document_xml_well_formed(&saved).expect("well-formed renamed main");
        let again = read_docx(&saved).expect("re-read");
        assert_eq!(again.part_names.main, "word/sub/main doc.xml");
        assert_eq!(again.document.nth_paragraph(1).unwrap().text, "NEW styled");
    }
}

#[test]
fn picture_inserted_into_a_renamed_main_lands_next_to_it() {
    let archive = read_docx(&renamed_package()).expect("read");
    let len = archive.document.nth_paragraph(1).unwrap().text.len() as u32;
    let edited = archive.document.insert_inline_image_at(
        LogicalPos {
            path: BlockPath::top(1),
            offset: len,
        },
        ImageBlob {
            content_type: "image/png".into(),
            data: vec![0x89, 0x50, 0x4e, 0x47, 1, 2, 3],
        },
        914_400,
        914_400,
    );
    let saved = write_docx(&archive, &edited).expect("write");
    let out = entries(&saved);
    let new_rels = String::from_utf8_lossy(entry(&out, "word/sub/_rels/main doc.xml.rels"));
    assert!(new_rels.contains("Id=\"rId6\""), "{new_rels}");
    /* The new media part sits where the rels target resolves from the
    main part's directory (`word/sub/media/…`), not under a fixed
    `word/media/`. */
    assert!(
        new_rels.contains("Target=\"media/image1.png\""),
        "{new_rels}"
    );
    assert!(out.iter().any(|(n, _)| n == "word/sub/media/image1.png"));
    let again = read_docx(&saved).expect("re-read");
    let keys: Vec<&str> = again
        .document
        .nth_paragraph(1)
        .unwrap()
        .inline_objects
        .iter()
        .filter_map(|io| io.kind.image_media_key())
        .collect();
    assert_eq!(keys, vec!["word/sub/media/image1.png"]);
}

#[test]
fn fixed_names_remain_the_fallback() {
    /* No `_rels/.rels` at all: `word/document.xml` is the main part. */
    let doc = format!(
        "<w:document {NS}><w:body><w:p><w:r><w:t>plain</w:t></w:r></w:p></w:body></w:document>"
    );
    let bytes = zip_of(&[("word/document.xml", doc.into_bytes())]);
    let archive = read_docx(&bytes).expect("read");
    assert_eq!(archive.part_names.main, "word/document.xml");
    assert_eq!(archive.document.nth_paragraph(0).unwrap().text, "plain");

    /* A root relationship naming a part the archive lacks: fall back, loudly. */
    let dot = rels(&[("rId1", format!("{REL}officeDocument"), "word/gone.xml")]);
    let doc = format!(
        "<w:document {NS}><w:body><w:p><w:r><w:t>plain</w:t></w:r></w:p></w:body></w:document>"
    );
    let bytes = zip_of(&[
        ("_rels/.rels", dot.into_bytes()),
        ("word/document.xml", doc.into_bytes()),
    ]);
    let archive = read_docx(&bytes).expect("read");
    assert_eq!(archive.document.nth_paragraph(0).unwrap().text, "plain");
    assert_eq!(
        archive.warnings,
        vec![DocxWarning::MainPartFallback {
            target: "word/gone.xml".into()
        }]
    );

    /* …and with no fallback either, a typed missing-entry error. */
    let dot = rels(&[("rId1", format!("{REL}officeDocument"), "word/gone.xml")]);
    let bytes = zip_of(&[("_rels/.rels", dot.into_bytes())]);
    assert!(matches!(
        read_docx(&bytes),
        Err(DocxError::MissingEntry(n)) if n == "word/gone.xml"
    ));
}

#[test]
fn a_main_target_escaping_the_package_is_a_typed_error() {
    for target in ["../../etc/passwd.xml", "word/../../x.xml", "..\\x.xml"] {
        let dot = rels(&[("rId1", format!("{REL}officeDocument"), target)]);
        let doc = format!("<w:document {NS}><w:body/></w:document>");
        let bytes = zip_of(&[
            ("_rels/.rels", dot.into_bytes()),
            ("word/document.xml", doc.into_bytes()),
        ]);
        assert!(
            matches!(read_docx(&bytes), Err(DocxError::UnsafePartName(_))),
            "{target}"
        );
    }
}

#[test]
fn a_self_referencing_header_and_main_terminate() {
    let document = format!(
        "<?xml version=\"1.0\"?><w:document {NS}><w:body>\
         <w:p><w:r><w:t>body</w:t></w:r></w:p>\
         <w:sectPr><w:headerReference w:type=\"default\" r:id=\"rId7\"/>\
         <w:pgSz w:w=\"11906\" w:h=\"16838\"/></w:sectPr></w:body></w:document>"
    );
    let header =
        format!("<?xml version=\"1.0\"?><w:hdr {NS}><w:p><w:r><w:t>head</w:t></w:r></w:p></w:hdr>");
    let dot = rels(&[("rId1", format!("{REL}officeDocument"), "word/document.xml")]);
    /* The main part relates to itself AND to the header; the header's
    own rels point back at the header and at the main part. */
    let doc_rels = rels(&[
        ("rId7", format!("{REL}header"), "header1.xml"),
        ("rId8", format!("{REL}officeDocument"), "document.xml"),
        ("rId9", format!("{REL}styles"), "document.xml"),
    ]);
    let header_rels = rels(&[
        ("rId1", format!("{REL}header"), "header1.xml"),
        (
            "rId2",
            format!("{REL}officeDocument"),
            "../word/document.xml",
        ),
        ("rId3", format!("{REL}image"), "header1.xml"),
    ]);
    let bytes = zip_of(&[
        ("_rels/.rels", dot.into_bytes()),
        ("word/document.xml", document.into_bytes()),
        ("word/_rels/document.xml.rels", doc_rels.into_bytes()),
        ("word/header1.xml", header.into_bytes()),
        ("word/_rels/header1.xml.rels", header_rels.into_bytes()),
    ]);
    let archive = read_docx(&bytes).expect("read terminates");
    assert_eq!(archive.document.nth_paragraph(0).unwrap().text, "body");
    assert_eq!(archive.document.headers.len(), 1);
    let saved = write_docx(&archive, &archive.document).expect("write terminates");
    assert_eq!(entries(&saved).len(), entries(&bytes).len());
    let _: &DocumentTree = &archive.document;
}
