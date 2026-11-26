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
fn a_mark_is_covered_unless_its_paragraph_ends_the_range() {
    let d = DocumentTree::from_text("a").insert_table(BlockPath::top(1), 1, 2);
    /* Cells are empty. A range from "a" through cell (0,0) into (0,1):
    "a" and (0,0) are run past (issue #489: their marks are covered, the
    one with text too), (0,1) ends the range at offset 0 (its mark is not
    selected). */
    let t = d.range_targets(&at(BlockPath::top(0), 0), &at(cell(0, 1, 0), 0));
    let marks: Vec<bool> = t.iter().map(|x| x.mark).collect();
    assert_eq!(marks, vec![true, true, false]);
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
    assert_eq!(
        t.len(),
        3,
        "head, the cell, tail - nothing for the billions of absent blocks"
    );
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
    assert!(
        d.cell_rect_targets(&BlockPath::top(0), (0, 9), (0, 9))
            .is_empty()
    );
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
    assert_eq!(
        out.paragraph_at_path(&BlockPath::top(0))
            .unwrap()
            .spans
            .len(),
        0
    );
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
    assert!(
        out.paragraph_at_path(&cell(0, 0, 0))
            .unwrap()
            .list_item
            .is_none()
    );
}

/* ================================================================
Issue #489 — a selection that includes a paragraph MARK formats it:
Word's pilcrow is part of the selection once the selection runs past
the paragraph's last character (into the next paragraph), and Select
All / a triple-click paragraph selection / a cell selection include it
too. A selection that stops at the last character does not.
================================================================ */

fn sized(pt: f32) -> SpanStyle {
    SpanStyle {
        font_size: Some(pt),
        ..Default::default()
    }
}

/// `["first", "second", "third"]`, every mark unformatted.
fn three() -> DocumentTree {
    DocumentTree::from_text("first")
        .split_paragraph(at(BlockPath::top(0), 5))
        .insert_text(at(BlockPath::top(1), 0), "second")
        .split_paragraph(at(BlockPath::top(1), 6))
        .insert_text(at(BlockPath::top(2), 0), "third")
}

fn mark_size(d: &DocumentTree, block: u32) -> Option<f32> {
    d.paragraph_at_path(&BlockPath::top(block))
        .expect("paragraph")
        .mark_style
        .as_deref()
        .and_then(|m| m.font_size)
}

fn shape(t: &[crate::RangeTarget]) -> Vec<(u32, u32, bool)> {
    t.iter().map(|x| (x.lo, x.hi, x.mark)).collect()
}

#[test]
fn a_range_that_runs_past_a_paragraph_end_covers_its_mark() {
    let d = three();
    /* "first" whole, into "second": the selection crosses the first
    mark; "second" ends the range at offset 2 (its mark is not reached). */
    let t = d.range_targets(&at(BlockPath::top(0), 0), &at(BlockPath::top(1), 2));
    assert_eq!(shape(&t), vec![(0, 5, true), (0, 2, false)]);
    /* Starting AT the end of "first" (only its mark) and ending at the
    start of "third": both marks it crosses are covered. */
    let t = d.range_targets(&at(BlockPath::top(0), 5), &at(BlockPath::top(2), 0));
    assert_eq!(shape(&t), vec![(5, 5, true), (0, 6, true), (0, 0, false)]);
}

#[test]
fn a_range_that_stops_at_the_last_character_leaves_the_mark() {
    let d = three();
    /* The whole text of one paragraph, and the tail of the last one of a
    multi-paragraph range: neither crosses its paragraph's mark. */
    let whole = d.range_targets(&at(BlockPath::top(1), 0), &at(BlockPath::top(1), 6));
    assert_eq!(shape(&whole), vec![(0, 6, false)]);
    let tail = d.range_targets(&at(BlockPath::top(0), 2), &at(BlockPath::top(2), 5));
    assert_eq!(
        shape(&tail),
        vec![(2, 5, true), (0, 6, true), (0, 5, false)]
    );
    /* And the formatting follows: the text is 8 pt, the mark keeps its
    (unmodeled) formatting. */
    let out = d.apply_style(
        at(BlockPath::top(1), 0),
        at(BlockPath::top(1), 6),
        sized(8.0),
    );
    let p = out.paragraph_at_path(&BlockPath::top(1)).unwrap();
    assert_eq!(p.style_at(0).font_size, Some(8.0));
    assert_eq!(mark_size(&out, 1), None, "the mark was not selected");
}

#[test]
fn a_selection_that_includes_the_last_mark_covers_it_once_it_reaches_it() {
    let d = three();
    let pos = |b: u32, o: u32| at(BlockPath::top(b), o);
    /* Select All / triple-click: the range ends at the last paragraph's
    text end and the selection includes its mark. */
    let all = d.range_targets_with_end_mark(&pos(0, 0), &pos(2, 5), true);
    assert!(all.iter().all(|x| x.mark), "{all:?}");
    let one = d.range_targets_with_end_mark(&pos(1, 0), &pos(1, 6), true);
    assert_eq!(shape(&one), vec![(0, 6, true)]);
    /* The flag never reaches a mark the range stops short of. */
    let short = d.range_targets_with_end_mark(&pos(1, 0), &pos(1, 3), true);
    assert_eq!(shape(&short), vec![(0, 3, false)]);
    /* Without it the walk is `range_targets`. */
    assert_eq!(
        d.range_targets_with_end_mark(&pos(0, 0), &pos(2, 5), false),
        d.range_targets(&pos(0, 0), &pos(2, 5))
    );
}

#[test]
fn formatting_a_selection_reaches_every_mark_it_covers() {
    let d = three();
    /* Into the next paragraph: the first mark takes the patch, the
    second paragraph's (not reached) does not. */
    let out = d.apply_style(
        at(BlockPath::top(0), 0),
        at(BlockPath::top(1), 0),
        sized(8.0),
    );
    assert_eq!(mark_size(&out, 0), Some(8.0));
    assert_eq!(mark_size(&out, 1), None);
    assert!(out.paragraph_at_path(&BlockPath::top(0)).unwrap().dirty);
    /* Select All: every mark, the last one included; a mark keeps what
    it had and only gains the patch. */
    let mut d = three();
    if let Some(Block::Paragraph(p)) = d.blocks.get_mut(2) {
        p.mark_style = Some(Box::new(SpanStyle {
            bold: Some(true),
            ..Default::default()
        }));
    }
    let targets =
        d.range_targets_with_end_mark(&at(BlockPath::top(0), 0), &at(BlockPath::top(2), 5), true);
    let out = d.apply_style_targets(&targets, sized(8.0));
    for b in 0..3 {
        assert_eq!(mark_size(&out, b), Some(8.0), "mark {b}");
        let p = out.paragraph_at_path(&BlockPath::top(b)).unwrap();
        assert_eq!(p.style_at(0).font_size, Some(8.0), "text {b}");
    }
    let last = out.paragraph_at_path(&BlockPath::top(2)).unwrap();
    assert_eq!(last.mark_style.as_deref().and_then(|m| m.bold), Some(true));
    /* A cell rectangle: every selected cell's mark, text or not. */
    let d = doc(2, 2);
    let rect = d.cell_rect_targets(&BlockPath::top(1), (0, 1), (0, 0));
    assert!(rect.iter().all(|x| x.mark));
    let out = d.apply_style_targets(&rect, sized(8.0));
    for r in 0..2 {
        let p = out.paragraph_at_path(&cell(r, 0, 0)).unwrap();
        assert_eq!(p.mark_style.as_deref().and_then(|m| m.font_size), Some(8.0));
        let beside = out.paragraph_at_path(&cell(r, 1, 0)).unwrap();
        assert!(
            beside.mark_style.is_none(),
            "the cell beside is not selected"
        );
    }
}

#[test]
fn tracked_formatting_resolves_the_same_targets() {
    /* Review mode formats through `apply_style_targets` + this recorder
    over ONE target list: the text change is recorded over the covered
    bytes, the mark takes the patch (its `<w:rPrChange>` is #372). */
    let d = three();
    let targets = d.range_targets(&at(BlockPath::top(0), 0), &at(BlockPath::top(1), 0));
    let out = d
        .apply_style_targets(&targets, sized(8.0))
        .tracked_format_change_targets(&targets, SpanStyle::default(), "A".into(), "t".into());
    let p0 = out.paragraph_at_path(&BlockPath::top(0)).unwrap();
    assert_eq!(mark_size(&out, 0), Some(8.0));
    let revs: Vec<(u32, u32, crate::RevisionKind)> = p0
        .revisions
        .iter()
        .map(|r| (r.start, r.end, r.kind))
        .collect();
    assert_eq!(revs, vec![(0, 5, crate::RevisionKind::FormatChange)]);
    assert!(p0.mark_revisions.is_empty());
    let p1 = out.paragraph_at_path(&BlockPath::top(1)).unwrap();
    assert!(p1.revisions.is_empty(), "nothing of the second paragraph");
}
