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

/// The semantic sample documents, by name.
fn sample_docs() -> Vec<(&'static str, DocumentTree)> {
    vec![("links", linked_doc()), ("metadata", metadata_doc())]
}

/// Local veraPDF probe: every [`sample_docs`] document × {1b, 2u, x3}
/// into `$PDF_SEMANTICS_OUT/<profile>/<name>.pdf`.
#[test]
#[ignore]
fn write_semantics_samples() {
    let Ok(out) = std::env::var("PDF_SEMANTICS_OUT") else {
        eprintln!("PDF_SEMANTICS_OUT unset — nothing written");
        return;
    };
    for (label, profile) in [
        ("1b", format_pdf::PdfProfile::A1b),
        ("2u", format_pdf::PdfProfile::A2u),
        ("x3", format_pdf::PdfProfile::X3),
    ] {
        let dir = std::path::Path::new(&out).join(label);
        std::fs::create_dir_all(&dir).expect("out dir");
        for (name, doc) in sample_docs() {
            let pdf = export(&engine_with(doc), profile);
            std::fs::write(dir.join(format!("{name}.pdf")), &pdf).expect("write pdf");
        }
    }
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

/* ================================================================
Task 2 — hyperlinks become link annotations.
================================================================ */

fn link(start: u32, end: u32, target: &str) -> engine::Hyperlink {
    engine::Hyperlink {
        start,
        end,
        target: target.into(),
        attrs: Vec::new(),
    }
}

/// "See example.com or the appendix." — an external link over
/// "example.com" and an internal one over "appendix" → `_Toc42`, whose
/// heading sits on page 2.
fn linked_doc() -> DocumentTree {
    let text = "See example.com or the appendix.\u{000C}";
    let body = engine::Paragraph {
        text: text.into(),
        hyperlinks: vec![link(4, 15, "https://example.com/"), link(23, 31, "#_Toc42")],
        ..Default::default()
    };
    let target = engine::Paragraph {
        text: "Appendix".into(),
        style_id: Some("Heading1".into()),
        bookmarks: vec![engine::Bookmark {
            name: "_Toc42".into(),
            id: None,
        }],
        ..Default::default()
    };
    doc_of(vec![
        engine::Block::Paragraph(body),
        engine::Block::Paragraph(target),
    ])
}

#[test]
fn hyperlinks_become_uri_and_dest_annotations() {
    let engine = engine_with(linked_doc());
    for profile in [format_pdf::PdfProfile::A1b, format_pdf::PdfProfile::A2u] {
        let pdf = pdf_text(&export(&engine, profile));
        assert_eq!(pdf.matches("/Subtype /Link").count(), 2, "{profile:?}");
        assert!(pdf.contains("/URI (https://example.com/)"), "{profile:?}");
        assert!(pdf.contains("/Contents (example.com)"), "{profile:?}");
        assert!(pdf.contains("/Contents (appendix)"), "{profile:?}");
        /* The internal link lands on the heading's page — the same page
        object the outline entry for "Appendix" names. */
        let outline = pdf.find("/Title (Appendix)").expect("outline entry");
        let o = outline + pdf[outline..].find("/Dest [").unwrap() + 7;
        let heading_page = pdf[o..].split_whitespace().next().unwrap();
        let annot = pdf.find("/Contents (appendix)").expect("annot");
        let obj = &pdf[annot..annot + pdf[annot..].find("endobj").unwrap()];
        assert!(
            obj.contains(&format!("/Dest [{heading_page} 0 R /XYZ")),
            "{profile:?}: {obj}"
        );
    }
    /* PDF/X-3: no annotations. */
    let x3 = pdf_text(&export(&engine, format_pdf::PdfProfile::X3));
    assert!(!x3.contains("/Annots"));
}

#[test]
fn toc_hyperlink_entries_jump_to_their_headings() {
    let mut engine = engine_with(doc_of(vec![
        styled("Introduction", Some("Heading1")),
        styled("Intro body.\u{000C}", None),
        styled("Background", Some("Heading2")),
    ]));
    let switches = bridge::TocSwitches {
        outline_min: 1,
        outline_max: 3,
        hyperlinks: true,
        hide_in_web: true,
        use_outline_levels: true,
        page_numbers: true,
    };
    let evt = engine.do_insert_toc(bpos_top(0, 0), switches);
    assert!(!matches!(evt, Event::Error { .. }), "InsertToc: {evt:?}");
    let evt = engine.do_update_fields();
    assert!(!matches!(evt, Event::Error { .. }), "UpdateFields: {evt:?}");
    let pdf = pdf_text(&export(&engine, format_pdf::PdfProfile::A2u));
    let links = pdf.matches("/Subtype /Link").count();
    assert!(links >= 2, "one link per TOC entry line: {links}");
    assert!(!pdf.contains("/S /URI"), "TOC links are internal");
    assert!(
        pdf.matches("/Dest [").count() >= links,
        "every link has a /Dest"
    );
}

/* ================================================================
Task 3 — core properties become /Info + XMP.
================================================================ */

const CORE_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>تقرير &amp; Report</dc:title><dc:subject>Accessibility</dc:subject><dc:creator>Ibrahim Z.</dc:creator><cp:keywords>pdf; ua</cp:keywords><dc:language>ar-SA</dc:language></cp:coreProperties>"#;

/// A two-paragraph document whose retained package carries `CORE_XML`
/// under a renamed part, reached through the package-root relationship.
fn metadata_doc() -> DocumentTree {
    let mut doc = doc_of(vec![
        styled("Accessible export", Some("Heading1")),
        styled("Body text.", None),
    ]);
    let rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties" Target="docProps/core2.xml"/></Relationships>"#;
    doc.source_package = Some(Arc::new(engine::SourcePackage::from_entries([
        ("_rels/.rels".to_string(), rels.as_bytes().to_vec()),
        (
            "docProps/core2.xml".to_string(),
            CORE_XML.as_bytes().to_vec(),
        ),
    ])));
    doc
}

#[test]
fn core_properties_reach_info_and_xmp() {
    let engine = engine_with(metadata_doc());
    let meta = pdf_semantics::document_metadata(engine.undo.current());
    assert_eq!(meta.title.as_deref(), Some("تقرير & Report"));
    assert_eq!(meta.author.as_deref(), Some("Ibrahim Z."));
    assert_eq!(meta.subject.as_deref(), Some("Accessibility"));
    assert_eq!(meta.keywords.as_deref(), Some("pdf; ua"));
    assert_eq!(meta.lang.as_deref(), Some("ar-SA"));
    for profile in [
        format_pdf::PdfProfile::A1b,
        format_pdf::PdfProfile::A2u,
        format_pdf::PdfProfile::X3,
    ] {
        let pdf = pdf_text(&export(&engine, profile));
        assert!(pdf.contains("/Author (Ibrahim Z.)"), "{profile:?}");
        assert!(pdf.contains("/Subject (Accessibility)"), "{profile:?}");
        assert!(pdf.contains("<rdf:li>Ibrahim Z.</rdf:li>"), "{profile:?}");
        assert!(
            pdf.contains("<rdf:li xml:lang=\"x-default\">تقرير &amp; Report</rdf:li>"),
            "{profile:?}"
        );
    }
}

#[test]
fn author_falls_back_to_the_modeled_setting_and_lang_to_doc_defaults() {
    let mut doc = doc_of(vec![styled("x", None)]);
    doc.settings.author = Some("Modeled Author".into());
    engine::GrabBag::push_into(
        &mut doc.style_run_defaults.grab_bag,
        br#"<w:lang w:val="en-GB" w:eastAsia="zh-CN" w:bidi="ar-EG"/>"#.to_vec(),
    );
    let meta = pdf_semantics::document_metadata(&doc);
    assert_eq!(meta.author.as_deref(), Some("Modeled Author"));
    assert_eq!(meta.title, None);
    assert_eq!(meta.lang.as_deref(), Some("en-GB"));
    assert_eq!(
        pdf_semantics::default_lang(&doc, "w:bidi").as_deref(),
        Some("ar-EG")
    );
}
