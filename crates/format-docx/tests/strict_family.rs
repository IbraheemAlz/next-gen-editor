//! Issue #325 — ISO/IEC 29500 Strict namespace family.
//!
//! No real Word-saved Strict file is available in this repo (LibreOffice
//! cannot produce one), so the Strict twin is derived programmatically
//! from `word_package_parts.docx` — every XML/rels part rewritten from
//! the Transitional to the Strict URIs by the family table — which pins
//! the contract (read parity, zero-edit identity, dirty-save family) but
//! not Word's exact Strict output. Verify against a real Strict file when
//! one is available.

use engine::{Block, BlockPath, DocumentTree, ImageBlob, LogicalPos};
use format_docx::schema::NsFamily;
use format_docx::schema::family::to_family;
use format_docx::{
    DocxWarning, check_document_xml_well_formed, check_part_xml_well_formed, read_docx, save_docx,
    write_docx, write_docx_with_notes,
};
use std::io::{Read, Write};

const FIXTURE: &[u8] = include_bytes!("fixtures/word_package_parts.docx");

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

fn zip_of(parts: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut buf = Vec::new();
    {
        let mut z = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, bytes) in parts {
            z.start_file(name.as_str(), opts).expect("start");
            z.write_all(bytes).expect("write");
        }
        z.finish().expect("finish");
    }
    buf
}

/// The Strict twin: every XML / rels part's schema URIs and relationship
/// types rewritten through the family table (media bytes untouched).
fn strictify(docx: &[u8]) -> Vec<u8> {
    let parts: Vec<(String, Vec<u8>)> = entries(docx)
        .into_iter()
        .map(|(name, bytes)| {
            let is_xml = name.ends_with(".xml") || name.ends_with(".rels");
            match std::str::from_utf8(&bytes) {
                Ok(text) if is_xml => (
                    name,
                    to_family(text, NsFamily::Strict).into_owned().into_bytes(),
                ),
                _ => (name, bytes),
            }
        })
        .collect();
    zip_of(&parts)
}

/// What the layout depends on, per paragraph (no source bytes).
fn model_digest(doc: &DocumentTree) -> Vec<String> {
    (0..doc.paragraph_count())
        .filter_map(|i| doc.nth_paragraph(i))
        .map(|p| {
            format!(
                "{:?}|{:?}|{:?}|{}",
                p.text,
                p.props,
                p.spans,
                p.inline_objects.len()
            )
        })
        .collect()
}

fn insert_picture(doc: &DocumentTree) -> DocumentTree {
    let len = doc.nth_paragraph(3).expect("paragraph 3").text.len() as u32;
    doc.insert_inline_image_at(
        LogicalPos {
            path: BlockPath::top(3),
            offset: len,
        },
        ImageBlob {
            content_type: "image/jpeg".into(),
            data: vec![0xFF, 0xD8, 0xFF, 0xE0, 0, 0x10, b'J', b'F', b'I', b'F'],
        },
        914_400,
        914_400,
    )
}

#[test]
fn the_twin_really_is_strict() {
    let strict = strictify(FIXTURE);
    let parts = entries(&strict);
    let doc = std::str::from_utf8(entry(&parts, "word/document.xml")).unwrap();
    assert!(doc.contains("http://purl.oclc.org/ooxml/wordprocessingml/main"));
    assert!(!doc.contains("schemas.openxmlformats.org/wordprocessingml"));
    let rels = std::str::from_utf8(entry(&parts, "word/_rels/document.xml.rels")).unwrap();
    assert!(rels.contains("http://purl.oclc.org/ooxml/officeDocument/relationships/image"));
    assert!(!rels.contains("schemas.openxmlformats.org/officeDocument"));
}

#[test]
fn strict_twin_reads_identically_to_its_transitional_source() {
    let t = read_docx(FIXTURE).expect("read transitional");
    let s = read_docx(&strictify(FIXTURE)).expect("read strict");
    assert!(
        s.warnings.is_empty(),
        "Strict is the fast path: {:?}",
        s.warnings
    );
    let digest = model_digest(&t.document);
    assert!(digest.len() >= 4, "fixture has content");
    assert_eq!(model_digest(&s.document), digest);
    /* Same pictures resolved through the (Strict) rels. */
    let mut tk: Vec<&String> = t.document.media.keys().collect();
    let mut sk: Vec<&String> = s.document.media.keys().collect();
    tk.sort();
    sk.sort();
    assert!(!tk.is_empty());
    assert_eq!(tk, sk);
    for k in tk {
        assert_eq!(t.document.media[k].data, s.document.media[k].data);
    }
}

#[test]
fn zero_edit_save_of_strict_is_byte_identical_for_every_part() {
    let strict = strictify(FIXTURE);
    let archive = read_docx(&strict).expect("read");
    let (saved, notes) = write_docx_with_notes(&archive, &archive.document).expect("write");
    assert!(notes.is_empty());
    let (out, src) = (entries(&saved), entries(&strict));
    let names = |e: &[(String, Vec<u8>)]| {
        let mut v = e.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>();
        v.sort();
        v
    };
    assert_eq!(names(&out), names(&src), "same parts");
    for (name, bytes) in &src {
        assert!(
            entry(&out, name) == bytes.as_slice(),
            "{name} drifted:\n--- src\n{}\n--- out\n{}",
            String::from_utf8_lossy(bytes),
            String::from_utf8_lossy(entry(&out, name))
        );
    }
}

#[test]
fn dirty_save_of_strict_stays_in_the_strict_family() {
    let strict = strictify(FIXTURE);
    let archive = read_docx(&strict).expect("read");
    let edited = insert_picture(&archive.document);
    let edited = edited.insert_text(
        LogicalPos {
            path: BlockPath::top(0),
            offset: 0,
        },
        "edited ",
    );
    let saved = write_docx(&archive, &edited).expect("write");
    check_document_xml_well_formed(&saved).expect("well-formed");
    let out = entries(&saved);

    let doc = std::str::from_utf8(entry(&out, "word/document.xml")).unwrap();
    assert!(doc.contains("edited "), "the edit landed");
    assert!(doc.contains("r:embed=\"rId12\""), "{doc}");
    let rels = std::str::from_utf8(entry(&out, "word/_rels/document.xml.rels")).unwrap();
    assert!(
        rels.contains(
            "Id=\"rId12\" Type=\"http://purl.oclc.org/ooxml/officeDocument/relationships/image\""
        ),
        "minted rel type follows the source family: {rels}"
    );
    for (name, bytes) in &out {
        if !(name.ends_with(".xml") || name.ends_with(".rels")) {
            continue;
        }
        let text = String::from_utf8_lossy(bytes);
        for transitional in [
            "schemas.openxmlformats.org/wordprocessingml/2006",
            "schemas.openxmlformats.org/drawingml/2006",
            "schemas.openxmlformats.org/officeDocument/2006/relationships",
            "schemas.openxmlformats.org/officeDocument/2006/math",
        ] {
            assert!(
                !text.contains(transitional),
                "{name} leaked a Transitional URI ({transitional})"
            );
        }
    }

    /* Re-reads clean, same family, and a further zero-edit save is stable. */
    let reread = read_docx(&saved).expect("re-read");
    assert!(reread.warnings.is_empty());
    assert_eq!(
        reread.document.media.len(),
        archive.document.media.len() + 1
    );
    let resaved = write_docx(&reread, &reread.document).expect("resave");
    assert_eq!(entries(&resaved), out);
}

#[test]
fn transitional_dirty_save_still_mints_transitional() {
    let archive = read_docx(FIXTURE).expect("read");
    let edited = insert_picture(&archive.document);
    let saved = write_docx(&archive, &edited).expect("write");
    let out = entries(&saved);
    let rels = std::str::from_utf8(entry(&out, "word/_rels/document.xml.rels")).unwrap();
    assert!(rels.contains(
        "Id=\"rId12\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/image\""
    ));
    assert!(!rels.contains("purl.oclc.org"));
}

#[test]
fn non_canonical_prefix_reads_with_a_typed_warning_not_an_empty_document() {
    let xml = "<?xml version=\"1.0\"?>\n<x:document xmlns:x=\"http://purl.oclc.org/ooxml/wordprocessingml/main\">\
        <x:body><x:p><x:r><x:t>hello</x:t></x:r></x:p></x:body></x:document>";
    let docx = zip_of(&[("word/document.xml".to_string(), xml.as_bytes().to_vec())]);
    let archive = read_docx(&docx).expect("read");
    assert_eq!(archive.document.nth_paragraph(0).unwrap().text, "hello");
    assert!(matches!(
        archive.warnings.as_slice(),
        [DocxWarning::NonCanonicalNamespaces {
            normalized: true,
            ..
        }]
    ));
    /* Regenerate-only: the saved part is canonical and re-reads silently. */
    let saved = write_docx(&archive, &archive.document).expect("write");
    check_document_xml_well_formed(&saved).expect("well-formed");
    let again = read_docx(&saved).expect("re-read");
    assert!(again.warnings.is_empty(), "{:?}", again.warnings);
    assert_eq!(again.document.nth_paragraph(0).unwrap().text, "hello");

    /* The default-namespace spelling too. */
    let xml = "<document xmlns=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\
        <body><p><r><t>dflt</t></r></p></body></document>";
    let docx = zip_of(&[("word/document.xml".to_string(), xml.as_bytes().to_vec())]);
    let archive = read_docx(&docx).expect("read");
    assert_eq!(archive.document.nth_paragraph(0).unwrap().text, "dflt");
    assert_eq!(archive.warnings.len(), 1);
}

#[test]
fn a_root_that_is_not_wordprocessingml_is_reported() {
    let xml = "<html xmlns=\"http://www.w3.org/1999/xhtml\"><body/></html>";
    let docx = zip_of(&[("word/document.xml".to_string(), xml.as_bytes().to_vec())]);
    let archive = read_docx(&docx).expect("read");
    assert_eq!(archive.warnings, vec![DocxWarning::NotWordprocessingMl]);
}

/* ===== Issue #394 — every WordprocessingML part, not just the main one ===== */

const NS_W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

const PACKAGE_RELS: &str = "<?xml version=\"1.0\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
    <Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/>\
    </Relationships>";

const DOCUMENT_RELS: &str = "<?xml version=\"1.0\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
    <Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles\" Target=\"styles.xml\"/>\
    <Relationship Id=\"rId2\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/header\" Target=\"header1.xml\"/>\
    <Relationship Id=\"rId3\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/footnotes\" Target=\"footnotes.xml\"/>\
    <Relationship Id=\"rId4\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering\" Target=\"numbering.xml\"/>\
    <Relationship Id=\"rId5\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments\" Target=\"comments.xml\"/>\
    </Relationships>";

/// A canonical main part whose `styles.xml`, `header1.xml`,
/// `numbering.xml` and `comments.xml` bind WordprocessingML to `x:` and
/// whose `footnotes.xml` makes it the default namespace (attributes
/// through a second, canonical `w:` binding — unprefixed attributes are
/// in no namespace).
fn sibling_prefix_package() -> Vec<u8> {
    let document = format!(
        "<?xml version=\"1.0\"?><w:document xmlns:w=\"{NS_W}\" \
         xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><w:body>\
         <w:p><w:pPr><w:pStyle w:val=\"Centred\"/></w:pPr><w:r><w:t>body</w:t></w:r></w:p>\
         <w:p><w:pPr><w:numPr><w:ilvl w:val=\"0\"/><w:numId w:val=\"1\"/></w:numPr></w:pPr><w:r><w:t>item</w:t></w:r></w:p>\
         <w:sectPr><w:headerReference w:type=\"default\" r:id=\"rId2\"/></w:sectPr></w:body></w:document>"
    );
    let styles = format!(
        "<?xml version=\"1.0\"?>\n<x:styles xmlns:x=\"{NS_W}\"><x:docDefaults><x:rPrDefault><x:rPr>\
         <x:sz x:val=\"28\"/></x:rPr></x:rPrDefault></x:docDefaults>\
         <x:style x:type=\"paragraph\" x:styleId=\"Centred\"><x:name x:val=\"Centred\"/>\
         <x:pPr><x:jc x:val=\"center\"/></x:pPr><x:rPr><x:b/></x:rPr></x:style></x:styles>"
    );
    let header = format!(
        "<?xml version=\"1.0\"?>\n<x:hdr xmlns:x=\"{NS_W}\"><x:p><x:r><x:t>header text</x:t></x:r></x:p></x:hdr>"
    );
    let footnotes = format!(
        "<?xml version=\"1.0\"?>\n<footnotes xmlns=\"{NS_W}\" xmlns:w=\"{NS_W}\">\
         <footnote w:id=\"1\"><p><r><t>note text</t></r></p></footnote></footnotes>"
    );
    let numbering = format!(
        "<?xml version=\"1.0\"?>\n<x:numbering xmlns:x=\"{NS_W}\"><x:abstractNum x:abstractNumId=\"0\">\
         <x:lvl x:ilvl=\"0\"><x:start x:val=\"1\"/><x:numFmt x:val=\"decimal\"/><x:lvlText x:val=\"%1.\"/></x:lvl>\
         </x:abstractNum><x:num x:numId=\"1\"><x:abstractNumId x:val=\"0\"/></x:num></x:numbering>"
    );
    let comments = format!(
        "<?xml version=\"1.0\"?>\n<x:comments xmlns:x=\"{NS_W}\"><x:comment x:id=\"0\" x:author=\"Rev\">\
         <x:p><x:r><x:t>comment text</x:t></x:r></x:p></x:comment></x:comments>"
    );
    let part = |name: &str, body: &str| (name.to_string(), body.as_bytes().to_vec());
    zip_of(&[
        part("_rels/.rels", PACKAGE_RELS),
        part("word/_rels/document.xml.rels", DOCUMENT_RELS),
        part("word/document.xml", &document),
        part("word/styles.xml", &styles),
        part("word/header1.xml", &header),
        part("word/footnotes.xml", &footnotes),
        part("word/numbering.xml", &numbering),
        part("word/comments.xml", &comments),
    ])
}

fn block_text(blocks: &[Block]) -> String {
    blocks
        .iter()
        .filter_map(Block::as_paragraph)
        .map(|p| p.text.as_str())
        .collect::<Vec<_>>()
        .join("|")
}

/// The styles cascade, the header renders, the footnote and the comment
/// read and the list marker resolves.
fn assert_siblings_read(archive: &format_docx::DocxArchive) {
    let doc = &archive.document;
    let p = doc.nth_paragraph(0).expect("body paragraph");
    assert_eq!(p.text, "body");
    assert_eq!(
        p.props.alignment,
        Some(engine::Alignment::Center),
        "style pPr"
    );
    assert_eq!(
        doc.resolve_style_run_cascade(Some("Centred")).bold,
        Some(true)
    );
    assert_eq!(doc.style_run_defaults.font_size, Some(14.0), "docDefaults");
    assert_eq!(block_text(&doc.headers["rId2"]), "header text");
    assert_eq!(block_text(&doc.footnote_stories[&1].body), "note text");
    let item = doc.nth_paragraph(1).expect("list paragraph");
    assert_eq!(item.resolved_marker.as_deref(), Some("1."), "numbering.xml");
    let comment = doc.comment_defs.get(&0).expect("comments.xml");
    assert_eq!(
        (comment.author.as_str(), comment.paragraphs.as_slice()),
        ("Rev", &["comment text".to_string()][..])
    );
}

/// Issue #394 — each non-canonical sibling is normalised before it is
/// parsed and reported once, by name; the part is then regenerate-only:
/// a zero-edit save re-emits its normalised bytes, which re-read
/// silently, and a regenerated header stays namespace-well-formed.
#[test]
fn non_canonical_sibling_parts_are_normalised_and_reported_per_part() {
    let archive = read_docx(&sibling_prefix_package()).expect("read");
    assert_siblings_read(&archive);
    let mut parts: Vec<(&str, bool)> = archive
        .warnings
        .iter()
        .map(|w| match w {
            DocxWarning::NonCanonicalNamespaces {
                part, normalized, ..
            } => (part.as_str(), *normalized),
            other => panic!("unexpected warning {other:?}"),
        })
        .collect();
    parts.sort();
    assert_eq!(
        parts,
        vec![
            ("word/comments.xml", true),
            ("word/footnotes.xml", true),
            ("word/header1.xml", true),
            ("word/numbering.xml", true),
            ("word/styles.xml", true),
        ],
        "the canonical main part is not reported"
    );

    let saved = write_docx(&archive, &archive.document).expect("write");
    assert_eq!(save_docx(&archive.document).expect("UI save"), saved);
    let out = entries(&saved);
    for name in [
        "word/styles.xml",
        "word/header1.xml",
        "word/footnotes.xml",
        "word/numbering.xml",
        "word/comments.xml",
    ] {
        check_part_xml_well_formed(&saved, name).expect(name);
        let text = std::str::from_utf8(entry(&out, name)).unwrap();
        assert!(
            !text.contains("<x:") && !text.contains("<footnote "),
            "{name}: {text}"
        );
    }
    let styles = std::str::from_utf8(entry(&out, "word/styles.xml")).unwrap();
    assert!(
        styles.starts_with("<?xml version=\"1.0\"?>\n<w:styles xmlns:w="),
        "{styles}"
    );
    let again = read_docx(&saved).expect("re-read");
    assert!(again.warnings.is_empty(), "{:?}", again.warnings);
    assert_siblings_read(&again);

    let mut doc = archive.document.clone();
    let mut blocks = doc.headers["rId2"].clone();
    if let Some(Block::Paragraph(p)) = blocks.first_mut() {
        p.text.push_str(" edited");
        p.dirty = true;
        p.source_xml = None;
        p.source_markup = None;
    }
    doc.headers.insert("rId2".into(), blocks);
    doc.hf_dirty.headers.insert("rId2".into());
    let edited = write_docx(&archive, &doc).expect("write edited header");
    check_part_xml_well_formed(&edited, "word/header1.xml").expect("header well-formed");
    let back = read_docx(&edited).expect("re-read edited header");
    assert!(back.warnings.is_empty(), "{:?}", back.warnings);
    assert_eq!(
        block_text(&back.document.headers["rId2"]),
        "header text edited"
    );

    /* `comments.xml` is patched in place (issue #282): the patch starts
    from the normalised bytes, so the appended comment is bound. */
    let (commented, _) = archive.document.insert_comment(
        LogicalPos::new(BlockPath::top(0), 0),
        LogicalPos::new(BlockPath::top(0), 4),
        "added".into(),
        "Rev".into(),
        "2026-10-09T00:00:00Z".into(),
    );
    let saved = write_docx(&archive, &commented).expect("write comment");
    check_part_xml_well_formed(&saved, "word/comments.xml").expect("comments well-formed");
    let back = read_docx(&saved).expect("re-read comment");
    assert!(back.warnings.is_empty(), "{:?}", back.warnings);
    let mut texts: Vec<String> = back
        .document
        .comment_defs
        .values()
        .flat_map(|c| c.paragraphs.clone())
        .collect();
    texts.sort();
    assert_eq!(texts, ["added", "comment text"]);
}

/// A canonical package stays on the fast path: no warning, every sibling
/// re-emitted verbatim.
#[test]
fn canonical_siblings_stay_on_the_fast_path() {
    let archive = read_docx(FIXTURE).expect("read");
    assert!(archive.warnings.is_empty(), "{:?}", archive.warnings);
    let saved = write_docx(&archive, &archive.document).expect("write");
    let (out, src) = (entries(&saved), entries(FIXTURE));
    for (name, bytes) in &src {
        assert!(entry(&out, name) == bytes.as_slice(), "{name} drifted");
    }
}
