//! Issue #379 — the content-keyed table layout cache survives repaints,
//! is verified on every hit, and reports a bad hit as
//! `TableCacheMismatch` (distinct from the paragraph LRU's
//! `CacheMismatch`).

use super::*;
use table_cache::{TABLE_CACHE_BYPASS, TABLE_CACHE_HITS};

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

/// A `depth`-deep autofit tower tagged `tag`: each level's first cell
/// holds a heading and the next level, the second cell wrapping prose.
fn tower(tag: usize, depth: usize) -> engine::Table {
    let mut inner: Option<engine::Table> = None;
    for level in (0..depth).rev() {
        let mut first = vec![engine::Block::Paragraph(para(&format!(
            "tower {tag} level {level}"
        )))];
        if let Some(t) = inner.take() {
            first.push(engine::Block::Table(t));
        }
        let mut second = cell(vec![engine::Block::Paragraph(para(
            "prose that wraps over a couple of lines inside its narrow cell",
        ))]);
        if level % 2 == 1 {
            second.props.v_align = engine::VerticalAlign::Center;
        }
        inner = Some(engine::Table {
            grid: if level % 2 == 0 {
                vec![2400, 1800]
            } else {
                Vec::new()
            },
            rows: vec![engine::TableRow {
                cells: vec![cell(first), second],
                ..Default::default()
            }],
            dirty: true,
            ..Default::default()
        });
    }
    inner.expect("depth >= 1")
}

/// `intro`, then `towers` 3-deep towers separated by body paragraphs.
fn towers_doc(towers: usize) -> DocumentTree {
    let mut doc = DocumentTree::new();
    doc.blocks.clear();
    doc.blocks
        .push_back(engine::Block::Paragraph(para("intro")));
    for t in 0..towers {
        doc.blocks.push_back(engine::Block::Table(tower(t, 3)));
        doc.blocks
            .push_back(engine::Block::Paragraph(para(&format!("between {t}"))));
    }
    doc
}

/// `(pages, grid layouts run, verified cache hits, notes)` of one full
/// (unculled) build on `engine`, caches as they stand.
fn build(engine: &Engine) -> (Vec<PageBox>, u64, u64, Vec<LayoutDegradeReason>) {
    TABLE_GRID_LAYOUTS.with(|n| n.set(0));
    TABLE_CACHE_HITS.with(|n| n.set(0));
    let (pages, _, _, info) = engine.build_pages(1.0, false, None).expect("layout");
    (
        pages,
        TABLE_GRID_LAYOUTS.with(std::cell::Cell::get),
        TABLE_CACHE_HITS.with(std::cell::Cell::get),
        info.degradations.iter().map(|d| d.reason).collect(),
    )
}

/// The geometry a FRESH engine (empty caches) lays `doc` out to.
fn fresh_fingerprint(doc: DocumentTree) -> u64 {
    let engine = tests::test_engine_with_doc(doc);
    let (pages, ..) = build(&engine);
    layout::geometry_fingerprint(&pages)
}

fn edit(engine: &mut Engine, path: EngineBlockPath, text: &str) {
    let doc = engine
        .undo
        .current()
        .insert_text(EnginePos { path, offset: 0 }, text);
    engine.undo.push(doc);
}

/// `[Block(b), Cell(0,0), Block(1), Cell(0,0), Block(1), Cell(0,0),
/// Block(0)]` — the innermost heading of the tower at body block `b`.
fn innermost_heading(b: u32) -> EngineBlockPath {
    let cell = EnginePathStep::Cell { row: 0, col: 0 };
    EngineBlockPath {
        steps: vec![
            EnginePathStep::Block(b),
            cell,
            EnginePathStep::Block(1),
            cell,
            EnginePathStep::Block(1),
            cell,
            EnginePathStep::Block(0),
        ],
    }
}

/// Issue #379 — the acceptance shape: a repaint after an edit OUTSIDE
/// every table re-lays no table at all (each is a verified hit), and the
/// cached paint is geometry-identical to a fresh engine's layout of the
/// edited document.
#[test]
fn a_repaint_after_a_body_edit_relays_no_table() {
    let _strict = StrictLayoutNotes::on();
    let mut engine = tests::test_engine_with_doc(towers_doc(6));
    let (_, cold_grids, cold_hits, _) = build(&engine);
    assert_eq!(cold_grids, 6 * 3, "cold: every level of every tower");
    assert_eq!(cold_hits, 0);

    edit(&mut engine, EngineBlockPath::top(0), "edited ");
    let (pages, grids, hits, notes) = build(&engine);
    assert!(notes.is_empty(), "{notes:?}");
    assert_eq!(grids, 0, "no table is re-laid after a body edit");
    assert_eq!(hits, 6, "one verified hit per top-level table");
    assert_eq!(
        layout::geometry_fingerprint(&pages),
        fresh_fingerprint(engine.undo.current().clone()),
        "the cached paint equals a fresh layout"
    );

    /* Undo returns to content the cache has already seen. */
    assert!(engine.undo.undo());
    let (pages, grids, _, _) = build(&engine);
    assert_eq!(grids, 0);
    assert_eq!(
        layout::geometry_fingerprint(&pages),
        fresh_fingerprint(engine.undo.current().clone())
    );
}

/// Issue #379 — an edit inside a nested cell re-lays exactly the tables
/// on the edited path (the inner table and each ancestor: their content
/// changed); every other table — including the unchanged inner tables
/// of the edited tower's siblings — is a verified hit.
#[test]
fn an_edit_in_a_nested_cell_relays_only_its_ancestors() {
    let _strict = StrictLayoutNotes::on();
    let mut engine = tests::test_engine_with_doc(towers_doc(4));
    let _ = build(&engine);
    /* The third tower sits at body block 5 (intro, t0, p, t1, p, t2). */
    edit(&mut engine, innermost_heading(5), "typed ");
    let (pages, grids, hits, _) = build(&engine);
    assert_eq!(
        grids, 3,
        "inner, middle and outer table of the edited tower"
    );
    assert_eq!(hits, 3, "the three untouched towers");
    assert_eq!(
        layout::geometry_fingerprint(&pages),
        fresh_fingerprint(engine.undo.current().clone())
    );
}

/// Issue #379 / #87 — every hit is verified: a poisoned entry (a stale
/// or colliding box that does not fit the table it would stand in for)
/// is dropped and re-laid with ONE `TableCacheMismatch` note per bad
/// entry — never the paragraph tier's `CacheMismatch` — the paint equals
/// a fresh layout, and the healed entries then serve silently.
#[test]
fn a_poisoned_table_cache_entry_is_relaid_and_reported_as_a_table_miss() {
    type Poison = fn(&mut TableBox);
    let poisons: [(&str, Poison); 4] = [
        ("rows dropped", |t| t.rows.clear()),
        ("column count", |t| t.columns.push(10.0)),
        ("cell padding", |t| {
            if let Some(c) = t.rows.get_mut(0).and_then(|r| r.cells.get_mut(0)) {
                c.padding_left += 1.0;
            }
        }),
        ("cell paragraph width", |t| {
            for row in &mut t.rows {
                for c in &mut row.cells {
                    for b in &mut c.content {
                        if let LayoutBlock::Paragraph(p) = b {
                            p.size.width += 3.0;
                        }
                    }
                }
            }
        }),
    ];
    let doc = towers_doc(2);
    let clean = fresh_fingerprint(doc.clone());
    for (label, poison) in poisons {
        let engine = tests::test_engine_with_doc(doc.clone());
        let _ = build(&engine);
        engine.layout_cache.borrow_mut().tables.poison_boxes(poison);
        let (pages, _, _, notes) = build(&engine);
        assert_eq!(layout::geometry_fingerprint(&pages), clean, "{label}");
        assert!(
            notes.contains(&LayoutDegradeReason::TableCacheMismatch),
            "{label}: {notes:?}"
        );
        assert!(
            !notes.contains(&LayoutDegradeReason::CacheMismatch),
            "{label}: a table miss is not a paragraph miss: {notes:?}"
        );
        let _strict = StrictLayoutNotes::on();
        let (pages, grids, hits, _) = build(&engine);
        assert_eq!(layout::geometry_fingerprint(&pages), clean, "{label}");
        assert_eq!(grids, 0, "{label}: healed");
        assert_eq!(hits, 2, "{label}: healed");
    }
}

/// Issue #379 — a cached paint reports what a fresh one would: the
/// `NestingCapped` note of a tower past the nesting cap is re-derived on
/// every hit.
#[test]
fn nesting_capped_is_reported_on_every_cached_paint() {
    let depth = MAX_TABLE_LAYOUT_DEPTH as usize + 4;
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
    let mut doc = DocumentTree::new();
    doc.blocks.clear();
    doc.blocks
        .push_back(engine::Block::Paragraph(para("intro")));
    doc.blocks
        .push_back(engine::Block::Table(inner.expect("tower")));
    let mut engine = tests::test_engine_with_doc(doc);
    let capped = |notes: &[LayoutDegradeReason]| {
        notes
            .iter()
            .filter(|r| **r == LayoutDegradeReason::NestingCapped)
            .count()
    };
    let (_, _, _, notes) = build(&engine);
    assert_eq!(capped(&notes), 1, "{notes:?}");
    edit(&mut engine, EngineBlockPath::top(0), "x");
    let (_, grids, hits, notes) = build(&engine);
    assert_eq!((grids, hits), (0, 1), "served from the cache");
    assert_eq!(capped(&notes), 1, "{notes:?}");
}

/// Issue #379 — content-identical tables at the same width and depth
/// share one entry; the same content at another width is its own entry
/// and lays out to the width it is offered.
#[test]
fn identical_tables_share_an_entry_and_width_is_part_of_the_key() {
    let _strict = StrictLayoutNotes::on();
    let mut doc = DocumentTree::new();
    doc.blocks.clear();
    for _ in 0..3 {
        doc.blocks.push_back(engine::Block::Table(tower(7, 2)));
    }
    let engine = tests::test_engine_with_doc(doc.clone());
    let (pages, grids, hits, _) = build(&engine);
    assert_eq!(grids, 2, "one tower laid out, its copies served");
    assert_eq!(hits, 2);
    assert_eq!(layout::geometry_fingerprint(&pages), fresh_fingerprint(doc));

    /* Same content, a narrower column: a miss, laid out at its width. */
    let narrow = |e: &Engine| -> TableBox {
        let doc = e.undo.current().clone();
        let engine::Block::Table(t) = &doc.blocks[0] else {
            panic!("a table")
        };
        let font_stack = FontStack::from_faces(e.fonts.clone(), "test-latin");
        let cfg = e.layout_cfg.clone().expect("cfg");
        let sctx = StyleContext::of(&doc);
        let mut cache = e.layout_cache.borrow_mut();
        layout_table_box(t, 200.0, &font_stack, &cfg, 1.0, sctx, &mut cache)
    };
    TABLE_CACHE_HITS.with(|n| n.set(0));
    let first = narrow(&engine);
    assert_eq!(
        TABLE_CACHE_HITS.with(std::cell::Cell::get),
        0,
        "a new width misses"
    );
    let body_width = pages
        .iter()
        .flat_map(|p| &p.blocks)
        .find_map(LayoutBlock::as_table)
        .expect("a body table")
        .size
        .width;
    assert!(
        first.size.width < body_width,
        "laid out for its own band ({} vs {body_width})",
        first.size.width
    );
    let again = narrow(&engine);
    assert_eq!(TABLE_CACHE_HITS.with(std::cell::Cell::get), 1);
    assert_eq!(
        layout::geometry_fingerprint(&[page_of(again)]),
        layout::geometry_fingerprint(&[page_of(first)])
    );
}

fn page_of(table: TableBox) -> PageBox {
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

/// Issue #379 — clearing the layout cache (font set change, new document,
/// recovery) drops the table entries too, and the cache stays within its
/// byte accounting.
#[test]
fn clearing_the_layout_cache_drops_table_entries() {
    let engine = tests::test_engine_with_doc(towers_doc(3));
    let _ = build(&engine);
    {
        let cache = engine.layout_cache.borrow();
        assert!(cache.tables.box_count() > 0);
        assert!(cache.tables.box_bytes() > 0);
    }
    engine.layout_cache.borrow_mut().clear();
    let cache = engine.layout_cache.borrow();
    assert_eq!(cache.tables.box_count(), 0);
    assert_eq!(cache.tables.box_bytes(), 0);
    assert_eq!(cache.len(), 0, "the paragraph LRU too");
}

/// The first body table of `doc`, mutably (copy-on-write).
fn first_table(doc: &mut DocumentTree) -> &mut engine::Table {
    doc.blocks
        .iter_mut()
        .find_map(|b| match b {
            engine::Block::Table(t) => Some(t),
            engine::Block::Paragraph(_) => None,
        })
        .expect("a body table")
}

/// The table nested in cell (0,0) of `t`.
fn inner_table(t: &mut engine::Table) -> &mut engine::Table {
    t.rows[0].cells[0]
        .blocks
        .iter_mut()
        .find_map(|b| match b {
            engine::Block::Table(t) => Some(t),
            engine::Block::Paragraph(_) => None,
        })
        .expect("a nested table")
}

/// Issue #379 — the content key sees every layout input of a table: an
/// edit to any table / row / cell property the layout reads (at the top
/// level or inside a nested table) misses the warm cache, and the paint
/// is identical — box for box, not only by geometry fingerprint (shading
/// and borders ride the boxes) — to a fresh engine's layout.
#[test]
fn every_layout_affecting_table_property_edit_misses_the_cache() {
    type Mutation = fn(&mut engine::Table);
    let mutations: [(&str, Mutation); 17] = [
        ("grid", |t| t.grid = vec![1200, 3600]),
        ("fixed layout", |t| {
            t.props.layout = engine::TableLayout::Fixed
        }),
        ("table cell margins", |t| {
            t.props.cell_margins.left_twips = Some(400);
        }),
        ("table borders", |t| {
            t.props.borders = Some(engine::CellBorders {
                inside_v: Some(engine::BorderStroke {
                    style: engine::BorderStyle::Single,
                    size_eighth_pt: 24,
                    ..Default::default()
                }),
                ..Default::default()
            });
        }),
        ("table alignment", |t| {
            t.props.alignment = Some(engine::Alignment::Center);
        }),
        ("table indent", |t| t.props.indent_twips = 720),
        ("bidi visual", |t| t.props.bidi_visual = true),
        ("row height", |t| {
            t.rows[0].props.height = Some(engine::RowHeight::Exact { twips: 3000 });
        }),
        ("header row", |t| t.rows[0].props.header = true),
        ("cant split", |t| t.rows[0].props.cant_split = true),
        ("cell shading", |t| {
            t.rows[0].cells[1].props.shading = Some([200, 10, 10, 255]);
        }),
        ("cell v-align", |t| {
            t.rows[0].cells[1].props.v_align = engine::VerticalAlign::Bottom;
        }),
        ("cell margins", |t| {
            t.rows[0].cells[1].props.cell_margins = Some(engine::CellMargins {
                left_twips: Some(500),
                ..Default::default()
            });
        }),
        ("cell borders", |t| {
            t.rows[0].cells[1].props.borders = Some(engine::CellBorders {
                left: Some(engine::BorderStroke {
                    style: engine::BorderStyle::Single,
                    size_eighth_pt: 24,
                    ..Default::default()
                }),
                ..Default::default()
            });
        }),
        ("grid span", |t| t.rows[0].cells[1].props.grid_span = 2),
        ("nested table grid", |t| {
            inner_table(t).grid = vec![600, 3000]
        }),
        ("nested cell margins", |t| {
            inner_table(t).rows[0].cells[1].props.cell_margins = Some(engine::CellMargins {
                right_twips: Some(700),
                ..Default::default()
            });
        }),
    ];
    let doc = towers_doc(2);
    for (label, mutate) in mutations {
        let mut engine = tests::test_engine_with_doc(doc.clone());
        let _ = build(&engine);
        let mut edited = engine.undo.current().clone();
        mutate(first_table(&mut edited));
        engine.undo.push(edited.clone());
        let (warm, grids, hits, _) = build(&engine);
        let fresh = {
            let e = tests::test_engine_with_doc(edited);
            build(&e).0
        };
        assert!(grids > 0, "{label}: the edited table is re-laid");
        /* The untouched tower is served; so may be the edited tower's
        unchanged inner levels, when they are offered the same width. */
        assert!(hits >= 1, "{label}: the untouched tower is served");
        assert_eq!(
            format!("{warm:?}"),
            format!("{fresh:?}"),
            "{label}: the warm paint equals a fresh layout"
        );
        /* And the edited table's new entry passes verification: the next
        paint re-lays nothing and notes nothing. */
        let _strict = StrictLayoutNotes::on();
        let (again, grids, _, _) = build(&engine);
        assert_eq!(grids, 0, "{label}: the new entry serves the next paint");
        assert_eq!(format!("{again:?}"), format!("{fresh:?}"), "{label}");
    }
}

/// Issue #379 — manual profiling harness for the table cache on the
/// table-heavy perf fixture (`tools/perf-fixtures` →
/// `tests/perf/tables-30p.docx`, 22 three-deep nested towers). Each
/// repaint is timed twice: with the table cache as it stands (#379) and
/// with it bypassed (`TABLE_CACHE_BYPASS`) — the pre-#379 behaviour,
/// where nothing table-shaped survived a paint (the paragraph LRU stays
/// warm in both). Run with:
/// `cargo test -p engine-wasm --release --lib profile_tables -- --ignored --nocapture`
#[test]
#[ignore = "manual profiling harness, not a correctness gate"]
fn profile_tables_layout() {
    use std::time::{Duration, Instant};
    const RUNS: usize = 9;
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/perf/tables-30p.docx"
    );
    let bytes = std::fs::read(path).expect("read tables-30p.docx (run tools/perf-fixtures)");
    let archive = format_docx::read_docx(&bytes).expect("parse tables-30p.docx");
    let mut engine = tests::test_engine_with_doc(archive.document);
    /* The editor's boot text metrics (`App.tsx` `RENDER_PAGE`). */
    if let Some(cfg) = engine.layout_cfg.as_mut() {
        cfg.px_size = 24.0;
        cfg.line_height = 36.0;
    }
    let median = |mut v: Vec<Duration>| {
        v.sort();
        v[v.len() / 2]
    };

    let t = Instant::now();
    let (pages, ..) = engine.build_pages(2.0, false, None).expect("cold");
    eprintln!(
        "[#379] build_pages FULL cold:                 {:>9.2?}  ({} pages)",
        t.elapsed(),
        pages.len()
    );
    {
        let cache = engine.layout_cache.borrow();
        eprintln!(
            "[#379] table cache: {} boxes, ~{} KiB",
            cache.tables.box_count(),
            cache.tables.box_bytes() / 1024
        );
    }

    /* `edit_at` — `None`: a body edit at the top; `Some(b)`: an edit in
    the innermost cell of the tower at body block `b`. */
    /* One timed repaint after a one-character edit; `bypass` = the
    pre-#379 behaviour (no table layout survives the call). */
    let mut repaint = |edit_at: Option<u32>, band: Option<f32>, bypass: bool| {
        let path = edit_at.map_or_else(|| EngineBlockPath::top(0), innermost_heading);
        edit(&mut engine, path, "x");
        TABLE_CACHE_BYPASS.with(|b| b.set(bypass));
        let t = Instant::now();
        let _ = engine.build_pages(2.0, false, band).expect("repaint");
        let elapsed = t.elapsed();
        TABLE_CACHE_BYPASS.with(|b| b.set(false));
        elapsed
    };
    for (label, edit_at, band) in [
        ("body edit, FULL      ", None, None),
        ("body edit, 2000px band", None, Some(2000.0)),
        ("nested-cell edit, FULL", Some(1), None),
        ("nested-cell edit, band", Some(1), Some(2000.0)),
    ] {
        /* Warm both paths once, then interleave the timed runs so load
        from elsewhere on the machine hits both sides alike. */
        let _ = (repaint(edit_at, band, true), repaint(edit_at, band, false));
        let (mut before, mut after) = (Vec::new(), Vec::new());
        for _ in 0..RUNS {
            before.push(repaint(edit_at, band, true));
            after.push(repaint(edit_at, band, false));
        }
        let (before, after) = (median(before), median(after));
        eprintln!(
            "[#379] repaint after {label}: pre-#379 {before:>9.2?}  ->  #379 {after:>9.2?}  \
             ({:.1}x)",
            before.as_secs_f64() / after.as_secs_f64().max(1e-9)
        );
    }
}
