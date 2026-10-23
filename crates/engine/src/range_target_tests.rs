//! Issue #509 - the range walker behind every range formatting command
//! (`DocumentTree::range_targets` / `cell_rect_targets`) and the batched
//! paragraph mutation it feeds.

use crate::{
    Block, BlockPath, DocumentTree, LogicalPos, PathStep, SpanStyle, VMergeRole, default_table_cell,
};

fn cell(r: u32, c: u32, inner: u32) -> BlockPath {
    BlockPath {
        steps: vec![
            PathStep::Block(1),
            PathStep::Cell { row: r, col: c },
            PathStep::Block(inner),
        ],
    }
}

fn at(path: BlockPath, offset: u32) -> LogicalPos {
    LogicalPos::new(path, offset)
}

/// `[Paragraph "head", Table rows x cols, Paragraph "tail"]`, every cell
/// holding "<row><col>".
fn doc(rows: u32, cols: u32) -> DocumentTree {
    let mut d = DocumentTree::from_text("head").insert_table(BlockPath::top(1), rows, cols);
    for r in 0..rows {
        for c in 0..cols {
            d = d.insert_text(at(cell(r, c, 0), 0), &format!("{r}{c}"));
        }
    }
    d.insert_text(at(BlockPath::top(2), 0), "tail")
}

fn paths(targets: &[crate::RangeTarget]) -> Vec<BlockPath> {
    targets.iter().map(|t| t.path.clone()).collect()
}

#[test]
fn a_range_across_cells_reaches_each_paragraph_in_order() {
    let d = doc(2, 2);
    let t = d.range_targets(&at(cell(0, 1, 0), 1), &at(cell(1, 1, 0), 1));
    assert_eq!(
        paths(&t),
        vec![cell(0, 1, 0), cell(1, 0, 0), cell(1, 1, 0)],
        "document order, row by row"
    );
    assert_eq!((t[0].lo, t[0].hi), (1, 2), "first: from the start offset");
    assert_eq!((t[1].lo, t[1].hi), (0, 2), "middle: whole");
    assert_eq!((t[2].lo, t[2].hi), (0, 1), "last: up to the end offset");
}

#[test]
fn the_ends_may_come_in_either_order() {
    let d = doc(2, 2);
    let a = d.range_targets(&at(cell(0, 1, 0), 0), &at(cell(1, 0, 0), 2));
    let b = d.range_targets(&at(cell(1, 0, 0), 2), &at(cell(0, 1, 0), 0));
    assert_eq!(a, b);
}

#[test]
fn a_range_from_the_body_into_a_cell_and_around_the_table() {
    let d = doc(2, 2);
    let t = d.range_targets(&at(BlockPath::top(0), 2), &at(cell(0, 1, 0), 1));
    assert_eq!(
        paths(&t),
        vec![BlockPath::top(0), cell(0, 0, 0), cell(0, 1, 0)]
    );
    assert_eq!((t[0].lo, t[0].hi), (2, 4));
    let all = d.range_targets(&at(BlockPath::top(0), 0), &at(BlockPath::top(2), 4));
    assert_eq!(all.len(), 6, "head, four cells, tail");
}

#[test]
fn a_range_inside_one_paragraph_is_one_target() {
    let d = doc(1, 1);
    let t = d.range_targets(&at(BlockPath::top(0), 1), &at(BlockPath::top(0), 3));
    assert_eq!(t.len(), 1);
    assert_eq!((t[0].lo, t[0].hi, t[0].mark), (1, 3, false));
}

#[test]
fn an_empty_paragraph_mark_is_covered_unless_it_ends_the_range() {
    let d = DocumentTree::from_text("a").insert_table(BlockPath::top(1), 1, 2);
    /* Cells are empty. A range from "a" through cell (0,0) into (0,1):
    (0,0) is a middle paragraph (mark covered), (0,1) ends the range at
    offset 0 (its mark is not selected). */
    let t = d.range_targets(&at(BlockPath::top(0), 0), &at(cell(0, 1, 0), 0));
    let marks: Vec<bool> = t.iter().map(|x| x.mark).collect();
    assert_eq!(marks, vec![false, true, false]);
    /* A collapsed caret in an empty paragraph covers nothing. */
    let caret = d.range_targets(&at(cell(0, 0, 0), 0), &at(cell(0, 0, 0), 0));
    assert_eq!(caret.len(), 1);
    assert!(!caret[0].mark && caret[0].lo == caret[0].hi);
}

#[test]
fn continuation_cells_are_skipped_and_nested_tables_are_entered() {
    let mut d = doc(2, 2);
    let Block::Table(table) = d.blocks.get_mut(1).expect("table") else {
        panic!("block 1 is the table");
    };
    table.rows[1].cells[0].props.v_merge = VMergeRole::Continue;
    let mut nested_holder = default_table_cell();
    let inner = DocumentTree::from_text("")
        .insert_table(BlockPath::top(0), 1, 1)
        .blocks
        .iter()
        .find_map(|b| b.as_table().cloned())
        .expect("nested");
    nested_holder.blocks.push(Block::Table(inner));
    table.rows[0].cells[0] = nested_holder;
    let t = d.range_targets(&at(BlockPath::top(0), 0), &at(BlockPath::top(2), 4));
    let got = paths(&t);
    assert!(
        !got.contains(&cell(1, 0, 0)),
        "a continuation cell's content is ignored"
    );
    let nested_para = BlockPath {
        steps: vec![
            PathStep::Block(1),
            PathStep::Cell { row: 0, col: 0 },
            PathStep::Block(1),
            PathStep::Cell { row: 0, col: 0 },
            PathStep::Block(0),
        ],
    };
    assert!(got.contains(&nested_para), "the nested table's paragraph");
    let pos = |p: &BlockPath| got.iter().position(|x| x == p).unwrap();
    assert!(
        pos(&cell(0, 0, 0)) < pos(&nested_para) && pos(&nested_para) < pos(&cell(0, 1, 0)),
        "nested paragraphs sit in document order between their neighbours"
    );
}

#[test]
fn wire_indices_are_clipped_not_iterated() {
    let d = doc(1, 1);
    let far = at(BlockPath::top(u32::MAX), 7);
    let t = d.range_targets(&at(BlockPath::top(0), 0), &far);
    assert_eq!(t.len(), 3, "head, the cell, tail - nothing for the billions of absent blocks");
    assert!(d.range_targets(&far, &far).is_empty());
}

#[test]
fn a_cell_rectangle_is_whole_paragraphs_with_marks_covered() {
    let d = doc(3, 3);
    let table = BlockPath::top(1);
    let t = d.cell_rect_targets(&table, (1, 2), (0, 1));
    assert_eq!(
        paths(&t),
        vec![cell(1, 0, 0), cell(1, 1, 0), cell(2, 0, 0), cell(2, 1, 0)]
    );
    assert!(t.iter().all(|x| x.lo == 0 && x.hi == 2));
    assert!(d.cell_rect_targets(&BlockPath::top(0), (0, 9), (0, 9)).is_empty());
}

#[test]
fn apply_style_reaches_every_paragraph_and_shares_the_rest() {
    let d = doc(2, 2);
    let bold = SpanStyle {
        bold: Some(true),
        ..Default::default()
    };
    let out = d.apply_style(at(cell(0, 0, 0), 0), at(cell(1, 1, 0), 2), bold.clone());
    for (r, c) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
        let p = out.paragraph_at_path(&cell(r, c, 0)).unwrap();
        assert_eq!(p.style_at(0).bold, Some(true), "({r},{c})");
    }
    assert_eq!(out.paragraph_at_path(&BlockPath::top(0)).unwrap().spans.len(), 0);
    /* An empty range changes nothing and dirties nothing. */
    let same = d.apply_style(at(cell(0, 0, 0), 1), at(cell(0, 0, 0), 1), bold);
    let before = d.paragraph_at_path(&cell(0, 0, 0)).unwrap();
    let after = same.paragraph_at_path(&cell(0, 0, 0)).unwrap();
    assert_eq!(before.dirty, after.dirty);
    assert_eq!(before.spans, after.spans);
}

#[test]
fn a_list_toggle_stamps_a_cell_marker() {
    let d = doc(1, 2);
    let out = d.toggle_list_on_range(
        at(cell(0, 1, 0), 0),
        at(cell(0, 1, 0), 0),
        crate::numbering::ListSynthesisKind::Bullet,
    );
    let p = out.paragraph_at_path(&cell(0, 1, 0)).unwrap();
    assert_eq!(p.resolved_marker.as_deref(), Some("\u{2022}"));
    assert!(out.paragraph_at_path(&cell(0, 0, 0)).unwrap().list_item.is_none());
}
