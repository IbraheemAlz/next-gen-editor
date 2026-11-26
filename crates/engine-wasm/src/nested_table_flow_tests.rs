//! Issues #563, #562 and #566 — authoring and walking NESTED tables.
//!
//! - #563: `InsertTable` with the caret in a table cell nests the new table
//!   in THAT cell (right after the caret's paragraph, in its own container,
//!   with a paragraph after the table inside the cell — the OOXML rule the
//!   body already follows), through the engine's one resolution
//!   (`DocumentTree::resolve_block_insert`). Before, the position was a
//!   top-level index only and the shell sent the caret's FIRST block step,
//!   so the table landed after the outer table.
//! - #562: Tab in the last cell of a nested table appends a row to THAT
//!   table — the innermost table handles Tab (`onlyoffice-cursor-memo.md`
//!   §7) — and lands in its first new cell. Before, `mutate_table`
//!   resolved the path's first step: the OUTER table grew and the caret,
//!   addressing a row that did not exist, was clamped out of the table.
//! - #566: Tab from a nested cell selects the next NESTED cell, Shift+Tab
//!   walks back, a quadruple click selects the nested cell (fixed by #551's
//!   `innermost_cell_path` / `cell_span`; pinned here on the issue's own
//!   document shape).

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

/// Apply `cmd`, which must succeed, then check the selection resolves in
/// its document (and, under `fuzz-native`, every fuzz invariant).
fn run(e: &mut Engine, cmd: Command) -> Event {
    let name = format!("{cmd:?}");
    let evt = apply(e, cmd);
    assert!(!matches!(evt, Event::Error { .. }), "{name}: {evt:?}");
    assert_in_document(e, &name);
    evt
}

fn assert_in_document(e: &Engine, what: &str) {
    let sel = e.selection.as_ref().expect("a selection");
    e.with_selection_doc(|doc| {
        assert_eq!(
            clamp_pos(doc, sel.anchor.clone()),
            sel.anchor,
            "{what}: anchor escaped"
        );
        assert_eq!(
            clamp_pos(doc, sel.caret.clone()),
            sel.caret,
            "{what}: caret escaped"
        );
    });
    #[cfg(feature = "fuzz-native")]
    e.check_invariants_for_fuzzing()
        .unwrap_or_else(|err| panic!("{what}: invariants: {err}"));
}

fn para(text: &str) -> engine::Block {
    engine::Block::Paragraph(engine::Paragraph {
        text: text.to_string(),
        ..Default::default()
    })
}

fn cell(blocks: Vec<engine::Block>) -> engine::TableCell {
    engine::TableCell {
        blocks,
        ..Default::default()
    }
}

fn table(rows: Vec<Vec<engine::TableCell>>, grid: Vec<i32>) -> engine::Table {
    engine::Table {
        grid,
        rows: rows
            .into_iter()
            .map(|cells| engine::TableRow {
                cells,
                ..Default::default()
            })
            .collect(),
        dirty: true,
        ..Default::default()
    }
}

fn doc_of(blocks: Vec<engine::Block>) -> DocumentTree {
    let mut doc = DocumentTree::new();
    doc.blocks.clear();
    for b in blocks {
        doc.blocks.push_back(b);
    }
    doc
}

/// `intro`, a 2 × 2 table `A | B` / `C | D` (3000 twips a column), `outro`.
fn simple_doc() -> DocumentTree {
    doc_of(vec![
        para("intro"),
        engine::Block::Table(table(
            vec![
                vec![cell(vec![para("A")]), cell(vec![para("B")])],
                vec![cell(vec![para("C")]), cell(vec![para("D")])],
            ],
            vec![3000, 3000],
        )),
        para("outro"),
    ])
}

/// The #562 shape: `intro`, a 2 × 2 table whose cell (0,1) holds a nested
/// `N1 | N2` table then `after`, `outro`.
fn nested_doc() -> DocumentTree {
    doc_of(vec![
        para("intro"),
        engine::Block::Table(table(
            vec![
                vec![
                    cell(vec![para("A")]),
                    cell(vec![
                        engine::Block::Table(table(
                            vec![vec![cell(vec![para("N1")]), cell(vec![para("N2")])]],
                            vec![1200, 1200],
                        )),
                        para("after"),
                    ]),
                ],
                vec![cell(vec![para("C")]), cell(vec![para("D")])],
            ],
            vec![3000, 3000],
        )),
        para("outro"),
    ])
}

/// The #566 document: `intro`, a 1 × 2 outer table whose cell (0,0) holds
/// `o1`, a nested 1 × 2 table (`i1`, `i2`) and an empty paragraph, and
/// whose cell (0,1) holds `o2`; then `outro`.
fn issue_566_doc() -> DocumentTree {
    doc_of(vec![
        para("intro"),
        engine::Block::Table(table(
            vec![vec![
                cell(vec![
                    para("o1"),
                    engine::Block::Table(table(
                        vec![vec![cell(vec![para("i1")]), cell(vec![para("i2")])]],
                        vec![1200, 1200],
                    )),
                    para(""),
                ]),
                cell(vec![para("o2")]),
            ]],
            vec![3000, 3000],
        )),
        para("outro"),
    ])
}

fn b(idx: u32) -> BridgePathStep {
    BridgePathStep::Block { idx }
}

fn c(row: u32, col: u32) -> BridgePathStep {
    BridgePathStep::Cell { row, col }
}

fn path(steps: Vec<BridgePathStep>) -> BridgeBlockPath {
    BridgeBlockPath { steps }
}

fn pos(steps: Vec<BridgePathStep>, offset: u32) -> BridgeLogicalPos {
    BridgeLogicalPos {
        path: path(steps),
        offset,
    }
}

fn selection(e: &Engine) -> (BridgeLogicalPos, BridgeLogicalPos) {
    let sel = e.selection.as_ref().expect("a selection");
    (sel.anchor.clone(), sel.caret.clone())
}

fn caret_at(e: &mut Engine, at: BridgeLogicalPos) {
    run(
        e,
        Command::SetSelection {
            range: BridgeLogicalRange {
                start: at.clone(),
                end: at.clone(),
            },
            caret: at.clone(),
        },
    );
    assert_eq!(selection(e).1, at, "the caret was accepted as given");
}

fn tab(e: &mut Engine, forward: bool) {
    let direction = if forward {
        MoveDirection::NextCell
    } else {
        MoveDirection::PrevCell
    };
    match run(
        e,
        Command::MoveCaret {
            direction,
            extend: false,
        },
    ) {
        Event::SelectionChanged { .. } => {}
        other => panic!("MoveCaret {direction:?}: {other:?}"),
    }
}

fn insert_table(e: &mut Engine, at: Vec<BridgePathStep>, rows: u32, cols: u32) -> Event {
    apply(
        e,
        Command::InsertTable {
            at: path(at),
            rows,
            cols,
        },
    )
}

fn type_text(e: &mut Engine, text: &str) {
    run(
        e,
        Command::InsertText {
            text: text.into(),
            at: None,
        },
    );
}

fn selected_plain(e: &mut Engine) -> String {
    match run(
        e,
        Command::GetSelectionAsClipboard {
            include_docx: Some(false),
        },
    ) {
        Event::ClipboardPayload { plain, .. } => plain,
        other => panic!("GetSelectionAsClipboard: {other:?}"),
    }
}

/// The block at `steps` of the live body.
fn block<'d>(doc: &'d DocumentTree, steps: Vec<BridgePathStep>) -> &'d engine::Block {
    doc.block_at(&bridge_to_engine_path(path(steps)))
        .expect("a block at that path")
}

fn table_at(doc: &DocumentTree, steps: Vec<BridgePathStep>) -> &engine::Table {
    block(doc, steps).as_table().expect("a table at that path")
}

/// A cell's blocks as `"text"` for a paragraph and `"<rows>x<cols>"` for a
/// table.
fn shape(blocks: &[engine::Block]) -> Vec<String> {
    blocks
        .iter()
        .map(|b| match b {
            engine::Block::Paragraph(p) => format!("{:?}", p.text),
            engine::Block::Table(t) => format!(
                "{}x{}",
                t.rows.len(),
                t.rows.first().map_or(0, |r| r.cells.len())
            ),
        })
        .collect()
}

fn body_shape(doc: &DocumentTree) -> Vec<String> {
    let blocks: Vec<engine::Block> = doc.blocks.iter().cloned().collect();
    shape(&blocks)
}

/// #563 — the caret in cell (0,1) (`B`, end of the text): the new table
/// lands in THAT cell right after `B`, a paragraph follows it inside the
/// cell, its grid fits the cell, the caret is in its first cell, the outer
/// table and the body are unchanged, one Undo removes it, and it survives a
/// `.docx` round-trip as a table nested in the cell.
#[test]
fn insert_table_with_a_cell_caret_nests_it_in_that_cell() {
    let mut e = tests::test_engine_with_doc(simple_doc());
    caret_at(&mut e, pos(vec![b(1), c(0, 1), b(0)], 1));
    let depth = e.undo.depth();

    let evt = insert_table(&mut e, vec![b(1), c(0, 1), b(1)], 2, 3);
    assert!(!matches!(evt, Event::Error { .. }), "{evt:?}");
    assert_in_document(&e, "InsertTable in a cell");
    assert_eq!(e.undo.depth(), depth + 1, "one undo step");

    let doc = e.undo.current().clone();
    assert_eq!(body_shape(&doc), vec!["\"intro\"", "2x2", "\"outro\""]);
    let outer = table_at(&doc, vec![b(1)]);
    assert_eq!(
        shape(&outer.rows[0].cells[1].blocks),
        vec!["\"B\"", "2x3", "\"\""],
        "nested after the caret's paragraph, an escape paragraph after it"
    );
    for (r, row) in outer.rows.iter().enumerate() {
        for (col, cl) in row.cells.iter().enumerate() {
            if (r, col) != (0, 1) {
                assert_eq!(cl.blocks.len(), 1, "cell ({r},{col}) untouched");
            }
        }
    }
    /* The grid divides the cell's content width (3000 twips less Word's
    stock 108 + 108 margins), never the A4 page width. */
    let nested = table_at(&doc, vec![b(1), c(0, 1), b(1)]);
    assert_eq!(nested.grid, vec![928, 928, 928]);
    assert!(outer.dirty, "the outer table regenerates on save");

    let (anchor, caret) = selection(&e);
    let first_cell = pos(vec![b(1), c(0, 1), b(1), c(0, 0), b(0)], 0);
    assert_eq!((anchor, caret), (first_cell.clone(), first_cell));

    /* Typing goes into the nested table's first cell. */
    type_text(&mut e, "n");
    let doc = e.undo.current().clone();
    assert_eq!(
        block(&doc, vec![b(1), c(0, 1), b(1), c(0, 0), b(0)])
            .as_paragraph()
            .map(|p| p.text.as_str()),
        Some("n")
    );

    /* Round-trip: the saved package re-reads with the table nested in
    the cell, the paragraph after it kept. */
    let saved = format_docx::save_docx(&doc).expect("save");
    let back = format_docx::read_docx(&saved).expect("reread").document;
    assert_eq!(body_shape(&back), vec!["\"intro\"", "2x2", "\"outro\""]);
    let back_outer = table_at(&back, vec![b(1)]);
    assert_eq!(
        shape(&back_outer.rows[0].cells[1].blocks),
        vec!["\"B\"", "2x3", "\"\""]
    );
    let back_nested = table_at(&back, vec![b(1), c(0, 1), b(1)]);
    assert_eq!(
        shape(&back_nested.rows[0].cells[0].blocks),
        vec!["\"n\""],
        "the typed text is in the nested table's first cell"
    );

    /* Undo twice (the typing, then the insert): the original cell. */
    run(&mut e, Command::Undo);
    run(&mut e, Command::Undo);
    let doc = e.undo.current().clone();
    assert_eq!(
        shape(&table_at(&doc, vec![b(1)]).rows[0].cells[1].blocks),
        vec!["\"B\""]
    );
}

/// #563 — the escape paragraph is added only when the table would end its
/// cell (or precede another table): a caret in a cell paragraph that has a
/// paragraph after it splices the table between the two.
#[test]
fn insert_table_between_two_cell_paragraphs_adds_no_escape_paragraph() {
    let mut doc = simple_doc();
    let mut outer = doc.blocks[1].as_table().unwrap().clone();
    outer.rows[1].cells[0].blocks = vec![para("x"), para("y")];
    doc.blocks.set(1, engine::Block::Table(outer));
    let mut e = tests::test_engine_with_doc(doc);
    caret_at(&mut e, pos(vec![b(1), c(1, 0), b(0)], 0));
    let evt = insert_table(&mut e, vec![b(1), c(1, 0), b(1)], 1, 1);
    assert!(!matches!(evt, Event::Error { .. }), "{evt:?}");
    assert_in_document(&e, "InsertTable between paragraphs");
    let doc = e.undo.current().clone();
    assert_eq!(
        shape(&table_at(&doc, vec![b(1)]).rows[1].cells[0].blocks),
        vec!["\"x\"", "1x1", "\"y\""]
    );
    assert_eq!(
        selection(&e).1,
        pos(vec![b(1), c(1, 0), b(1), c(0, 0), b(0)], 0)
    );
}

/// #563 — a caret already inside a NESTED table nests one level deeper:
/// the position resolves at any depth.
#[test]
fn insert_table_inside_a_nested_cell_nests_again() {
    let mut e = tests::test_engine_with_doc(nested_doc());
    caret_at(&mut e, pos(vec![b(1), c(0, 1), b(0), c(0, 1), b(0)], 2));
    let evt = insert_table(&mut e, vec![b(1), c(0, 1), b(0), c(0, 1), b(1)], 1, 2);
    assert!(!matches!(evt, Event::Error { .. }), "{evt:?}");
    assert_in_document(&e, "InsertTable two levels deep");
    let doc = e.undo.current().clone();
    let level2 = table_at(&doc, vec![b(1), c(0, 1), b(0)]);
    assert_eq!(
        shape(&level2.rows[0].cells[1].blocks),
        vec!["\"N2\"", "1x2", "\"\""]
    );
    assert_eq!(
        shape(&level2.rows[0].cells[0].blocks),
        vec!["\"N1\""],
        "the sibling nested cell is untouched"
    );
    assert!(
        table_at(&doc, vec![b(1)]).dirty && level2.dirty,
        "every table around the edit regenerates"
    );
    assert_eq!(
        selection(&e).1,
        pos(vec![b(1), c(0, 1), b(0), c(0, 1), b(1), c(0, 0), b(0)], 0)
    );
}

/// #563 — a position that is no block position (a cell that does not
/// exist, a path ending in a cell step, a "cell" of a paragraph) is a typed
/// refusal that changes nothing; it never falls back to a top-level index.
#[test]
fn insert_table_at_no_block_position_is_refused() {
    let mut e = tests::test_engine_with_doc(simple_doc());
    caret_at(&mut e, pos(vec![b(1), c(0, 0), b(0)], 0));
    let before = e.undo.current().clone();
    let depth = e.undo.depth();
    let sel = selection(&e);
    for at in [
        vec![b(1), c(5, 5), b(0)],
        vec![b(1), c(0, 0)],
        vec![b(0), c(0, 0), b(0)],
        vec![b(9), c(0, 0), b(0)],
    ] {
        match insert_table(&mut e, at.clone(), 2, 2) {
            Event::Error { kind, .. } => {
                assert_eq!(kind, Some(bridge::ErrorKind::NoSuchTarget), "{at:?}");
            }
            other => panic!("{at:?}: expected a refusal, got {other:?}"),
        }
        assert_eq!(e.undo.depth(), depth, "{at:?}: nothing pushed");
        assert_eq!(selection(&e), sel, "{at:?}: selection kept");
        assert_eq!(body_shape(e.undo.current()), body_shape(&before));
    }
}

/// #563 — review mode: the nested table is a tracked insertion (every row
/// an inserted row) at the same position as the untracked insert.
#[test]
fn tracked_insert_table_in_a_cell_records_inserted_rows() {
    let mut e = tests::test_engine_with_doc(simple_doc());
    run(&mut e, Command::ToggleTrackChanges { enabled: true });
    caret_at(&mut e, pos(vec![b(1), c(1, 1), b(0)], 1));
    let evt = insert_table(&mut e, vec![b(1), c(1, 1), b(1)], 2, 2);
    assert!(!matches!(evt, Event::Error { .. }), "{evt:?}");
    assert_in_document(&e, "tracked InsertTable in a cell");
    let doc = e.undo.current().clone();
    assert_eq!(body_shape(&doc), vec!["\"intro\"", "2x2", "\"outro\""]);
    assert_eq!(
        shape(&table_at(&doc, vec![b(1)]).rows[1].cells[1].blocks),
        vec!["\"D\"", "2x2", "\"\""]
    );
    let nested = table_at(&doc, vec![b(1), c(1, 1), b(1)]);
    for (r, row) in nested.rows.iter().enumerate() {
        assert!(
            row.props
                .revisions
                .iter()
                .any(|rev| rev.kind == engine::RevisionKind::Insert),
            "nested row {r} is an inserted row"
        );
    }
    assert!(
        table_at(&doc, vec![b(1)])
            .rows
            .iter()
            .all(|row| row.props.revisions.is_empty()),
        "the outer rows are not marked"
    );
    assert_eq!(
        selection(&e).1,
        pos(vec![b(1), c(1, 1), b(1), c(0, 0), b(0)], 0)
    );
}

/// #562 — Tab in the LAST cell of a nested table appends a row to that
/// nested table (never the outer one) and lands, collapsed, in its first
/// new cell; typing goes there; one Undo removes the row.
#[test]
fn tab_in_the_last_nested_cell_appends_a_row_to_the_nested_table() {
    let mut e = tests::test_engine_with_doc(nested_doc());
    caret_at(&mut e, pos(vec![b(1), c(0, 1), b(0), c(0, 1), b(0)], 2));
    let depth = e.undo.depth();

    tab(&mut e, true);
    let doc = e.undo.current().clone();
    assert_eq!(table_at(&doc, vec![b(1)]).rows.len(), 2, "outer unchanged");
    let nested = table_at(&doc, vec![b(1), c(0, 1), b(0)]);
    assert_eq!(nested.rows.len(), 2, "the nested table grew");
    assert_eq!(nested.rows[1].cells.len(), 2);
    assert_eq!(body_shape(&doc), vec!["\"intro\"", "2x2", "\"outro\""]);
    assert_eq!(e.undo.depth(), depth + 1);
    let landed = pos(vec![b(1), c(0, 1), b(0), c(1, 0), b(0)], 0);
    assert_eq!(selection(&e), (landed.clone(), landed));

    type_text(&mut e, "x");
    let doc = e.undo.current().clone();
    assert_eq!(
        shape(&table_at(&doc, vec![b(1), c(0, 1), b(0)]).rows[1].cells[0].blocks),
        vec!["\"x\""]
    );
    assert_eq!(
        doc.blocks[2].as_paragraph().map(|p| p.text.as_str()),
        Some("outro"),
        "nothing typed after the table"
    );

    run(&mut e, Command::Undo);
    run(&mut e, Command::Undo);
    let doc = e.undo.current().clone();
    assert_eq!(table_at(&doc, vec![b(1), c(0, 1), b(0)]).rows.len(), 1);
    assert_eq!(table_at(&doc, vec![b(1)]).rows.len(), 2);
}

/// #562 — review mode is not part of the Tab path's contract, but the
/// tracked row helpers resolve the nested table too: an inserted row is
/// stamped on the nested table, a deleted row marked there.
#[test]
fn tracked_row_helpers_address_the_nested_table() {
    let doc = nested_doc();
    let nested = bridge_to_engine_path(path(vec![b(1), c(0, 1), b(0)]));
    let grown = doc.tracked_insert_row(nested.clone(), 1, "me", "2026-11-26T00:00:00Z");
    let t = grown.table_at_path(&nested).expect("nested table");
    assert_eq!(t.rows.len(), 2);
    assert!(
        t.rows[1]
            .props
            .revisions
            .iter()
            .any(|r| r.kind == engine::RevisionKind::Insert)
    );
    assert_eq!(
        grown.blocks[1].as_table().map(|t| t.rows.len()),
        Some(2),
        "outer rows unchanged"
    );
    let marked = doc.tracked_delete_row(nested.clone(), 0, "me", "2026-11-26T00:00:00Z");
    let t = marked.table_at_path(&nested).expect("nested table");
    assert!(
        t.rows[0]
            .props
            .revisions
            .iter()
            .any(|r| r.kind == engine::RevisionKind::Delete)
    );
    assert!(
        marked.blocks[1]
            .as_table()
            .is_some_and(|o| o.rows.iter().all(|r| r.props.revisions.is_empty()))
    );
}

/// #562 — a table at its growth cap does not grow: Tab in its last cell
/// stays put (the step is decided before anything mutates).
#[test]
fn tab_in_the_last_cell_of_a_capped_table_stays() {
    let cols = engine::MAX_TABLE_COLS as usize;
    let rows = (engine::MAX_TABLE_CELLS / cols as u64) as usize;
    let full = table(
        (0..rows)
            .map(|_| (0..cols).map(|_| cell(vec![para("")])).collect())
            .collect(),
        vec![100; cols],
    );
    let doc = doc_of(vec![
        engine::Block::Table(table(
            vec![vec![cell(vec![engine::Block::Table(full), para("")])]],
            vec![6000],
        )),
        para(""),
    ]);
    let last = pos(
        vec![
            b(0),
            c(0, 0),
            b(0),
            c(rows as u32 - 1, cols as u32 - 1),
            b(0),
        ],
        0,
    );
    assert_eq!(
        cell_tab_step(&doc, &last, &last, true),
        CellTabStep::Stay,
        "{rows} x {cols} is at the cell cap"
    );
}

/// #566 — on the issue's own document: from `i1`, Tab selects exactly
/// `i2` (the next NESTED cell, not the outer cell around the nested
/// table), Shift+Tab selects `i1`, a quadruple click in `i2` selects `i2`;
/// then Tab in `i2` — the nested table's last cell — appends a row to the
/// nested table (#562) and the outer table keeps its one row.
#[test]
fn issue_566_tab_between_nested_cells_selects_the_nested_cell() {
    let mut e = tests::test_engine_with_doc(issue_566_doc());
    let i1 = vec![b(1), c(0, 0), b(1), c(0, 0), b(0)];
    let i2 = vec![b(1), c(0, 0), b(1), c(0, 1), b(0)];
    caret_at(&mut e, pos(i1.clone(), 0));

    tab(&mut e, true);
    assert_eq!(selection(&e), (pos(i2.clone(), 0), pos(i2.clone(), 2)));
    assert_eq!(selected_plain(&mut e), "i2");

    tab(&mut e, false);
    assert_eq!(selection(&e), (pos(i1.clone(), 0), pos(i1.clone(), 2)));
    assert_eq!(selected_plain(&mut e), "i1");

    /* Quadruple click inside `i2`. */
    let geom = e.document_geometry().expect("geometry");
    let line = geom
        .iter()
        .find(|l| l.path == path(i2.clone()))
        .expect("a line for i2");
    let at = BridgePoint {
        x: line.slots.iter().map(|s| s.x).sum::<f32>() / line.slots.len().max(1) as f32,
        y: line.y_top + line.height / 2.0,
    };
    run(&mut e, Command::SelectCellAt { at });
    assert_eq!(selection(&e), (pos(i2.clone(), 0), pos(i2.clone(), 2)));

    /* Tab in the nested table's last cell, with its content selected. */
    tab(&mut e, true);
    let doc = e.undo.current().clone();
    assert_eq!(
        table_at(&doc, vec![b(1)]).rows.len(),
        1,
        "outer still 1 row"
    );
    assert_eq!(table_at(&doc, vec![b(1), c(0, 0), b(1)]).rows.len(), 2);
    let landed = pos(vec![b(1), c(0, 0), b(1), c(1, 0), b(0)], 0);
    assert_eq!(selection(&e), (landed.clone(), landed));
}

/// #563 / #378 — Insert Table nests at the caret level by level up to the
/// depth layout still draws as grids (`MAX_TABLE_LAYOUT_DEPTH`); one level
/// more is a typed refusal that changes nothing.
#[test]
fn insert_table_nesting_stops_at_the_layout_depth() {
    let mut e = tests::test_engine_with_doc(doc_of(vec![para("top")]));
    caret_at(&mut e, pos(vec![b(0)], 0));
    /* The InsertTable position right after the caret's paragraph. */
    let after_caret = |e: &Engine| {
        let mut at = selection(e).1.path.steps;
        let Some(BridgePathStep::Block { idx }) = at.pop() else {
            panic!("the caret is in a paragraph");
        };
        at.push(b(idx + 1));
        at
    };
    for level in 1..=MAX_TABLE_LAYOUT_DEPTH {
        let at = after_caret(&e);
        let evt = insert_table(&mut e, at, 1, 1);
        assert!(
            !matches!(evt, Event::Error { .. }),
            "level {level}: {evt:?}"
        );
        assert_in_document(&e, &format!("level {level}"));
        let cells = selection(&e)
            .1
            .path
            .steps
            .iter()
            .filter(|s| matches!(s, BridgePathStep::Cell { .. }))
            .count() as u32;
        assert_eq!(cells, level, "the caret is in the level-{level} table");
    }
    let depth = e.undo.depth();
    let caret = selection(&e).1;
    let at = after_caret(&e);
    match insert_table(&mut e, at, 1, 1) {
        Event::Error { kind, .. } => assert_eq!(kind, Some(bridge::ErrorKind::OutOfRange)),
        other => panic!("one level past the cap: expected a refusal, got {other:?}"),
    }
    assert_eq!(e.undo.depth(), depth);
    assert_eq!(selection(&e).1, caret);
}
