//! Tables: which paragraphs are table cells and rows, and the assembly of
//! the main-document paragraphs into [`engine::Table`] blocks ([MS-DOC]
//! 2.4.3 Overview of Tables; 2.6.2 Paragraph Properties for the
//! membership sprms; 2.6.3 Table Properties through [`crate::tap`]).
//!
//! Written from the specification. A table has no start or end marker in
//! the text: a paragraph's TABLE DEPTH (`sprmPFInTable`, `sprmPItap`,
//! `sprmPDtap`) says it belongs to a table, and two kinds of marks
//! structure it —
//!
//! - a CELL MARK ends the last paragraph of a cell: character 0x07 at depth
//!   1, a paragraph mark (0x0D) with `sprmPFInnerTableCell` deeper;
//! - a TTP MARK (table terminating paragraph, the row mark) follows a
//!   row's cells: 0x07 with `sprmPFTtp` at depth 1, 0x0D with
//!   `sprmPFInnerTtp` deeper. It belongs to no cell and carries the row's
//!   table properties ([`Tap`]).
//!
//! A cell holds paragraphs of its depth and tables one level deeper;
//! consecutive rows of one depth form one table unless their direction or
//! table style differ (2.4.3). The assembly is one pass with an explicit
//! stack of open tables (no recursion), bounded to the depth the `.docx`
//! model nests ([`MAX_TABLE_DEPTH`]); paragraphs below it stay in the
//! deepest modelled cell as plain paragraphs and are reported.
//!
//! Geometry: every row mark gives its cells' edges (`rgdxaCenter`, twips
//! from the logical left margin). The table grid is the union of every
//! row's edges, so a cell spanning several columns of another row gets
//! the matching `grid_span` (how Word 2000 and later store a horizontal
//! merge); the Word 97 merge flags (`TCGRF.horzMerge`, `sprmTMerge`)
//! fold the merged cells into their first one. The leading edge is the
//! table indent.

use engine::{
    Block, CellBorders, CellProperties, CellWidth, MAX_POINT_FIELD_TWIPS, Paragraph, RowProperties,
    Table, TableCell, TableLayout, TableProperties, TableRow, VMergeRole, VerticalAlign,
};

use crate::le::u32_at;
use crate::reader::ParaOut;
use crate::sprm::Prl;
use crate::tap::{CellDef, HORZ_MERGED, Tap};

/// Deepest table depth (`sprmPItap`) built as a table: the depth the
/// `.docx` reader's model follows (its outermost table is nesting level
/// 0, so depths 1 ..= 64 here).
pub(crate) const MAX_TABLE_DEPTH: u32 = format_docx::parts::table::MAX_TABLE_NESTING_DEPTH;

/// Most grid columns one table gets: a cell's `grid_span` is a `u8`. A
/// table whose rows' edges add up to more falls back to one column per
/// cell (see [`build_table`]).
const MAX_GRID_COLUMNS: usize = u8::MAX as usize;

/// The width a table whose rows define no edges at all is spread over:
/// the text width of the default section (8.5 in less 1.25 in margins).
const DEFAULT_TABLE_TWIPS: i32 = 8640;

// Paragraph sprms of table membership (2.6.2).
const P_F_IN_TABLE: u16 = 0x2416;
const P_F_TTP: u16 = 0x2417;
const P_ITAP: u16 = 0x6649;
const P_DTAP: u16 = 0x664A;
const P_F_INNER_TABLE_CELL: u16 = 0x244B;
const P_F_INNER_TTP: u16 = 0x244C;

/// A paragraph's table membership, from its direct paragraph sprms.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Membership {
    /// `sprmPFInTable`.
    in_table: bool,
    /// `sprmPItap`, adjusted by every later `sprmPDtap`.
    itap: Option<i64>,
    /// `sprmPFTtp`: this cell mark is a row mark.
    ttp: bool,
    /// `sprmPFInnerTableCell`: this paragraph mark is a nested cell mark.
    inner_cell: bool,
    /// `sprmPFInnerTtp`: this paragraph mark is a nested row mark.
    inner_ttp: bool,
}

/// What a paragraph's mark ends.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum MarkKind {
    /// A paragraph (inside a cell when the depth is not zero).
    #[default]
    Paragraph,
    /// The last paragraph of a cell.
    CellEnd,
    /// A row (the TTP mark; not a paragraph of any cell).
    RowEnd,
}

/// A paragraph's place in the table structure.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct TableMark {
    /// The table depth: 0 outside tables.
    pub(crate) depth: u32,
    pub(crate) kind: MarkKind,
}

impl Membership {
    /// Apply one paragraph sprm (any other sprm is ignored).
    pub(crate) fn apply(&mut self, p: &Prl) {
        let on = p.operand.first().is_some_and(|&v| v != 0);
        let int = || u32_at(p.operand, 0).map(|v| i64::from(v as i32));
        match p.op {
            P_F_IN_TABLE => self.in_table = on,
            P_F_TTP => self.ttp = on,
            P_F_INNER_TABLE_CELL => self.inner_cell = on,
            P_F_INNER_TTP => self.inner_ttp = on,
            /* A depth MUST be non-negative: a negative one reads as 0. */
            P_ITAP => {
                if let Some(v) = int() {
                    self.itap = Some(v.max(0));
                }
            }
            P_DTAP => {
                if let Some(v) = int() {
                    let base = self.itap.unwrap_or(i64::from(self.in_table));
                    self.itap = Some(base.saturating_add(v).max(0));
                }
            }
            _ => {}
        }
    }

    /// The table depth: `sprmPItap` (with its `sprmPDtap` adjustments),
    /// at least 1 when `sprmPFInTable` is set (Word 97 writes no depth).
    fn depth(&self) -> u32 {
        let d = self.itap.unwrap_or(0).max(i64::from(self.in_table));
        u32::try_from(d).unwrap_or(u32::MAX)
    }

    /// What the paragraph ending in `mark` (0x0D, 0x07, or a section
    /// mark) is (2.4.3). A 0x07 is a cell mark whatever the sprms say —
    /// at depth 1 at least.
    pub(crate) fn mark(&self, mark: char) -> TableMark {
        let depth = self.depth();
        if mark == '\u{7}' {
            let depth = depth.max(1);
            let row = self.ttp || (depth > 1 && self.inner_ttp);
            return TableMark {
                depth,
                kind: if row {
                    MarkKind::RowEnd
                } else {
                    MarkKind::CellEnd
                },
            };
        }
        let kind = if depth > 1 && self.inner_ttp {
            MarkKind::RowEnd
        } else if depth > 1 && self.inner_cell {
            MarkKind::CellEnd
        } else {
            MarkKind::Paragraph
        };
        TableMark { depth, kind }
    }
}

/// The blocks of the main document and what was flattened.
pub(crate) struct Assembled {
    pub(crate) blocks: Vec<Block>,
    /// Paragraphs of tables nested deeper than [`MAX_TABLE_DEPTH`]: kept
    /// as plain paragraphs of the deepest modelled cell.
    pub(crate) flattened: u32,
}

/// One row as the text gave it: its cells' blocks and its row mark's
/// table properties (`None` for a row whose mark never came).
struct RowIn {
    cells: Vec<Vec<Block>>,
    tap: Option<Box<Tap>>,
}

/// One open table on the assembly stack.
#[derive(Default)]
struct Level {
    rows: Vec<RowIn>,
    /// The finished cells of the row being read.
    cells: Vec<Vec<Block>>,
    /// The blocks of the cell being read.
    cur: Vec<Block>,
}

/// Group the main document's paragraphs into blocks: paragraphs at depth
/// 0 stay paragraphs, the rest become tables (nested ones in their
/// cells).
pub(crate) fn assemble(paras: Vec<ParaOut>) -> Assembled {
    let mut out: Vec<Block> = Vec::new();
    let mut stack: Vec<Level> = Vec::new();
    let mut flattened = 0u32;
    for p in paras {
        let ParaOut {
            paragraph,
            mark,
            tap,
            ..
        } = p;
        let (depth, kind) = if mark.depth > MAX_TABLE_DEPTH {
            /* Below the modelled depth: the paragraph stays in the deepest
            cell, its cell / row marks no boundaries. A row mark holds no
            text of its own. */
            if mark.kind == MarkKind::RowEnd && paragraph.text.is_empty() {
                continue;
            }
            flattened = flattened.saturating_add(1);
            (MAX_TABLE_DEPTH, MarkKind::Paragraph)
        } else {
            (mark.depth, mark.kind)
        };
        let depth = depth as usize;
        while stack.len() > depth {
            close(&mut stack, &mut out);
        }
        while stack.len() < depth {
            stack.push(Level::default());
        }
        let Some(level) = stack.last_mut() else {
            out.push(Block::Paragraph(paragraph));
            continue;
        };
        match kind {
            MarkKind::Paragraph => level.cur.push(Block::Paragraph(paragraph)),
            MarkKind::CellEnd => {
                level.cur.push(Block::Paragraph(paragraph));
                let cell = std::mem::take(&mut level.cur);
                level.cells.push(cell);
            }
            MarkKind::RowEnd => {
                /* The row mark is no paragraph of any cell; text in one (a
                damaged file) ends one more cell rather than vanish. */
                if !paragraph.text.is_empty() {
                    level.cur.push(Block::Paragraph(paragraph));
                }
                if !level.cur.is_empty() {
                    let cell = std::mem::take(&mut level.cur);
                    level.cells.push(cell);
                }
                /* A row mark with no cell before it has no row to end. */
                if level.cells.is_empty() {
                    continue;
                }
                /* Two adjacent rows belong to different tables when their
                direction or table style differ (2.4.3). */
                let other_table = level.rows.last().is_some_and(|prev| {
                    matches!((prev.tap.as_deref(), tap.as_deref()),
                        (Some(a), Some(b)) if a.bidi() != b.bidi() || a.istd != b.istd)
                });
                if other_table {
                    let cells = std::mem::take(&mut level.cells);
                    close(&mut stack, &mut out);
                    stack.push(Level {
                        cells,
                        ..Level::default()
                    });
                }
                if let Some(level) = stack.last_mut() {
                    let cells = std::mem::take(&mut level.cells);
                    level.rows.push(RowIn { cells, tap });
                }
            }
        }
    }
    while !stack.is_empty() {
        close(&mut stack, &mut out);
    }
    Assembled {
        blocks: out,
        flattened,
    }
}

/// Close the innermost open table: a row whose mark never came (the text
/// ended inside it, or a damaged file) still ends, with no table
/// properties; the table goes into the cell being read one level up, or
/// into the body.
fn close(stack: &mut Vec<Level>, out: &mut Vec<Block>) {
    let Some(mut level) = stack.pop() else {
        return;
    };
    if !level.cur.is_empty() {
        level.cells.push(std::mem::take(&mut level.cur));
    }
    if !level.cells.is_empty() {
        level.rows.push(RowIn {
            cells: std::mem::take(&mut level.cells),
            tap: None,
        });
    }
    if level.rows.is_empty() {
        return;
    }
    let table = Block::Table(build_table(level.rows));
    match stack.last_mut() {
        Some(parent) => parent.cur.push(table),
        None => out.push(table),
    }
}

/// `true` when a merged-away cell's block is worth keeping in the cell
/// that absorbs it: its own contents are not rendered (2.9.317), so only
/// what would otherwise be lost — text or a table — moves.
fn has_content(b: &Block) -> bool {
    match b {
        Block::Paragraph(p) => !p.text.is_empty(),
        Block::Table(_) => true,
    }
}

/// A cell's blocks as the model needs them: at least one, ending in a
/// paragraph (the cell mark's paragraph always does; a cell cut short by
/// a damaged file might not).
fn cell_blocks(mut blocks: Vec<Block>) -> Vec<Block> {
    if !matches!(blocks.last(), Some(Block::Paragraph(_))) {
        blocks.push(Block::Paragraph(Paragraph::default()));
    }
    blocks
}

/// One cell of a row after the row's horizontal merges.
struct MergedCell {
    blocks: Vec<Block>,
    /// Cell definitions `first .. end` (a merged set spans several).
    first: usize,
    end: usize,
}

/// Fold every cell whose definition says it is merged into the cell
/// before it (`TCGRF.horzMerge`, `sprmTMerge`) into that cell.
fn merged_cells(cells: Vec<Vec<Block>>, defs: &[CellDef]) -> Vec<MergedCell> {
    let mut out: Vec<MergedCell> = Vec::with_capacity(cells.len());
    for (i, blocks) in cells.into_iter().enumerate() {
        let merged = defs.get(i).is_some_and(|d| d.horz_merge == HORZ_MERGED);
        match out.last_mut() {
            Some(owner) if merged => {
                owner.end = i + 1;
                owner.blocks.extend(blocks.into_iter().filter(has_content));
            }
            _ => out.push(MergedCell {
                blocks,
                first: i,
                end: i + 1,
            }),
        }
    }
    out
}

/// The edges of a row's `n` cells, strictly increasing: the row mark's
/// `rgdxaCenter`, a cell past its definitions taking the row's average
/// width. `None` when the row defines no cell.
fn row_edges(n: usize, tap: Option<&Tap>) -> Option<Vec<i32>> {
    let defined = tap.map_or(&[][..], |t| &t.edges[..]);
    let (&first, &last) = (defined.first()?, defined.last()?);
    if defined.len() < 2 {
        return None;
    }
    let avg = ((last - first) / (defined.len() as i32 - 1)).max(1);
    let mut edges = Vec::with_capacity(n + 1);
    edges.push(first);
    let mut prev = first;
    for i in 1..=n {
        let want = defined.get(i).copied().unwrap_or(prev.saturating_add(avg));
        prev = want.max(prev.saturating_add(1));
        edges.push(prev);
    }
    Some(edges)
}

/// `n` cells spread evenly over `from .. to`.
fn spread_edges(n: usize, from: i32, to: i32) -> Vec<i32> {
    let n32 = n.max(1) as i64;
    let mut prev = i32::MIN;
    (0..=n)
        .map(|i| {
            let e = i64::from(from) + (i64::from(to) - i64::from(from)) * i as i64 / n32;
            let e = i32::try_from(e).unwrap_or(to);
            prev = if i == 0 {
                e
            } else {
                e.max(prev.saturating_add(1))
            };
            prev
        })
        .collect()
}

/// The model of one cell. `geometric` is the width its layout region
/// covers (`None` in a table without edges); a percentage or automatic
/// preferred width is kept as such, any other is the region's width.
fn cell(
    blocks: Vec<Block>,
    defs: &[CellDef],
    first: usize,
    end: usize,
    span: u8,
    geometric: Option<i32>,
) -> TableCell {
    let def = defs.get(first).cloned().unwrap_or_default();
    let last = end.checked_sub(1).and_then(|i| defs.get(i));
    let [top, left, bottom, right] = def.borders.clone();
    let right = last.map_or(right, |l| l.borders[3].clone());
    let any = top.is_some() || left.is_some() || bottom.is_some() || right.is_some();
    let width = match def.preferred_width() {
        Some(w @ (CellWidth::Pct(_) | CellWidth::Auto)) => Some(w),
        preferred => geometric
            .map(|w| CellWidth::Dxa(w.min(MAX_POINT_FIELD_TWIPS)))
            .or(preferred),
    };
    TableCell {
        props: CellProperties {
            grid_span: span.max(1),
            v_merge: match def.vert_merge {
                1 => VMergeRole::Continue,
                3 => VMergeRole::Restart,
                _ => VMergeRole::None,
            },
            width,
            borders: any.then_some(CellBorders {
                top,
                left,
                bottom,
                right,
                inside_h: None,
                inside_v: None,
            }),
            shading: def.shading.flatten(),
            v_align: match def.v_align {
                1 => VerticalAlign::Center,
                2 => VerticalAlign::Bottom,
                _ => VerticalAlign::Top,
            },
            cell_margins: def.margins,
            grab_bag: None,
        },
        blocks: cell_blocks(blocks),
        source_markup: None,
    }
}

/// Build one table from its rows.
///
/// The grid is the union of every row's cell edges (a row without edges
/// is spread over the first laid-out row's extent); each cell spans the
/// grid columns between its edges, the first cell of a row from the
/// table's leading edge (the model has no "grid before", issue #624: a
/// row that starts further in widens its first cell instead of shifting
/// its columns out of line). The indent is that leading edge. A table whose
/// rows define no edges at all, or whose edges would need more than
/// [`MAX_GRID_COLUMNS`] grid columns, gets one even column per cell and
/// is laid out automatically.
fn build_table(rows: Vec<RowIn>) -> Table {
    let edges: Vec<Option<Vec<i32>>> = rows
        .iter()
        .map(|r| row_edges(r.cells.len(), r.tap.as_deref()))
        .collect();
    let extent = edges
        .iter()
        .flatten()
        .next()
        .and_then(|e| Some((*e.first()?, *e.last()?)));
    let edges: Option<Vec<Vec<i32>>> = extent.map(|(from, to)| {
        rows.iter()
            .zip(edges)
            .map(|(r, e)| e.unwrap_or_else(|| spread_edges(r.cells.len(), from, to)))
            .collect()
    });
    let mut grid_edges: Vec<i32> = edges.iter().flatten().flatten().copied().collect();
    grid_edges.sort_unstable();
    grid_edges.dedup();
    let gridded = edges.is_some() && grid_edges.len() - 1 <= MAX_GRID_COLUMNS;
    let col_of = |x: i32| grid_edges.binary_search(&x).unwrap_or_else(|i| i);

    let first_tap = rows.iter().find_map(|r| r.tap.as_deref()).cloned();
    let mut out_rows: Vec<TableRow> = Vec::with_capacity(rows.len());
    let mut max_cells = 1usize;
    for (ri, row) in rows.into_iter().enumerate() {
        let tap = row.tap.as_deref();
        let defs = tap.map_or(&[][..], |t| &t.cells[..]);
        let row_edges = edges.as_ref().and_then(|e| e.get(ri));
        let merged = merged_cells(row.cells, defs);
        max_cells = max_cells.max(merged.len());
        let cells = merged
            .into_iter()
            .enumerate()
            .map(|(k, m)| {
                let edge = |i: usize| row_edges.and_then(|e| e.get(i).copied());
                let (span, geometric) = match (edge(m.first), edge(m.end)) {
                    (Some(l), Some(r)) if gridded => {
                        let left = if k == 0 { 0 } else { col_of(l) };
                        let right = col_of(r).max(left + 1);
                        let width = grid_edges[right.min(grid_edges.len() - 1)] - grid_edges[left];
                        ((right - left) as u8, Some(width))
                    }
                    (Some(l), Some(r)) => (1, Some(r - l)),
                    _ => (1, None),
                };
                cell(m.blocks, defs, m.first, m.end, span, geometric)
            })
            .collect();
        /* A header row only counts as one in an unbroken run from the
        first row (2.6.3 sprmTTableHeader). */
        let header = tap.is_some_and(|t| t.header)
            && out_rows
                .last()
                .is_none_or(|prev: &TableRow| prev.props.header);
        out_rows.push(TableRow {
            props: RowProperties {
                height: tap.and_then(|t| t.height),
                cant_split: tap.is_some_and(Tap::cant_split),
                header,
                ..RowProperties::default()
            },
            cells,
            source_markup: None,
        });
    }

    let grid: Vec<i32> = if gridded {
        grid_edges.windows(2).map(|w| w[1] - w[0]).collect()
    } else {
        let per_col =
            (DEFAULT_TABLE_TWIPS / max_cells.min(DEFAULT_TABLE_TWIPS as usize) as i32).max(720);
        vec![per_col; max_cells]
    };
    let tap = first_tap.unwrap_or_default();
    /* The leading edge: the grid's first edge, else the origin
    sprmTDxaLeft / sprmTDxaGapHalf give. */
    let indent = if gridded {
        grid_edges[0]
    } else {
        tap.dxa_left
            .map_or(0, |l| l - tap.dxa_gap_half.unwrap_or(0))
    };
    let borders = tap.table_borders.clone().filter(|b| {
        [
            &b.top,
            &b.left,
            &b.bottom,
            &b.right,
            &b.inside_h,
            &b.inside_v,
        ]
        .iter()
        .any(|e| e.is_some())
    });
    Table {
        grid,
        props: TableProperties {
            width: tap.width,
            alignment: tap.alignment(),
            indent_twips: indent.clamp(-MAX_POINT_FIELD_TWIPS, MAX_POINT_FIELD_TWIPS),
            borders,
            cell_margins: tap.padding,
            table_style_id: None,
            /* fAutofit off (the default) keeps the column widths the
            file laid out: a fixed layout. */
            layout: if gridded && !tap.autofit {
                TableLayout::Fixed
            } else {
                TableLayout::Autofit
            },
            bidi_visual: tap.bidi(),
            grab_bag: None,
        },
        rows: out_rows,
        /* Built by the importer: no source bytes to replay. */
        dirty: true,
        source_xml: None,
        body_xml: None,
        source_markup: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn membership(sprms: &[(u16, &[u8])]) -> Membership {
        let mut m = Membership::default();
        for (op, operand) in sprms {
            m.apply(&Prl { op: *op, operand });
        }
        m
    }

    #[test]
    fn marks_follow_the_depth_and_the_inner_flags() {
        let none = Membership::default();
        assert_eq!(none.mark('\r'), TableMark::default());
        /* A bare 0x07 is a depth-1 cell mark. */
        assert_eq!(
            none.mark('\u{7}'),
            TableMark {
                depth: 1,
                kind: MarkKind::CellEnd
            }
        );
        let ttp = membership(&[(P_F_IN_TABLE, &[1]), (P_F_TTP, &[1])]);
        assert_eq!(ttp.mark('\u{7}').kind, MarkKind::RowEnd);
        /* A TTP flag on a paragraph mark is not a row mark at depth 1. */
        assert_eq!(ttp.mark('\r').kind, MarkKind::Paragraph);
        let inner = membership(&[
            (P_F_IN_TABLE, &[1]),
            (P_ITAP, &2u32.to_le_bytes()),
            (P_F_INNER_TABLE_CELL, &[1]),
        ]);
        assert_eq!(
            inner.mark('\r'),
            TableMark {
                depth: 2,
                kind: MarkKind::CellEnd
            }
        );
        let inner_ttp = membership(&[
            (P_ITAP, &3u32.to_le_bytes()),
            (P_DTAP, &(-1i32).to_le_bytes()),
            (P_F_INNER_TABLE_CELL, &[1]),
            (P_F_INNER_TTP, &[1]),
        ]);
        assert_eq!(
            inner_ttp.mark('\r'),
            TableMark {
                depth: 2,
                kind: MarkKind::RowEnd
            }
        );
        /* A negative depth reads as 0; a huge one stays huge (past the
        model's depth), never wraps. */
        let neg = membership(&[(P_ITAP, &(-5i32).to_le_bytes())]);
        assert_eq!(neg.mark('\r').depth, 0);
        let huge = membership(&[
            (P_ITAP, &i32::MAX.to_le_bytes()),
            (P_DTAP, &i32::MAX.to_le_bytes()),
        ]);
        assert_eq!(huge.mark('\r').depth, 2 * (i32::MAX as u32));
    }

    #[test]
    fn edges_are_strictly_increasing_and_extend_past_the_definitions() {
        let mut tap = Tap::default();
        tap.edges = vec![0, 100, 100, 50];
        assert_eq!(
            row_edges(5, Some(&tap)),
            Some(vec![0, 100, 101, 102, 118, 134])
        );
        assert_eq!(row_edges(2, None), None);
        assert_eq!(spread_edges(3, 0, 90), vec![0, 30, 60, 90]);
        assert_eq!(spread_edges(3, 0, 1), vec![0, 1, 2, 3]);
    }
}
