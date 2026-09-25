//! Issue #79 — right-to-left tables (`<w:tblPr><w:bidiVisual/>`).
//!
//! ECMA-376 §17.4.1: a `bidiVisual` table presents its cells right to
//! left — the first cell of every row is the visually rightmost one. The
//! mirroring is purely *visual*: the grid, the column widths, the spans,
//! the vertical merges and the logical cell order are exactly those of the
//! left-to-right table. So the table is laid out LTR as usual (column
//! widths, cell content, row heights, vMerge extents) and then mirrored
//! ONCE, at the box level, by [`mirror_bidi_visual`]:
//!
//! * every cell's parent-relative `origin.x` is reflected across the table
//!   width (`x' = W − x − w`), so logical column 1 lands at the right edge
//!   and its successors run leftward;
//! * the start/end edges resolve to the right/left *visual* edges: the
//!   cell padding (`<w:tcMar>` / `<w:tblCellMar>` `left|start` /
//!   `right|end`), the cell borders (`<w:tcBorders>`) and the table's
//!   outer borders (`<w:tblBorders>`) swap left ↔ right.
//!
//! Everything downstream — the renderer, PDF export, caret geometry and
//! hit-testing — is a pure traversal of parent-relative origins and the
//! per-cell `padding_*` / `borders` fields, so they follow the mirrored
//! boxes with no direction knowledge of their own. The cell vector stays
//! in logical order, so `Cell { row, col }` paths (and Tab order) are
//! unchanged. The paginator's row split (`table_split`) clones cell boxes
//! verbatim, so continuation fragments and repeated header rows keep the
//! mirrored geometry.
//!
//! Nested tables are mirrored by their own layout call (their own flag);
//! cell content is never touched — text direction inside a cell is the
//! paragraph's business.

use crate::boxes::TableBox;

/// Mirror a left-to-right laid-out [`TableBox`] into its `bidiVisual`
/// (right-to-left) presentation. See the module docs. An involution:
/// applying it twice restores the input.
pub fn mirror_bidi_visual(table: &mut TableBox) {
    let width = table.size.width;
    for row in &mut table.rows {
        for cell in &mut row.cells {
            cell.origin.x = width - cell.origin.x - cell.size.width;
            std::mem::swap(&mut cell.padding_left, &mut cell.padding_right);
            std::mem::swap(&mut cell.borders.left, &mut cell.borders.right);
        }
    }
    let outer = &mut table.outer_borders;
    std::mem::swap(&mut outer.left, &mut outer.right);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::boxes::{
        LayoutBlock, LineBox, ParagraphBox, Point, Size, TableCellBox, TableRowBox,
    };
    use crate::page::A4Page;
    use crate::paginate::{PageGeometry, Paginator};
    use text_pipeline::ShapingDirection;

    fn para(n: usize, line_h: f32) -> ParagraphBox {
        ParagraphBox {
            origin: Point { x: 0.0, y: 0.0 },
            size: Size {
                width: 50.0,
                height: n as f32 * line_h,
            },
            lines: (0..n)
                .map(|i| LineBox {
                    origin: Point {
                        x: 0.0,
                        y: i as f32 * line_h,
                    },
                    baseline: line_h * 0.8,
                    height: line_h,
                    width: 50.0,
                    runs: Vec::new(),
                    alignment: text_pipeline::Alignment::Start,
                    source_start: 0,
                    segments: Vec::new(),
                    segment: 0,
                })
                .collect(),
            direction: ShapingDirection::Rtl,
            marker: None,
            source_paragraph_id: ParagraphBox::NO_SOURCE_ID,
            fields: Vec::new(),
            page_break_after_line: Vec::new(),
            borders: None,
            shading: None,
            keep_next: false,
        }
    }

    fn stroke() -> engine::BorderStroke {
        engine::BorderStroke {
            style: engine::BorderStyle::Single,
            size_eighth_pt: 4,
            color: None,
        }
    }

    /// Cells `(x, width, grid_span)` laid out LTR, `n_lines` per cell.
    fn row(cells: &[(f32, f32, u8)], n_lines: usize, line_h: f32) -> TableRowBox {
        let h = n_lines as f32 * line_h;
        TableRowBox {
            origin: Point { x: 0.0, y: 0.0 },
            size: Size {
                width: 300.0,
                height: h,
            },
            cells: cells
                .iter()
                .map(|&(x, w, span)| TableCellBox {
                    origin: Point { x, y: 0.0 },
                    size: Size {
                        width: w,
                        height: h,
                    },
                    grid_span: span,
                    v_merge: engine::VMergeRole::None,
                    borders: engine::CellBorders {
                        left: Some(stroke()),
                        ..Default::default()
                    },
                    shading: None,
                    content: vec![LayoutBlock::Paragraph(para(n_lines, line_h))],
                    padding_left: 3.0,
                    padding_top: 0.0,
                    padding_right: 7.0,
                    padding_bottom: 0.0,
                    content_offset: 0,
                })
                .collect(),
            header: false,
            cant_split: false,
            source_row: 0,
        }
    }

    fn table(rows: Vec<TableRowBox>) -> TableBox {
        let mut rows = rows;
        let mut y = 0.0;
        for (i, r) in rows.iter_mut().enumerate() {
            r.origin.y = y;
            r.source_row = i as u32;
            y += r.size.height;
        }
        TableBox {
            origin: Point::default(),
            size: Size {
                width: 300.0,
                height: y,
            },
            columns: vec![100.0, 80.0, 120.0],
            rows,
            outer_borders: engine::CellBorders {
                left: Some(stroke()),
                ..Default::default()
            },
        }
    }

    const THREE: &[(f32, f32, u8)] = &[(0.0, 100.0, 1), (100.0, 80.0, 1), (180.0, 120.0, 1)];

    #[test]
    fn column_one_is_rightmost_and_successors_run_leftward() {
        let mut t = table(vec![row(THREE, 1, 10.0)]);
        mirror_bidi_visual(&mut t);
        let xs: Vec<(f32, f32)> = t.rows[0]
            .cells
            .iter()
            .map(|c| (c.origin.x, c.size.width))
            .collect();
        /* Logical order kept, widths kept, positions mirrored. */
        assert_eq!(xs, vec![(200.0, 100.0), (120.0, 80.0), (0.0, 120.0)]);
        assert_eq!(t.columns, vec![100.0, 80.0, 120.0], "grid unchanged");
        let c0 = &t.rows[0].cells[0];
        assert_eq!(c0.origin.x + c0.size.width, t.size.width, "flush right");
    }

    #[test]
    fn start_end_edges_resolve_to_right_left_visual_edges() {
        let mut t = table(vec![row(THREE, 1, 10.0)]);
        mirror_bidi_visual(&mut t);
        for c in &t.rows[0].cells {
            assert_eq!((c.padding_left, c.padding_right), (7.0, 3.0));
            assert!(c.borders.left.is_none() && c.borders.right.is_some());
        }
        assert!(t.outer_borders.left.is_none() && t.outer_borders.right.is_some());
    }

    #[test]
    fn spans_mirror_as_one_block_and_mirroring_is_an_involution() {
        let spanned = row(&[(0.0, 180.0, 2), (180.0, 120.0, 1)], 1, 10.0);
        let mut t = table(vec![row(THREE, 1, 10.0), spanned]);
        let before = format!("{:?}", t.rows);
        mirror_bidi_visual(&mut t);
        let r1 = &t.rows[1].cells;
        assert_eq!((r1[0].origin.x, r1[0].size.width), (120.0, 180.0));
        assert_eq!((r1[1].origin.x, r1[1].size.width), (0.0, 120.0));
        /* The spanned cell's left edge lines up with the column-2 edge
        of the unspanned row above (grid lines stay aligned). */
        assert_eq!(r1[0].origin.x, t.rows[0].cells[1].origin.x);
        mirror_bidi_visual(&mut t);
        assert_eq!(format!("{:?}", t.rows), before);
    }

    /// Issue #91 interplay — a mirrored table taller than a page splits
    /// at row boundaries (and inside a too-tall row's cells); every
    /// fragment on every page keeps column 1 at the right edge.
    #[test]
    fn mirroring_holds_on_continuation_pages() {
        let page = A4Page::a4();
        let geom = PageGeometry {
            width: page.width,
            height: page.height,
            margins: page.margin,
            header_offset: 36.0,
            footer_offset: 36.0,
        };
        let mut rows: Vec<TableRowBox> = (0..80).map(|_| row(THREE, 1, 20.0)).collect();
        /* One row taller than a page → split inside its cells. */
        rows.push(row(THREE, 60, 20.0));
        rows[0].header = true;
        let mut t = table(rows);
        mirror_bidi_visual(&mut t);
        let mut pag = Paginator::with_default_bands(geom, None, None);
        pag.push_block(LayoutBlock::Table(t), 0.0, 0.0);
        let pages = pag.finish();
        assert!(pages.len() >= 3, "{} pages", pages.len());
        let mut fragments = 0;
        for p in &pages {
            for b in &p.blocks {
                let LayoutBlock::Table(tb) = b else { continue };
                fragments += 1;
                for r in &tb.rows {
                    let xs: Vec<f32> = r.cells.iter().map(|c| c.origin.x).collect();
                    assert_eq!(xs, vec![200.0, 120.0, 0.0], "row {}", r.source_row);
                    assert_eq!(r.cells[0].padding_right, 3.0);
                }
            }
        }
        assert!(fragments >= 3);
    }
}
