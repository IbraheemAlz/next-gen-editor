//! Issue #258 — the D5.1 `tools/pdf-validate` tier-a corpus had a single
//! hand-regenerated fixture (`toc-leaders.docx`, issue #210). This module is
//! the same "regeneratable fixture, generated in code, never a hand-made
//! binary" idiom (mirrors `toc_pdf_export_tests.rs`'s own
//! `generate_toc_leaders_pdf_validate_fixture` and `tools/perf-fixtures`)
//! extended to the feature surface veraPDF actually needs to exercise: inline
//! images (PNG / JPEG / BMP), a table with borders + shading, a justified
//! Arabic paragraph (Kashida — text-pipeline's RTL-typography moat), and
//! footnotes + endnotes.
//!
//! Every fixture is built directly against `engine::DocumentTree`'s own
//! model methods (`insert_table`, `insert_inline_image_at`, `insert_note_at`,
//! `insert_text_box_at`, `set_cell_borders`, `set_cell_shading`) — the same
//! API surface `Engine`'s `do_*` command handlers call, so there is nothing
//! bridge- or wasm-specific about the shape of these documents. Each
//! generator additionally drives the real `Engine::build_pages` →
//! `do_export_pdf` pipeline once as a native sanity net (catches an obvious
//! break — a panic, an `Event::Error`, a malformed header — long before
//! `tools/pdf-validate`'s browser harness would), but the exhaustive
//! per-feature PDF-content assertions belong to each feature's own test
//! module (`format-pdf`'s `image_export_tests.rs`, `a11y_note_tests.rs`,
//! …) — duplicating them here would just be a second copy to keep in sync.
//!
//! `#[ignore]`d like #210's regenerator: run explicitly (or via `tools/
//! pdf-validate/run.mjs --regen`) whenever a fixture's shape needs to
//! change, never as part of the workspace test gate.

use super::*;

/// Scaffold shared by every fixture generator: a native `Engine` with a
/// Latin (`test-latin`) AND an Arabic (`test-arabic`) face registered — most
/// fixtures only need Latin, but registering both uniformly means the
/// per-script `FontStack` fallback (`build_pages_pass`,
/// `FontStack::from_faces(self.fonts.clone(), &cfg.font_id)`) is always
/// ready for whichever fixture needs Arabic shaping, with no per-fixture
/// wiring. Mirrors `toc_pdf_export_tests::build_engine`, duplicated here for
/// the same reason that one is not `pub(crate)` and reused: this module is
/// a sibling, not a descendant.
fn build_fixture_engine(doc: DocumentTree, primary_font_id: &str) -> Engine {
    let mut e = assemble_engine(None, None);
    let latin = LoadedFont::parse(
        "test-latin".to_string(),
        include_bytes!("../../../ts/fonts/LiberationSans-Regular.ttf").to_vec(),
    )
    .expect("parse LiberationSans");
    let arabic = LoadedFont::parse(
        "test-arabic".to_string(),
        include_bytes!("../../../ts/fonts/Amiri-Regular.ttf").to_vec(),
    )
    .expect("parse Amiri");
    e.fonts.insert("test-latin".to_string(), Arc::new(latin));
    e.fonts.insert("test-arabic".to_string(), Arc::new(arabic));
    e.layout_cfg = Some(RenderConfig {
        font_id: primary_font_id.to_string(),
        base_direction: ShapingDirection::Ltr,
        px_size: 16.0,
        line_height: 26.0,
        alignment: Alignment::Start,
        scale: 1.0,
        base_scale: 1.0,
        zoom: 1.0,
    });
    e.undo = UndoStack::new(doc, 100);
    e.selection = Some(SelectionState {
        anchor: bpos_top(0, 0),
        caret: bpos_top(0, 0),
        ideal_x: None,
        kind: SelectionKind::Linear,
    });
    e
}

/// Native sanity net: lay out `doc` and export it under every archival
/// profile veraPDF actually validates (X-3 has no veraPDF flavour and its
/// own extensive coverage in `format-pdf`'s own test suite — skipped here
/// to avoid this net needing to know X-3's `/Title` / date requirements).
/// A failure here means the fixture is broken before it ever reaches
/// `tools/pdf-validate`'s browser harness.
fn assert_exports_cleanly(engine: &Engine, label: &str) {
    let (pages, ..) = engine
        .build_pages(1.0, false, None)
        .unwrap_or_else(|e| panic!("{label}: layout failed: {e:?}"));
    assert!(!pages.is_empty(), "{label}: layout produced no pages");
    for profile in [
        format_pdf::PdfProfile::Plain,
        format_pdf::PdfProfile::A1b,
        format_pdf::PdfProfile::A2u,
    ] {
        match engine.do_export_pdf(profile) {
            Event::PdfExported { bytes, pages: n } => {
                assert!(
                    bytes.starts_with(b"%PDF"),
                    "{label} {profile:?}: missing %PDF header"
                );
                assert!(n > 0, "{label} {profile:?}: zero pages exported");
            }
            other => panic!("{label} {profile:?}: export failed: {other:?}"),
        }
    }
}

/// Write `bytes` to `tests/corpus/tier-a/<name>.docx`, overwriting any
/// existing fixture — the regenerator's whole point.
fn write_corpus_fixture(name: &str, bytes: &[u8]) {
    let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/corpus/tier-a")
        .join(format!("{name}.docx"));
    std::fs::write(&out, bytes).unwrap_or_else(|e| panic!("write {}: {e}", out.display()));
    eprintln!("wrote {} ({} bytes)", out.display(), bytes.len());
}

/* ====================================================================
Fixture 1 — inline images (PNG + JPEG + BMP, issue #121 / #189 / #207).
==================================================================== */

fn images_fixture_doc() -> DocumentTree {
    let mut doc =
        DocumentTree::from_text("Photo gallery — PNG, JPEG and BMP samples follow: ");
    let path = EngineBlockPath::top(0);

    const PNG_W: u32 = 6;
    const PNG_H: u32 = 4;
    let png_pixels: Vec<u8> = (0..PNG_H)
        .flat_map(|y| {
            (0..PNG_W).flat_map(move |x| [(30 * x) as u8, (50 * y) as u8, 210, 255])
        })
        .collect();
    let png_bytes = format_pdf::test_images::png_rgba(PNG_W, PNG_H, &png_pixels);

    let jpeg_bytes = format_pdf::test_images::jpeg(24, 16, 3);

    const BMP_W: u32 = 8;
    const BMP_H: u32 = 6;
    let bmp_pixels: Vec<u8> = (0..BMP_H)
        .flat_map(|y| (0..BMP_W).flat_map(move |x| [(20 * x) as u8, 40, (25 * y) as u8]))
        .collect();
    let bmp_bytes = format_pdf::test_images::bmp(BMP_W, BMP_H, 24, 3, None, &bmp_pixels, false);

    for (mime, bytes, w, h) in [
        ("image/png", png_bytes, PNG_W, PNG_H),
        ("image/jpeg", jpeg_bytes, 24u32, 16u32),
        ("image/bmp", bmp_bytes, BMP_W, BMP_H),
    ] {
        let text_len = doc
            .paragraph_at_path(&path)
            .expect("body paragraph")
            .text
            .len() as u32;
        /* 1-inch display width, aspect-preserved — the natural pixel
        counts above are tiny synthetic fixtures, not meant to look
        good, only to exercise every decoder. */
        let width_emu = 914_400i64;
        let height_emu = (width_emu as f64 * f64::from(h) / f64::from(w)) as i64;
        doc = doc.insert_inline_image_at(
            EnginePos::new(path.clone(), text_len),
            engine::ImageBlob {
                content_type: mime.to_string(),
                data: bytes,
            },
            width_emu,
            height_emu,
        );
    }
    doc
}

#[test]
#[ignore = "regenerates tests/corpus/tier-a/images.docx; run explicitly, not part of the workspace test gate"]
fn generate_images_pdf_validate_fixture() {
    let doc = images_fixture_doc();
    assert_eq!(doc.media.len(), 3, "PNG + JPEG + BMP must all register");

    let engine = build_fixture_engine(doc.clone(), "test-latin");
    assert_exports_cleanly(&engine, "images");

    let Event::DocumentSaved { bytes, .. } = engine.save_docx_bytes("images fixture generator")
    else {
        panic!("SaveDocument must succeed");
    };
    let reread = format_docx::read_docx(&bytes).expect("reread generated .docx");
    assert_eq!(reread.document.media.len(), 3, "media must round-trip");
    write_corpus_fixture("images", &bytes);
}

/* ====================================================================
Fixture 2 — a table with borders + shading (Phase 5 PR 3 / Sprint 10).
==================================================================== */

fn table_borders_fixture_doc() -> DocumentTree {
    let mut doc = DocumentTree::from_text("Quarterly regional sales (USD, thousands):");
    let table_path = EngineBlockPath::top(1);
    doc = doc.insert_table(table_path.clone(), 3, 3);

    let grid: [[&str; 3]; 3] = [
        ["Region", "Q1", "Q2"],
        ["North", "128", "154"],
        ["South", "97", "112"],
    ];
    let mut blocks: Vec<engine::Block> = doc.blocks.iter().cloned().collect();
    let engine::Block::Table(table) = &mut blocks[1] else {
        panic!("insert_table must place a table at index 1");
    };
    for (r, row_vals) in grid.iter().enumerate() {
        for (c, text) in row_vals.iter().enumerate() {
            table.rows[r].cells[c].blocks = vec![engine::Block::Paragraph(engine::Paragraph {
                text: (*text).to_string(),
                ..Default::default()
            })];
        }
    }
    doc.blocks = blocks.into_iter().collect();

    /* Header row shading — a light blue fill behind "Region"/"Q1"/"Q2". */
    for c in 0..3u32 {
        doc = doc.set_cell_shading(table_path.clone(), 0, c, Some([0xDD, 0xE9, 0xFC, 0xFF]));
    }
    /* A non-default double border on one data cell, proving `<w:tcBorders>`
    (not just the table's own default single-border grid) reaches the
    writer + the PDF stroke path. */
    let double_red = engine::BorderStroke {
        style: engine::BorderStyle::Double,
        size_eighth_pt: 12,
        color: Some([0xB0, 0x00, 0x00, 0xFF]),
    };
    doc = doc.set_cell_borders(
        table_path,
        1,
        1,
        engine::CellBorders {
            top: Some(double_red.clone()),
            left: Some(double_red.clone()),
            bottom: Some(double_red.clone()),
            right: Some(double_red),
            ..Default::default()
        },
    );
    doc
}

#[test]
#[ignore = "regenerates tests/corpus/tier-a/table-borders.docx; run explicitly, not part of the workspace test gate"]
fn generate_table_borders_pdf_validate_fixture() {
    let doc = table_borders_fixture_doc();
    let engine::Block::Table(table) = &doc.blocks[1] else {
        panic!("expected a table at index 1");
    };
    assert_eq!(table.rows.len(), 3, "3 rows");
    assert_eq!(table.rows[0].cells.len(), 3, "3 cols");
    assert!(
        table.rows[0].cells[0].props.shading.is_some(),
        "header cell must carry shading"
    );
    assert!(
        table.rows[1].cells[1].props.borders.is_some(),
        "data cell must carry the custom border"
    );

    let engine = build_fixture_engine(doc.clone(), "test-latin");
    assert_exports_cleanly(&engine, "table-borders");

    let Event::DocumentSaved { bytes, .. } =
        engine.save_docx_bytes("table-borders fixture generator")
    else {
        panic!("SaveDocument must succeed");
    };
    let reread = format_docx::read_docx(&bytes).expect("reread generated .docx");
    let engine::Block::Table(reread_table) = &reread.document.blocks[1] else {
        panic!("reread must keep the table at index 1");
    };
    assert_eq!(reread_table.rows.len(), 3, "round-tripped row count");
    write_corpus_fixture("table-borders", &bytes);
}

/* ====================================================================
Fixture 3 — a justified Arabic paragraph (Kashida — text-pipeline's
RTL-typography moat, `justify_kashida.rs`).
==================================================================== */

const ARABIC_TEXT: &str = "الحمد لله الذي جعل السلام طريقاً للتفاهم بين الأمم، \
وجعل التسامح أساساً للتعايش المشترك بين الشعوب والثقافات المختلفة في هذا العالم \
الواسع الكبير، فازدهرت به الحضارة الإنسانية على مر العصور والأزمان المتعاقبة.";

fn arabic_kashida_fixture_doc() -> DocumentTree {
    let doc = DocumentTree::from_text(ARABIC_TEXT);
    let mut blocks: Vec<engine::Block> = doc.blocks.iter().cloned().collect();
    let engine::Block::Paragraph(p) = &mut blocks[0] else {
        panic!("from_text must produce one paragraph");
    };
    p.direct_overrides.alignment = Some(engine::Alignment::Justify);
    p.direct_overrides.direction = Some(engine::TextDirection::Rtl);
    engine::recompute_paragraph_props(p, &doc.styles, &doc.style_defaults);
    let mut doc = doc;
    doc.blocks = blocks.into_iter().collect();
    doc
}

#[test]
#[ignore = "regenerates tests/corpus/tier-a/arabic-kashida.docx; run explicitly, not part of the workspace test gate"]
fn generate_arabic_kashida_pdf_validate_fixture() {
    let doc = arabic_kashida_fixture_doc();
    let engine::Block::Paragraph(p) = &doc.blocks[0] else {
        panic!("expected a paragraph");
    };
    assert_eq!(p.props.alignment, Some(engine::Alignment::Justify));
    assert_eq!(p.props.direction, Some(engine::TextDirection::Rtl));

    let engine = build_fixture_engine(doc.clone(), "test-arabic");
    /* Real layout must actually justify a wrapped line (the paragraph is
    long enough to wrap on an A4-width page) and, per `justify_line`
    (`crates/layout/src/paragraph.rs`), a fully-Arabic line always routes
    through `distribute_to_kashida_points` (pure Kashida when every glyph
    on the line is Arabic-script, the Mixed split otherwise) — this
    fixture's pure-Arabic text guarantees at least one of the two. */
    let (pages, ..) = engine.build_pages(1.0, false, None).expect("layout");
    let para = pages[0].blocks[0]
        .as_paragraph()
        .expect("paragraph block");
    assert!(para.lines.len() >= 2, "must wrap to exercise justification");
    assert_exports_cleanly(&engine, "arabic-kashida");

    let Event::DocumentSaved { bytes, .. } =
        engine.save_docx_bytes("arabic-kashida fixture generator")
    else {
        panic!("SaveDocument must succeed");
    };
    let reread = format_docx::read_docx(&bytes).expect("reread generated .docx");
    let reread_p = reread
        .document
        .nth_paragraph(0)
        .expect("round-tripped paragraph");
    assert_eq!(reread_p.direct_overrides.alignment, Some(engine::Alignment::Justify));
    write_corpus_fixture("arabic-kashida", &bytes);
}

/* ====================================================================
Fixture 4 — footnotes + endnotes (issue #80 / #129).
==================================================================== */

fn notes_fixture_doc() -> DocumentTree {
    let doc = DocumentTree::from_text(
        "The first documented reference appears in the archive, with further \
         detail available in the appendix.",
    );
    let path = EngineBlockPath::top(0);

    let text = doc.paragraph_at_path(&path).expect("paragraph").text.clone();
    let footnote_at = text.find("archive").expect("fixture text contains 'archive'") as u32;
    let (doc, fid) = doc.insert_note_at(EnginePos::new(path.clone(), footnote_at), engine::NoteKind::Footnote);
    let mut fbody = engine::Paragraph {
        text: "\u{FFFC} Primary source: internal records, cross-checked against three \
               independent archives."
            .to_string(),
        dirty: true,
        ..Default::default()
    };
    fbody.inline_objects.push(engine::InlineObject {
        at: 0,
        kind: engine::InlineKind::NoteSelfRef {
            kind: engine::NoteKind::Footnote,
        },
        anchor: None,
        source_xml: None,
    });
    let doc =
        doc.with_updated_note_story(engine::NoteKind::Footnote, fid as i32, vec![engine::Block::Paragraph(fbody)]);

    /* Re-derive the offset from the LIVE (post-footnote-insert) text — the
    footnote's own U+FFFC sentinel shifted every later byte offset, and
    re-finding rather than hand-computing the shift keeps this immune to
    that detail changing. */
    let text2 = doc.paragraph_at_path(&path).expect("paragraph").text.clone();
    let endnote_at = text2.find("appendix").expect("fixture text contains 'appendix'") as u32;
    let (doc, eid) = doc.insert_note_at(EnginePos::new(path, endnote_at), engine::NoteKind::Endnote);
    let mut ebody = engine::Paragraph {
        text: "\u{FFFC} See Appendix C for the full transcript and translation notes.".to_string(),
        dirty: true,
        ..Default::default()
    };
    ebody.inline_objects.push(engine::InlineObject {
        at: 0,
        kind: engine::InlineKind::NoteSelfRef {
            kind: engine::NoteKind::Endnote,
        },
        anchor: None,
        source_xml: None,
    });
    doc.with_updated_note_story(engine::NoteKind::Endnote, eid as i32, vec![engine::Block::Paragraph(ebody)])
}

#[test]
#[ignore = "regenerates tests/corpus/tier-a/notes.docx; run explicitly, not part of the workspace test gate"]
fn generate_notes_pdf_validate_fixture() {
    let doc = notes_fixture_doc();
    assert_eq!(doc.footnote_stories.len(), 1, "one footnote story");
    assert_eq!(doc.endnote_stories.len(), 1, "one endnote story");

    let engine = build_fixture_engine(doc.clone(), "test-latin");
    assert_exports_cleanly(&engine, "notes");

    let Event::DocumentSaved { bytes, .. } = engine.save_docx_bytes("notes fixture generator")
    else {
        panic!("SaveDocument must succeed");
    };
    let reread = format_docx::read_docx(&bytes).expect("reread generated .docx");
    /* The writer synthesizes the standard Word separator /
    continuationSeparator entries `footnotes.xml` / `endnotes.xml` always
    carry (`NoteType::Separator` / `ContinuationSeparator`) — count only
    our own `Normal` note, not the full round-tripped map. */
    let normal_notes = |stories: &std::collections::HashMap<i32, engine::NoteStory>| {
        stories
            .values()
            .filter(|s| s.note_type == engine::NoteType::Normal)
            .count()
    };
    assert_eq!(
        normal_notes(&reread.document.footnote_stories),
        1,
        "footnote must round-trip"
    );
    assert_eq!(
        normal_notes(&reread.document.endnote_stories),
        1,
        "endnote must round-trip"
    );
    write_corpus_fixture("notes", &bytes);
}

/* ====================================================================
Fixture 5 — a floating text box (issue #83).
==================================================================== */

fn text_box_fixture_doc() -> DocumentTree {
    let doc = DocumentTree::from_text("See the callout for the executive summary.");
    let path = EngineBlockPath::top(0);
    let offset = doc
        .paragraph_at_path(&path)
        .expect("paragraph")
        .text
        .find("callout")
        .expect("fixture text contains 'callout'") as u32;
    /* 2in × 1in, Word's own EMU unit. */
    let (doc, _host, at) =
        doc.insert_text_box_at(EnginePos::new(path, offset), 1_828_800, 914_400);

    let mut blocks: Vec<engine::Block> = doc.blocks.iter().cloned().collect();
    let engine::Block::Paragraph(p) = &mut blocks[0] else {
        panic!("text box host must stay a paragraph");
    };
    let io = p
        .inline_objects
        .iter_mut()
        .find(|io| io.at == at)
        .expect("text box inline object");
    let engine::InlineKind::TextBox { story, .. } = &mut io.kind else {
        panic!("expected InlineKind::TextBox");
    };
    story.body = vec![engine::Block::Paragraph(engine::Paragraph {
        text: "Executive summary: revenue grew 12% year over year.".to_string(),
        ..Default::default()
    })];
    story.dirty = true;
    let mut doc = doc;
    doc.blocks = blocks.into_iter().collect();
    doc
}

#[test]
#[ignore = "regenerates tests/corpus/tier-a/text-box.docx; run explicitly, not part of the workspace test gate"]
fn generate_text_box_pdf_validate_fixture() {
    let doc = text_box_fixture_doc();
    assert_eq!(
        doc.text_box_addresses().len(),
        1,
        "exactly one text box in the body"
    );

    let engine = build_fixture_engine(doc.clone(), "test-latin");
    assert_exports_cleanly(&engine, "text-box");

    let Event::DocumentSaved { bytes, .. } = engine.save_docx_bytes("text-box fixture generator")
    else {
        panic!("SaveDocument must succeed");
    };
    let reread = format_docx::read_docx(&bytes).expect("reread generated .docx");
    assert_eq!(
        reread.document.text_box_addresses().len(),
        1,
        "text box must round-trip"
    );
    write_corpus_fixture("text-box", &bytes);
}
