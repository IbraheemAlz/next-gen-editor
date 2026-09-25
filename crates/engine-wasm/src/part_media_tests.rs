//! Issue #188 — relationship ids are scoped per OPC part: a header's
//! `rId5` and the body's `rId5` may name different pictures. Media is
//! keyed by the resolved target path, so each part paints its own
//! picture on canvas and in PDF (and #78's band pictures paint at all).

use super::*;

const BODY_KEY: &str = "word/media/image1.jpeg";
const HEADER_KEY: &str = "word/media/image2.jpeg";

fn body_jpeg() -> Vec<u8> {
    format_pdf::test_images::jpeg(24, 16, 3)
}

fn header_jpeg() -> Vec<u8> {
    format_pdf::test_images::jpeg(16, 24, 3)
}

fn fixture_doc() -> DocumentTree {
    let bytes = format_docx::test_fixtures::part_scoped_media_docx(&body_jpeg(), &header_jpeg());
    format_docx::read_docx(&bytes).expect("read").document
}

fn first_image_key(blocks: &[engine::Block]) -> Option<String> {
    blocks.iter().find_map(|b| match b {
        engine::Block::Paragraph(p) => p
            .inline_objects
            .iter()
            .find_map(|io| io.kind.image_media_key().map(str::to_string)),
        engine::Block::Table(_) => None,
    })
}

/// The reader resolves each part's `rId5` against that part's own rels:
/// two distinct blobs keyed by target path (the footer's absolute-form
/// target dedupes onto the body's picture), and each picture carries its
/// part-local `rel_id` for the writer plus the resolved media key.
#[test]
fn reader_keys_media_by_part_resolved_target() {
    let doc = fixture_doc();
    let mut keys: Vec<&str> = doc.media.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, [BODY_KEY, HEADER_KEY], "two blobs, deduped by target");
    assert_eq!(doc.media[BODY_KEY].data, body_jpeg());
    assert_eq!(doc.media[HEADER_KEY].data, header_jpeg());
    assert_eq!(doc.media[HEADER_KEY].content_type, "image/jpeg");

    let body: Vec<engine::Block> = doc.blocks.iter().cloned().collect();
    assert_eq!(first_image_key(&body).as_deref(), Some(BODY_KEY));
    assert_eq!(
        first_image_key(&doc.headers["rId7"]).as_deref(),
        Some(HEADER_KEY)
    );
    assert_eq!(
        first_image_key(&doc.footers["rId8"]).as_deref(),
        Some(BODY_KEY)
    );
    /* The part-local serialization id is untouched. */
    for blocks in [&body, &doc.headers["rId7"], &doc.footers["rId8"]] {
        let rel = blocks.iter().find_map(|b| {
            b.as_paragraph()?
                .inline_objects
                .iter()
                .find_map(|io| match &io.kind {
                    engine::InlineKind::Image { rel_id, .. } => Some(rel_id.clone()),
                    _ => None,
                })
        });
        assert_eq!(rel.as_deref(), Some("rId5"));
    }
}

/// Canvas: the display list draws the body picture from the body's blob
/// and the header / footer band pictures from their own parts' blobs; the
/// keys it names are exactly the `media_entries` keys the shell decodes.
/// PDF: both distinct payloads are embedded (the body picture reused by
/// the footer is written once).
#[test]
fn each_part_paints_its_own_picture_on_canvas_and_in_pdf() {
    let doc = fixture_doc();
    let engine = crate::tests::test_engine_with_doc(doc.clone());
    let (pages, _, _, info) = engine.build_pages(1.0, false, None).expect("layout");
    assert!(info.degradations.is_empty(), "{:?}", info.degradations);
    let scene = render::scene::build_document_scene(&pages, 0.0);
    let mut draws: Vec<(String, f64)> = scene
        .cmds
        .iter()
        .filter_map(|c| match c {
            render::scene::DisplayCmd::DrawImage { rect, rel_id } => {
                Some((rel_id.clone(), rect.y0))
            }
            _ => None,
        })
        .collect();
    assert_eq!(draws.len(), 3, "body + header + footer pictures: {draws:?}");
    /* One page: header band above the body, footer band below it. */
    draws.sort_by(|a, b| a.1.total_cmp(&b.1));
    let (header, body, footer) = (&draws[0], &draws[1], &draws[2]);
    assert!(header.1 < 72.0, "header band sits in the top margin");
    assert_eq!(header.0, HEADER_KEY, "the header paints ITS rId5");
    assert_eq!(body.0, BODY_KEY);
    assert_eq!(
        footer.0, BODY_KEY,
        "the footer's rId5 names the body picture"
    );
    for (key, _) in &draws {
        assert!(doc.media.contains_key(key), "{key} resolves to a blob");
    }
    assert_ne!(doc.media[&header.0].data, doc.media[&body.0].data);

    let Event::PdfExported { bytes, .. } = engine.do_export_pdf(format_pdf::PdfProfile::A2u) else {
        panic!("engine pdf export");
    };
    let count = |needle: &[u8]| bytes.windows(needle.len()).filter(|w| *w == needle).count();
    assert_eq!(count(&body_jpeg()), 1, "body picture embedded once");
    assert_eq!(count(&header_jpeg()), 1, "header picture embedded");
}

/// The engine-wasm UI save path keeps the package byte-identical (the
/// pictures' part-local ids are written back unchanged), and a reread
/// resolves the same blobs; the package-less minimal writer re-keys the
/// colliding ids into one `document.xml.rels` without losing a picture.
#[test]
fn part_scoped_pictures_survive_both_save_paths() {
    let doc = fixture_doc();
    let saved = format_docx::save_docx(&doc).expect("save");
    let back = format_docx::read_docx(&saved).expect("reread").document;
    assert_eq!(back.media[HEADER_KEY].data, header_jpeg());
    assert_eq!(back.media[BODY_KEY].data, body_jpeg());
    assert_eq!(
        first_image_key(&back.headers["rId7"]).as_deref(),
        Some(HEADER_KEY)
    );

    let mut bare = doc.clone();
    bare.source_package = None;
    let minimal = format_docx::build_minimal_docx(&bare).expect("minimal");
    let back = format_docx::read_docx(&minimal)
        .expect("reread minimal")
        .document;
    let body: Vec<engine::Block> = back.blocks.iter().cloned().collect();
    let key = first_image_key(&body).expect("body picture");
    assert_eq!(back.media[&key].data, body_jpeg());
    assert!(
        back.media.values().any(|b| b.data == header_jpeg()),
        "the header's blob is still in the package"
    );
}
