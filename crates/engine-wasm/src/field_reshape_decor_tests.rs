//! Issue #504 — the #43 field reshape re-lays a paragraph whose PAGE /
//! NUMPAGES result changed with its page, then swaps the fresh box in.
//! The body swap used to take the whole fresh box — a bare one out of the
//! paragraph LRU — so a bordered / shaded PAGE paragraph lost its rule,
//! its fill and its tracked-mark pilcrow (and the keep / page-break flags
//! the body loop stamped) once its page number changed. The header /
//! footer band loop never stamped decorations at all, so a shaded header
//! paragraph painted bare on every page. Both now go through
//! `paragraph_decor::stamp_paragraph_decor`; the body swap keeps every
//! placement stamp (`transplant_reshaped_paragraph`).

use super::*;
use render::scene::{DisplayCmd, DisplayList};
use table_cache::TABLE_CACHE_HITS;

const YELLOW: [u8; 4] = [0xFF, 0xFF, 0x00, 0xFF];
const PALE_BLUE: [u8; 4] = [0xDD, 0xEB, 0xF7, 0xFF];
const RED_RULE: [u8; 4] = [0xC0, 0x00, 0x00, 0xFF];

/// A 1.5 pt red bottom rule (`<w:pBdr><w:bottom w:sz="12"/>`).
fn bottom_rule() -> engine::CellBorders {
    engine::CellBorders {
        bottom: Some(engine::BorderStroke {
            style: engine::BorderStyle::Single,
            size_eighth_pt: 12,
            color: Some(RED_RULE),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// `"Page <cached>"` with a PAGE field over the cached digits.
fn page_paragraph(cached: &str, props: engine::ParaProperties) -> engine::Paragraph {
    engine::Paragraph {
        text: format!("Page {cached}"),
        fields: vec![engine::Field {
            start: 5,
            end: 5 + cached.len() as u32,
            instruction: "PAGE".into(),
            span: None,
            source: None,
        }],
        props,
        ..Default::default()
    }
}

/// `intro` / the decorated PAGE paragraph / `tail`. The PAGE paragraph
/// has a yellow fill, a red bottom rule, a tracked-insert mark and
/// keep-with-next + keep-lines; `break_before` puts it on page 2.
fn body_doc(cached: &str, break_before: bool) -> DocumentTree {
    let mut page = page_paragraph(
        cached,
        engine::ParaProperties {
            shading: Some(YELLOW),
            borders: Some(bottom_rule()),
            keep_next: Some(true),
            keep_lines: Some(true),
            page_break_before: break_before,
            ..Default::default()
        },
    );
    page.mark_revisions = vec![engine::Revision {
        start: 0,
        end: 0,
        kind: engine::RevisionKind::Insert,
        author: "Reviewer".into(),
        date: String::new(),
        id: None,
        prev_attrs: None,
        move_name: None,
    }];
    let mut doc = DocumentTree::from_text("intro");
    doc.blocks.push_back(engine::Block::Paragraph(page));
    doc.blocks
        .push_back(engine::Block::Paragraph(engine::Paragraph {
            text: "tail".into(),
            ..Default::default()
        }));
    doc
}

fn layout_of(engine: &Engine) -> Vec<PageBox> {
    let (pages, _, _, info) = engine.build_pages(1.0, false, None).expect("layout");
    assert!(info.degradations.is_empty(), "{:?}", info.degradations);
    pages
}

/// The PAGE paragraph: the first block of the page that holds it.
fn page_para(page: &PageBox, block: usize) -> &ParagraphBox {
    match &page.blocks[block] {
        LayoutBlock::Paragraph(p) => p,
        LayoutBlock::Table(_) => panic!("block {block} is a table"),
    }
}

fn glyph_ids(p: &ParagraphBox) -> Vec<u32> {
    p.lines
        .iter()
        .flat_map(|l| l.runs.iter())
        .flat_map(|r| r.glyphs.iter())
        .map(|g| u32::from(g.id))
        .collect()
}

/// Every `FillRect` of the scene painted in exactly `rgba`.
fn fills_of(scene: &DisplayList, rgba: [u8; 4]) -> Vec<(f32, f32, f32, f32)> {
    scene
        .cmds
        .iter()
        .filter_map(|c| match c {
            DisplayCmd::FillRect { rect, paint } if paint.solid_rgba8() == Some(rgba) => Some((
                rect.x0 as f32,
                rect.y0 as f32,
                rect.x1 as f32,
                rect.y1 as f32,
            )),
            _ => None,
        })
        .collect()
}

fn near(a: (f32, f32, f32, f32), b: (f32, f32, f32, f32)) -> bool {
    (a.0 - b.0).abs() < 0.01
        && (a.1 - b.1).abs() < 0.01
        && (a.2 - b.2).abs() < 0.01
        && (a.3 - b.3).abs() < 0.01
}

/// Issue #504 — a page break inserted before a bordered, shaded,
/// tracked-mark PAGE paragraph moves it to page 2; the reshape swaps
/// "Page 1" for "Page 2" and the paragraph keeps its fill, its rule, its
/// pilcrow and its pagination stamps. The restamped paint is the paint of
/// a twin document whose cached result already reads "2" (no reshape).
#[test]
fn a_page_paragraph_keeps_its_decorations_through_the_reshape() {
    let mut engine = tests::test_engine_with_doc(body_doc("1", false));
    let before = layout_of(&engine);
    assert_eq!(before.len(), 1, "everything fits page 1");
    let on_page_1 = page_para(&before[0], 1).clone();
    assert_eq!(
        on_page_1.shading,
        Some(YELLOW),
        "first pass stamps the fill"
    );

    /* Insert a page break before the PAGE paragraph. */
    let mut edited = engine.undo.current().clone();
    match edited.blocks.get_mut(1) {
        Some(engine::Block::Paragraph(p)) => p.props.page_break_before = true,
        _ => panic!("block 1 is the PAGE paragraph"),
    }
    engine.undo.push(edited);
    let pages = layout_of(&engine);
    assert_eq!(pages.len(), 2);
    let restamped = page_para(&pages[1], 0);

    /* The twin: same document, its cached result already current. */
    let twin_pages = layout_of(&tests::test_engine_with_doc(body_doc("2", true)));
    let twin = page_para(&twin_pages[1], 0);

    /* The reshape ran: the model still caches "1", the paint reads "2". */
    assert_eq!(glyph_ids(restamped), glyph_ids(twin), "paints Page 2");
    assert_ne!(glyph_ids(restamped), glyph_ids(&on_page_1));

    /* Decorations, stamped from the model by the ONE helper. */
    assert_eq!(restamped.shading, Some(YELLOW));
    assert_eq!(restamped.borders, Some(bottom_rule()));
    assert_eq!(restamped.review_mark, Some(REVISION_INSERT_COLOR));
    /* Placement stamps of the body loop + the paginator survive. */
    assert!(restamped.keep_next, "keep-with-next");
    assert!(restamped.flow.keep_lines, "keep-lines");
    assert!(restamped.flow.page_break_before, "page-break-before");
    assert_eq!(restamped.flow, twin.flow);
    assert_eq!(restamped.source_paragraph_id, twin.source_paragraph_id);
    assert_eq!(restamped.fields.len(), 1);
    assert_eq!(restamped.fields[0].evaluated_text.as_deref(), Some("2"));
    assert_eq!(
        layout::geometry_fingerprint(&pages),
        layout::geometry_fingerprint(&twin_pages),
        "the restamped pages are the twin's"
    );

    /* The display list: identical to the twin's, and the fill / rule sit
    exactly on the paragraph's rect on page 2. */
    let scene = render::scene::build_document_scene(&pages, 0.0);
    assert_eq!(
        scene,
        render::scene::build_document_scene(&twin_pages, 0.0),
        "the restamp paints what a current document paints"
    );
    let page = &pages[1];
    let x0 = page.margins.left + restamped.origin.x;
    let y0 = pages[0].size.height + page.margins.top + restamped.origin.y;
    let rect = (
        x0,
        y0,
        x0 + restamped.size.width,
        y0 + restamped.size.height,
    );
    let yellow = fills_of(&scene, YELLOW);
    assert_eq!(yellow.len(), 1, "{yellow:?}");
    assert!(
        near(yellow[0], rect),
        "fill {:?} vs rect {rect:?}",
        yellow[0]
    );
    let rule = fills_of(&scene, RED_RULE);
    assert_eq!(rule.len(), 1, "{rule:?}");
    let r = rule[0];
    assert!(
        (r.0 - rect.0).abs() < 0.01 && (r.2 - rect.2).abs() < 0.01,
        "{r:?}"
    );
    assert!(
        r.1 < rect.3 && r.3 > rect.3 && (r.3 - r.1) < 4.0,
        "a hairline across the bottom edge {}: {r:?}",
        rect.3
    );
}

/// `first<FF>` / `second`: two pages, each with the header part `rid`.
fn two_page_doc_with_header(blocks: Vec<engine::Block>) -> DocumentTree {
    let mut doc = DocumentTree::from_text("first\u{000C}");
    doc.blocks
        .push_back(engine::Block::Paragraph(engine::Paragraph {
            text: "second".into(),
            ..Default::default()
        }));
    doc.with_updated_header_part("ngeHdr504", blocks)
        .set_section_hf_ref_at(
            engine::LogicalPos::new(engine::BlockPath::top(0), 0),
            true,
            engine::HeaderFooterRole::Default,
            Some("ngeHdr504"),
        )
}

fn header_para(page: &PageBox, block: usize) -> &ParagraphBox {
    match &page.header.as_ref().expect("a header band").blocks[block] {
        LayoutBlock::Paragraph(p) => p,
        LayoutBlock::Table(_) => panic!("header block {block} is a table"),
    }
}

/// Issue #504 — a shaded, ruled header paragraph holding a PAGE field:
/// every page's band is re-laid with its own number ("Page 99" cached,
/// "Page 1" / "Page 2" painted) and keeps the fill and the rule (the band
/// loop used to stamp no decoration at all).
#[test]
fn a_header_page_paragraph_keeps_its_shading_on_every_page() {
    let header = page_paragraph(
        "99",
        engine::ParaProperties {
            shading: Some(PALE_BLUE),
            borders: Some(bottom_rule()),
            ..Default::default()
        },
    );
    let engine =
        tests::test_engine_with_doc(two_page_doc_with_header(vec![engine::Block::Paragraph(
            header,
        )]));
    let pages = layout_of(&engine);
    assert_eq!(pages.len(), 2);
    let scene = render::scene::build_document_scene(&pages, 0.0);
    let fills = fills_of(&scene, PALE_BLUE);
    let rules = fills_of(&scene, RED_RULE);
    assert_eq!(fills.len(), 2, "one fill per page: {fills:?}");
    assert_eq!(rules.len(), 2, "one rule per page: {rules:?}");
    let mut top = 0.0_f32;
    for (i, page) in pages.iter().enumerate() {
        let p = header_para(page, 0);
        assert_eq!(
            glyph_ids(p).len(),
            "Page 1".len(),
            "page {}: the band was re-laid with its number",
            i + 1
        );
        assert_eq!(p.shading, Some(PALE_BLUE), "page {}", i + 1);
        assert_eq!(p.borders, Some(bottom_rule()), "page {}", i + 1);
        let x0 = page.margins.left + p.origin.x;
        let y0 = top + page.header_band_top() + p.origin.y;
        let rect = (x0, y0, x0 + p.size.width, y0 + p.size.height);
        assert!(
            near(fills[i], rect),
            "page {}: fill {:?} vs rect {rect:?}",
            i + 1,
            fills[i]
        );
        let r = rules[i];
        assert!(
            r.1 < rect.3 && r.3 > rect.3 && (r.3 - r.1) < 4.0,
            "page {}: a hairline across {}: {r:?}",
            i + 1,
            rect.3
        );
        top += page.size.height;
    }
    assert_ne!(
        glyph_ids(header_para(&pages[0], 0)),
        glyph_ids(header_para(&pages[1], 0)),
        "each page paints its own number"
    );
}

/// Issue #504 / #379 — the band reshape re-lays a header holding a table
/// whose cell paragraph is shaded: the table comes back from the
/// content-keyed table cache as a VERIFIED hit (`decor_matches` holds on
/// the stamped cell box — strict notes panic on a `TableCacheMismatch`),
/// and the re-laid band's cell paragraph keeps its fill.
#[test]
fn a_reshaped_band_keeps_its_decorated_table_cached() {
    let _strict = StrictLayoutNotes::on();
    let cell_para = engine::Paragraph {
        text: "cell".into(),
        props: engine::ParaProperties {
            shading: Some(YELLOW),
            ..Default::default()
        },
        ..Default::default()
    };
    let table = engine::Table {
        grid: vec![4000],
        rows: vec![engine::TableRow {
            cells: vec![engine::TableCell {
                blocks: vec![engine::Block::Paragraph(cell_para)],
                ..Default::default()
            }],
            ..Default::default()
        }],
        ..Default::default()
    };
    let engine = tests::test_engine_with_doc(two_page_doc_with_header(vec![
        engine::Block::Paragraph(page_paragraph("99", engine::ParaProperties::default())),
        engine::Block::Table(table),
    ]));
    TABLE_CACHE_HITS.with(|n| n.set(0));
    let pages = layout_of(&engine);
    assert!(
        TABLE_CACHE_HITS.with(std::cell::Cell::get) > 0,
        "the re-laid band's table is a cache hit"
    );
    for (i, page) in pages.iter().enumerate() {
        assert_eq!(
            glyph_ids(header_para(page, 0)).len(),
            "Page 1".len(),
            "page {}: the band was re-laid",
            i + 1
        );
        let LayoutBlock::Table(t) = &page.header.as_ref().expect("band").blocks[1] else {
            panic!("page {}: header block 1 is the table", i + 1);
        };
        let LayoutBlock::Paragraph(cp) = &t.rows[0].cells[0].content[0] else {
            panic!("page {}: the cell opens with a paragraph", i + 1);
        };
        assert_eq!(cp.shading, Some(YELLOW), "page {}", i + 1);
    }
    let scene = render::scene::build_document_scene(&pages, 0.0);
    assert_eq!(fills_of(&scene, YELLOW).len(), 2, "one cell fill per page");
}
