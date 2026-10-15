//! Issue #328 — page borders, line numbers and drop caps, end to end:
//! `.docx` fixture → reader → layout (`PageBox::decor`) → display list
//! → PDF export.

use super::*;

/// Native engine with the Latin test face at scale 1 (1 layout unit =
/// 1 pt), like the PDF semantics scaffold.
fn decor_engine(docx: &[u8]) -> Engine {
    let mut e = assemble_engine(None, None);
    let latin = LoadedFont::parse(
        "test-latin".to_string(),
        include_bytes!("../../../ts/fonts/LiberationSans-Regular.ttf").to_vec(),
    )
    .expect("parse LiberationSans");
    e.fonts.insert("test-latin".to_string(), Arc::new(latin));
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
    let evt = e.load_docx_bytes(docx, "LoadDocx", None);
    assert!(matches!(evt, Event::DocumentLoaded { .. }), "{evt:?}");
    e
}

fn pages_of(engine: &Engine) -> Vec<PageBox> {
    let (pages, _, _, info) = engine.build_pages(1.0, false, None).expect("layout");
    assert!(info.degradations.is_empty(), "{:?}", info.degradations);
    pages
}

fn pdf(engine: &Engine, profile: format_pdf::PdfProfile) -> Vec<u8> {
    match engine.do_export_pdf(profile) {
        Event::PdfExported { bytes, .. } => bytes,
        other => panic!("{profile:?}: export failed: {other:?}"),
    }
}

fn streams(pdf: &[u8]) -> String {
    format_pdf::test_support::content_streams(pdf)
        .iter()
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect()
}

/// The fixture's border colour, `1F3864`.
const BORDER_RGBA: [u8; 4] = [0x1F, 0x38, 0x64, 255];

#[test]
fn page_borders_read_from_sect_pr() {
    let doc = format_docx::read_docx(&format_docx::test_fixtures::page_borders_docx())
        .expect("read")
        .document;
    let sections = doc.effective_sections();
    let b = sections[0].page_borders.as_ref().expect("pgBorders typed");
    assert_eq!(b.offset_from, engine::PageBorderOffset::Page);
    assert_eq!(b.display, engine::PageBorderDisplay::AllPages);
    let top = b.top.as_ref().expect("top edge");
    assert_eq!((top.size_eighth_pt, top.space_pt), (24, Some(24)));
    assert_eq!(top.color, Some(BORDER_RGBA));
    assert!(sections[0].line_numbering.is_none());
}

#[test]
fn page_borders_frame_every_page_inside_the_page_edge() {
    let engine = decor_engine(&format_docx::test_fixtures::page_borders_docx());
    let pages = pages_of(&engine);
    assert_eq!(pages.len(), 1);
    let page = &pages[0];
    let b = page.decor.borders.as_ref().expect("border box");
    /* `w:offsetFrom="page"`, `w:space="24"` on every edge: the box sits
    24 pt inside each page edge (A4 = 595.3 × 841.9 pt). */
    assert_eq!((b.origin.x, b.origin.y), (24.0, 24.0));
    assert!((b.size.width - (page.size.width - 48.0)).abs() < 0.01);
    assert!((b.size.height - (page.size.height - 48.0)).abs() < 0.01);
    assert!(!b.behind, "zOrder defaults to front");
    assert!(page.decor.line_numbers.is_empty());

    /* The display list strokes four edges in the border colour, after
    (over) every glyph run. */
    let scene = render::scene::build_document_scene(&pages, 0.0);
    let strokes: Vec<usize> = scene
        .cmds
        .iter()
        .enumerate()
        .filter_map(|(i, c)| match c {
            render::scene::DisplayCmd::FillRect { paint, .. }
                if paint.solid_rgba8() == Some(BORDER_RGBA) =>
            {
                Some(i)
            }
            _ => None,
        })
        .collect();
    assert_eq!(strokes.len(), 4, "four edges");
    let last_glyphs = scene
        .cmds
        .iter()
        .rposition(|c| matches!(c, render::scene::DisplayCmd::DrawGlyphRun(_)))
        .expect("text painted");
    assert!(
        strokes[0] > last_glyphs,
        "front border paints over the text"
    );

    assert_eq!(
        format!("{:#018x}", layout::geometry_fingerprint(&pages)),
        PAGE_BORDERS_FINGERPRINT
    );
}

/// Pinned geometry (page + border box) of `page_borders_docx`.
const PAGE_BORDERS_FINGERPRINT: &str = "0xee4f7dac36d45f51";

#[test]
fn page_border_display_and_z_order_follow_the_section() {
    let mut doc = format_docx::read_docx(&format_docx::test_fixtures::page_borders_docx())
        .expect("read")
        .document;
    /* Two pages (a page break), borders on the first page only, behind. */
    let mut blocks: Vec<engine::Block> = doc.blocks.iter().cloned().collect();
    blocks.push(engine::Block::Paragraph(engine::Paragraph {
        text: "\u{000C}second page".into(),
        ..Default::default()
    }));
    doc.blocks = blocks.into_iter().collect();
    let b = doc.body_section.page_borders.as_mut().expect("borders");
    b.display = engine::PageBorderDisplay::FirstPage;
    b.z_back = true;
    let mut engine = decor_engine(&format_docx::test_fixtures::page_borders_docx());
    engine.undo = UndoStack::new(doc, 100);
    let pages = pages_of(&engine);
    assert_eq!(pages.len(), 2);
    assert!(pages[0].decor.borders.as_ref().is_some_and(|b| b.behind));
    assert!(pages[1].decor.borders.is_none(), "display=firstPage");
    let scene = render::scene::build_document_scene(&pages[..1], 0.0);
    let first_stroke = scene
        .cmds
        .iter()
        .position(|c| {
            matches!(c, render::scene::DisplayCmd::FillRect { paint, .. }
                if paint.solid_rgba8() == Some(BORDER_RGBA))
        })
        .expect("stroke");
    let first_glyphs = scene
        .cmds
        .iter()
        .position(|c| matches!(c, render::scene::DisplayCmd::DrawGlyphRun(_)))
        .expect("text");
    assert!(
        first_stroke < first_glyphs,
        "zOrder=back paints under the text"
    );
}

#[test]
fn page_borders_reach_the_pdf_as_artifact_strokes() {
    let engine = decor_engine(&format_docx::test_fixtures::page_borders_docx());
    /* Untagged: four stroked lines in the border colour. */
    let plain = streams(&pdf(&engine, format_pdf::PdfProfile::Plain));
    let colour = "0.12156863 0.21960784 0.39215687 RG";
    assert!(plain.contains(colour), "border colour stroked: {plain}");
    /* Tagged: the strokes sit inside an artifact sequence. */
    let tagged = streams(&pdf(&engine, format_pdf::PdfProfile::Ua1));
    let at = tagged.find(colour).expect("tagged border stroke");
    let open = tagged[..at]
        .rfind("/Artifact BMC")
        .expect("artifact opened");
    assert!(
        !tagged[open..at].contains("EMC"),
        "border strokes are inside the artifact sequence"
    );
}

#[test]
fn line_numbering_reads_from_sect_pr() {
    let doc = format_docx::read_docx(&format_docx::test_fixtures::line_numbers_docx())
        .expect("read")
        .document;
    let n = doc.effective_sections()[0]
        .line_numbering
        .expect("lnNumType typed");
    assert_eq!(n.count_by, 1);
    assert_eq!(n.start, 0);
    assert_eq!(n.distance_pt, Some(18.0));
    assert_eq!(n.restart, engine::LineNumberRestart::NewPage);
}

#[test]
fn line_numbers_count_body_lines_and_skip_suppressed_paragraphs_and_tables() {
    let engine = decor_engine(&format_docx::test_fixtures::line_numbers_docx());
    let pages = pages_of(&engine);
    assert_eq!(pages.len(), 1);
    let page = &pages[0];
    assert!(page.decor.borders.is_none());
    /* Every line of the counted paragraphs, in order; the suppressed
    paragraph (block 2) and the table cell take no number. */
    let counted: Vec<(&layout::ParagraphBox, &layout::LineBox)> = page
        .blocks
        .iter()
        .enumerate()
        .filter(|(j, _)| *j != 2)
        .filter_map(|(_, b)| b.as_paragraph())
        .flat_map(|p| p.lines.iter().map(move |l| (p, l)))
        .collect();
    let numbers: Vec<&str> = page
        .decor
        .line_numbers
        .iter()
        .map(|n| n.text.as_str())
        .collect();
    let expected: Vec<String> = (1..=counted.len()).map(|n| n.to_string()).collect();
    assert_eq!(numbers, expected);
    assert!(counted.len() >= 5, "the fixture wraps: {}", counted.len());
    /* Right-aligned `w:distance` (18 pt) left of the text column, on
    each line's baseline. */
    let column_left = page.margins.left;
    for (n, (p, l)) in page.decor.line_numbers.iter().zip(&counted) {
        let right = n.origin.x + n.width;
        assert!((right - (column_left - 18.0)).abs() < 0.01, "{right}");
        let line_baseline = page.margins.top + p.origin.y + l.origin.y + l.baseline;
        assert!((n.origin.y + n.baseline - line_baseline).abs() < 0.01);
        /* The `LineNumber` style: docDefaults' 11 pt. */
        assert_eq!(n.run.attrs.px_size, 11.0);
    }
    assert_eq!(
        format!("{:#018x}", layout::geometry_fingerprint(&pages)),
        LINE_NUMBERS_FINGERPRINT
    );
}

/// Pinned geometry (page + numbers) of `line_numbers_docx`.
const LINE_NUMBERS_FINGERPRINT: &str = "0x89ec82341f3f5d79";

/// The line-number strings of each page of `doc`.
fn numbers_per_page(doc: DocumentTree) -> Vec<Vec<String>> {
    let mut engine = decor_engine(&format_docx::test_fixtures::line_numbers_docx());
    engine.undo = UndoStack::new(doc, 100);
    pages_of(&engine)
        .iter()
        .map(|p| {
            p.decor
                .line_numbers
                .iter()
                .map(|n| n.text.clone())
                .collect()
        })
        .collect()
}

#[test]
fn line_numbers_honour_count_by_start_and_restart() {
    let mut doc = format_docx::read_docx(&format_docx::test_fixtures::line_numbers_docx())
        .expect("read")
        .document;
    /* A page break before two more one-line paragraphs; count by 2
    starting at 10 (`w:start` is zero-based: 9). */
    let mut blocks: Vec<engine::Block> = doc.blocks.iter().cloned().collect();
    for text in ["\u{000C}one", "two"] {
        blocks.push(engine::Block::Paragraph(engine::Paragraph {
            text: text.into(),
            ..Default::default()
        }));
    }
    doc.blocks = blocks.into_iter().collect();
    let n = doc.body_section.line_numbering.as_mut().expect("numbering");
    n.count_by = 2;
    n.start = 9;
    let per_page = numbers_per_page(doc.clone());
    assert_eq!(per_page.len(), 2, "{per_page:?}");
    /* Only even numbers show, from 10; page 2 restarts (`newPage`). */
    assert_eq!(per_page[0].first().map(String::as_str), Some("10"));
    assert!(
        per_page[0]
            .iter()
            .all(|t| t.parse::<u32>().unwrap() % 2 == 0)
    );
    assert_eq!(per_page[1].first().map(String::as_str), Some("10"));
    /* `continuous` carries the count over the page break. */
    doc.body_section.line_numbering.as_mut().unwrap().restart =
        engine::LineNumberRestart::Continuous;
    let continuous = numbers_per_page(doc);
    let last_p1: u32 = continuous[0].last().unwrap().parse().unwrap();
    assert!(
        continuous[1]
            .iter()
            .all(|t| t.parse::<u32>().unwrap() > last_p1),
        "{continuous:?}"
    );
}

#[test]
fn line_numbers_reach_the_pdf_as_layout_artifact_text() {
    let engine = decor_engine(&format_docx::test_fixtures::line_numbers_docx());
    let tagged = streams(&pdf(&engine, format_pdf::PdfProfile::Ua1));
    assert!(
        tagged.contains("/Type /Layout"),
        "line numbers are a layout artifact"
    );
    /* The untagged export shows the numbers: one more text object (and
    more shown strings) than the document without numbering. */
    let plain = streams(&pdf(&engine, format_pdf::PdfProfile::Plain));
    let mut doc = format_docx::read_docx(&format_docx::test_fixtures::line_numbers_docx())
        .expect("read")
        .document;
    doc.body_section.line_numbering = None;
    let mut bare = decor_engine(&format_docx::test_fixtures::line_numbers_docx());
    bare.undo = UndoStack::new(doc, 100);
    let without = streams(&pdf(&bare, format_pdf::PdfProfile::Plain));
    assert_eq!(
        plain.matches("BT").count(),
        without.matches("BT").count() + 1
    );
    assert!(plain.matches("Tj").count() > without.matches("Tj").count());
}

/// A zero-edit save replays the `<w:sectPr>` bytes; a page-setup change
/// regenerates the element, and the regenerated one still carries the
/// typed `<w:pgBorders>` / `<w:lnNumType>`.
#[test]
fn regenerated_sect_pr_keeps_page_borders_and_line_numbering() {
    for fixture in [
        format_docx::test_fixtures::page_borders_docx(),
        format_docx::test_fixtures::line_numbers_docx(),
    ] {
        let doc = format_docx::read_docx(&fixture).expect("read").document;
        let before = doc.body_section.clone();
        let zero = format_docx::save_docx(&doc).expect("save");
        let again = format_docx::read_docx(&zero).expect("re-read").document;
        assert_eq!(again.body_section.without_source(), before.without_source());

        let mut edited = doc.clone();
        edited.body_section.geometry.margin_left += 36.0;
        let saved = format_docx::save_docx(&edited).expect("save edited");
        let back = format_docx::read_docx(&saved)
            .expect("re-read edited")
            .document;
        assert_eq!(back.body_section.page_borders, before.page_borders);
        assert_eq!(back.body_section.line_numbering, before.line_numbering);
        assert_ne!(
            back.body_section.source_xml, before.source_xml,
            "the sectPr regenerated"
        );
    }
}
