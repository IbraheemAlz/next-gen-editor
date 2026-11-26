//! Issue #561 — deleting and copying a range whose ends sit in different
//! containers (`range_delete.rs`): body ↔ cell, across cells and rows,
//! whole rows, whole tables, and the #551 cell spans through a nested
//! table. Rules: `range_delete.rs` module docs.

use crate::{
    Block, BlockPath, DocumentTree, LogicalPos, Paragraph, PathStep, Table, TableCell, TableRow,
    VMergeRole,
};

fn para(text: &str) -> Block {
    Block::Paragraph(Paragraph {
        text: text.to_string(),
        ..Default::default()
    })
}

fn cell(blocks: Vec<Block>) -> TableCell {
    TableCell {
        blocks,
        ..Default::default()
    }
}

fn table(rows: Vec<Vec<TableCell>>) -> Block {
    let cols = rows.first().map_or(1, Vec::len);
    Block::Table(Table {
        grid: vec![2000; cols],
        rows: rows
            .into_iter()
            .map(|cells| TableRow {
                cells,
                ..Default::default()
            })
            .collect(),
        dirty: true,
        ..Default::default()
    })
}

/// A table of one-paragraph cells.
fn grid(texts: &[&[&str]]) -> Block {
    table(
        texts
            .iter()
            .map(|row| row.iter().map(|t| cell(vec![para(t)])).collect())
            .collect(),
    )
}

fn doc_of(blocks: Vec<Block>) -> DocumentTree {
    let mut d = DocumentTree::new();
    d.blocks.clear();
    for b in blocks {
        d.blocks.push_back(b);
    }
    d
}

/// `intro`, a 2x2 `A B / C D` table, `outro`.
fn body_doc() -> DocumentTree {
    doc_of(vec![
        para("intro"),
        grid(&[&["A", "B"], &["C", "D"]]),
        para("outro"),
    ])
}

fn path(steps: &[PathStep]) -> BlockPath {
    BlockPath {
        steps: steps.to_vec(),
    }
}

fn b(i: u32) -> PathStep {
    PathStep::Block(i)
}

fn c(row: u32, col: u32) -> PathStep {
    PathStep::Cell { row, col }
}

fn at(steps: &[PathStep], offset: u32) -> LogicalPos {
    LogicalPos::new(path(steps), offset)
}

/// Every block of a container, as text: a paragraph's text, a table's
/// rows of cells of those (recursively).
#[derive(Debug, PartialEq, Eq)]
enum Shape {
    P(String),
    T(Vec<Vec<Vec<Shape>>>),
}

fn p(text: &str) -> Shape {
    Shape::P(text.to_string())
}

fn shape_of(blocks: &[Block]) -> Vec<Shape> {
    blocks
        .iter()
        .map(|b| match b {
            Block::Paragraph(p) => Shape::P(p.text.clone()),
            Block::Table(t) => Shape::T(
                t.rows
                    .iter()
                    .map(|r| r.cells.iter().map(|c| shape_of(&c.blocks)).collect())
                    .collect(),
            ),
        })
        .collect()
}

fn shape(doc: &DocumentTree) -> Vec<Shape> {
    let blocks: Vec<Block> = doc.blocks.iter().cloned().collect();
    shape_of(&blocks)
}

/// `T` of one-paragraph cells.
fn t(rows: &[&[&str]]) -> Shape {
    Shape::T(
        rows.iter()
            .map(|r| r.iter().map(|x| vec![p(x)]).collect())
            .collect(),
    )
}

#[test]
fn body_into_a_cell_keeps_the_head_and_removes_the_whole_rows_it_covers() {
    let d = body_doc();
    /* `in|tro` .. `C|`: the start paragraph keeps its head and is NOT
    merged into the cell (memo §7); row 0 is covered whole and goes (memo
    §6); `C`'s covered head goes, `D` is not covered. */
    let (out, caret) = d.delete_range_at(at(&[b(0)], 2), at(&[b(1), c(1, 0), b(0)], 1));
    assert_eq!(shape(&out), vec![p("in"), t(&[&["", "D"]]), p("outro")]);
    assert_eq!(caret, at(&[b(0)], 2));
    /* `delete_range` is the same edit. */
    assert_eq!(
        shape(&d.delete_range(at(&[b(0)], 2), at(&[b(1), c(1, 0), b(0)], 1))),
        shape(&out)
    );
}

#[test]
fn a_cell_into_the_body_keeps_the_tail_and_removes_the_rows_after_the_start() {
    let d = body_doc();
    /* `A|` .. `out|ro`: row 1 is covered whole and goes; `B` is covered
    whole but its row is not (A's head stays): it is emptied, the cell
    stays. */
    let (out, caret) = d.delete_range_at(at(&[b(1), c(0, 0), b(0)], 1), at(&[b(2)], 3));
    assert_eq!(shape(&out), vec![p("intro"), t(&[&["A", ""]]), p("ro")]);
    assert_eq!(caret, at(&[b(1), c(0, 0), b(0)], 1));
}

#[test]
fn a_range_from_a_tables_first_position_removes_the_table() {
    let d = body_doc();
    let (out, caret) = d.delete_range_at(at(&[b(1), c(0, 0), b(0)], 0), at(&[b(2)], 2));
    assert_eq!(shape(&out), vec![p("intro"), p("tro")]);
    assert_eq!(caret, at(&[b(1)], 0), "the tail took the table's place");
    /* From the body to the table's last position: removed too. */
    let (out, caret) = d.delete_range_at(at(&[b(0)], 5), at(&[b(1), c(1, 1), b(0)], 1));
    assert_eq!(shape(&out), vec![p("intro"), p("outro")]);
    assert_eq!(caret, at(&[b(0)], 5));
}

#[test]
fn a_range_across_rows_inside_one_table_empties_cells_and_keeps_them() {
    let d = body_doc();
    let (out, caret) =
        d.delete_range_at(at(&[b(1), c(0, 0), b(0)], 1), at(&[b(1), c(1, 1), b(0)], 0));
    assert_eq!(
        shape(&out),
        vec![p("intro"), t(&[&["A", ""], &["", "D"]]), p("outro")]
    );
    assert_eq!(caret, at(&[b(1), c(0, 0), b(0)], 1));
}

#[test]
fn a_range_over_one_whole_row_removes_that_row() {
    let d = body_doc();
    let (out, caret) =
        d.delete_range_at(at(&[b(1), c(0, 0), b(0)], 0), at(&[b(1), c(0, 1), b(0)], 1));
    assert_eq!(shape(&out), vec![p("intro"), t(&[&["C", "D"]]), p("outro")]);
    assert_eq!(
        caret,
        at(&[b(1), c(0, 0), b(0)], 0),
        "the row that took its place"
    );
}

#[test]
fn a_range_that_is_one_whole_table_leaves_an_empty_paragraph() {
    let d = body_doc();
    let (out, caret) =
        d.delete_range_at(at(&[b(1), c(0, 0), b(0)], 0), at(&[b(1), c(1, 1), b(0)], 1));
    assert_eq!(shape(&out), vec![p("intro"), p(""), p("outro")]);
    assert_eq!(caret, at(&[b(1)], 0));
}

/// The #551 document: `intro`, then a 2×2 table — (0,0) `A`; (0,1) a
/// nested `N1 | N2` table then `after`; (1,0) `before` then a nested
/// `M1 | M2` table; (1,1) only a nested `O1 | O2` table — then `outro`.
fn nested_doc() -> DocumentTree {
    doc_of(vec![
        para("intro"),
        table(vec![
            vec![
                cell(vec![para("A")]),
                cell(vec![grid(&[&["N1", "N2"]]), para("after")]),
            ],
            vec![
                cell(vec![para("before"), grid(&[&["M1", "M2"]])]),
                cell(vec![grid(&[&["O1", "O2"]])]),
            ],
        ]),
        para("outro"),
    ])
}

/// The blocks of cell `(row, col)` of the outer table.
fn outer_cell(doc: &DocumentTree, row: usize, col: usize) -> Vec<Shape> {
    let Some(Block::Table(t)) = doc.blocks.get(1) else {
        panic!("the outer table survives");
    };
    shape_of(&t.rows[row].cells[col].blocks)
}

#[test]
fn deleting_a_cells_whole_span_through_a_nested_table_empties_the_cell() {
    let d = nested_doc();
    let cases = [
        /* Starts with a nested table. */
        (
            (0, 1),
            at(&[b(1), c(0, 1), b(0), c(0, 0), b(0)], 0),
            at(&[b(1), c(0, 1), b(1)], 5),
        ),
        /* Ends with one. */
        (
            (1, 0),
            at(&[b(1), c(1, 0), b(0)], 0),
            at(&[b(1), c(1, 0), b(1), c(0, 1), b(0)], 2),
        ),
        /* Is only one. */
        (
            (1, 1),
            at(&[b(1), c(1, 1), b(0), c(0, 0), b(0)], 0),
            at(&[b(1), c(1, 1), b(0), c(0, 1), b(0)], 2),
        ),
    ];
    for ((row, col), from, to) in cases {
        let (out, caret) = d.delete_range_at(from, to);
        assert_eq!(
            outer_cell(&out, row, col),
            vec![p("")],
            "cell ({row},{col})"
        );
        assert_eq!(
            caret,
            at(&[b(1), c(row as u32, col as u32), b(0)], 0),
            "cell ({row},{col}): the caret is the cell's one paragraph"
        );
        /* Typing over it leaves exactly the typed text in the cell. */
        let typed = out.insert_text(caret, "Z");
        assert_eq!(outer_cell(&typed, row, col), vec![p("Z")]);
        /* Nothing else moved. */
        for (r, cl) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
            if (r, cl) != (row, col) {
                assert_eq!(outer_cell(&out, r, cl), outer_cell(&d, r, cl));
            }
        }
        assert_eq!(shape(&out)[0], p("intro"));
        assert_eq!(shape(&out)[2], p("outro"));
    }
}

#[test]
fn a_partly_covered_nested_table_keeps_its_cells() {
    let d = nested_doc();
    /* `N|1` .. `af|ter`: the nested table is not covered whole. */
    let (out, caret) = d.delete_range_at(
        at(&[b(1), c(0, 1), b(0), c(0, 0), b(0)], 1),
        at(&[b(1), c(0, 1), b(1)], 2),
    );
    assert_eq!(
        outer_cell(&out, 0, 1),
        vec![t(&[&["N", ""]]), p("ter")],
        "no paragraph merges across the nested cell boundary"
    );
    assert_eq!(caret, at(&[b(1), c(0, 1), b(0), c(0, 0), b(0)], 1));
}

#[test]
fn a_comment_in_a_removed_row_follows_the_row_that_takes_its_place() {
    let d = body_doc()
        .insert_comment(
            at(&[b(1), c(1, 1), b(0)], 0),
            at(&[b(1), c(1, 1), b(0)], 1),
            "on D".into(),
            "A".into(),
            "2026-01-01T00:00:00Z".into(),
        )
        .0;
    let out = d.delete_range(at(&[b(0)], 2), at(&[b(1), c(1, 0), b(0)], 1));
    let r = &out.comment_ranges[0];
    assert_eq!(r.start, at(&[b(1), c(0, 1), b(0)], 0), "row 1 moved up");
    assert_eq!(out.text_range(r.start.clone(), r.end.clone()), "D");
}

#[test]
fn removing_the_row_that_starts_a_vertical_merge_restarts_it_below() {
    let mut restart = cell(vec![para("top")]);
    restart.props.v_merge = VMergeRole::Restart;
    let mut cont = cell(vec![para("")]);
    cont.props.v_merge = VMergeRole::Continue;
    let d = doc_of(vec![
        para("intro"),
        table(vec![
            vec![restart, cell(vec![para("x")])],
            vec![cont, cell(vec![para("y")])],
        ]),
        para("outro"),
    ]);
    /* Body into (1,1): row 0 goes whole. */
    let out = d.delete_range(at(&[b(0)], 5), at(&[b(1), c(1, 1), b(0)], 0));
    let Some(Block::Table(tb)) = out.blocks.get(1) else {
        panic!("the table survives");
    };
    assert_eq!(tb.rows.len(), 1);
    assert_eq!(tb.rows[0].cells[0].props.v_merge, VMergeRole::Restart);
}

#[test]
fn a_cell_rectangle_is_cleared_cell_by_cell() {
    let d = doc_of(vec![
        para("intro"),
        grid(&[&["a", "b", "c"], &["d", "e", "f"], &["g", "h", "i"]]),
        para("outro"),
    ]);
    let (out, caret) = d
        .clear_cell_rect(&path(&[b(1)]), (0, 1), (1, 2))
        .expect("a rectangle");
    assert_eq!(
        shape(&out),
        vec![
            p("intro"),
            t(&[&["a", "", ""], &["d", "", ""], &["g", "h", "i"]]),
            p("outro"),
        ]
    );
    assert_eq!(caret, at(&[b(1), c(0, 1), b(0)], 0));
    /* A cell with several paragraphs and a nested table empties to one. */
    let d = nested_doc();
    let (out, _) = d
        .clear_cell_rect(&path(&[b(1)]), (0, 1), (0, 1))
        .expect("a rectangle");
    assert_eq!(outer_cell(&out, 0, 1), vec![p("")]);
}

#[test]
fn plain_text_walks_every_covered_paragraph() {
    let d = body_doc();
    /* A body range over the whole table: cells by tabs, rows by newlines
    (UX_BEHAVIOR_SPEC §V.1) — it used to skip the table. */
    assert_eq!(
        d.text_range(at(&[b(0)], 2), at(&[b(2)], 3)),
        "tro\nA\tB\nC\tD\nout"
    );
    /* Body into a cell, and a cell into the body. */
    assert_eq!(
        d.text_range(at(&[b(0)], 2), at(&[b(1), c(1, 0), b(0)], 1)),
        "tro\nA\tB\nC"
    );
    assert_eq!(
        d.text_range(at(&[b(1), c(0, 1), b(0)], 0), at(&[b(2)], 3)),
        "B\nC\tD\nout"
    );
    /* The #551 cell spans: the whole cell, nested table included. */
    let n = nested_doc();
    assert_eq!(
        n.text_range(
            at(&[b(1), c(0, 1), b(0), c(0, 0), b(0)], 0),
            at(&[b(1), c(0, 1), b(1)], 5)
        ),
        "N1\tN2\nafter"
    );
    assert_eq!(
        n.text_range(
            at(&[b(1), c(1, 0), b(0)], 0),
            at(&[b(1), c(1, 0), b(1), c(0, 1), b(0)], 2)
        ),
        "before\nM1\tM2"
    );
    assert_eq!(
        n.text_range(
            at(&[b(1), c(1, 1), b(0), c(0, 0), b(0)], 0),
            at(&[b(1), c(1, 1), b(0), c(0, 1), b(0)], 2)
        ),
        "O1\tO2"
    );
    /* Inside one paragraph and across paragraphs of one container: as
    before. */
    assert_eq!(d.text_range(at(&[b(0)], 1), at(&[b(0)], 4)), "ntr");
    let two = doc_of(vec![para("one"), para("two")]);
    assert_eq!(two.text_range(at(&[b(0)], 1), at(&[b(1)], 2)), "ne\ntw");
}

#[test]
fn a_crossed_cells_own_line_breaks_become_spaces() {
    let d = doc_of(vec![
        para("intro"),
        table(vec![vec![
            cell(vec![para("x"), para("y")]),
            cell(vec![para("z")]),
        ]]),
        para(""),
    ]);
    assert_eq!(
        d.text_range(at(&[b(0)], 0), at(&[b(2)], 0)),
        "intro\nx y\tz\n",
        "one row stays one line, like `to_plain_text`"
    );
    /* A range inside ONE cell keeps its paragraph breaks. */
    assert_eq!(
        d.text_range(at(&[b(1), c(0, 0), b(0)], 0), at(&[b(1), c(0, 0), b(1)], 1)),
        "x\ny"
    );
}

#[test]
fn a_cell_rectangles_plain_text_is_its_rows() {
    let d = doc_of(vec![
        para("intro"),
        grid(&[&["a", "b", "c"], &["d", "e", "f"]]),
        para("outro"),
    ]);
    let targets = d.cell_rect_targets(&path(&[b(1)]), (0, 1), (1, 2));
    assert_eq!(d.targets_plain_text(&targets), "b\tc\ne\tf");
}

#[test]
fn ends_that_address_no_paragraph_delete_nothing() {
    let d = body_doc();
    let (out, _) = d.delete_range_at(at(&[b(1)], 0), at(&[b(1), c(1, 1), b(0)], 1));
    assert_eq!(shape(&out), shape(&d));
    let (out, _) = d.delete_range_at(at(&[b(0)], 1), at(&[b(9), c(0, 0), b(0)], 1));
    assert_eq!(shape(&out), shape(&d));
    assert_eq!(d.text_range(at(&[b(1)], 0), at(&[b(2)], 1)), "");
}

#[test]
fn a_comment_in_a_cleared_cell_collapses_onto_its_start() {
    let d = doc_of(vec![
        para("intro"),
        grid(&[&["a", "bbb"], &["c", "d"]]),
        para("outro"),
    ])
    .insert_comment(
        at(&[b(1), c(0, 1), b(0)], 1),
        at(&[b(1), c(0, 1), b(0)], 3),
        "on b".into(),
        "A".into(),
        "2026-01-01T00:00:00Z".into(),
    )
    .0;
    let (out, _) = d
        .clear_cell_rect(&path(&[b(1)]), (0, 1), (1, 1))
        .expect("a rectangle");
    assert_eq!(
        shape(&out),
        vec![p("intro"), t(&[&["a", ""], &["c", ""]]), p("outro")]
    );
    let r = &out.comment_ranges[0];
    assert_eq!(r.start, at(&[b(1), c(0, 1), b(0)], 0));
    assert_eq!(r.end, r.start);
}
