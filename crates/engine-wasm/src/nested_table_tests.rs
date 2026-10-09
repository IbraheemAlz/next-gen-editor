//! Issue #318 — nested-table layout cost.
//!
//! Every level of a nested table used to re-lay every descendant once
//! per autofit pass (probe layout, min-content probe, final layout), so
//! the grid layouts of a `d`-deep tower grew like `F(2d)` (≈ 2.618^d):
//! 46 368 table layouts at depth 12, never finishing at the reader's
//! 64-level cap. One top-level table layout now memoizes inner tables,
//! column solves and intrinsic widths (verified on every hit), the
//! autofit probe reads a nested table's width off its column solve, and
//! a table past [`MAX_TABLE_LAYOUT_DEPTH`] is flattened to its
//! paragraphs with a `NestingCapped` note.

use super::*;

fn para(text: &str) -> engine::Paragraph {
    engine::Paragraph {
        text: text.to_string(),
        ..Default::default()
    }
}

fn cell(blocks: Vec<engine::Block>) -> engine::TableCell {
    engine::TableCell {
        blocks,
        ..Default::default()
    }
}

/// One level of the mixed tower: an autofit / fixed / gridded / gridless
/// two-column table whose first cell carries `inner` (the next level
/// down), with a wrapping prose cell and, on every other level, a second
/// row with a vertically centred short cell.
fn level(i: usize, inner: Option<engine::Table>) -> engine::Table {
    let mut first = vec![engine::Block::Paragraph(para(&format!(
        "Level {i} heading"
    )))];
    if let Some(t) = inner {
        first.push(engine::Block::Table(t));
    }
    let mut rows = vec![engine::TableRow {
        cells: vec![
            cell(first),
            cell(vec![engine::Block::Paragraph(para(
                "the quick brown fox jumps over the lazy dog while the \
                 table nests one level deeper",
            ))]),
        ],
        ..Default::default()
    }];
    if i % 2 == 1 {
        let mut centred = cell(vec![engine::Block::Paragraph(para("mid"))]);
        centred.props.v_align = engine::VerticalAlign::Center;
        rows.push(engine::TableRow {
            cells: vec![
                centred,
                cell(vec![engine::Block::Paragraph(para(&format!(
                    "row two {i}"
                )))]),
            ],
            ..Default::default()
        });
    }
    let mut table = engine::Table {
        rows,
        dirty: true,
        ..Default::default()
    };
    if i % 2 == 0 {
        table.grid = vec![2400, 1800];
    }
    if i % 3 == 2 {
        table.props.layout = engine::TableLayout::Fixed;
    }
    table
}

/// A `depth`-level mixed tower, built bottom-up (no recursion, so a
/// 200-deep fixture costs no stack here).
fn mixed_tower(depth: usize) -> engine::Table {
    let mut inner = None;
    for i in (0..depth).rev() {
        inner = Some(level(i, inner));
    }
    inner.expect("depth >= 1")
}

/// The corpus shape (`table_nested_200_deep.docx`): single-column,
/// single-cell tables, each cell one paragraph and the next level.
fn plain_tower(depth: usize) -> engine::Table {
    let mut inner: Option<engine::Table> = None;
    for i in (0..depth).rev() {
        let mut blocks = vec![engine::Block::Paragraph(para(&format!("level {i}")))];
        if let Some(t) = inner.take() {
            blocks.push(engine::Block::Table(t));
        }
        inner = Some(engine::Table {
            grid: vec![2400],
            rows: vec![engine::TableRow {
                cells: vec![cell(blocks)],
                ..Default::default()
            }],
            dirty: true,
            ..Default::default()
        });
    }
    inner.expect("depth >= 1")
}

fn doc_with(table: engine::Table) -> DocumentTree {
    let mut doc = DocumentTree::new();
    doc.blocks.clear();
    doc.blocks
        .push_back(engine::Block::Paragraph(para("intro")));
    doc.blocks.push_back(engine::Block::Table(table));
    doc.blocks
        .push_back(engine::Block::Paragraph(para("outro")));
    doc
}

/// Issue #318 — pinned with the pre-memo layout: the memo must be output
/// identical on a nested table that exercises autofit (fit + shrink),
/// fixed layout, grid / gridless columns, multi-row levels and vAlign.
#[test]
fn nested_tower_geometry_is_pinned() {
    let engine = tests::test_engine_with_doc(doc_with(mixed_tower(7)));
    let (pages, _, _, info) = engine.build_pages(2.0, false, None).expect("layout");
    assert!(info.degradations.is_empty(), "{:?}", info.degradations);
    let fp = layout::geometry_fingerprint(&pages);
    eprintln!(
        "NESTED TABLE FINGERPRINT mixed_tower_7 = {fp:#x} ({} pages)",
        pages.len()
    );
    assert_eq!(fp, PINNED_MIXED_TOWER_7, "nested-table fixture moved");

    let engine = tests::test_engine_with_doc(doc_with(plain_tower(9)));
    let (pages, _, _, info) = engine.build_pages(2.0, false, None).expect("layout");
    assert!(info.degradations.is_empty(), "{:?}", info.degradations);
    let fp = layout::geometry_fingerprint(&pages);
    eprintln!(
        "NESTED TABLE FINGERPRINT plain_tower_9 = {fp:#x} ({} pages)",
        pages.len()
    );
    assert_eq!(fp, PINNED_PLAIN_TOWER_9, "nested-table fixture moved");

    /* A multi-page table whose rows each carry a small nested tower: the
    paginator splits rows around nested content (#91 / #159 shapes). */
    let engine = tests::test_engine_with_doc(doc_with(rows_of_towers(40)));
    let (pages, _, _, info) = engine.build_pages(2.0, false, None).expect("layout");
    assert!(info.degradations.is_empty(), "{:?}", info.degradations);
    assert!(pages.len() > 1, "the fixture must span pages");
    let fp = layout::geometry_fingerprint(&pages);
    eprintln!(
        "NESTED TABLE FINGERPRINT rows_of_towers_40 = {fp:#x} ({} pages)",
        pages.len()
    );
    assert_eq!(fp, PINNED_ROWS_OF_TOWERS_40, "nested-table fixture moved");
}

/// `rows` rows: a 3-deep mixed tower beside a prose cell in each.
fn rows_of_towers(rows: usize) -> engine::Table {
    engine::Table {
        rows: (0..rows)
            .map(|r| engine::TableRow {
                cells: vec![
                    cell(vec![
                        engine::Block::Paragraph(para(&format!("row {r}"))),
                        engine::Block::Table(mixed_tower(3)),
                    ]),
                    cell(vec![engine::Block::Paragraph(para(
                        "a prose cell that wraps over a couple of lines at this width",
                    ))]),
                ],
                ..Default::default()
            })
            .collect(),
        dirty: true,
        ..Default::default()
    }
}

const PINNED_MIXED_TOWER_7: u64 = 0xcb9f7de1f168b482;
const PINNED_PLAIN_TOWER_9: u64 = 0xa687449ecac5ac96;
const PINNED_ROWS_OF_TOWERS_40: u64 = 0x33b3ba96d6fdce87;

/// Grid layouts (memo misses) the body build of `doc` runs.
fn grid_layouts_for(doc: DocumentTree) -> (u64, Vec<PageBox>, LazyLayoutInfo) {
    let (cost, pages, info) = layout_cost_of(doc);
    (cost.0, pages, info)
}

/// `(grid layouts, autofit solves)` (memo misses) of the body build.
fn layout_cost_of(doc: DocumentTree) -> ((u64, u64), Vec<PageBox>, LazyLayoutInfo) {
    let engine = tests::test_engine_with_doc(doc);
    TABLE_GRID_LAYOUTS.with(|n| n.set(0));
    AUTOFIT_SOLVES.with(|n| n.set(0));
    let (pages, _, _, info) = engine.build_pages(1.0, false, None).expect("layout");
    let cost = (
        TABLE_GRID_LAYOUTS.with(std::cell::Cell::get),
        AUTOFIT_SOLVES.with(std::cell::Cell::get),
    );
    (cost, pages, info)
}

fn capped(info: &LazyLayoutInfo) -> usize {
    info.degradations
        .iter()
        .filter(|d| d.reason == LayoutDegradeReason::NestingCapped)
        .count()
}

/// Issue #318 — every nesting level of a tower is laid out as a grid
/// exactly ONCE (it was `F(2d)` times in total — 46 368 grid layouts at
/// depth 12, ~2.5e26 at the reader's 64-level cap); the autofit column
/// solves — one per distinct width an ancestor's probe offers a level —
/// stay quadratic.
#[test]
fn nested_tower_cost_is_linear_in_depth() {
    for depth in [4u64, 8, 16, 31] {
        let _strict = StrictLayoutNotes::on();
        let ((grids, solves), _, info) = layout_cost_of(doc_with(plain_tower(depth as usize)));
        assert_eq!(capped(&info), 0, "{depth} levels are under the cap");
        eprintln!(
            "[#318] plain tower depth {depth}: {grids} grid layouts, {solves} autofit solves"
        );
        assert_eq!(grids, depth, "one grid layout per level");
        assert!(
            solves <= 2 * depth * depth,
            "depth {depth}: {solves} autofit solves — the nested-table memo regressed"
        );
    }
    let ((grids, solves), _, _) = layout_cost_of(doc_with(mixed_tower(16)));
    eprintln!("[#318] mixed tower depth 16: {grids} grid layouts, {solves} autofit solves");
    assert_eq!(grids, 16, "one grid layout per level");
    assert!(
        solves <= 2 * 16 * 16,
        "mixed tower: {solves} autofit solves"
    );
}

fn painted_texts(blocks: &[LayoutBlock], texts: &[&str], out: &mut Vec<String>) {
    for b in blocks {
        match b {
            LayoutBlock::Paragraph(p) => {
                out.push(texts[p.source_paragraph_id as usize].to_string());
            }
            LayoutBlock::Table(t) => {
                for row in &t.rows {
                    for cell in &row.cells {
                        painted_texts(&cell.content, texts, out);
                    }
                }
            }
        }
    }
}

/// Grid levels of a laid-out table (1 = no nested table).
fn grid_depth(t: &TableBox) -> u32 {
    1 + t
        .rows
        .iter()
        .flat_map(|r| &r.cells)
        .flat_map(|c| &c.content)
        .filter_map(|b| match b {
            LayoutBlock::Table(t) => Some(grid_depth(t)),
            LayoutBlock::Paragraph(_) => None,
        })
        .max()
        .unwrap_or(0)
}

/// Issue #318 — a 200-deep tower lays out its first
/// `MAX_TABLE_LAYOUT_DEPTH` levels as grids and flattens the rest to
/// their paragraphs: every paragraph is painted exactly once, in
/// document order (so `source_paragraph_id` still indexes the PDF text
/// table), and the build reports ONE `NestingCapped` note.
#[test]
fn tower_past_the_cap_flattens_to_its_paragraphs_with_one_note() {
    let depth = 200;
    let doc = doc_with(plain_tower(depth));
    let mut texts: Vec<&str> = Vec::new();
    for b in doc.blocks.iter() {
        walk_block_texts(b, &mut texts);
    }
    assert_eq!(texts.len(), depth + 2);
    let (n, pages, info) = grid_layouts_for(doc.clone());
    eprintln!("[#318] plain tower depth {depth}: {n} grid layouts");
    assert_eq!(
        n,
        u64::from(MAX_TABLE_LAYOUT_DEPTH),
        "one grid layout per level up to the cap, none past it"
    );
    assert_eq!(capped(&info), 1, "{:?}", info.degradations);
    assert!(
        info.degradations
            .iter()
            .any(|d| d.reason == LayoutDegradeReason::NestingCapped && d.page.is_none()),
        "a sub-paginator note"
    );

    let mut painted = Vec::new();
    for page in &pages {
        painted_texts(&page.blocks, &texts, &mut painted);
    }
    assert_eq!(painted, texts, "flattened paragraphs keep document order");

    /* The grid stops at the cap; the last grid level's cell holds the
    flattened rest. */
    let deepest = pages
        .iter()
        .flat_map(|p| &p.blocks)
        .filter_map(|b| match b {
            LayoutBlock::Table(t) => Some(grid_depth(t)),
            LayoutBlock::Paragraph(_) => None,
        })
        .max();
    assert_eq!(deepest, Some(MAX_TABLE_LAYOUT_DEPTH));
}

/// Issue #318 — the cap is a recovery: under the strict switch it is a
/// hard failure, like a strict paginator watchdog's notes.
#[test]
#[should_panic(expected = "layout watchdog (strict): NestingCapped")]
fn strict_mode_turns_the_nesting_cap_into_a_failure() {
    let _strict = StrictLayoutNotes::on();
    let engine =
        tests::test_engine_with_doc(doc_with(plain_tower(MAX_TABLE_LAYOUT_DEPTH as usize + 1)));
    let _ = engine.build_pages(1.0, false, None);
}

/// Issue #318 — exactly `MAX_TABLE_LAYOUT_DEPTH` levels stay a grid.
#[test]
fn a_tower_at_the_cap_is_nominal() {
    let _strict = StrictLayoutNotes::on();
    let (_, pages, info) = grid_layouts_for(doc_with(plain_tower(MAX_TABLE_LAYOUT_DEPTH as usize)));
    assert_eq!(capped(&info), 0);
    let deepest = pages
        .iter()
        .flat_map(|p| &p.blocks)
        .filter_map(|b| match b {
            LayoutBlock::Table(t) => Some(grid_depth(t)),
            LayoutBlock::Paragraph(_) => None,
        })
        .max();
    assert_eq!(deepest, Some(MAX_TABLE_LAYOUT_DEPTH));
}

fn test_fonts() -> FontStack {
    let bytes = include_bytes!("../../../ts/fonts/LiberationSans-Regular.ttf").to_vec();
    let font = LoadedFont::parse("test-latin".to_string(), bytes).expect("parse test font");
    let mut faces: HashMap<String, Arc<LoadedFont>> = HashMap::new();
    faces.insert("test-latin".to_string(), Arc::new(font));
    FontStack::from_faces(faces, "test-latin")
}

fn empty_sctx() -> StyleContext<'static> {
    StyleContext {
        styles: Box::leak(Box::default()),
        run_defaults: Box::leak(Box::default()),
        note_markers: None,
        note_self_mark: None,
        theme: None,
        theme_key: 0,
    }
}

fn page_with(table: TableBox) -> PageBox {
    PageBox {
        size: Size {
            width: 600.0,
            height: 800.0,
        },
        margins: layout::Margins {
            top: 0.0,
            right: 0.0,
            bottom: 0.0,
            left: 0.0,
        },
        blocks: vec![LayoutBlock::Table(table)],
        header: None,
        footer: None,
        header_offset: 0.0,
        footer_offset: 0.0,
        footnotes: layout::NoteBand::default(),
        endnotes: layout::NoteBand::default(),
        hf_role: layout::HeaderRole::Default,
        page_number: 1,
        floats: Vec::new(),
    }
}

/// Issue #318 / #87 — a memo hit is a prediction, verified before use: a
/// poisoned entry (a box that does not fit the table it stands in for)
/// is re-laid from scratch with a `CacheMismatch` note, and the result
/// is the clean layout.
#[test]
fn a_poisoned_memo_entry_is_relaid_and_reported() {
    let fonts = test_fonts();
    let cfg = RenderConfig {
        font_id: "test-latin".to_string(),
        base_direction: ShapingDirection::Ltr,
        px_size: 12.0,
        line_height: 16.0,
        alignment: Alignment::Start,
        scale: 1.0,
        base_scale: 1.0,
        zoom: 1.0,
    };
    let table = plain_tower(3);
    let engine::Block::Table(inner) = &table.rows[0].cells[0].blocks[1] else {
        panic!("level 1 is a table");
    };
    let mut cache = new_layout_cache();
    let mut tl = TableLayout::new(&fonts, &cfg, 1.0, empty_sctx(), cache.get_mut());
    let _ = drain_layout_notes();
    let clean = layout_table_box_at(&mut tl, inner, 300.0, 1);
    let clean_fp = layout::geometry_fingerprint(&[page_with(clean.clone())]);
    let key = (node_addr(inner), 300.0_f32.to_bits());
    assert!(tl.boxes.contains_key(&key), "an inner table is memoized");
    /* Poison it: the box of a table with no rows. */
    let mut poisoned = clean;
    poisoned.rows.clear();
    tl.boxes.insert(key, poisoned);
    let relaid = layout_table_box_at(&mut tl, inner, 300.0, 1);
    assert_eq!(
        layout::geometry_fingerprint(&[page_with(relaid)]),
        clean_fp,
        "demoted to the full layout"
    );
    assert_eq!(
        drain_layout_notes()
            .iter()
            .map(|d| d.reason)
            .collect::<Vec<_>>(),
        vec![LayoutDegradeReason::CacheMismatch]
    );
    /* The healed entry serves the next hit silently. */
    let _strict = StrictLayoutNotes::on();
    let again = layout_table_box_at(&mut tl, inner, 300.0, 1);
    assert_eq!(layout::geometry_fingerprint(&[page_with(again)]), clean_fp);
}
