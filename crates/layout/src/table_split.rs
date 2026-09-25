//! Issue #91 — pure row-split helpers for the paginator's table path.
//!
//! The paginator ([`crate::paginate::Paginator`]) decides *where* a table
//! breaks; this module owns the box surgery:
//!
//! - [`row_units`] — the indivisible row groups. A vertical merge
//!   (`Restart` + the `Continue` cells below it) is one unit: breaking
//!   it would leave the merged cell's box hanging past its fragment.
//! - [`split_row_in_cells`] — the continuation of ONE row inside its
//!   cells: every cell's content is cut at the deepest line boundary
//!   within the budget (paragraphs at a line, nested tables move whole —
//!   nested-table splitting is a follow-up), the tail cells re-based so
//!   hit-testing still maps to the model (`content_offset`). Issue #91
//!   reached it only for rows taller than a page ([`SplitRule::AnyCell`]);
//!   issue #155 also cuts a row that merely does not fit the rest of the
//!   page — Word's default "allow row to break across pages" — under the
//!   stricter [`SplitRule::EveryCell`].
//! - [`restub_continuation`] / [`recompute_vmerge_spans`] — when a cut
//!   lands inside a merge group (only on the degraded paths: a group, or
//!   a single row, taller than a page), the fragment that inherits the
//!   group's `Continue` cells gets a stub cell to paint the merged
//!   region's frame, and every `Restart` cell's height is re-derived from
//!   the rows it spans *inside its own fragment*.
//!
//! Every function is a bounded single pass over its input — no loop in
//! here can fail to terminate; progress is the caller's contract (see
//! the `push_table_split` docs).

use crate::boxes::{LayoutBlock, Point, TableCellBox, TableRowBox};

/// Index of the cell in `row` that starts at grid column `col`, if any
/// (a cell spanning over `col` from the left does not start there).
fn cell_at_grid_col(row: &TableRowBox, col: u32) -> Option<usize> {
    let mut c = 0u32;
    for (i, cell) in row.cells.iter().enumerate() {
        if c == col {
            return Some(i);
        }
        c += u32::from(cell.grid_span.max(1));
        if c > col {
            return None;
        }
    }
    None
}

/// Exclusive end row of every vertical merge that starts in row `r`
/// (`r + 1` when none does).
fn vmerge_span_end(rows: &[TableRowBox], r: usize) -> usize {
    let mut end = r + 1;
    let mut col = 0u32;
    for cell in &rows[r].cells {
        if matches!(cell.v_merge, engine::VMergeRole::Restart) {
            let mut k = r + 1;
            while k < rows.len()
                && cell_at_grid_col(&rows[k], col).is_some_and(|i| {
                    matches!(rows[k].cells[i].v_merge, engine::VMergeRole::Continue)
                })
            {
                k += 1;
            }
            end = end.max(k);
        }
        col += u32::from(cell.grid_span.max(1));
    }
    end
}

/// The table's indivisible row groups as `[start, end)` ranges, in
/// order, covering every row exactly once. A row is its own unit unless
/// a vertical merge ties it to its neighbours; overlapping / chained
/// merges fold into one unit.
pub(crate) fn row_units(rows: &[TableRowBox]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut s = 0usize;
    while s < rows.len() {
        let mut e = s + 1;
        let mut r = s;
        while r < e {
            e = e.max(vmerge_span_end(rows, r));
            r += 1;
        }
        out.push((s, e));
        s = e;
    }
    out
}

/// Deepest bottom edge of a cell's stacked content.
fn content_bottom(blocks: &[LayoutBlock]) -> f32 {
    blocks
        .iter()
        .map(|b| b.origin().y + b.size().height)
        .fold(0.0_f32, f32::max)
}

/// Cut one cell's content at the deepest line boundary within `budget`
/// (content-box-relative). Whole blocks whose bottom fits stay in the
/// head at their own origins; the first block that does not is a
/// paragraph cut at a line (its head part may be empty) or a nested
/// table that moves whole. The tail opens at `y = 0` and keeps the
/// original gaps between the blocks after the cut. The third value is
/// the input index of the tail's first block (a cut paragraph repeats
/// its own index — both halves are one model paragraph).
fn split_cell_blocks(
    blocks: &[LayoutBlock],
    budget: f32,
) -> (Vec<LayoutBlock>, Vec<LayoutBlock>, usize) {
    let mut head: Vec<LayoutBlock> = Vec::new();
    let mut tail: Vec<LayoutBlock> = Vec::new();
    for (i, b) in blocks.iter().enumerate() {
        let origin = b.origin();
        let h = b.size().height;
        if origin.y + h <= budget {
            head.push(b.clone());
            continue;
        }
        let mut tail_first = i;
        /* The cut block: what of it lands in the tail, and how tall. */
        let mut cut_tail: Option<LayoutBlock> = None;
        match b {
            LayoutBlock::Paragraph(p) => {
                let (hd, tl) = crate::paginate::split_paragraph_at_line(p, budget - origin.y);
                if let Some(hd) = hd {
                    head.push(LayoutBlock::Paragraph(hd));
                }
                match tl {
                    Some(mut tl) => {
                        tl.origin = Point {
                            x: origin.x,
                            y: 0.0,
                        };
                        cut_tail = Some(LayoutBlock::Paragraph(tl));
                    }
                    /* Every line fit (the box only overran by trailing
                    spacing): the paragraph went whole. */
                    None => tail_first = i + 1,
                }
            }
            LayoutBlock::Table(t) => {
                let mut t = t.clone();
                t.origin = Point {
                    x: origin.x,
                    y: 0.0,
                };
                cut_tail = Some(LayoutBlock::Table(t));
            }
        }
        let mut y = cut_tail.as_ref().map_or(0.0, |b| b.size().height);
        let cut_bottom = origin.y + h;
        if let Some(c) = cut_tail {
            tail.push(c);
        }
        let mut prev_bottom = cut_bottom;
        for rest in &blocks[i + 1..] {
            let mut c = rest.clone();
            let o = c.origin();
            y += (o.y - prev_bottom).max(0.0);
            prev_bottom = o.y + c.size().height;
            c.set_origin(Point { x: o.x, y });
            y += c.size().height;
            tail.push(c);
        }
        return (head, tail, tail_first);
    }
    (head, tail, blocks.len())
}

/// The outcome of cutting one oversize row inside its cells.
pub(crate) struct RowSplit {
    pub head: TableRowBox,
    /// `None` when every cell's content fit and only the row's minimum
    /// height overran (the remainder of a `<w:trHeight>` floor is empty
    /// space and is dropped rather than carried as a blank row).
    pub tail: Option<TableRowBox>,
}

/// Which cells must place something for a row cut to be acceptable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SplitRule {
    /// Issue #91 — the row is taller than the room a fresh page offers:
    /// it can never land whole, so ANY cell placing a line is progress.
    AnyCell,
    /// Issue #155 — the row only fails to fit the rest of the page (Word's
    /// default "allow row to break across pages"). The cut is taken only
    /// when EVERY cell that has content keeps at least its first line (or
    /// first block) on this page; otherwise the row moves whole, since a
    /// fresh page may host it better. A cell with no content, and a
    /// vertical-merge `Continue` cell, place nothing and veto nothing.
    EveryCell,
}

/// Issue #155 — a cell's content with its leading `vAlign` offset
/// removed (the layout pass shifts centred / bottom-aligned content down
/// by the slack; content otherwise starts at `y = 0`). The parts of a
/// split row are top-aligned — per-part vertical alignment is issue
/// #158 — so the cut measures the content from the cell's top instead of
/// losing a short centred cell to the tail.
fn top_aligned(blocks: &[LayoutBlock]) -> std::borrow::Cow<'_, [LayoutBlock]> {
    let lead = blocks.first().map_or(0.0, |b| b.origin().y);
    if lead <= 0.0 {
        return std::borrow::Cow::Borrowed(blocks);
    }
    std::borrow::Cow::Owned(
        blocks
            .iter()
            .map(|b| {
                let mut b = b.clone();
                let o = b.origin();
                b.set_origin(Point {
                    x: o.x,
                    y: o.y - lead,
                });
                b
            })
            .collect(),
    )
}

/// Issue #91 / #155 — continue a row inside its cells. Each
/// non-`Continue` cell's content is cut at the deepest line boundary that
/// fits `avail` minus the cell's vertical padding; the head row is as
/// tall as its tallest head cell, the tail row as its tallest tail cell.
/// Returns `None` when `rule` is not met — no cell ([`SplitRule::AnyCell`])
/// or not every cell with content ([`SplitRule::EveryCell`]) placed
/// anything — the caller then moves the row whole or falls back to
/// atomic placement (`OversizeLine`). Both parts are top-aligned (a
/// `vAlign` offset does not survive the cut — issue #158).
pub(crate) fn split_row_in_cells(
    row: &TableRowBox,
    avail: f32,
    rule: SplitRule,
) -> Option<RowSplit> {
    let mut head_cells: Vec<TableCellBox> = Vec::with_capacity(row.cells.len());
    let mut tail_cells: Vec<TableCellBox> = Vec::with_capacity(row.cells.len());
    let mut progressed = false;
    let mut any_tail = false;
    let mut head_h = 0.0_f32;
    let mut tail_h = 0.0_f32;
    for cell in &row.cells {
        if matches!(cell.v_merge, engine::VMergeRole::Continue) {
            head_cells.push(cell.clone());
            tail_cells.push(cell.clone());
            continue;
        }
        let pads = cell.padding_top + cell.padding_bottom;
        let inner = avail - pads;
        let content = top_aligned(&cell.content);
        let (head, tail, tail_first) = if inner > 0.0 {
            split_cell_blocks(&content, inner)
        } else {
            (Vec::new(), content.to_vec(), 0)
        };
        if rule == SplitRule::EveryCell && head.is_empty() && !content.is_empty() {
            return None;
        }
        if !head.is_empty() {
            progressed = true;
            head_h = head_h.max(content_bottom(&head) + pads);
        }
        if !tail.is_empty() {
            any_tail = true;
            tail_h = tail_h.max(content_bottom(&tail) + pads);
        }
        let mut hc = cell.clone();
        hc.content = head;
        head_cells.push(hc);
        let mut tc = cell.clone();
        tc.content = tail;
        tc.content_offset = cell.content_offset + tail_first as u32;
        tail_cells.push(tc);
    }
    if !progressed && any_tail {
        return None;
    }
    if !any_tail {
        /* Only the height floor overran: the head row fills the budget
        and nothing continues. */
        let h = avail.min(row.size.height).max(head_h);
        let mut head = row.clone();
        head.size.height = h;
        for c in head.cells.iter_mut() {
            c.size.height = h;
        }
        return Some(RowSplit { head, tail: None });
    }
    let build = |cells: Vec<TableCellBox>, h: f32| {
        let mut r = row.clone();
        r.size.height = h;
        r.cells = cells;
        for c in r.cells.iter_mut() {
            c.size.height = h;
        }
        r
    };
    Some(RowSplit {
        head: build(head_cells, head_h),
        tail: Some(build(tail_cells, tail_h)),
    })
}

/// Issue #91 — the first row of a fragment that opens inside a merge
/// group inherits `Continue` cells whose `Restart` stayed behind. Turn
/// each into an empty `Restart` stub so the merged region still paints
/// its frame (borders / shading) on the new page. The stub carries no
/// content: the model cell is a `Continue`, and nothing may hit-test or
/// type into it.
pub(crate) fn restub_continuation(row: &mut TableRowBox) {
    for c in row.cells.iter_mut() {
        if matches!(c.v_merge, engine::VMergeRole::Continue) {
            c.v_merge = engine::VMergeRole::Restart;
            c.content.clear();
        }
    }
}

/// Re-derive every `Restart` cell's height from the rows it spans inside
/// this (fragment's) row list: its own row plus the consecutive
/// `Continue` cells below it at the same grid column. On an uncut table
/// this reproduces the layout pass's accumulation exactly.
pub(crate) fn recompute_vmerge_spans(rows: &mut [TableRowBox]) {
    let mut patches: Vec<(usize, usize, f32)> = Vec::new();
    for r in 0..rows.len() {
        let mut col = 0u32;
        for (i, cell) in rows[r].cells.iter().enumerate() {
            if matches!(cell.v_merge, engine::VMergeRole::Restart) {
                let mut h = rows[r].size.height;
                let mut k = r + 1;
                while k < rows.len()
                    && cell_at_grid_col(&rows[k], col).is_some_and(|j| {
                        matches!(rows[k].cells[j].v_merge, engine::VMergeRole::Continue)
                    })
                {
                    h += rows[k].size.height;
                    k += 1;
                }
                patches.push((r, i, h));
            }
            col += u32::from(cell.grid_span.max(1));
        }
    }
    for (r, i, h) in patches {
        rows[r].cells[i].size.height = h;
    }
}

/// Re-stack `rows` from `y = 0`; returns the total height.
pub(crate) fn restack_rows(rows: &mut [TableRowBox]) -> f32 {
    let mut y = 0.0_f32;
    for r in rows.iter_mut() {
        r.origin.y = y;
        y += r.size.height;
    }
    y
}

/// Watchdog progress measure for a table: rows, plus the lines of every
/// cell paragraph. A row continued inside its cells keeps its row count
/// but strictly loses lines, so a legitimate multi-page row never reads
/// as churn.
pub(crate) fn table_progress_units(rows: &[TableRowBox]) -> u64 {
    let mut lines = 0u64;
    for r in rows {
        for c in &r.cells {
            if matches!(c.v_merge, engine::VMergeRole::Continue) {
                continue;
            }
            crate::boxes::for_each_paragraph_in_blocks(&c.content, &mut |p| {
                lines += p.lines.len() as u64;
            });
        }
    }
    ((rows.len() as u64) << 32) | (lines & 0xffff_ffff)
}

/// Issue #155 — an upper bound on the distinct cuts [`split_row_in_cells`]
/// can make in `row`: one per line of every cell paragraph plus one per
/// block. Each shrink step of the paginator's row-part negotiation
/// removes at least one line (or block) from the tallest head cell, so
/// this bounds that loop by construction.
pub(crate) fn row_cut_points(row: &TableRowBox) -> usize {
    let mut n = 0usize;
    for c in &row.cells {
        if matches!(c.v_merge, engine::VMergeRole::Continue) {
            continue;
        }
        n += c.content.len();
        crate::boxes::for_each_paragraph_in_blocks(&c.content, &mut |p| {
            n += p.lines.len();
        });
    }
    n
}
