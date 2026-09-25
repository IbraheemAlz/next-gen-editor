//! Issue #152 — the single choke point that keeps block-indexed side
//! tables in step with structural (block-level) edits.
//!
//! [`DocumentTree::comment_ranges`] addresses its endpoints by
//! [`LogicalPos`] — a block *path*, not a stable id — so any mutation
//! that inserts or removes blocks in front of an anchor must rewrite the
//! anchor or the comment drifts onto a different paragraph. Every block
//! insertion / removal in this crate (tables, TOC stubs + regeneration,
//! paragraph splits behind section breaks and TOC insertion) calls one
//! of the entry points here on the POST-mutation tree:
//!
//! - [`DocumentTree::remap_block_splice`] — `removed` blocks starting at
//!   `at` inside one container were replaced by `inserted` blocks
//!   ([`DocumentTree::remap_block_indices`] is the top-level `±delta`
//!   convenience).
//! - [`DocumentTree::remap_paragraph_split`] — one paragraph was split
//!   in two at a byte offset.
//! - [`DocumentTree::remap_table_cells`] (issue #253) — a table's interior
//!   was restructured (row / column insert + delete, merge, split): every
//!   anchor inside one of its cells is re-addressed by a per-cell
//!   [`CellMove`] the table command derives from its PRE-mutation shape.
//!
//! In-paragraph text edits (and paragraph merges) remap through
//! `text_remap` (issue #252), which keeps the paragraph source markup in
//! step from the same edit record.
//!
//! Audit (issue #152): `comment_ranges` is the only block-indexed side
//! table on the tree. Bookmarks, revisions, hyperlinks, fields and note
//! references ride their paragraph inline; section boundaries derive
//! from `Paragraph::section_end`; TOC regions are derived by scanning.
//! A future side table keyed by block path must be remapped here too.
//!
//! Undo needs nothing: the side table lives on the immutable tree, so
//! every undo snapshot carries its own consistent copy.

use crate::{BlockPath, DocumentTree, LogicalPos, PathStep};
use core::cmp::Ordering;

/// Issue #253 — where an anchor inside cell `(row, col)` of a restructured
/// table goes. Row / column indices are the POST-mutation cell addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellMove {
    /// The cell kept its address (and its content).
    Keep,
    /// The cell kept its content but moved to a new address; the rest of
    /// the anchor path (block + offset inside the cell) is unchanged.
    To { row: u32, col: u32 },
    /// The cell (or its content) is gone: the anchor collapses onto the
    /// start (`at_end == false`) or the end of the content of the cell at
    /// `(row, col)`.
    Collapse { row: u32, col: u32, at_end: bool },
}

impl DocumentTree {
    /// Top-level convenience over [`Self::remap_block_splice`]: `delta > 0`
    /// = `delta` blocks were inserted in front of block `at`; `delta < 0`
    /// = `-delta` blocks starting at `at` were removed.
    pub fn remap_block_indices(&mut self, at: u32, delta: i64) {
        let magnitude = u32::try_from(delta.unsigned_abs()).unwrap_or(u32::MAX);
        if delta >= 0 {
            self.remap_block_splice(&[], at, 0, magnitude);
        } else {
            self.remap_block_splice(&[], at, magnitude, 0);
        }
    }

    /// `removed` blocks at `at..at + removed` of the container addressed
    /// by `container` (the path steps leading INTO a block list: empty =
    /// the body, `[Block(t), Cell{r,c}]` = a table cell) were replaced by
    /// `inserted` blocks. Must run on the post-mutation tree.
    ///
    /// Anchors before `at` are untouched; anchors after the replaced run
    /// shift by `inserted - removed`. An anchor INSIDE the replaced run
    /// maps onto the replacement block at the same relative index (the
    /// last one when the replacement is shorter; offset clamped to the
    /// new paragraph); with no replacement at all it collapses onto the
    /// block that now sits at `at` (offset 0), or the end of the block
    /// before it when the removal emptied the container's tail.
    pub fn remap_block_splice(
        &mut self,
        container: &[PathStep],
        at: u32,
        removed: u32,
        inserted: u32,
    ) {
        if (removed == 0 && inserted == 0) || self.comment_ranges.is_empty() {
            return;
        }
        let mut ranges = std::mem::take(&mut self.comment_ranges);
        for r in &mut ranges {
            r.start = self.splice_pos(&r.start, container, at, removed, inserted);
            r.end = self.splice_pos(&r.end, container, at, removed, inserted);
        }
        self.comment_ranges = ranges;
    }

    /// The paragraph at `para` was split at byte `offset` into a left
    /// half (still at `para`) and a right half inserted right after it.
    /// Must run on the post-mutation tree. Anchors in later blocks of the
    /// same container shift down by one; anchors inside the split
    /// paragraph past the split point move onto the right half. A range
    /// START at exactly the split point follows the text to the right
    /// half; a range END there stays at the end of the left half.
    pub fn remap_paragraph_split(&mut self, para: &BlockPath, offset: u32) {
        let Some((PathStep::Block(idx), container)) = para.steps.split_last() else {
            return;
        };
        if self.comment_ranges.is_empty() {
            return;
        }
        let idx = *idx;
        let mut ranges = std::mem::take(&mut self.comment_ranges);
        for r in &mut ranges {
            let collapsed = r.start == r.end;
            r.start = self.splice_pos(&r.start, container, idx + 1, 0, 1);
            r.end = self.splice_pos(&r.end, container, idx + 1, 0, 1);
            r.start = split_pos(&r.start, para, idx, offset, true);
            r.end = if collapsed {
                r.start.clone()
            } else {
                split_pos(&r.end, para, idx, offset, false)
            };
        }
        self.comment_ranges = ranges;
    }

    fn splice_pos(
        &self,
        pos: &LogicalPos,
        container: &[PathStep],
        at: u32,
        removed: u32,
        inserted: u32,
    ) -> LogicalPos {
        let d = container.len();
        if pos.path.steps.len() <= d || pos.path.steps[..d] != *container {
            return pos.clone();
        }
        let PathStep::Block(i) = pos.path.steps[d] else {
            return pos.clone();
        };
        if i < at {
            return pos.clone();
        }
        let end = at.saturating_add(removed);
        if i >= end {
            let mut out = pos.clone();
            out.path.steps[d] = PathStep::Block(i - removed + inserted);
            return out;
        }
        /* Inside the replaced run: the anchored block itself is gone. */
        let at_block = |n: u32| {
            let mut steps = container.to_vec();
            steps.push(PathStep::Block(n));
            BlockPath { steps }
        };
        let rel = i - at;
        if inserted > 0 {
            let path = at_block(at + rel.min(inserted - 1));
            let offset = if rel < inserted { pos.offset } else { u32::MAX };
            let offset = self
                .paragraph_at_path(&path)
                .map_or(0, |p| offset.min(p.text.len() as u32));
            return LogicalPos { path, offset };
        }
        let here = at_block(at);
        if self.block_at(&here).is_some() || at == 0 {
            return LogicalPos {
                path: here,
                offset: 0,
            };
        }
        let prev = at_block(at - 1);
        let offset = self
            .paragraph_at_path(&prev)
            .map_or(0, |p| p.text.len() as u32);
        LogicalPos { path: prev, offset }
    }
}

impl DocumentTree {
    /// Issue #253 — the table at `table` had its interior restructured.
    /// `f(row, col)` says where an anchor in the PRE-mutation cell
    /// `(row, col)` goes (see [`CellMove`]); anchors outside the table are
    /// untouched. Must run on the post-mutation tree. A collapse target
    /// that no longer exists (the table lost its last row) falls back to
    /// the block after the table, else the end of the block before it. A
    /// range whose end lands before its start collapses onto the start.
    pub fn remap_table_cells(&mut self, table: &BlockPath, f: impl Fn(u32, u32) -> CellMove) {
        if self.comment_ranges.is_empty() {
            return;
        }
        let mut ranges = std::mem::take(&mut self.comment_ranges);
        for r in &mut ranges {
            r.start = self.cell_pos(&r.start, table, &f);
            r.end = self.cell_pos(&r.end, table, &f);
            if pos_before(&r.end, &r.start) {
                r.end = r.start.clone();
            }
        }
        self.comment_ranges = ranges;
    }

    fn cell_pos(
        &self,
        pos: &LogicalPos,
        table: &BlockPath,
        f: &impl Fn(u32, u32) -> CellMove,
    ) -> LogicalPos {
        let d = table.steps.len();
        if pos.path.steps.len() <= d + 1 || pos.path.steps[..d] != table.steps[..] {
            return pos.clone();
        }
        let PathStep::Cell { row, col } = pos.path.steps[d] else {
            return pos.clone();
        };
        match f(row, col) {
            CellMove::Keep => pos.clone(),
            CellMove::To { row, col } => {
                let mut out = pos.clone();
                out.path.steps[d] = PathStep::Cell { row, col };
                out
            }
            CellMove::Collapse { row, col, at_end } => self
                .cell_edge(table, row, col, at_end)
                .unwrap_or_else(|| self.beside_block(table)),
        }
    }

    /// The first / last paragraph edge directly inside cell `(row, col)`.
    fn cell_edge(&self, table: &BlockPath, row: u32, col: u32, at_end: bool) -> Option<LogicalPos> {
        let t = self.table_at_path(table)?;
        let cell = t.rows.get(row as usize)?.cells.get(col as usize)?;
        let mut paras = cell
            .blocks
            .iter()
            .enumerate()
            .filter_map(|(i, b)| b.as_paragraph().map(|p| (i, p)));
        let (k, p) = if at_end {
            paras.next_back()?
        } else {
            paras.next()?
        };
        let mut steps = table.steps.clone();
        steps.push(PathStep::Cell { row, col });
        steps.push(PathStep::Block(k as u32));
        Some(LogicalPos {
            path: BlockPath { steps },
            offset: if at_end { p.text.len() as u32 } else { 0 },
        })
    }

    /// The paragraph right after the block at `block` (offset 0), else the
    /// end of the paragraph right before it, else `block` itself.
    fn beside_block(&self, block: &BlockPath) -> LogicalPos {
        if let Some((PathStep::Block(i), container)) = block.steps.split_last() {
            let at = |n: u32| {
                let mut steps = container.to_vec();
                steps.push(PathStep::Block(n));
                BlockPath { steps }
            };
            let next = at(i + 1);
            if self.paragraph_at_path(&next).is_some() {
                return LogicalPos::new(next, 0);
            }
            if *i > 0 {
                let prev = at(i - 1);
                if let Some(p) = self.paragraph_at_path(&prev) {
                    return LogicalPos::new(prev, p.text.len() as u32);
                }
            }
        }
        LogicalPos::new(block.clone(), 0)
    }
}

/// `a` strictly precedes `b` in document order.
fn pos_before(a: &LogicalPos, b: &LogicalPos) -> bool {
    match a.path.cmp_doc_order(&b.path) {
        Ordering::Less => true,
        Ordering::Equal => a.offset < b.offset,
        Ordering::Greater => false,
    }
}

/// [`DocumentTree::remap_paragraph_split`] for one endpoint that already
/// had the later-block shift applied (the split paragraph keeps `idx`).
fn split_pos(pos: &LogicalPos, para: &BlockPath, idx: u32, at: u32, is_start: bool) -> LogicalPos {
    if pos.path != *para {
        return pos.clone();
    }
    let moves = if is_start {
        pos.offset >= at
    } else {
        pos.offset > at
    };
    if !moves {
        return pos.clone();
    }
    let mut out = pos.clone();
    if let Some(last) = out.path.steps.last_mut() {
        *last = PathStep::Block(idx + 1);
    }
    out.offset = pos.offset - at;
    out
}

#[cfg(test)]
mod tests {
    use crate::{Block, BlockPath, DocumentTree, LogicalPos, PathStep, SectionType, UndoStack};

    /// Three paragraphs; the comment covers "target" (all of block 2).
    fn commented_doc() -> DocumentTree {
        let doc = DocumentTree::from_paragraphs(["first".into(), "second".into(), "target".into()]);
        doc.insert_comment(
            LogicalPos::new(BlockPath::top(2), 0),
            LogicalPos::new(BlockPath::top(2), 6),
            "c".into(),
            "A".into(),
            "2026-01-01T00:00:00Z".into(),
        )
        .0
    }

    /// The text the (single) comment range covers — the invariant the
    /// issue's acceptance pins: the comment stays on ITS paragraph.
    fn commented_text(doc: &DocumentTree) -> String {
        let r = &doc.comment_ranges[0];
        doc.text_range(r.start.clone(), r.end.clone())
    }

    fn top(doc: &DocumentTree) -> (u32, u32, u32, u32) {
        let r = &doc.comment_ranges[0];
        let idx = |p: &LogicalPos| match p.path.steps.as_slice() {
            [PathStep::Block(i)] => *i,
            other => panic!("expected a top-level path, got {other:?}"),
        };
        (idx(&r.start), r.start.offset, idx(&r.end), r.end.offset)
    }

    #[test]
    fn insert_table_above_keeps_the_comment_on_its_paragraph() {
        let doc = commented_doc();
        let with_table = doc.insert_table(BlockPath::top(0), 2, 2);
        assert!(matches!(with_table.blocks[0], Block::Table(_)));
        assert_eq!(top(&with_table), (3, 0, 3, 6));
        assert_eq!(commented_text(&with_table), "target");
    }

    #[test]
    fn insert_table_with_escape_paragraph_shifts_by_two() {
        /* Inserting in front of a table needs a trailing paragraph too. */
        let doc = commented_doc().insert_table(BlockPath::top(0), 1, 1);
        let doc = doc.insert_table(BlockPath::top(0), 1, 1);
        assert!(matches!(doc.blocks[1], Block::Paragraph(_)));
        assert_eq!(top(&doc), (5, 0, 5, 6));
        assert_eq!(commented_text(&doc), "target");
    }

    #[test]
    fn insert_table_below_leaves_the_comment_alone() {
        let doc = commented_doc();
        let after = doc.insert_table(BlockPath::top(3), 1, 1);
        assert_eq!(top(&after), (2, 0, 2, 6));
    }

    #[test]
    fn delete_table_above_shifts_back_and_round_trips() {
        let doc = commented_doc();
        let with_table = doc.insert_table(BlockPath::top(1), 2, 2);
        assert_eq!(top(&with_table), (3, 0, 3, 6));
        let back = with_table.delete_table(BlockPath::top(1));
        assert_eq!(top(&back), (2, 0, 2, 6));
        assert_eq!(commented_text(&back), "target");
        /* A no-op delete (not a table) must not shift anything. */
        let noop = back.delete_table(BlockPath::top(0));
        assert_eq!(top(&noop), (2, 0, 2, 6));
    }

    #[test]
    fn deleting_a_table_that_holds_an_anchor_collapses_onto_the_next_block() {
        let doc = DocumentTree::from_paragraphs(["a".into(), "b".into()]).insert_table(
            BlockPath::top(1),
            1,
            1,
        );
        let cell_para = BlockPath {
            steps: vec![
                PathStep::Block(1),
                PathStep::Cell { row: 0, col: 0 },
                PathStep::Block(0),
            ],
        };
        let (doc, _) = doc.insert_comment(
            LogicalPos::new(BlockPath::top(0), 0),
            LogicalPos::new(cell_para, 0),
            "c".into(),
            "A".into(),
            "d".into(),
        );
        let doc = doc.delete_table(BlockPath::top(1));
        let r = &doc.comment_ranges[0];
        assert_eq!(r.start, LogicalPos::new(BlockPath::top(0), 0));
        assert_eq!(r.end, LogicalPos::new(BlockPath::top(1), 0));
    }

    #[test]
    fn page_break_above_does_not_move_the_comment() {
        /* `InsertPageBreak` flips `page_break_before` — no block moves. */
        let doc =
            commented_doc().set_page_break_before(LogicalPos::new(BlockPath::top(1), 0), true);
        assert_eq!(top(&doc), (2, 0, 2, 6));
        assert_eq!(commented_text(&doc), "target");
    }

    #[test]
    fn section_break_above_keeps_the_comment_on_its_paragraph() {
        let doc = commented_doc();
        let split = doc
            .insert_section_break_at(LogicalPos::new(BlockPath::top(0), 2), SectionType::NextPage);
        assert_eq!(split.blocks.len(), 4);
        assert_eq!(top(&split), (3, 0, 3, 6));
        assert_eq!(commented_text(&split), "target");
    }

    #[test]
    fn splitting_the_commented_paragraph_carries_the_tail_anchor() {
        let doc = DocumentTree::from_text("hello world");
        let (doc, _) = doc.insert_comment(
            LogicalPos::new(BlockPath::top(0), 2),
            LogicalPos::new(BlockPath::top(0), 9),
            "c".into(),
            "A".into(),
            "d".into(),
        );
        let split = doc.split_paragraph(LogicalPos::new(BlockPath::top(0), 6));
        assert_eq!(top(&split), (0, 2, 1, 3));
        /* Start exactly at the split point follows the right half. */
        let (doc2, _) = DocumentTree::from_text("hello world").insert_comment(
            LogicalPos::new(BlockPath::top(0), 6),
            LogicalPos::new(BlockPath::top(0), 11),
            "c".into(),
            "A".into(),
            "d".into(),
        );
        let split = doc2.split_paragraph(LogicalPos::new(BlockPath::top(0), 6));
        assert_eq!(top(&split), (1, 0, 1, 5));
        assert_eq!(commented_text(&split), "world");
    }

    #[test]
    fn undo_restores_the_pre_insertion_anchor() {
        let doc = commented_doc();
        let mut undo = UndoStack::new(doc.clone(), 100);
        undo.push(doc.insert_table(BlockPath::top(0), 1, 1));
        assert_eq!(top(undo.current()), (3, 0, 3, 6));
        assert!(undo.undo());
        assert_eq!(top(undo.current()), (2, 0, 2, 6));
        assert_eq!(commented_text(undo.current()), "target");
    }

    #[test]
    fn splice_remaps_anchors_inside_a_replaced_run() {
        let mut doc =
            DocumentTree::from_paragraphs(["a".into(), "bb".into(), "cc".into(), "d".into()]);
        doc = doc
            .insert_comment(
                LogicalPos::new(BlockPath::top(2), 1),
                LogicalPos::new(BlockPath::top(3), 1),
                "c".into(),
                "A".into(),
                "d".into(),
            )
            .0;
        /* Replace blocks 1..=2 with a single one-byte paragraph. */
        let mut blocks = doc.blocks.clone();
        blocks.remove(1);
        blocks.remove(1);
        blocks.insert(1, doc.blocks[0].clone());
        doc.blocks = blocks;
        doc.remap_block_splice(&[], 1, 2, 1);
        let r = &doc.comment_ranges[0];
        assert_eq!(r.start, LogicalPos::new(BlockPath::top(1), 1));
        assert_eq!(r.end, LogicalPos::new(BlockPath::top(2), 1));
    }

    /* ---- issue #253: table interior restructuring ---- */

    fn cell(row: u32, col: u32, block: u32) -> BlockPath {
        BlockPath {
            steps: vec![
                PathStep::Block(1),
                PathStep::Cell { row, col },
                PathStep::Block(block),
            ],
        }
    }

    /// "before" / a `rows × cols` table whose cell (r, c) reads "r{r}c{c}"
    /// / "after"; the comment covers all of cell `(ar, ac)`.
    fn table_doc(rows: u32, cols: u32, ar: u32, ac: u32) -> DocumentTree {
        let mut doc = DocumentTree::from_paragraphs(["before".into(), "after".into()])
            .insert_table(BlockPath::top(1), rows, cols);
        assert!(matches!(doc.blocks[1], Block::Table(_)));
        for r in 0..rows {
            for c in 0..cols {
                doc = doc.insert_text(LogicalPos::new(cell(r, c, 0), 0), &format!("r{r}c{c}"));
            }
        }
        doc.insert_comment(
            LogicalPos::new(cell(ar, ac, 0), 0),
            LogicalPos::new(cell(ar, ac, 0), 4),
            "c".into(),
            "A".into(),
            "d".into(),
        )
        .0
    }

    type CellAnchor = (u32, u32, u32);

    fn anchor_cell(doc: &DocumentTree) -> (CellAnchor, CellAnchor) {
        let r = &doc.comment_ranges[0];
        let at = |p: &LogicalPos| match p.path.steps.as_slice() {
            [
                PathStep::Block(1),
                PathStep::Cell { row, col },
                PathStep::Block(_),
            ] => (*row, *col, p.offset),
            other => panic!("expected a cell path, got {other:?}"),
        };
        (at(&r.start), at(&r.end))
    }

    fn table_path() -> BlockPath {
        BlockPath::top(1)
    }

    #[test]
    fn deleting_the_anchor_row_moves_it_to_the_next_rows_first_cell() {
        let doc = table_doc(3, 2, 1, 1).delete_row(table_path(), 1);
        assert_eq!(anchor_cell(&doc), ((1, 0, 0), (1, 0, 0)));
        /* The paragraph it lands on is the old row 2's first cell. */
        assert_eq!(doc.paragraph_at_path(&cell(1, 0, 0)).unwrap().text, "r2c0");
    }

    #[test]
    fn deleting_the_last_row_moves_the_anchor_to_the_row_above() {
        let doc = table_doc(2, 2, 1, 1).delete_row(table_path(), 1);
        let ((r, c, _), _) = anchor_cell(&doc);
        assert_eq!((r, c), (0, 0));
    }

    #[test]
    fn deleting_a_row_above_shifts_and_below_keeps() {
        let doc = table_doc(3, 2, 2, 1);
        let up = doc.delete_row(table_path(), 0);
        assert_eq!(anchor_cell(&up), ((1, 1, 0), (1, 1, 4)));
        assert_eq!(commented_text(&up), "r2c1");
        let doc = table_doc(3, 2, 0, 1);
        let kept = doc.delete_row(table_path(), 2);
        assert_eq!(anchor_cell(&kept), ((0, 1, 0), (0, 1, 4)));
        /* Out-of-range delete: nothing moves. */
        let noop = doc.delete_row(table_path(), 9);
        assert_eq!(anchor_cell(&noop), anchor_cell(&doc));
    }

    #[test]
    fn inserting_a_row_above_the_anchor_shifts_it_down() {
        let doc = table_doc(2, 2, 1, 0);
        let above = doc.insert_row(table_path(), 1);
        assert_eq!(anchor_cell(&above), ((2, 0, 0), (2, 0, 4)));
        assert_eq!(commented_text(&above), "r1c0");
        let below = doc.insert_row(table_path(), 2);
        assert_eq!(anchor_cell(&below), ((1, 0, 0), (1, 0, 4)));
    }

    #[test]
    fn column_insert_and_delete_remap_the_anchor() {
        let doc = table_doc(2, 3, 1, 1);
        let ins = doc.insert_column(table_path(), 0);
        assert_eq!(anchor_cell(&ins), ((1, 2, 0), (1, 2, 4)));
        assert_eq!(commented_text(&ins), "r1c1");
        let del_left = doc.delete_column(table_path(), 0);
        assert_eq!(commented_text(&del_left), "r1c1");
        /* Deleting the anchor's column: the cell sliding into its place. */
        let del_own = doc.delete_column(table_path(), 1);
        assert_eq!(anchor_cell(&del_own), ((1, 1, 0), (1, 1, 0)));
        assert_eq!(
            del_own.paragraph_at_path(&cell(1, 1, 0)).unwrap().text,
            "r1c2"
        );
        /* Deleting the last column under the anchor: its left neighbour. */
        let doc = table_doc(2, 2, 0, 1);
        let del_last = doc.delete_column(table_path(), 1);
        assert_eq!(anchor_cell(&del_last), ((0, 0, 4), (0, 0, 4)));
    }

    #[test]
    fn merging_keeps_the_anchor_in_the_merged_cell() {
        /* Anchor in the owner: unchanged. */
        let doc = table_doc(2, 3, 0, 0).merge_cells(table_path(), 0, 0, 0, 1);
        assert_eq!(anchor_cell(&doc), ((0, 0, 0), (0, 0, 4)));
        assert_eq!(commented_text(&doc), "r0c0");
        /* Anchor in a merged-away partner: onto the owner. */
        let doc = table_doc(2, 3, 0, 1).merge_cells(table_path(), 0, 0, 0, 1);
        assert_eq!(anchor_cell(&doc), ((0, 0, 4), (0, 0, 4)));
        /* Anchor right of the merge: shifts left with its cell. */
        let doc = table_doc(2, 3, 0, 2).merge_cells(table_path(), 0, 0, 0, 1);
        assert_eq!(anchor_cell(&doc), ((0, 1, 0), (0, 1, 4)));
        assert_eq!(commented_text(&doc), "r0c2");
        /* Vertical merge: the continuation cell's anchor joins the owner;
        a cell right of the merged block in the continuation row shifts. */
        let doc = table_doc(2, 3, 1, 0).merge_cells(table_path(), 0, 0, 1, 1);
        let ((r, c, _), _) = anchor_cell(&doc);
        assert_eq!((r, c), (0, 0));
        let doc = table_doc(2, 3, 1, 2).merge_cells(table_path(), 0, 0, 1, 1);
        assert_eq!(anchor_cell(&doc), ((1, 1, 0), (1, 1, 4)));
        assert_eq!(commented_text(&doc), "r1c2");
    }

    #[test]
    fn splitting_a_merged_cell_shifts_the_cells_right_of_it() {
        let merged = table_doc(2, 3, 1, 2).merge_cells(table_path(), 0, 0, 1, 1);
        assert_eq!(anchor_cell(&merged), ((1, 1, 0), (1, 1, 4)));
        let split = merged.split_cell(table_path(), 0, 0);
        assert_eq!(anchor_cell(&split), ((1, 2, 0), (1, 2, 4)));
        assert_eq!(commented_text(&split), "r1c2");
    }

    #[test]
    fn undo_restores_the_pre_restructure_anchor() {
        let doc = table_doc(3, 2, 1, 1);
        let mut undo = UndoStack::new(doc.clone(), 100);
        undo.push(doc.delete_row(table_path(), 1));
        assert_eq!(anchor_cell(undo.current()), ((1, 0, 0), (1, 0, 0)));
        assert!(undo.undo());
        assert_eq!(anchor_cell(undo.current()), ((1, 1, 0), (1, 1, 4)));
        assert_eq!(commented_text(undo.current()), "r1c1");
    }

    #[test]
    fn anchors_outside_the_table_are_untouched_by_table_edits() {
        let doc = table_doc(2, 2, 0, 0);
        let (doc, _) = doc.insert_comment(
            LogicalPos::new(BlockPath::top(2), 0),
            LogicalPos::new(BlockPath::top(2), 5),
            "c2".into(),
            "A".into(),
            "d".into(),
        );
        for d in [
            doc.delete_row(table_path(), 0),
            doc.insert_row(table_path(), 0),
            doc.delete_column(table_path(), 0),
            doc.insert_column(table_path(), 0),
            doc.merge_cells(table_path(), 0, 0, 1, 1),
        ] {
            let r = &d.comment_ranges[1];
            assert_eq!(r.start, LogicalPos::new(BlockPath::top(2), 0));
            assert_eq!(r.end, LogicalPos::new(BlockPath::top(2), 5));
        }
    }
}
