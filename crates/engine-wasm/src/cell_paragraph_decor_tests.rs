//! Issue #455 — a paragraph inside a table cell carries its `<w:pBdr>`
//! borders, `<w:shd>` shading and tracked paragraph-mark pilcrow like a
//! body or story paragraph (the cell loop used to copy none of them).
//! The fixture is `format_docx::test_fixtures::cell_paragraph_decor_docx`
//! (a 2 x 2 table: a yellow-shaded paragraph with a red bottom rule, a
//! blue box with a pale-blue fill beside a plain paragraph, a tracked
//! inserted and a tracked deleted paragraph mark).

use super::*;
use render::scene::{DisplayCmd, DisplayList};
use table_cache::TABLE_CACHE_HITS;

const YELLOW: [u8; 4] = [0xFF, 0xFF, 0x00, 0xFF];
const PALE_BLUE: [u8; 4] = [0xDD, 0xEB, 0xF7, 0xFF];
const RED_RULE: [u8; 4] = [0xC0, 0x00, 0x00, 0xFF];
const NAVY_BOX: [u8; 4] = [0x1F, 0x38, 0x64, 0xFF];

fn fixture_doc() -> DocumentTree {
    let bytes = format_docx::test_fixtures::cell_paragraph_decor_docx();
    format_docx::read_docx(&bytes).expect("read").document
}

fn layout_of(engine: &Engine) -> Vec<PageBox> {
    let (pages, _, _, info) = engine.build_pages(1.0, false, None).expect("layout");
    assert!(info.degradations.is_empty(), "{:?}", info.degradations);
    pages
}

fn table_of(pages: &[PageBox]) -> &TableBox {
    pages[0]
        .blocks
        .iter()
        .find_map(|b| match b {
            LayoutBlock::Table(t) => Some(t),
            LayoutBlock::Paragraph(_) => None,
        })
        .expect("the fixture's table")
}

fn cell_para(t: &TableBox, row: usize, col: usize, idx: usize) -> &ParagraphBox {
    match &t.rows[row].cells[col].content[idx] {
        LayoutBlock::Paragraph(p) => p,
        LayoutBlock::Table(_) => panic!("cell ({row},{col}) block {idx} is a table"),
    }
}

/// The paragraph's absolute rect on page 1: `(x0, y0, x1, y1)` in px.
fn abs_rect(pages: &[PageBox], row: usize, col: usize, idx: usize) -> (f32, f32, f32, f32) {
    let page = &pages[0];
    let t = table_of(pages);
    let r = &t.rows[row];
    let c = &r.cells[col];
    let p = cell_para(t, row, col, idx);
    let x0 = page.margins.left + t.origin.x + r.origin.x + c.origin.x + c.padding_left + p.origin.x;
    let y0 = page.margins.top + t.origin.y + r.origin.y + c.origin.y + c.padding_top + p.origin.y;
    (x0, y0, x0 + p.size.width, y0 + p.size.height)
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

fn fill_count(pages: &[PageBox]) -> usize {
    render::scene::build_document_scene(pages, 0.0)
        .cmds
        .iter()
        .filter(|c| matches!(c, DisplayCmd::FillRect { .. }))
        .count()
}

/// Every paragraph of every cell of the fixture's table, mutably.
fn for_each_cell_paragraph(doc: &mut DocumentTree, mut f: impl FnMut(&mut engine::Paragraph)) {
    for b in doc.blocks.iter_mut() {
        let engine::Block::Table(t) = b else { continue };
        for row in &mut t.rows {
            for cell in &mut row.cells {
                for blk in &mut cell.blocks {
                    if let engine::Block::Paragraph(p) = blk {
                        f(p);
                    }
                }
            }
        }
    }
}

/// The decoration-free twin of the fixture: same text, same structure.
fn stripped_doc() -> DocumentTree {
    let mut doc = fixture_doc();
    for_each_cell_paragraph(&mut doc, |p| {
        p.props.borders = None;
        p.props.shading = None;
        p.mark_revisions.clear();
    });
    doc
}

#[test]
fn cell_paragraphs_carry_borders_shading_and_the_review_mark() {
    let engine = tests::test_engine_with_doc(fixture_doc());
    let pages = layout_of(&engine);
    let t = table_of(&pages);

    /* (0,0): yellow fill + a red bottom rule only. */
    let shaded = cell_para(t, 0, 0, 0);
    assert_eq!(shaded.shading, Some(YELLOW));
    let b = shaded.borders.as_ref().expect("a bottom rule");
    let bottom = b.bottom.as_ref().expect("bottom edge");
    assert_eq!(bottom.color, Some(RED_RULE));
    assert_eq!(bottom.size_eighth_pt, 12);
    assert!(b.top.is_none() && b.left.is_none() && b.right.is_none());
    assert_eq!(shaded.review_mark, None);

    /* (0,1): a four-edge navy box over a pale-blue fill, then a bare one. */
    let boxed = cell_para(t, 0, 1, 0);
    assert_eq!(boxed.shading, Some(PALE_BLUE));
    let b = boxed.borders.as_ref().expect("a box");
    for edge in [&b.top, &b.left, &b.bottom, &b.right] {
        assert_eq!(edge.as_ref().and_then(|s| s.color), Some(NAVY_BOX));
    }
    let plain = cell_para(t, 0, 1, 1);
    assert!(plain.borders.is_none() && plain.shading.is_none() && plain.review_mark.is_none());

    /* Row 1: the tracked marks take the revision tints. */
    let inserted = cell_para(t, 1, 0, 0);
    assert_eq!(inserted.review_mark, Some(REVISION_INSERT_COLOR));
    assert!(inserted.borders.is_none() && inserted.shading.is_none());
    let deleted = cell_para(t, 1, 1, 0);
    assert_eq!(deleted.review_mark, Some(REVISION_DELETE_COLOR));
    assert!(deleted.borders.is_none() && deleted.shading.is_none());
}

#[test]
fn the_decorations_paint_in_the_cell_and_never_move_a_box() {
    let engine = tests::test_engine_with_doc(fixture_doc());
    let pages = layout_of(&engine);
    let bare = layout_of(&tests::test_engine_with_doc(stripped_doc()));

    /* Paint-only: the geometry is the decoration-free document's. */
    assert_eq!(
        layout::geometry_fingerprint(&pages),
        layout::geometry_fingerprint(&bare),
        "decorations never move a box"
    );
    assert_eq!(
        format!("{:#018x}", layout::geometry_fingerprint(&pages)),
        PINNED_FINGERPRINT,
        "the fixture's geometry moved"
    );

    let scene = render::scene::build_document_scene(&pages, 0.0);

    /* The yellow fill is exactly the cell paragraph's rect. */
    let (x0, y0, x1, y1) = abs_rect(&pages, 0, 0, 0);
    let yellow = fills_of(&scene, YELLOW);
    assert_eq!(yellow.len(), 1, "{yellow:?}");
    let got = yellow[0];
    assert!(
        (got.0 - x0).abs() < 0.01
            && (got.1 - y0).abs() < 0.01
            && (got.2 - x1).abs() < 0.01
            && (got.3 - y1).abs() < 0.01,
        "fill {got:?} vs paragraph rect {:?}",
        (x0, y0, x1, y1)
    );
    assert!(y1 - y0 > 20.0, "a wrapped two-line paragraph: {}", y1 - y0);

    /* The red bottom rule is centred on the paragraph's bottom edge and
    spans its width (the stroke is a thin horizontal rect). */
    let rule = fills_of(&scene, RED_RULE);
    assert_eq!(rule.len(), 1, "{rule:?}");
    let r = rule[0];
    assert!((r.0 - x0).abs() < 0.01 && (r.2 - x1).abs() < 0.01, "{r:?}");
    assert!(
        r.1 < y1 && r.3 > y1 && (r.3 - r.1) < 4.0,
        "a hairline across y = {y1}: {r:?}"
    );

    /* The boxed paragraph: pale-blue fill + four navy strokes. */
    let (bx0, by0, bx1, by1) = abs_rect(&pages, 0, 1, 0);
    let pale = fills_of(&scene, PALE_BLUE);
    assert_eq!(pale.len(), 1, "{pale:?}");
    assert!((pale[0].0 - bx0).abs() < 0.01 && (pale[0].3 - by1).abs() < 0.01);
    assert!((pale[0].1 - by0).abs() < 0.01 && (pale[0].2 - bx1).abs() < 0.01);
    assert_eq!(fills_of(&scene, NAVY_BOX).len(), 4, "four box edges");

    /* Each tracked mark adds its 4-rect pilcrow, nothing else. */
    let mut marks_off = fixture_doc();
    for_each_cell_paragraph(&mut marks_off, |p| p.mark_revisions.clear());
    let without = layout_of(&tests::test_engine_with_doc(marks_off));
    assert_eq!(fill_count(&pages), fill_count(&without) + 2 * 4);
}

/// The PDF export paints the cell paragraph's fill and rule: the shading
/// pass is the paragraph's rect in `1 1 0 rg`, the rule a stroked line in
/// the border colour; under the tagged PDF/UA profile every painting
/// operator sits inside marked content (an `/Artifact` for the decoration
/// passes, `/MCID` sequences for the text — issue #360).
#[test]
fn pdf_export_paints_the_cell_paragraph_fill_and_rule() {
    use format_pdf::test_support::content_streams;

    let engine = pdf_engine(fixture_doc());
    let export = |profile| match engine.do_export_pdf(profile) {
        Event::PdfExported { bytes, .. } => bytes,
        other => panic!("{profile:?}: export failed: {other:?}"),
    };

    /* Untagged PDF/A-1b: the page's one content stream. */
    let bytes = export(format_pdf::PdfProfile::A1b);
    let streams = content_streams(&bytes);
    assert_eq!(streams.len(), 1, "one page");
    let text = String::from_utf8_lossy(&streams[0]).into_owned();
    /* Yellow fill, then a rect + `f`. */
    let fill_at = text.find("1 1 0 rg").expect("the yellow shading fill");
    let after = &text[fill_at..];
    assert!(
        after
            .split_whitespace()
            .skip(4)
            .take(6)
            .collect::<Vec<_>>()
            .ends_with(&["re", "f"]),
        "a rect fill follows the colour: {}",
        &after[..after.len().min(80)]
    );
    assert_eq!(text.matches("1 1 0 rg").count(), 1, "one shaded paragraph");
    /* The rule: a red stroke colour then `m`, `l`, `S`. */
    let red = "0.7529412 0 0 RG";
    assert!(text.contains(red), "the rule's stroke colour: {text}");
    assert_eq!(text.matches(red).count(), 1, "one rule");
    let stroke_at = text.find(red).unwrap();
    let ops: Vec<&str> = text[stroke_at..]
        .split_whitespace()
        .skip(4)
        .take(9)
        .collect();
    /* `1.5 w x y m x y l S`: 12/8 pt wide, one horizontal segment. */
    assert_eq!(
        (ops[0], ops[1], ops[4], ops[7], ops[8]),
        ("1.5", "w", "m", "l", "S"),
        "{ops:?}"
    );
    assert_eq!(ops[3], ops[6], "horizontal: {ops:?}");

    /* Tagged: every painting operator is marked content. */
    let bytes = export(format_pdf::PdfProfile::Ua1);
    let streams = content_streams(&bytes);
    assert_eq!(streams.len(), 1, "one page");
    let text = String::from_utf8_lossy(&streams[0]).into_owned();
    assert!(text.contains("1 1 0 rg"), "tagged export keeps the fill");
    assert!(text.contains(red), "tagged export keeps the rule");
    let loose = unmarked_paint_operators(&text);
    assert!(loose.is_empty(), "paint outside marked content: {loose:?}");
}

/// Painting operators of a content stream outside any `BDC` / `BMC` …
/// `EMC` sequence (the tagged-export rule of issue #360).
fn unmarked_paint_operators(stream: &str) -> Vec<String> {
    const PAINT: [&str; 12] = [
        "f", "F", "f*", "S", "s", "B", "B*", "b", "b*", "sh", "Do", "Tj",
    ];
    let mut depth = 0usize;
    let mut loose = Vec::new();
    for tok in stream.split_whitespace() {
        match tok {
            "BDC" | "BMC" => depth += 1,
            "EMC" => depth = depth.saturating_sub(1),
            "TJ" => {
                if depth == 0 {
                    loose.push(tok.to_string());
                }
            }
            t if PAINT.contains(&t) => {
                if depth == 0 {
                    loose.push(t.to_string());
                }
            }
            _ => {}
        }
    }
    loose
}

/// A native engine over `doc` with the Latin + Arabic test faces, ready
/// for `do_export_pdf` (the `pdf_semantics_tests` scaffold).
fn pdf_engine(doc: DocumentTree) -> Engine {
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
    e.undo = UndoStack::new(doc, 100);
    e.selection = Some(SelectionState {
        anchor: bpos_top(0, 0),
        caret: bpos_top(0, 0),
        ideal_x: None,
        kind: SelectionKind::Linear,
    });
    e
}

fn build(engine: &Engine) -> (Vec<PageBox>, u64, u64) {
    TABLE_GRID_LAYOUTS.with(|n| n.set(0));
    TABLE_CACHE_HITS.with(|n| n.set(0));
    let (pages, _, _, info) = engine.build_pages(1.0, false, None).expect("layout");
    assert!(info.degradations.is_empty(), "{:?}", info.degradations);
    (
        pages,
        TABLE_GRID_LAYOUTS.with(std::cell::Cell::get),
        TABLE_CACHE_HITS.with(std::cell::Cell::get),
    )
}

/// Issue #455 / #379 — the decorated cell paragraph does not cost the
/// table cache its hits: an edit outside the table re-lays no table, and
/// the verified hit (which now also checks the stamped decorations) never
/// reports a `TableCacheMismatch` (strict notes panic on one).
#[test]
fn a_body_edit_keeps_a_decorated_table_cached() {
    let _strict = StrictLayoutNotes::on();
    let mut engine = tests::test_engine_with_doc(fixture_doc());
    let (_, cold_grids, _) = build(&engine);
    assert_eq!(cold_grids, 1, "cold: the one table");

    let doc = engine.undo.current().insert_text(
        EnginePos {
            path: EngineBlockPath::top(0),
            offset: 0,
        },
        "edited ",
    );
    engine.undo.push(doc);
    let (pages, grids, hits) = build(&engine);
    assert_eq!(grids, 0, "no table is re-laid after a body edit");
    assert_eq!(hits, 1, "the decorated table is a verified hit");
    let cached = table_of(&pages);
    assert_eq!(cell_para(cached, 0, 0, 0).shading, Some(YELLOW));
    assert_eq!(
        cell_para(cached, 1, 0, 0).review_mark,
        Some(REVISION_INSERT_COLOR)
    );
    let fresh = {
        let e = tests::test_engine_with_doc(engine.undo.current().clone());
        build(&e).0
    };
    assert_eq!(format!("{pages:?}"), format!("{fresh:?}"));
}

/// Issue #455 / #379 — an edit to a cell paragraph's decoration alone
/// (the paragraph LRU key ignores them) is part of the table key: the
/// table is re-laid, never served stale, and the strict notes prove no
/// stale hit was caught after the fact by the verification.
#[test]
fn a_decoration_edit_in_a_cell_is_never_served_stale() {
    type Mutation = fn(&mut DocumentTree);
    fn cell_paragraph(doc: &mut DocumentTree, row: usize, col: usize) -> &mut engine::Paragraph {
        let t = doc
            .blocks
            .iter_mut()
            .find_map(|b| match b {
                engine::Block::Table(t) => Some(t),
                engine::Block::Paragraph(_) => None,
            })
            .expect("table");
        match t.rows[row].cells[col].blocks.first_mut() {
            Some(engine::Block::Paragraph(p)) => p,
            _ => panic!("cell ({row},{col}) opens with a paragraph"),
        }
    }
    let mutations: [(&str, Mutation); 5] = [
        ("shading colour", |d| {
            cell_paragraph(d, 0, 0).props.shading = Some([0, 255, 0, 255]);
        }),
        ("shading removed", |d| {
            cell_paragraph(d, 0, 0).props.shading = None;
        }),
        ("border removed", |d| {
            cell_paragraph(d, 0, 0).props.borders = None;
        }),
        ("border added", |d| {
            cell_paragraph(d, 1, 0).props.borders = Some(engine::CellBorders {
                top: Some(engine::BorderStroke {
                    style: engine::BorderStyle::Single,
                    size_eighth_pt: 16,
                    ..Default::default()
                }),
                ..Default::default()
            });
        }),
        ("mark revision cleared", |d| {
            cell_paragraph(d, 1, 0).mark_revisions.clear();
        }),
    ];
    for (label, mutate) in mutations {
        let mut engine = tests::test_engine_with_doc(fixture_doc());
        let _ = build(&engine);
        let mut edited = engine.undo.current().clone();
        mutate(&mut edited);
        engine.undo.push(edited.clone());
        let _strict = StrictLayoutNotes::on();
        let (warm, grids, _) = build(&engine);
        assert!(grids > 0, "{label}: the edited table is re-laid");
        let fresh = build(&tests::test_engine_with_doc(edited)).0;
        assert_eq!(
            format!("{warm:?}"),
            format!("{fresh:?}"),
            "{label}: the warm paint equals a fresh layout"
        );
    }
}

/// Pinned with the fixture (A4, 72 pt margins, the 16 px test face at
/// scale 1, Word's font-derived line pitch).
const PINNED_FINGERPRINT: &str = "0x2dffac664ee433f6";
