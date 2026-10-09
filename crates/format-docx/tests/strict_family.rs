//! Issue #325 — ISO/IEC 29500 Strict namespace family.
//!
//! No real Word-saved Strict file is available in this repo (LibreOffice
//! cannot produce one), so the Strict twin is derived programmatically
//! from `word_package_parts.docx` — every XML/rels part rewritten from
//! the Transitional to the Strict URIs by the family table — which pins
//! the contract (read parity, zero-edit identity, dirty-save family) but
//! not Word's exact Strict output. Verify against a real Strict file when
//! one is available.

use engine::{BlockPath, DocumentTree, ImageBlob, LogicalPos};
use format_docx::schema::NsFamily;
use format_docx::schema::family::to_family;
use format_docx::{
    DocxWarning, check_document_xml_well_formed, read_docx, write_docx, write_docx_with_notes,
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
