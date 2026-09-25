//! Issue #135 — `write_docx` adds the OPC plumbing for pictures inserted
//! into an OPENED document: a `word/media/*` part, a relationship with a
//! non-colliding id, and a `[Content_Types].xml` default when the
//! extension is new — while every original entry stays byte-identical.

use engine::{BlockPath, DocumentTree, ImageBlob, InlineKind, LogicalPos};
use format_docx::opc::relationships::parse_relationships;
use format_docx::{DocxArchive, check_document_xml_well_formed, read_docx, write_docx};
use std::io::Read;

const FIXTURE: &[u8] = include_bytes!("fixtures/word_package_parts.docx");
const IMAGE_REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/image";

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

fn media_names(all: &[(String, Vec<u8>)]) -> Vec<&str> {
    all.iter()
        .map(|(n, _)| n.as_str())
        .filter(|n| n.starts_with("word/media/"))
        .collect()
}

/// Insert a picture at the end of paragraph 3 (`pictures ￼￼`).
fn with_inserted_picture(doc: &DocumentTree, mime: &str, bytes: &[u8]) -> DocumentTree {
    let len = doc.nth_paragraph(3).expect("paragraph 3").text.len() as u32;
    doc.insert_inline_image_at(
        LogicalPos {
            path: BlockPath::top(3),
            offset: len,
        },
        ImageBlob {
            content_type: mime.into(),
            data: bytes.to_vec(),
        },
        914_400,
        914_400,
    )
}

fn image_rel_ids(doc: &DocumentTree, para: u32) -> Vec<String> {
    doc.nth_paragraph(para)
        .expect("paragraph")
        .inline_objects
        .iter()
        .filter_map(|io| match &io.kind {
            InlineKind::Image { rel_id, .. } => Some(rel_id.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn inserted_picture_gets_a_media_part_relationship_and_content_type() {
    let archive: DocxArchive = read_docx(FIXTURE).expect("read");
    let source = entries(FIXTURE);
    const JPEG: &[u8] = &[0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, b'J', b'F', b'I', b'F'];
    let edited = with_inserted_picture(&archive.document, "image/jpeg", JPEG);
    let saved = write_docx(&archive, &edited).expect("write");
    check_document_xml_well_formed(&saved).expect("well-formed document.xml");
    let out = entries(&saved);

    /* Three media parts: the two originals byte-identical, the new one
    named above the highest existing `imageN`. */
    assert_eq!(
        media_names(&out),
        vec![
            "word/media/image1.png",
            "word/media/image2.png",
            "word/media/image3.jpeg"
        ]
    );
    for name in ["word/media/image1.png", "word/media/image2.png"] {
        assert_eq!(entry(&out, name), entry(&source, name), "{name}");
    }
    assert_eq!(entry(&out, "word/media/image3.jpeg"), JPEG);

    /* The rels part: every source row kept byte-identical (additive
    splice), one new image row with an id above EVERY rels part's rIdN
    (document rels top out at rId11). */
    let src_rels = std::str::from_utf8(entry(&source, "word/_rels/document.xml.rels")).unwrap();
    let out_rels = std::str::from_utf8(entry(&out, "word/_rels/document.xml.rels")).unwrap();
    let head = src_rels.strip_suffix("</Relationships>").unwrap();
    assert!(out_rels.starts_with(head), "{out_rels}");
    let rels = parse_relationships(out_rels.as_bytes()).expect("rels parse");
    assert_eq!(rels.items.len(), 12);
    let new = rels.by_id("rId12").expect("new image relationship");
    assert_eq!(new.rel_type, IMAGE_REL);
    assert_eq!(new.target, "media/image3.jpeg");
    let mut ids: Vec<&str> = rels.items.iter().map(|r| r.id.as_str()).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), 12, "relationship ids are unique");

    /* Content types gain exactly one `jpeg` default; source rows kept. */
    let src_ct = std::str::from_utf8(entry(&source, "[Content_Types].xml")).unwrap();
    let out_ct = std::str::from_utf8(entry(&out, "[Content_Types].xml")).unwrap();
    assert!(out_ct.starts_with(src_ct.strip_suffix("</Types>").unwrap()));
    assert!(out_ct.ends_with(r#"<Default Extension="jpeg" ContentType="image/jpeg"/></Types>"#));

    /* Every other sibling entry is byte-identical. */
    for (name, bytes) in &source {
        if matches!(
            name.as_str(),
            "word/document.xml" | "word/_rels/document.xml.rels" | "[Content_Types].xml"
        ) {
            continue;
        }
        assert_eq!(entry(&out, name), bytes.as_slice(), "{name} drifted");
    }

    /* The saved document references the package id, and re-reads with
    all three pictures resolvable. */
    let doc_xml = std::str::from_utf8(entry(&out, "word/document.xml")).unwrap();
    assert!(doc_xml.contains(r#"r:embed="rId12""#), "{doc_xml}");
    assert!(
        !doc_xml.contains("nge_img"),
        "engine ids never reach the file"
    );
    let reread = read_docx(&saved).expect("re-read");
    let ids = image_rel_ids(&reread.document, 3);
    assert_eq!(ids, vec!["rId9", "rId10", "rId12"]);
    /* Issue #188 — blobs are keyed by the part-resolved target path. */
    let keys: Vec<&str> = reread
        .document
        .nth_paragraph(3)
        .expect("paragraph 3")
        .inline_objects
        .iter()
        .filter_map(|io| io.kind.image_media_key())
        .collect();
    assert_eq!(keys.len(), 3);
    for key in &keys {
        assert!(reread.document.media.contains_key(*key), "{key} resolves");
    }
    assert_eq!(reread.document.media[keys[2]].data, JPEG);

    /* The live tree keeps its engine id — only the written copy renames. */
    assert!(
        image_rel_ids(&edited, 3)
            .iter()
            .any(|id| id.starts_with("nge_img_"))
    );
}

#[test]
fn inserted_png_reuses_the_existing_default_and_a_resave_is_stable() {
    let archive = read_docx(FIXTURE).expect("read");
    const PNG: &[u8] = &[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 9];
    let edited = with_inserted_picture(&archive.document, "image/png", PNG);
    let saved = write_docx(&archive, &edited).expect("write");
    let out = entries(&saved);
    let src = entries(FIXTURE);
    /* `png` is already typed — content types untouched. */
    assert_eq!(
        entry(&out, "[Content_Types].xml"),
        entry(&src, "[Content_Types].xml")
    );
    assert_eq!(
        media_names(&out),
        vec![
            "word/media/image1.png",
            "word/media/image2.png",
            "word/media/image3.png"
        ]
    );

    /* Open the saved file and save again with no edit: the picture is an
    imported one now — nothing more is added, nothing drifts. */
    let reopened = read_docx(&saved).expect("re-read");
    let resaved = write_docx(&reopened, &reopened.document).expect("resave");
    assert_eq!(entries(&resaved), out, "zero-edit resave is byte-identical");
}

#[test]
fn picture_inserted_then_deleted_is_not_written() {
    let archive = read_docx(FIXTURE).expect("read");
    let edited = with_inserted_picture(&archive.document, "image/gif", b"GIF89a");
    /* Delete the sentinel again: the blob stays in `media`, unreferenced. */
    let len = edited.nth_paragraph(3).unwrap().text.len() as u32;
    let deleted = edited.delete_range(
        LogicalPos {
            path: BlockPath::top(3),
            offset: len - 3,
        },
        LogicalPos {
            path: BlockPath::top(3),
            offset: len,
        },
    );
    assert!(!deleted.media.is_empty());
    let saved = write_docx(&archive, &deleted).expect("write");
    let out = entries(&saved);
    let src = entries(FIXTURE);
    assert_eq!(media_names(&out).len(), 2);
    for name in ["[Content_Types].xml", "word/_rels/document.xml.rels"] {
        assert_eq!(entry(&out, name), entry(&src, name), "{name}");
    }
}
