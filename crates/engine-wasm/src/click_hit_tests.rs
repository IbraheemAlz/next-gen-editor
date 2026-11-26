//! Issues #568 / #572 (epic #555 row 3, clean-room cursor memo §2) — a
//! click resolves to its CELL first and to the paragraph that OWNS the
//! space it lands in, not to whichever line is nearest.
//!
//! * #568 — a click in the empty lower part of a short table cell, beside
//!   a taller neighbour cell of the same row, used to land on the
//!   neighbour's line at that height (`nearest_line` ranked lines by y
//!   distance first, x only breaking ties). The cell under the point now
//!   wins (its border box, the merged extent for a vertical merge,
//!   mirrored for a `<w:bidiVisual>` table), then the nearest line inside
//!   it: below its last line = its last line.
//! * #572 — paragraph spacing belongs to its paragraph: a click in the
//!   space-after of `upper` lands on `upper`'s last line, one in the
//!   space-before of `lower` on `lower`'s first line, wherever in the gap
//!   the nearer line box is.
//!
//! Every case goes through the ONE resolution (`nearest_line`) that
//! `HitTest`, `PlaceCaretAtPoint`, `ExtendSelectionToPoint` and
//! `SelectWordAt` share.

use super::*;

fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    use std::task::{Context, Poll, Waker};
    let mut cx = Context::from_waker(Waker::noop());
    let mut fut = Box::pin(fut);
    match fut.as_mut().poll(&mut cx) {
        Poll::Ready(v) => v,
        Poll::Pending => panic!("Engine::apply suspended in a native test"),
    }
}

fn apply(e: &mut Engine, cmd: Command) -> Event {
    block_on(e.apply(cmd))
}

fn para(text: &str) -> engine::Paragraph {
    engine::Paragraph {
        text: text.to_string(),
        ..Default::default()
    }
}

fn spaced(text: &str, before_pt: i32, after_pt: i32) -> engine::Paragraph {
    let mut p = para(text);
    p.props.spacing = engine::Spacing {
        before_twips: before_pt * 20,
        after_twips: after_pt * 20,
    };
    p
}

fn rtl(text: &str) -> engine::Paragraph {
    let mut p = para(text);
    p.props.direction = Some(engine::TextDirection::Rtl);
    p
}

fn cell_of(paras: Vec<engine::Paragraph>) -> engine::TableCell {
    engine::TableCell {
        blocks: paras.into_iter().map(engine::Block::Paragraph).collect(),
        ..Default::default()
    }
}

fn row_of(cells: Vec<engine::TableCell>) -> engine::TableRow {
    engine::TableRow {
        cells,
        ..Default::default()
    }
}

/// `intro`, `table`, `outro` — the table is block 1.
fn doc_with_table(table: engine::Table) -> DocumentTree {
    let mut doc = DocumentTree::new();
    doc.blocks.clear();
    doc.blocks
        .push_back(engine::Block::Paragraph(para("intro")));
    doc.blocks.push_back(engine::Block::Table(table));
    doc.blocks
        .push_back(engine::Block::Paragraph(para("outro")));
    doc
}

fn doc_of(paras: Vec<engine::Paragraph>) -> DocumentTree {
    let mut doc = DocumentTree::new();
    doc.blocks.clear();
    for p in paras {
        doc.blocks.push_back(engine::Block::Paragraph(p));
    }
    doc
}

/// `[Block(1), Cell{row, col}, Block(block)]`.
fn cell_path(row: u32, col: u32, block: u32) -> BridgeBlockPath {
    BridgeBlockPath {
        steps: vec![
            BridgePathStep::Block { idx: 1 },
            BridgePathStep::Cell { row, col },
            BridgePathStep::Block { idx: block },
        ],
    }
}

fn lines_of<'g>(geom: &'g [LineGeom], path: &BridgeBlockPath) -> Vec<&'g LineGeom> {
    let lines: Vec<&LineGeom> = geom.iter().filter(|l| &l.path == path).collect();
    assert!(!lines.is_empty(), "no geometry line for {path:?}");
    lines
}

fn line_of<'g>(geom: &'g [LineGeom], path: &BridgeBlockPath) -> &'g LineGeom {
    lines_of(geom, path)[0]
}

fn mid_y(l: &LineGeom) -> f32 {
    l.y_top + l.height / 2.0
}

/// The leftmost / rightmost caret stop of a line (its ink extent).
fn ink(l: &LineGeom) -> (f32, f32) {
    l.slots
        .iter()
        .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), s| {
            (lo.min(s.x), hi.max(s.x))
        })
}

fn hit(e: &mut Engine, x: f32, y: f32) -> BridgeLogicalPos {
    match apply(
        e,
        Command::HitTest {
            at: BridgePoint { x, y },
        },
    ) {
        Event::HitResult { pos } => pos,
        other => panic!("HitTest: {other:?}"),
    }
}

fn caret_after_click(e: &mut Engine, x: f32, y: f32) -> BridgeLogicalPos {
    let at = BridgePoint { x, y };
    match apply(e, Command::PlaceCaretAtPoint { page: 0, at }) {
        Event::SelectionChanged { range, .. } => {
            assert_eq!(range.start, range.end, "a click places a collapsed caret");
            range.start
        }
        other => panic!("PlaceCaretAtPoint: {other:?}"),
    }
}

/* ------------------------------------------------------------------ */
/* Issue #568 — the cell first. */

/// 2 x 2, the issue's repro: cell (0,0) = `a`, cell (0,1) = three soft-
/// broken lines `b1 b2 b3`, row 1 = `c` / `d`.
fn uneven_row_table() -> engine::Table {
    engine::Table {
        grid: vec![2400, 2400],
        rows: vec![
            row_of(vec![
                cell_of(vec![para("a")]),
                cell_of(vec![para("b1\u{2028}b2\u{2028}b3")]),
            ]),
            row_of(vec![cell_of(vec![para("c")]), cell_of(vec![para("d")])]),
        ],
        dirty: true,
        ..Default::default()
    }
}

/// The issue's acceptance: `HitTest` / `PlaceCaretAtPoint` at cell (0,0)'s
/// x and `b2`'s y land in cell (0,0) — and so does every other point of
/// the short cell's empty lower part, down to the row's bottom edge
/// (where `b3`'s line box, and even row 1's `c`, are nearer than `a`).
#[test]
fn a_click_below_a_short_cells_text_stays_in_that_cell() {
    let mut e = tests::test_engine_with_doc(doc_with_table(uneven_row_table()));
    let geom = e.document_geometry().expect("geometry");
    let a = line_of(&geom, &cell_path(0, 0, 0));
    let b = lines_of(&geom, &cell_path(0, 1, 0));
    assert_eq!(b.len(), 3, "b1 / b2 / b3 are three lines");
    let (b2, b3) = (b[1], b[2]);
    let c = line_of(&geom, &cell_path(1, 0, 0));
    let cell = *a.cells.last().expect("a cell line carries its cell");
    assert_eq!(a.cells.len(), 1);
    assert!(
        b2.y_top >= a.y_top + a.height - 1e-3,
        "b2 sits below `a`'s line: {} vs {} + {}",
        b2.y_top,
        a.y_top,
        a.height
    );

    /* Right of `a`'s ink, inside cell (0,0): the logical end of `a`. */
    let x_empty = (ink(a).1 + cell.x1) / 2.0;
    let row_bottom = cell.y1 - 0.5;
    for (what, y) in [
        ("b2's height", mid_y(b2)),
        ("b3's height", mid_y(b3)),
        ("the row's bottom edge", row_bottom),
    ] {
        let pos = hit(&mut e, x_empty, y);
        assert_eq!(pos.path, cell_path(0, 0, 0), "HitTest at {what}");
        assert_eq!(pos.offset, 1, "HitTest at {what}: the end of `a`");
        let caret = caret_after_click(&mut e, x_empty, y);
        assert_eq!(caret.path, cell_path(0, 0, 0), "click at {what}");
    }
    /* What the old "nearest line box" rule saw at the row's bottom: `b3`
    (and row 1's `c`) nearer than `a`. */
    assert!(line_y_dist(b3, row_bottom) < line_y_dist(a, row_bottom));
    assert!(line_y_dist(c, row_bottom) < line_y_dist(a, row_bottom));

    /* Over `a`'s own column of ink, far below it: still cell (0,0). */
    let (lo, hi) = ink(a);
    let pos = hit(&mut e, (lo + hi) / 2.0, mid_y(b3));
    assert_eq!(pos.path, cell_path(0, 0, 0));

    /* The neighbour is untouched: a click on `b2` in cell (0,1). */
    let (lo, hi) = ink(b2);
    let pos = hit(&mut e, (lo + hi) / 2.0, mid_y(b2));
    assert_eq!(pos.path, cell_path(0, 1, 0));
    /* `b1` 0..2, U+2028 2..5, `b2` 5..7. */
    assert!((5..=7).contains(&pos.offset), "inside `b2`: {pos:?}");

    /* Row 1's cell (1,0) right below the row edge. */
    let pos = hit(&mut e, x_empty, cell.y1 + 0.5);
    assert_eq!(pos.path, cell_path(1, 0, 0));
}

/// A double-click (`SelectWordAt`, `word_target_geom`) in the empty part of
/// the short cell selects in THAT cell: past `a`'s end it is `a`.
#[test]
fn a_double_click_below_a_short_cells_text_selects_in_that_cell() {
    let mut e = tests::test_engine_with_doc(doc_with_table(uneven_row_table()));
    let geom = e.document_geometry().expect("geometry");
    let a = line_of(&geom, &cell_path(0, 0, 0));
    let b2 = lines_of(&geom, &cell_path(0, 1, 0))[1];
    let cell = *a.cells.last().expect("cell");
    let at = BridgePoint {
        x: (ink(a).1 + cell.x1) / 2.0,
        y: mid_y(b2),
    };
    match apply(&mut e, Command::SelectWordAt { at }) {
        Event::SelectionChanged { range, .. } => {
            assert_eq!(range.start.path, cell_path(0, 0, 0), "{range:?}");
            assert_eq!(range.end.path, cell_path(0, 0, 0), "{range:?}");
            assert_eq!((range.start.offset, range.end.offset), (0, 1));
        }
        other => panic!("SelectWordAt: {other:?}"),
    }
}

/// A short cell made of several paragraphs: below the LAST one, still the
/// cell's last line (memo §2: "otherwise the last line").
#[test]
fn below_a_multi_paragraph_short_cell_lands_on_its_last_line() {
    let table = engine::Table {
        grid: vec![2400, 2400],
        rows: vec![row_of(vec![
            cell_of(vec![para("one"), para("two")]),
            cell_of((1..=6).map(|i| para(&format!("tall {i}"))).collect()),
        ])],
        dirty: true,
        ..Default::default()
    };
    let mut e = tests::test_engine_with_doc(doc_with_table(table));
    let geom = e.document_geometry().expect("geometry");
    let two = line_of(&geom, &cell_path(0, 0, 1));
    let tall5 = line_of(&geom, &cell_path(0, 1, 4));
    let cell = *two.cells.last().expect("cell");
    let pos = hit(&mut e, (ink(two).1 + cell.x1) / 2.0, mid_y(tall5));
    assert_eq!(pos.path, cell_path(0, 0, 1));
    assert_eq!(pos.offset, 3, "the end of `two`");
    /* The cell's last line owns the cell down to its bottom edge, its
    first line up to its top edge (top padding included). */
    let one = line_of(&geom, &cell_path(0, 0, 0));
    assert!(
        (two.band_bottom - cell.y1).abs() < 1e-3,
        "{} vs {cell:?}",
        two.band_bottom
    );
    assert!((one.band_top - cell.y0).abs() < 1e-3);
    /* Between the two paragraphs the bands meet without overlap. */
    assert!((one.band_bottom - two.band_top).abs() < 1e-3);
}

/// A vertical merge resolves to its merge start, and a horizontal merge
/// (grid span) covers all its grid columns (memo §2, Tables).
#[test]
fn merged_cells_resolve_to_the_merge_start_and_cover_their_span() {
    let restart = |text: &str| engine::TableCell {
        props: engine::CellProperties {
            v_merge: engine::VMergeRole::Restart,
            ..Default::default()
        },
        blocks: vec![engine::Block::Paragraph(para(text))],
        ..Default::default()
    };
    let cont = || engine::TableCell {
        props: engine::CellProperties {
            v_merge: engine::VMergeRole::Continue,
            ..Default::default()
        },
        blocks: vec![engine::Block::Paragraph(para(""))],
        ..Default::default()
    };
    let wide = engine::TableCell {
        props: engine::CellProperties {
            grid_span: 2,
            ..Default::default()
        },
        blocks: vec![engine::Block::Paragraph(para("w"))],
        ..Default::default()
    };
    let table = engine::Table {
        grid: vec![2400, 2400],
        rows: vec![
            row_of(vec![restart("m"), cell_of(vec![para("r0")])]),
            row_of(vec![cont(), cell_of(vec![para("r1")])]),
            row_of(vec![cont(), cell_of(vec![para("r2a"), para("r2b")])]),
            row_of(vec![wide]),
            row_of(vec![cell_of(vec![para("x")]), cell_of(vec![para("y")])]),
        ],
        dirty: true,
        ..Default::default()
    };
    let mut e = tests::test_engine_with_doc(doc_with_table(table));
    let geom = e.document_geometry().expect("geometry");
    let m = line_of(&geom, &cell_path(0, 0, 0));
    let merged = *m.cells.last().expect("cell");
    let r2b = line_of(&geom, &cell_path(2, 1, 1));
    assert!(
        merged.y1 >= r2b.y_top + r2b.height - 1e-3,
        "the restart cell spans the merged rows: {merged:?} vs r2b at {}",
        r2b.y_top
    );
    /* Rows 1 and 2 of the merged column: the merge start, cell (0,0). */
    let x = (ink(m).1 + merged.x1) / 2.0;
    for (what, y) in [
        ("row 1", mid_y(line_of(&geom, &cell_path(1, 1, 0)))),
        ("row 2", mid_y(r2b)),
    ] {
        let pos = hit(&mut e, x, y);
        assert_eq!(pos.path, cell_path(0, 0, 0), "{what}");
        assert_eq!(pos.offset, 1, "{what}: the end of `m`");
    }
    /* The spanning cell of row 3 under its second grid column. */
    let w = line_of(&geom, &cell_path(3, 0, 0));
    let w_cell = *w.cells.last().expect("cell");
    assert!(
        w_cell.x1 > merged.x1 + 10.0,
        "row 3's cell spans both columns"
    );
    let pos = hit(&mut e, w_cell.x1 - 5.0, mid_y(w));
    assert_eq!(pos.path, cell_path(3, 0, 0));
    /* Row 4's second cell is still its own. */
    let y4 = line_of(&geom, &cell_path(4, 1, 0));
    let pos = hit(&mut e, w_cell.x1 - 5.0, mid_y(y4));
    assert_eq!(pos.path, cell_path(4, 1, 0));
}

/// A `<w:bidiVisual>` table: cell (0,0) is the RIGHTMOST one (the layout
/// mirrors it, `table_bidi.rs`); a click in its empty lower part stays in
/// it, and left of its RTL ink is that paragraph's logical END (#553).
#[test]
fn an_rtl_tables_short_cell_keeps_its_clicks() {
    let table = engine::Table {
        grid: vec![2400, 2400],
        props: engine::TableProperties {
            bidi_visual: true,
            ..Default::default()
        },
        rows: vec![row_of(vec![
            cell_of(vec![rtl("a")]),
            cell_of((1..=4).map(|i| rtl(&format!("b{i}"))).collect()),
        ])],
        dirty: true,
        ..Default::default()
    };
    let mut e = tests::test_engine_with_doc(doc_with_table(table));
    let geom = e.document_geometry().expect("geometry");
    let a = line_of(&geom, &cell_path(0, 0, 0));
    let b3 = line_of(&geom, &cell_path(0, 1, 2));
    let cell = *a.cells.last().expect("cell");
    let other = *b3.cells.last().expect("cell");
    assert!(
        cell.x0 >= other.x1 - 1e-3,
        "cell (0,0) is right of cell (0,1)"
    );
    /* Left of `a`'s ink (an RTL paragraph: the logical end side). */
    let pos = hit(&mut e, (cell.x0 + ink(a).0) / 2.0, mid_y(b3));
    assert_eq!(pos.path, cell_path(0, 0, 0));
    assert_eq!(pos.offset, 1, "left of RTL ink = the logical end");
    /* Right of it, in the cell's right part: the logical start. */
    let pos = hit(&mut e, cell.x1 - 1.0, mid_y(b3));
    assert_eq!(pos.path, cell_path(0, 0, 0));
    assert_eq!(pos.offset, 0);
    /* The tall cell on the left keeps its own lines. */
    let (lo, hi) = ink(b3);
    assert_eq!(
        hit(&mut e, (lo + hi) / 2.0, mid_y(b3)).path,
        cell_path(0, 1, 2)
    );
}

/// A point beside a table (left of it, in its row band) resolves to the
/// row's NEAREST cell — the grid column under x, clamped (memo §2) — and
/// a cell containing a narrower nested table keeps a click beside that
/// nested table instead of handing it to a sibling cell.
#[test]
fn beside_a_table_and_beside_a_nested_table_the_nearest_cell_of_the_row_wins() {
    let fixed = || engine::TableProperties {
        layout: engine::TableLayout::Fixed,
        ..Default::default()
    };
    let inner = engine::Table {
        grid: vec![900],
        props: fixed(),
        rows: vec![row_of(vec![cell_of(vec![para("in")])])],
        dirty: true,
        ..Default::default()
    };
    let mut host = cell_of(vec![para("host")]);
    host.blocks.push(engine::Block::Table(inner));
    host.blocks.push(engine::Block::Paragraph(para("after")));
    let table = engine::Table {
        grid: vec![3600, 1800],
        props: engine::TableProperties {
            indent_twips: 1440,
            ..fixed()
        },
        rows: vec![row_of(vec![
            host,
            cell_of((1..=8).map(|i| para(&format!("side {i}"))).collect()),
        ])],
        dirty: true,
        ..Default::default()
    };
    let mut e = tests::test_engine_with_doc(doc_with_table(table));
    let geom = e.document_geometry().expect("geometry");
    let host_line = line_of(&geom, &cell_path(0, 0, 0));
    let host_cell = *host_line.cells.last().expect("cell");
    let inner_path = BridgeBlockPath {
        steps: vec![
            BridgePathStep::Block { idx: 1 },
            BridgePathStep::Cell { row: 0, col: 0 },
            BridgePathStep::Block { idx: 1 },
            BridgePathStep::Cell { row: 0, col: 0 },
            BridgePathStep::Block { idx: 0 },
        ],
    };
    let inner_line = line_of(&geom, &inner_path);
    assert_eq!(inner_line.cells.len(), 2, "outer + inner cell");
    let inner_cell = inner_line.cells[1];
    assert!(
        inner_cell.x1 + 20.0 < host_cell.x1,
        "the nested table is narrower: {inner_cell:?} in {host_cell:?}"
    );

    /* Beside the nested table, inside the host cell, near the sibling
    cell's edge: the nested table's row (the block at that y), not the
    sibling cell. */
    let pos = hit(&mut e, host_cell.x1 - 2.0, mid_y(inner_line));
    assert_eq!(pos.path, inner_path);

    /* Left of the indented table, at the host cell's line: the host cell
    (the nearest cell of the row), not the body paragraph above. */
    let pos = hit(&mut e, host_cell.x0 - 30.0, mid_y(host_line));
    assert_eq!(pos.path, cell_path(0, 0, 0));
    assert_eq!(pos.offset, 0, "left of the ink: the logical start");
}

/* ------------------------------------------------------------------ */
/* Issue #572 — paragraph spacing belongs to its paragraph. */

/// `upper` (space-after `after` pt), `lower` (space-before `before` pt),
/// `third`: the gap between `upper` and `lower` splits at `upper`'s
/// space-after, whatever line box is nearer.
fn spacing_case(after: i32, before: i32) {
    let mut e = tests::test_engine_with_doc(doc_of(vec![
        spaced("upper", 0, after),
        spaced("lower", before, 0),
        para("third"),
    ]));
    let scale = e.scale();
    let geom = e.document_geometry().expect("geometry");
    let up = line_of(&geom, &BridgeBlockPath::top(0));
    let low = line_of(&geom, &BridgeBlockPath::top(1));
    let gap_top = up.y_top + up.height;
    let gap = low.y_top - gap_top;
    let want = (after + before) as f32 * scale;
    assert!(
        (gap - want).abs() < 0.5,
        "gap {gap} vs {want} ({after}/{before})"
    );
    let split = gap_top + after as f32 * scale;
    /* The bands tile the gap at the split. */
    assert!(
        (up.band_bottom - split).abs() < 1e-2,
        "{after}/{before}: upper band ends at {}",
        up.band_bottom
    );
    assert!(
        (low.band_top - split).abs() < 1e-2,
        "{after}/{before}: lower band starts at {}",
        low.band_top
    );

    let right_of_ink = ink(up).1.max(ink(low).1) + 40.0;
    for f in [0.02, 0.25, 0.5, 0.75, 0.98] {
        let y = gap_top + gap * f;
        let pos = hit(&mut e, right_of_ink, y);
        if y < split - 0.25 {
            assert_eq!(
                pos,
                bpos_top(0, 5),
                "{after}/{before} at {f}: upper's space-after = upper's end"
            );
        } else if y > split + 0.25 {
            assert_eq!(
                pos,
                bpos_top(1, 5),
                "{after}/{before} at {f}: lower's space-before = lower's line"
            );
        }
        /* The click path agrees with the query. */
        assert_eq!(caret_after_click(&mut e, right_of_ink, y), pos);
    }
}

/// The issue's measured case and its mirror images: after only, before
/// only, both (asymmetric, both ways).
#[test]
fn a_click_in_paragraph_spacing_lands_in_the_paragraph_that_owns_it() {
    spacing_case(24, 12);
    spacing_case(12, 24);
    spacing_case(24, 0);
    spacing_case(0, 24);
    spacing_case(24, 24);
}

/// Inside a multi-line paragraph the bands are the line boxes (no change
/// for adjacent line boxes); the first line of the first paragraph of a
/// page owns its space-before up to the content top.
#[test]
fn line_boxes_keep_their_bands_and_the_page_top_spacing_is_owned() {
    let long = "word ".repeat(80);
    let mut e =
        tests::test_engine_with_doc(doc_of(vec![spaced(long.trim_end(), 18, 6), para("next")]));
    let scale = e.scale();
    let geom = e.document_geometry().expect("geometry");
    let lines = lines_of(&geom, &BridgeBlockPath::top(0));
    assert!(lines.len() >= 3, "a wrapped paragraph");
    let first = lines[0];
    assert!((first.y_top - first.band_top - 18.0 * scale).abs() < 1e-2);
    for l in &lines[1..lines.len() - 1] {
        assert_eq!((l.band_top, l.band_bottom), (l.y_top, l.y_top + l.height));
    }
    let last = lines[lines.len() - 1];
    assert!((last.band_bottom - last.y_top - last.height - 6.0 * scale).abs() < 1e-2);
    for w in lines.windows(2) {
        assert!(
            (w[0].band_bottom - w[1].band_top).abs() < 1e-2,
            "adjacent lines abut"
        );
    }
    /* A click on the middle of line 2 is line 2's. */
    let l2 = lines[1];
    let (lo, hi) = ink(l2);
    let pos = hit(&mut e, (lo + hi) / 2.0, mid_y(l2));
    assert!(pos.offset >= l2.start_byte && pos.offset <= l2.end_byte);
    /* In the space-before, above the first line: the first line. */
    let pos = hit(&mut e, ink(first).1 + 5.0, first.y_top - 9.0 * scale);
    assert_eq!(pos.path, BridgeBlockPath::top(0));
    assert_eq!(pos.offset, first.end_byte);
}

/// A two-column section: each line hit-tests over its own COLUMN, so a
/// click in column 2 is column 2's even where column 1 has a line at the
/// same height, and a column-2 paragraph's space-after is its own.
#[test]
fn a_two_column_section_hit_tests_by_column_and_keeps_spacing_per_column() {
    let mut doc = doc_of(
        (0..90)
            .map(|i| spaced(&format!("column paragraph {i}"), 0, 12))
            .collect(),
    );
    doc.body_section.columns = engine::ColumnSpec {
        count: 2,
        gutter_pt: 36.0,
    };
    let mut e = tests::test_engine_with_doc(doc);
    let scale = e.scale();
    let geom = e.document_geometry().expect("geometry");
    let page0: Vec<&LineGeom> = geom.iter().filter(|l| l.y_top < 700.0 * scale).collect();
    let mid_x = {
        let (lo, hi) = page0
            .iter()
            .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), l| {
                (lo.min(l.hit_left), hi.max(l.hit_left + l.hit_width))
            });
        (lo + hi) / 2.0
    };
    let col2: Vec<&LineGeom> = page0
        .iter()
        .copied()
        .filter(|l| l.hit_left > mid_x)
        .collect();
    let col1: Vec<&LineGeom> = page0
        .iter()
        .copied()
        .filter(|l| l.hit_left < mid_x)
        .collect();
    assert!(
        col1.len() > 5 && col2.len() > 5,
        "content flows into column 2"
    );
    for l in &col1 {
        assert!(
            l.hit_left + l.hit_width <= col2[0].hit_left,
            "columns do not overlap"
        );
    }
    /* A col-2 line whose y range overlaps a col-1 line. */
    let (c2, c1) = col2
        .iter()
        .find_map(|c2| {
            col1.iter()
                .find(|c1| (c1.y_top - c2.y_top).abs() < c2.height / 2.0)
                .map(|c1| (*c2, *c1))
        })
        .expect("a column-2 line beside a column-1 line");
    let (lo, hi) = ink(c2);
    let pos = hit(&mut e, (lo + hi) / 2.0, mid_y(c2));
    assert_eq!(
        pos.path, c2.path,
        "column 2's line, not column 1's {:?}",
        c1.path
    );
    /* Column 2's space-after (12 pt below c2), right of its ink. */
    let y = c2.y_top + c2.height + 4.0 * scale;
    let pos = hit(&mut e, hi + 10.0, y);
    assert_eq!(pos.path, c2.path);
    assert_eq!(pos.offset, c2.end_byte);
    /* The column gap splits at its middle. */
    let c1_right = c1.hit_left + c1.hit_width;
    let gap_mid = (c1_right + c2.hit_left) / 2.0;
    assert_eq!(hit(&mut e, gap_mid - 2.0, mid_y(c2)).path, c1.path);
    assert_eq!(hit(&mut e, gap_mid + 2.0, mid_y(c2)).path, c2.path);
}

/// `block_hit_claims` never lets a claim reach past the neighbouring block
/// of the same column: a block that opened a fresh column without its
/// space-before (the paginator's page-top rule) claims nothing above
/// another column's block, and between two neighbours the upper block's
/// space-after is served first.
#[test]
fn block_hit_claims_stay_inside_the_gap_to_the_column_neighbour() {
    let e = tests::test_engine_with_doc(doc_of(vec![para("one"), para("two")]));
    let (pages, ..) = e.build_pages(1.0, false, None).expect("layout");
    let LayoutBlock::Paragraph(proto) = pages[0].blocks[0].clone() else {
        panic!("a paragraph");
    };
    /* `(x, width)`, `(y, height)`, `(before, after)`. */
    let block = |(x, width): (f32, f32), (y, height): (f32, f32), (before, after): (f32, f32)| {
        let mut p = proto.clone();
        p.origin = Point { x, y };
        p.size = Size { width, height };
        p.flow.space_before = before;
        p.flow.space_after = after;
        LayoutBlock::Paragraph(p)
    };
    /* A (after 5) then B (before 8) but only a 10 px gap between them:
    A's space-after is served first, B claims the 5 px left — the claims
    never overlap. */
    let claims = block_hit_claims(
        &[
            block((0.0, 200.0), (0.0, 10.0), (0.0, 5.0)),
            block((0.0, 200.0), (20.0, 10.0), (8.0, 0.0)),
        ],
        None,
    );
    assert_eq!(claims, vec![(0.0, 5.0), (5.0, 0.0)]);
    /* Column 2's first block (beside, not below, column 1's) at the page
    top: only up to the content top. */
    let claims = block_hit_claims(
        &[
            block((0.0, 200.0), (0.0, 300.0), (0.0, 12.0)),
            block((236.0, 200.0), (0.0, 10.0), (24.0, 0.0)),
        ],
        None,
    );
    assert_eq!(claims, vec![(0.0, 12.0), (0.0, 0.0)]);
    /* A full-width block above two columns: column 2's first block moved
    there without its before gap, so it claims none of the gap the
    full-width block's space-after already owns. */
    let claims = block_hit_claims(
        &[
            block((0.0, 436.0), (0.0, 10.0), (0.0, 6.0)),
            block((0.0, 200.0), (22.0, 10.0), (6.0, 0.0)),
            block((236.0, 200.0), (16.0, 10.0), (6.0, 0.0)),
        ],
        None,
    );
    assert_eq!(claims[0], (0.0, 6.0));
    assert_eq!(claims[1], (6.0, 0.0));
    assert_eq!(claims[2], (0.0, 0.0));
    /* A cell's blocks fill it: the first owns the space above, the last
    the space below. */
    let claims = block_hit_claims(
        &[
            block((0.0, 200.0), (2.0, 10.0), (0.0, 0.0)),
            block((0.0, 200.0), (12.0, 10.0), (0.0, 0.0)),
        ],
        Some((-4.0, 40.0)),
    );
    assert_eq!(claims, vec![(6.0, 0.0), (0.0, 18.0)]);
}
