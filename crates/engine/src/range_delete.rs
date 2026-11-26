//! Issue #561 — deleting a range whose two ends sit in different
//! containers of one story (a body paragraph and a table cell, two cells,
//! a cell and a nested table inside it), and the plain text of any range.
//!
//! The rules, pinned from the clean-room cursor memo
//! (`plans/cleanroom/memos/onlyoffice-cursor-memo.md` §6 / §7) and
//! `plans/UX_BEHAVIOR_SPEC.md` §IV.10 / §V.1:
//!
//! - **Deletion never crosses a cell boundary** (memo §7). Two paragraphs
//!   merge only when they share a container (the same-container rule of
//!   [`DocumentTree::delete_range`]); a body paragraph never merges with a
//!   cell's, nor one cell's with another's. The text the range covers is
//!   removed from every paragraph it reaches — the same paragraphs, in the
//!   same document order, as [`DocumentTree::range_targets`] (issue #509).
//! - **A covered cell stays.** A cell the range covers only in part loses
//!   the covered text (its own paragraphs merge as in any container); a
//!   cell it covers whole keeps one empty paragraph (the last one's mark,
//!   Word's end-of-cell mark).
//! - **A row the range covers whole is removed** (memo §6: a selection that
//!   reaches outside a table contributes whole rows), and so is a table the
//!   range covers whole — every row, nested tables included. A table that
//!   is the WHOLE range (a Tab-selected cell holding only a nested table) is
//!   replaced by an empty paragraph, the caret's home. The tracked twin
//!   (`try_tracked_delete_range`, issue #365) marks every row a range
//!   ENTERING a table touches; whether the untracked delete should remove a
//!   partly covered row too (Word's selection snaps to whole rows, ours does
//!   not) is open — issue #615.
//! - A **`TABLE_CELLS` rectangle** clears its cells ([`Self::clear_cell_rect`]):
//!   Word's Delete over a cell selection empties the cells and keeps them.
//!
//! Every edit goes through the tree's existing primitives — the
//! same-container [`DocumentTree::delete_range`] (section-marker fold,
//! anchor remap) and the block / table-cell anchor remaps of
//! `block_remap` — so comment anchors follow the text.

use crate::{
    Block, BlockPath, CellMove, DocumentTree, LogicalPos, Paragraph, PathStep, RangeTarget, Table,
    VMergeRole, order_positions, replace_block_in_top, same_parent,
};

/// What a deletion did to a table it reached.
enum TableCut {
    /// The range covered every row: the caller removes the table.
    Whole,
    /// Rows and / or cell text went; the position the range's start now
    /// sits at, inside the table.
    Kept(LogicalPos),
}

/// The path of block `idx` of the container `c` leads into.
fn child(c: &[PathStep], idx: u32) -> BlockPath {
    let mut steps = c.to_vec();
    steps.push(PathStep::Block(idx));
    BlockPath { steps }
}

/// The block index `pos` sits under at depth `d` (its step `d`).
fn block_index_at(pos: &LogicalPos, d: usize) -> Option<u32> {
    match pos.path.steps.get(d)? {
        PathStep::Block(i) => Some(*i),
        PathStep::Cell { .. } => None,
    }
}

/// The cell `pos` sits in at depth `d` (its step `d`).
fn cell_at(pos: &LogicalPos, d: usize) -> Option<(u32, u32)> {
    match pos.path.steps.get(d)? {
        PathStep::Cell { row, col } => Some((*row, *col)),
        PathStep::Block(_) => None,
    }
}

impl DocumentTree {
    /// Issue #561 — [`Self::delete_range`] and where the caret goes: the
    /// position the range's start now sits at, which is also where a
    /// replacement (typing over the selection, a paste) is inserted. For a
    /// range inside one container that is the start itself; across
    /// containers the start's own table may be gone (a Tab-selected cell
    /// whose content begins with a nested table).
    pub fn delete_range_at(&self, start: LogicalPos, end: LogicalPos) -> (Self, LogicalPos) {
        let (start, end) = order_positions(start, end);
        if start.path == end.path || same_parent(&start.path, &end.path) {
            return (self.delete_range(start.clone(), end), start);
        }
        self.delete_across_containers(&start, &end)
            .unwrap_or_else(|| (self.clone(), start))
    }

    /// The cross-container branch of [`Self::delete_range_at`]: both ends
    /// (ordered) address paragraphs in different containers. `None` when
    /// either end addresses no paragraph or the paths do not meet in one
    /// container (nothing is deleted then).
    pub(crate) fn delete_across_containers(
        &self,
        start: &LogicalPos,
        end: &LogicalPos,
    ) -> Option<(Self, LogicalPos)> {
        self.paragraph_at_path(&start.path)?;
        self.paragraph_at_path(&end.path)?;
        let (ss, es) = (&start.path.steps, &end.path.steps);
        let k = ss.iter().zip(es).take_while(|(a, b)| a == b).count();
        /* The container both ends live in: where they part at two blocks,
        it is the one holding those blocks; where they part at two cells of
        one table, it is the one holding that table. */
        let c = match (ss.get(k)?, es.get(k)?) {
            (PathStep::Block(_), PathStep::Block(_)) => &ss[..k],
            (PathStep::Cell { .. }, PathStep::Cell { .. }) => &ss[..k.checked_sub(1)?],
            _ => return None,
        };
        let (doc, caret) = self.delete_between(c, start, end)?;
        Some((doc.with_list_markers_refreshed(), caret))
    }

    /// Delete `[from, to]` inside the container `c` leads into (the body or
    /// a cell's block list); both ends address paragraphs at or below it,
    /// `from` first. Returns the tree and where `from` landed.
    fn delete_between(
        &self,
        c: &[PathStep],
        from: &LogicalPos,
        to: &LogicalPos,
    ) -> Option<(Self, LogicalPos)> {
        let d = c.len();
        let i = block_index_at(from, d)?;
        let j = block_index_at(to, d)?;
        let from_direct = from.path.steps.len() == d + 1;
        let to_direct = to.path.steps.len() == d + 1;
        if i == j {
            if from_direct || to_direct {
                /* One paragraph (or, defensively, a malformed pair). */
                return Some((self.delete_range(from.clone(), to.clone()), from.clone()));
            }
            /* Both ends inside the table at `i`. */
            return match self.cut_table(&child(c, i), from, to)? {
                (doc, TableCut::Kept(caret)) => Some((doc, caret)),
                /* The range IS this table: an empty paragraph takes its
                place and holds the caret. */
                (doc, TableCut::Whole) => {
                    let doc = doc.splice_container(c, i, 1, vec![empty_paragraph()])?;
                    Some((doc, LogicalPos::new(child(c, i), 0)))
                }
            };
        }
        if j < i {
            return None;
        }
        let mut doc = self.clone();
        /* 1. The end side's table, first: nothing before it moves. */
        let mut end_whole = false;
        if !to_direct {
            let first = doc.first_position_in(&child(c, j))?;
            let (next, cut) = doc.cut_table(&child(c, j), &first, to)?;
            doc = next;
            end_whole = matches!(cut, TableCut::Whole);
        }
        /* 2. The blocks between, merged through the same-container rule
        (its section-marker fold and anchor remap included). Afterwards
        the end block sits at `end_at`. */
        let end_at = match (from_direct, to_direct) {
            (true, true) => {
                return Some((self.delete_range(from.clone(), to.clone()), from.clone()));
            }
            /* The start paragraph keeps its head; whatever lies between it
            and the end table goes, the last mark before the table stays
            (the paragraph the start merges into). */
            (true, false) => {
                let to_end = if j > i + 1 {
                    doc.end_position_of(&child(c, j - 1))
                } else {
                    doc.end_position_of(&from.path)
                };
                doc = doc.delete_range(from.clone(), to_end);
                i + 1
            }
            /* The end paragraph keeps its tail; whatever lies between the
            start table and it merges into it (an empty head merges away,
            so the tail keeps its own paragraph formatting). */
            (false, true) => {
                let from_start = if j > i + 1 {
                    LogicalPos::new(child(c, i + 1), 0)
                } else {
                    LogicalPos::new(to.path.clone(), 0)
                };
                doc = doc.delete_range(from_start, to.clone());
                i + 1
            }
            /* Two tables: what lies between collapses to ONE empty
            paragraph, so the tables never become adjacent. */
            (false, false) => {
                if j > i + 1 {
                    let between = child(c, i + 1);
                    if j == i + 2 && doc.paragraph_at_path(&between).is_none() {
                        doc = doc.splice_container(c, i + 1, 1, vec![empty_paragraph()])?;
                    } else {
                        let last = doc.end_position_of(&child(c, j - 1));
                        doc = doc.delete_range(LogicalPos::new(between, 0), last);
                    }
                    i + 2
                } else {
                    i + 1
                }
            }
        };
        /* 3. The start side's table. */
        let mut start_whole = false;
        let mut caret = from.clone();
        if !from_direct {
            let last = doc.last_position_in(&child(c, i))?;
            let (next, cut) = doc.cut_table(&child(c, i), from, &last)?;
            doc = next;
            match cut {
                TableCut::Whole => start_whole = true,
                TableCut::Kept(at) => caret = at,
            }
        }
        /* 4. Whole tables go, the later one first. */
        if end_whole {
            doc = doc.splice_container(c, end_at, 1, Vec::new())?;
        }
        if start_whole {
            doc = doc.splice_container(c, i, 1, Vec::new())?;
            /* The block that took the start's place holds the caret; when
            it is not a paragraph (another table, or nothing) an empty one
            is put there. */
            if doc.paragraph_at_path(&child(c, i)).is_none() {
                doc = doc.splice_container(c, i, 0, vec![empty_paragraph()])?;
            }
            caret = LogicalPos::new(child(c, i), 0);
        }
        Some((doc, caret))
    }

    /// Delete `[from, to]` inside the table at `t` (both ends in its cells,
    /// `from` first): the rows the range covers whole go, every other cell
    /// it reaches loses the covered text ([`Self::delete_between`] per
    /// cell, from the last one back). `Whole` when it covers every row —
    /// the caller removes the table.
    fn cut_table(
        &self,
        t: &BlockPath,
        from: &LogicalPos,
        to: &LogicalPos,
    ) -> Option<(Self, TableCut)> {
        let d = t.steps.len();
        let table = self.table_at_path(t)?;
        let (rs, cs) = cell_at(from, d)?;
        let (re, ce) = cell_at(to, d)?;
        let n_rows = table.rows.len() as u32;
        if rs > re || re >= n_rows {
            return None;
        }
        let at_row_start = self
            .row_edge(t, rs, false)
            .is_some_and(|p| p.path == from.path && self.snap_pos(from.clone()).offset == 0);
        let at_row_end = self
            .row_edge(t, re, true)
            .is_some_and(|p| p.path == to.path && self.snap_pos(to.clone()).offset >= p.offset);
        let whole = |r: u32| (r > rs || at_row_start) && (r < re || at_row_end);
        let whole_rows: Vec<u32> = (rs..=re).filter(|&r| whole(r)).collect();
        if whole_rows.len() as u32 == n_rows {
            return Some((self.clone(), TableCut::Whole));
        }
        let mut doc = self.clone();
        let mut caret = from.clone();
        for r in (rs..=re).rev() {
            if whole(r) {
                continue;
            }
            let Some(row) = table.rows.get(r as usize) else {
                continue;
            };
            let lo = if r == rs { cs } else { 0 };
            let hi = if r == re {
                ce
            } else {
                (row.cells.len() as u32).saturating_sub(1)
            };
            for col in (lo..=hi).rev() {
                let Some(cell) = row.cells.get(col as usize) else {
                    continue;
                };
                if cell.props.v_merge == VMergeRole::Continue {
                    continue;
                }
                let cell_path = t.clone().push(PathStep::Cell { row: r, col });
                let Some((first, last)) = doc.cell_paragraph_bounds(&cell_path) else {
                    continue;
                };
                let is_first = (r, col) == (rs, cs);
                let f = if is_first {
                    from.clone()
                } else {
                    LogicalPos::new(first, 0)
                };
                let l = if (r, col) == (re, ce) {
                    to.clone()
                } else {
                    doc.end_position_of(&last)
                };
                let (next, landed) = doc.delete_between(&cell_path.steps, &f, &l)?;
                doc = next;
                if is_first {
                    caret = landed;
                }
            }
        }
        if !whole_rows.is_empty() {
            doc = doc.remove_rows(t, &whole_rows)?;
            if whole(rs) {
                /* The start's row went: the start of the row that took its
                place, else (the rows went to the table's end) the end of
                what is left. */
                caret = match doc.row_edge(t, rs, false) {
                    Some(p) => p,
                    None => doc.last_position_in(t)?,
                };
            }
        }
        Some((doc, TableCut::Kept(caret)))
    }

    /// The first (`at_end == false`: deep first paragraph, offset 0) or
    /// last (deep last paragraph, its end) position of row `row` of the
    /// table at `t`, over the cells a range walk visits (vertically merged
    /// continuations skipped, as in [`Self::range_targets`]).
    fn row_edge(&self, t: &BlockPath, row: u32, at_end: bool) -> Option<LogicalPos> {
        let r = self.table_at_path(t)?.rows.get(row as usize)?;
        let cols: Vec<u32> = (0..r.cells.len() as u32)
            .filter(|&c| r.cells[c as usize].props.v_merge != VMergeRole::Continue)
            .collect();
        let cell = |c: u32| t.clone().push(PathStep::Cell { row, col: c });
        if at_end {
            cols.iter().rev().find_map(|&c| {
                let (_, last) = self.cell_paragraph_bounds(&cell(c))?;
                Some(self.end_position_of(&last))
            })
        } else {
            cols.iter().find_map(|&c| {
                let (first, _) = self.cell_paragraph_bounds(&cell(c))?;
                Some(LogicalPos::new(first, 0))
            })
        }
    }

    /// The first paragraph position (deep, offset 0) under the block at
    /// `block`.
    fn first_position_in(&self, block: &BlockPath) -> Option<LogicalPos> {
        let b = self.block_at(block)?;
        let path = crate::first_paragraph_in_blocks(std::iter::once(b), &block.parent())?;
        Some(LogicalPos::new(rebase_first(path, block), 0))
    }

    /// The last paragraph position (deep, its end) under the block at
    /// `block`.
    fn last_position_in(&self, block: &BlockPath) -> Option<LogicalPos> {
        let b = self.block_at(block)?;
        let path = crate::last_paragraph_in_blocks(std::iter::once(b), &block.parent())?;
        Some(self.end_position_of(&rebase_first(path, block)))
    }

    /// The end of the paragraph at `path`; offset 0 at a path that is not
    /// a paragraph (a table between two deleted blocks — the same-container
    /// merge then treats it as an empty tail).
    fn end_position_of(&self, path: &BlockPath) -> LogicalPos {
        let len = self
            .paragraph_at_path(path)
            .map_or(0, |p| p.text.len() as u32);
        LogicalPos::new(path.clone(), len)
    }

    /// Replace `remove` blocks at `at` of the container `c` leads into with
    /// `insert`, every table on the way marked dirty, comment anchors
    /// remapped ([`Self::remap_block_splice`]). `None` when `c` addresses
    /// no container or the run is out of range.
    fn splice_container(
        &self,
        c: &[PathStep],
        at: u32,
        remove: u32,
        insert: Vec<Block>,
    ) -> Option<Self> {
        let (at, remove) = (at as usize, remove as usize);
        let inserted = insert.len() as u32;
        let mut out = self.clone();
        match c.split_last() {
            None => {
                if at + remove > out.blocks.len() {
                    return None;
                }
                for _ in 0..remove {
                    out.blocks.remove(at);
                }
                for (k, b) in insert.into_iter().enumerate() {
                    out.blocks.insert(at + k, b);
                }
            }
            Some((PathStep::Cell { row, col }, table_steps)) => {
                let table_path = BlockPath {
                    steps: table_steps.to_vec(),
                };
                let mut t = out.table_at_path(&table_path)?.clone();
                let cell = t
                    .rows
                    .get_mut(*row as usize)?
                    .cells
                    .get_mut(*col as usize)?;
                if at + remove > cell.blocks.len() {
                    return None;
                }
                cell.blocks.splice(at..at + remove, insert);
                t.dirty = true;
                t.source_xml = None;
                let mut blocks = out.blocks.clone();
                replace_block_in_top(&mut blocks, &table_path, Block::Table(t))?;
                out.blocks = blocks;
            }
            Some((PathStep::Block(_), _)) => return None,
        }
        out.remap_block_splice(c, at as u32, remove as u32, inserted);
        Some(out)
    }

    /// Remove rows `rows` (ascending) of the table at `t` (any depth),
    /// re-rooting vertical merges whose first row went, comment anchors in
    /// a removed row collapsed onto the row that takes its place (else the
    /// one above), later rows' anchors shifted up — the
    /// [`Self::delete_row`] rule for a run of rows.
    fn remove_rows(&self, t: &BlockPath, rows: &[u32]) -> Option<Self> {
        let mut table = self.table_at_path(t)?.clone();
        let before = table.rows.len() as u32;
        for &r in rows.iter().rev() {
            if (r as usize) < table.rows.len() {
                table.rows.remove(r as usize);
            }
        }
        heal_vertical_merges(&mut table);
        table.dirty = true;
        table.source_xml = None;
        let left = table.rows.len() as u32;
        let mut out = self.clone();
        let mut blocks = out.blocks.clone();
        replace_block_in_top(&mut blocks, t, Block::Table(table))?;
        out.blocks = blocks;
        let removed_before = |r: u32| rows.iter().filter(|&&x| x < r).count() as u32;
        out.remap_table_cells(t, |r, col| {
            if r >= before {
                CellMove::Keep
            } else if !rows.contains(&r) {
                match removed_before(r) {
                    0 => CellMove::Keep,
                    n => CellMove::To { row: r - n, col },
                }
            } else {
                let here = r - removed_before(r);
                if here < left {
                    CellMove::Collapse {
                        row: here,
                        col: 0,
                        at_end: false,
                    }
                } else {
                    CellMove::Collapse {
                        row: left.saturating_sub(1),
                        col: 0,
                        at_end: left > 0,
                    }
                }
            }
        });
        Some(out)
    }

    /// Issue #561 — clear every cell of the rectangle
    /// `rows.0..=rows.1` x `cols.0..=cols.1` of the table at `table` (a
    /// `TABLE_CELLS` selection's Delete): each cell keeps one empty
    /// paragraph, the cells and rows stay (memo §7 — deletion never
    /// crosses a cell boundary). Returns the tree and the start of the
    /// rectangle's first cell, or `None` when the rectangle holds no cell.
    pub fn clear_cell_rect(
        &self,
        table: &BlockPath,
        rows: (u32, u32),
        cols: (u32, u32),
    ) -> Option<(Self, LogicalPos)> {
        let t = self.table_at_path(table)?;
        let mut cells = Vec::new();
        for (ri, row) in t.rows.iter().enumerate() {
            let r = ri as u32;
            if r < rows.0 || r > rows.1 {
                continue;
            }
            for (ci, cell) in row.cells.iter().enumerate() {
                let c = ci as u32;
                if c < cols.0 || c > cols.1 || cell.props.v_merge == VMergeRole::Continue {
                    continue;
                }
                cells.push(table.clone().push(PathStep::Cell { row: r, col: c }));
            }
        }
        let mut doc = self.clone();
        let mut caret = None;
        for cell in cells.iter().rev() {
            let Some((first, last)) = doc.cell_paragraph_bounds(cell) else {
                continue;
            };
            let to = doc.end_position_of(&last);
            let (next, landed) =
                doc.delete_between(&cell.steps, &LogicalPos::new(first, 0), &to)?;
            doc = next;
            caret = Some(landed);
        }
        Some((doc.with_list_markers_refreshed(), caret?))
    }

    /// Issue #561 — the plain text of a range's paragraphs, as the
    /// clipboard's `text/plain` carries it (UX_BEHAVIOR_SPEC §V.1, Word's
    /// convention): paragraphs joined with `\n`; the cells of one table row
    /// with `\t` (one per column stepped over, so a skipped vertically
    /// merged continuation keeps the row's width), rows with `\n`. Inside
    /// a cell of a table the range crosses, the cell's own line breaks and
    /// tabs become spaces so one row stays one line — the rule
    /// [`Self::to_plain_text`] applies to a whole table. `targets` is what
    /// [`Self::range_targets`] / [`Self::cell_rect_targets`] resolved.
    pub fn targets_plain_text(&self, targets: &[RangeTarget]) -> String {
        let (Some(first), Some(last)) = (targets.first(), targets.last()) else {
            return String::new();
        };
        let (fs, ls) = (&first.path.steps, &last.path.steps);
        /* A cell (a step prefix ending in its `Cell` step) the range
        crosses: not every target lies inside it. */
        let crossed = |steps: &[PathStep]| {
            steps.iter().enumerate().any(|(p, step)| {
                matches!(step, PathStep::Cell { .. })
                    && !(fs.starts_with(&steps[..=p]) && ls.starts_with(&steps[..=p]))
            })
        };
        let mut out = String::new();
        let mut prev: Option<&[PathStep]> = None;
        for t in targets {
            let steps = t.path.steps.as_slice();
            if let Some(p) = prev {
                let k = p.iter().zip(steps).take_while(|(a, b)| a == b).count();
                match (p.get(k), steps.get(k)) {
                    (
                        Some(PathStep::Cell { row: r1, col: c1 }),
                        Some(PathStep::Cell { row: r2, col: c2 }),
                    ) if r1 == r2 => {
                        for _ in 0..c2.saturating_sub(*c1).max(1) {
                            out.push('\t');
                        }
                    }
                    (Some(PathStep::Cell { .. }), Some(PathStep::Cell { .. })) => out.push('\n'),
                    _ if crossed(&steps[..k]) => out.push(' '),
                    _ => out.push('\n'),
                }
            }
            prev = Some(steps);
            let Some(p) = self.paragraph_at_path(&t.path) else {
                continue;
            };
            let (lo, hi) = (p.snap_offset(t.lo) as usize, p.snap_offset(t.hi) as usize);
            if lo >= hi {
                continue;
            }
            let text = &p.text[lo..hi];
            if crossed(steps) {
                out.extend(text.chars().map(|ch| match ch {
                    '\n' | '\r' | '\t' => ' ',
                    other => other,
                }));
            } else {
                out.push_str(text);
            }
        }
        out
    }
}

/// `path` — found by a descent rooted at `block`'s parent over `block`
/// alone, so its step at `block`'s depth reads `Block(0)` — re-rooted at
/// `block`'s own index.
fn rebase_first(mut path: BlockPath, block: &BlockPath) -> BlockPath {
    let d = block.steps.len() - 1;
    if let (Some(step), Some(own)) = (path.steps.get_mut(d), block.steps.last()) {
        *step = *own;
    }
    path
}

/// A cell becomes visible content again when the row that started its
/// vertical merge is gone: a `Continue` cell with no merged cell starting
/// at the same grid column in the row above restarts the merge.
fn heal_vertical_merges(t: &mut Table) {
    let starts = |cells: &[crate::TableCell]| -> Vec<(u32, VMergeRole)> {
        let mut g = 0u32;
        cells
            .iter()
            .map(|c| {
                let at = g;
                g += u32::from(c.props.grid_span.max(1));
                (at, c.props.v_merge)
            })
            .collect()
    };
    let mut above: Vec<(u32, VMergeRole)> = Vec::new();
    for row in &mut t.rows {
        let cols = starts(&row.cells);
        for (cell, (g, role)) in row.cells.iter_mut().zip(&cols) {
            if *role == VMergeRole::Continue
                && !above
                    .iter()
                    .any(|(ga, ra)| ga == g && *ra != VMergeRole::None)
            {
                cell.props.v_merge = VMergeRole::Restart;
            }
        }
        above = starts(&row.cells);
    }
}

/// The paragraph a removed table leaves for the caret.
fn empty_paragraph() -> Block {
    Block::Paragraph(Paragraph::default())
}
