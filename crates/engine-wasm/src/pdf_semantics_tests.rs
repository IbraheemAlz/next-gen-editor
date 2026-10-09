//! Issue #360 — PDF outline, link annotations, XMP metadata and tagging,
//! driven end to end through `Engine::do_export_pdf`.

use super::*;

/// Native engine with the Latin + Arabic test faces (the
/// `pdf_validate_fixtures_tests` scaffold, duplicated: siblings).
fn semantics_engine() -> Engine {
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
        font_id: "test-latin".to_string(),
        base_direction: ShapingDirection::Ltr,
        px_size: 16.0,
        line_height: 26.0,
        alignment: Alignment::Start,
        scale: 1.0,
        base_scale: 1.0,
        zoom: 1.0,
    });
    e
}

/// The exporter call the byte-identity harness compares — the untagged
/// output of `profile`.
fn export_untagged(engine: &Engine, profile: format_pdf::PdfProfile) -> Vec<u8> {
    match engine.do_export_pdf(profile) {
        Event::PdfExported { bytes, .. } => bytes,
        other => panic!("{profile:?}: export failed: {other:?}"),
    }
}

/// Byte-identity harness: every `tests/corpus/tier-a/*.docx` × {1b, 2u,
/// x3}, untagged, into `$PDF_SEMANTICS_OUT/<profile>/<name>.pdf`.
#[test]
#[ignore]
fn write_tier_a_untagged_exports() {
    let Ok(out) = std::env::var("PDF_SEMANTICS_OUT") else {
        eprintln!("PDF_SEMANTICS_OUT unset — nothing written");
        return;
    };
    let corpus = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus/tier-a");
    let mut names: Vec<_> = std::fs::read_dir(&corpus)
        .expect("tier-a corpus")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "docx"))
        .collect();
    names.sort();
    for (label, profile) in [
        ("1b", format_pdf::PdfProfile::A1b),
        ("2u", format_pdf::PdfProfile::A2u),
        ("x3", format_pdf::PdfProfile::X3),
    ] {
        let dir = std::path::Path::new(&out).join(label);
        std::fs::create_dir_all(&dir).expect("out dir");
        for path in &names {
            let mut engine = semantics_engine();
            let bytes = std::fs::read(path).expect("read docx");
            let evt = engine.load_docx_bytes(&bytes, "LoadDocx", None);
            assert!(matches!(evt, Event::DocumentLoaded { .. }), "{evt:?}");
            let pdf = export_untagged(&engine, profile);
            let stem = path.file_stem().unwrap().to_string_lossy().to_string();
            std::fs::write(dir.join(format!("{stem}.pdf")), &pdf).expect("write pdf");
        }
    }
}

fn engine_with(doc: DocumentTree) -> Engine {
    let mut e = semantics_engine();
    e.undo = UndoStack::new(doc, 100);
    e.selection = Some(SelectionState {
        anchor: bpos_top(0, 0),
        caret: bpos_top(0, 0),
        ideal_x: None,
        kind: SelectionKind::Linear,
    });
    e
}

fn styled(text: &str, style: Option<&str>) -> engine::Block {
    engine::Block::Paragraph(engine::Paragraph {
        text: text.into(),
        style_id: style.map(str::to_string),
        ..Default::default()
    })
}

fn doc_of(blocks: Vec<engine::Block>) -> DocumentTree {
    let mut d = DocumentTree::from_text("");
    d.blocks = blocks.into_iter().collect();
    d
}

fn export(engine: &Engine, profile: format_pdf::PdfProfile) -> Vec<u8> {
    match engine.do_export_pdf(profile) {
        Event::PdfExported { bytes, .. } => bytes,
        other => panic!("{profile:?}: export failed: {other:?}"),
    }
}

fn pdf_text(pdf: &[u8]) -> String {
    String::from_utf8_lossy(pdf).into_owned()
}

/* ================================================================
Task 1 — outline from `Heading N` paragraphs.
================================================================ */

#[test]
fn heading_styles_become_the_pdf_outline() {
    let engine = engine_with(doc_of(vec![
        styled("Introduction", Some("Heading1")),
        styled("Intro body.\u{000C}", None),
        styled("Background\tdetails", Some("Heading2")),
        styled("Conclusion", Some("Heading1")),
    ]));
    let pdf = pdf_text(&export(&engine, format_pdf::PdfProfile::A1b));
    assert!(pdf.contains("/PageMode /UseOutlines"));
    assert!(pdf.contains("/Type /Outlines"));
    assert!(pdf.contains("/Count 3"), "three entries, all open");
    for title in ["(Introduction)", "(Background details)", "(Conclusion)"] {
        assert!(pdf.contains(&format!("/Title {title}")), "{title}");
    }
    /* "Background" (Heading 2) nests under "Introduction". */
    let intro = pdf.find("/Title (Introduction)").expect("intro");
    let intro_dict = &pdf[intro..intro + pdf[intro..].find(">>").unwrap()];
    assert!(intro_dict.contains("/Count 1"), "{intro_dict}");
    /* The FORM FEED pushes "Background" to page 2 — its /Dest names a
    different page object than "Introduction"'s. */
    let dest_page = |title: &str| {
        let at = pdf.find(&format!("/Title ({title})")).expect(title);
        let d = at + pdf[at..].find("/Dest [").expect("dest") + 7;
        pdf[d..].split_whitespace().next().unwrap().to_string()
    };
    assert_ne!(dest_page("Introduction"), dest_page("Background details"));
    assert_eq!(dest_page("Background details"), dest_page("Conclusion"));
}

#[test]
fn a_document_without_headings_has_no_outline() {
    let engine = engine_with(doc_of(vec![
        styled("Plain one.", None),
        styled("Plain two.", Some("Normal")),
    ]));
    let pdf = pdf_text(&export(&engine, format_pdf::PdfProfile::A1b));
    assert!(!pdf.contains("/Outlines"));
    assert!(!pdf.contains("/PageMode"));
}
