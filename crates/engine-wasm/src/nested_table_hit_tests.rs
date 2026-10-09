//! Issue #377 — the pixel → logical map reaches tables nested inside
//! table cells.
//!
//! `collect_table_line_geom` used to map only a top-level table's own
//! cell paragraphs, so a click / drag / double-click inside a nested
//! table resolved to the nearest OUTER-cell line. It now recurses into
//! nested `TableBox`es (origins accumulated like the renderer's walk,
//! paths extended by `[Cell{r,c}, Block(b)]` per level), bounded by the
//! layout nesting cap.

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

fn cell(blocks: Vec<engine::Block>) -> engine::TableCell {
    engine::TableCell {
        blocks,
        ..Default::default()
    }
}

/// One table level: cell (0,0) holds `heading` and then `inner` (when
/// given), cell (0,1) a prose paragraph, so every level has a sibling
/// cell sharing its row band.
fn level(heading: &str, inner: Option<engine::Table>) -> engine::Table {
    let mut first = vec![engine::Block::Paragraph(para(heading))];
    if let Some(t) = inner {
        first.push(engine::Block::Table(t));
    }
    engine::Table {
        grid: vec![3600, 1800],
        rows: vec![engine::TableRow {
            cells: vec![
                cell(first),
                cell(vec![engine::Block::Paragraph(para(&format!(
                    "{heading} sibling prose"
                )))]),
            ],
            ..Default::default()
        }],
        dirty: true,
        ..Default::default()
    }
}

/// `intro`, a 3-deep nested table (outer → middle → inner), `outro`.
fn three_deep_doc() -> DocumentTree {
    let inner = level("inner cell text", None);
    let middle = level("middle cell", Some(inner));
    let outer = level("outer cell", Some(middle));
    let mut doc = DocumentTree::new();
    doc.blocks.clear();
    doc.blocks
        .push_back(engine::Block::Paragraph(para("intro")));
    doc.blocks.push_back(engine::Block::Table(outer));
    doc.blocks
        .push_back(engine::Block::Paragraph(para("outro")));
    doc
}

fn cell_step() -> BridgePathStep {
    BridgePathStep::Cell { row: 0, col: 0 }
}

/// `[Block(1), Cell(0,0), Block(1), Cell(0,0), …, Block(0)]` — the first
/// paragraph of cell (0,0) at nesting `level` (0 = the outer table).
fn heading_path(level: usize) -> BridgeBlockPath {
    let mut steps = vec![BridgePathStep::Block { idx: 1 }];
    for _ in 0..level {
        steps.push(cell_step());
        steps.push(BridgePathStep::Block { idx: 1 });
    }
    steps.push(cell_step());
    steps.push(BridgePathStep::Block { idx: 0 });
    BridgeBlockPath { steps }
}

fn inner_path() -> BridgeBlockPath {
    heading_path(2)
}

fn line_for<'g>(geom: &'g [LineGeom], path: &BridgeBlockPath) -> &'g LineGeom {
    geom.iter()
        .find(|l| &l.path == path)
        .unwrap_or_else(|| panic!("no geometry line for {path:?}"))
}

/// A point inside the line's text: halfway along its caret slots, at
/// half height.
fn inside(line: &LineGeom) -> BridgePoint {
    let (lo, hi) = line
        .slots
        .iter()
        .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), s| {
            (lo.min(s.x), hi.max(s.x))
        });
    BridgePoint {
        x: (lo + hi) / 2.0,
        y: line.y_top + line.height / 2.0,
    }
}

fn engine_with(doc: DocumentTree) -> Engine {
    tests::test_engine_with_doc(doc)
}

/// Absolute device-px x of the first painted glyph of `text`'s line in
/// the render walk (`render::scene`: page margin + table origin + row +
/// cell origin + cell padding + paragraph + line origin, recursively).
fn painted_line_x(pages: &[PageBox], scale: f32, texts: &[&str], want: &str) -> f32 {
    fn walk(
        blocks: &[LayoutBlock],
        base_x: f32,
        texts: &[&str],
        want: &str,
        out: &mut Option<f32>,
    ) {
        for b in blocks {
            match b {
                LayoutBlock::Paragraph(p) => {
                    if texts.get(p.source_paragraph_id as usize) == Some(&want) {
                        let line = p.lines.first().expect("a line");
                        out.get_or_insert(base_x + p.origin.x + line.origin.x);
                    }
                }
                LayoutBlock::Table(t) => {
                    let tx = base_x + t.origin.x;
                    for row in &t.rows {
                        for c in &row.cells {
                            let content_x = tx + row.origin.x + c.origin.x + c.padding_left;
                            walk(&c.content, content_x, texts, want, out);
                        }
                    }
                }
            }
        }
    }
    let _ = scale;
    let mut out = None;
    for page in pages {
        walk(&page.blocks, page.margins.left, texts, want, &mut out);
    }
    out.unwrap_or_else(|| panic!("{want:?} is painted"))
}

/// Issue #377 — the acceptance test: `HitTest` into a 3-deep nested
/// table returns the INNER paragraph's `BlockPath`, and every nesting
/// level's paragraph is mapped where it is painted.
#[test]
fn hit_test_into_a_three_deep_nested_table_returns_the_inner_paragraph() {
    let doc = three_deep_doc();
    let mut texts: Vec<&str> = Vec::new();
    for b in doc.blocks.iter() {
        walk_block_texts(b, &mut texts);
    }
    let mut e = engine_with(doc.clone());
    let geom = e.document_geometry().expect("geometry");
    let (pages, ..) = e.build_pages(e.scale(), false, None).expect("layout");

    for (level, text) in ["outer cell", "middle cell", "inner cell text"]
        .into_iter()
        .enumerate()
    {
        let path = heading_path(level);
        let line = line_for(&geom, &path);
        /* Mapped where the renderer paints it — the origins accumulate
        through every level, cell padding included. */
        let painted = painted_line_x(&pages, e.scale(), &texts, text);
        assert!(
            (line.start_x - painted).abs() < 1e-3,
            "{text}: geometry x {} vs painted x {painted}",
            line.start_x
        );
        let at = inside(line);
        match apply(&mut e, Command::HitTest { at }) {
            Event::HitResult { pos } => {
                assert_eq!(pos.path, path, "{text}: hit {at:?}");
                assert!(pos.offset > 0 && pos.offset < text.len() as u32, "{pos:?}");
            }
            other => panic!("HitTest: {other:?}"),
        }
    }

    /* The sibling prose cell of the inner table shares the inner row's
    band: x routes the click to it, not to the inner heading. */
    let mut sibling = inner_path();
    let n = sibling.steps.len();
    sibling.steps[n - 2] = BridgePathStep::Cell { row: 0, col: 1 };
    let line = line_for(&geom, &sibling);
    match apply(&mut e, Command::HitTest { at: inside(line) }) {
        Event::HitResult { pos } => assert_eq!(pos.path, sibling),
        other => panic!("HitTest: {other:?}"),
    }
}

/// Issue #377 — the interactive consumers ride the same map: a click
/// places the caret inside the nested cell (caret rect on the inner
/// line), typing lands in the inner paragraph, a double-click selects
/// an inner word, and a drag from the inner cell to the outer heading
/// extends the selection with rects in both cells.
#[test]
fn click_type_select_word_and_drag_inside_a_nested_cell() {
    let mut e = engine_with(three_deep_doc());
    let geom = e.document_geometry().expect("geometry");
    let inner = line_for(&geom, &inner_path());
    let at = inside(inner);

    match apply(&mut e, Command::PlaceCaretAtPoint { page: 0, at }) {
        Event::SelectionChanged { range, caret, .. } => {
            assert_eq!(range.start.path, inner_path());
            assert_eq!(range.start, range.end);
            assert_eq!(caret.y, inner.y_top, "the caret sits on the inner line");
            assert!(caret.x >= inner.hit_left && caret.x <= inner.hit_left + inner.hit_width);
        }
        other => panic!("PlaceCaretAtPoint: {other:?}"),
    }
    let _ = apply(
        &mut e,
        Command::InsertText {
            at: None,
            text: "Z".into(),
        },
    );
    let doc = e.undo.current();
    let inner_text = doc
        .paragraph_at_path(&bridge_to_engine_path(inner_path()))
        .expect("inner paragraph")
        .text
        .clone();
    assert_eq!(
        inner_text.len(),
        "inner cell text".len() + 1,
        "{inner_text:?}"
    );
    assert!(inner_text.contains('Z'), "{inner_text:?}");
    assert_eq!(
        doc.paragraph_at_path(&bridge_to_engine_path(heading_path(0)))
            .expect("outer heading")
            .text,
        "outer cell",
        "the outer cell is untouched"
    );

    /* Double-click inside "text" (the inner heading's last word). */
    let geom = e.document_geometry().expect("geometry");
    let inner = line_for(&geom, &inner_path());
    let word_byte = inner_text.rfind("text").expect("`text`") as u32 + 2;
    let word_x = inner
        .slots
        .iter()
        .find(|s| s.byte == word_byte)
        .expect("a slot inside `text`")
        .x;
    let word_at = BridgePoint {
        x: word_x,
        y: inner.y_top + inner.height / 2.0,
    };
    match apply(&mut e, Command::SelectWordAt { at: word_at }) {
        Event::SelectionChanged { range, rects, .. } => {
            assert_eq!(range.start.path, inner_path());
            assert_eq!(range.end.path, inner_path());
            let text = e
                .undo
                .current()
                .paragraph_at_path(&bridge_to_engine_path(inner_path()))
                .expect("inner")
                .text
                .clone();
            let word = &text[range.start.offset as usize..range.end.offset as usize];
            assert!(word.trim() == "text", "selected {word:?} of {text:?}");
            assert!(!rects.is_empty());
            for r in &rects {
                assert_eq!(r.y, inner.y_top, "the word rect is on the inner line");
            }
        }
        other => panic!("SelectWordAt: {other:?}"),
    }

    /* Drag: caret in the inner cell, extend to the outer heading. */
    let outer = line_for(&geom, &heading_path(0));
    let _ = apply(&mut e, Command::PlaceCaretAtPoint { page: 0, at });
    match apply(
        &mut e,
        Command::ExtendSelectionToPoint {
            page: 0,
            at: inside(outer),
        },
    ) {
        Event::SelectionChanged { range, rects, .. } => {
            let paths = [range.start.path.clone(), range.end.path.clone()];
            assert!(paths.contains(&inner_path()), "{range:?}");
            assert!(paths.contains(&heading_path(0)), "{range:?}");
            assert!(
                rects.iter().any(|r| r.y == outer.y_top),
                "a rect on the outer line"
            );
            assert!(
                rects.iter().any(|r| r.y == inner.y_top),
                "a rect on the inner line"
            );
        }
        other => panic!("ExtendSelectionToPoint: {other:?}"),
    }
}

/// Issue #377 — a selection set on the inner paragraph reports its caret
/// and highlight on the inner line (caret rects used to fall back to the
/// document's first line for a nested position).
#[test]
fn caret_and_selection_rects_of_a_nested_position_sit_on_its_line() {
    let mut e = engine_with(three_deep_doc());
    let geom = e.document_geometry().expect("geometry");
    let inner = line_for(&geom, &inner_path());
    let start = BridgeLogicalPos {
        path: inner_path(),
        offset: 0,
    };
    let end = BridgeLogicalPos {
        path: inner_path(),
        offset: "inner".len() as u32,
    };
    match apply(
        &mut e,
        Command::SetSelection {
            range: bridge::LogicalRange {
                start: start.clone(),
                end: end.clone(),
            },
            caret: end,
        },
    ) {
        Event::SelectionChanged { caret, rects, .. } => {
            assert_eq!(caret.y, inner.y_top);
            assert!(caret.x > inner.start_x, "caret after `inner`");
            assert_eq!(rects.len(), 1, "{rects:?}");
            assert_eq!(rects[0].y, inner.y_top);
            assert!((rects[0].x - inner.start_x).abs() < 1e-3);
            assert!(rects[0].w > 0.0);
        }
        other => panic!("SetSelection: {other:?}"),
    }
}

/// `depth`-level single-cell tower (`table_nested_200_deep.docx`'s
/// shape): every level's cell holds `level i` and the next level.
fn tower_doc(depth: usize) -> DocumentTree {
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
        .push_back(engine::Block::Table(inner.expect("depth >= 1")));
    doc
}

/// Issue #377 — the recursion stops at the layout nesting cap and never
/// emits a path that names the wrong paragraph: past the cap the deepest
/// grid level's cells hold flattened paragraphs (#318) that are not 1:1
/// with the model blocks, so that level is left unmapped. Every emitted
/// line resolves to a paragraph long enough for its byte range, the
/// mapped levels are exactly `0 .. MAX_TABLE_LAYOUT_DEPTH - 1`, and the
/// same holds for a tower exactly at the cap.
#[test]
fn nested_hit_map_is_bounded_and_sound_past_the_cap() {
    for depth in [MAX_TABLE_LAYOUT_DEPTH as usize, 40] {
        let doc = tower_doc(depth);
        let e = engine_with(doc.clone());
        let geom = e.document_geometry().expect("geometry");
        let mut mapped: Vec<String> = Vec::new();
        for line in &geom {
            let p = doc
                .paragraph_at_path(&bridge_to_engine_path(line.path.clone()))
                .unwrap_or_else(|| panic!("depth {depth}: {:?} names no paragraph", line.path));
            assert!(line.end_byte as usize <= p.text.len(), "{:?}", line.path);
            let cells = line
                .path
                .steps
                .iter()
                .filter(|s| matches!(s, BridgePathStep::Cell { .. }))
                .count();
            assert!(cells < MAX_TABLE_LAYOUT_DEPTH as usize, "{:?}", line.path);
            if cells > 0 {
                assert_eq!(p.text, format!("level {}", cells - 1), "{:?}", line.path);
                mapped.push(p.text.clone());
            }
        }
        let want: Vec<String> = (0..MAX_TABLE_LAYOUT_DEPTH - 1)
            .map(|i| format!("level {i}"))
            .collect();
        assert_eq!(mapped, want, "depth {depth}");
    }
}
