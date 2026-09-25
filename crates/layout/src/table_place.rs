//! Issue #173 — horizontal table placement (`<w:tblPr><w:jc>` +
//! `<w:tblInd>`).
//!
//! ECMA-376 §17.4.28 (`jc`, table alignment) positions the whole table
//! within the text extents of its band — the column for a body table,
//! the cell's content box for a nested one, the header/footer band, the
//! note area. §17.4.51 (`tblInd`) adds an indentation *before the
//! leading edge* of the table: the left edge of a left-to-right table,
//! the right edge of a `bidiVisual` (right-to-left) one.
//!
//! The resolution runs once, at table layout time, against the width the
//! table was laid out for, and is recorded on the box as
//! [`TableBox::placement_dx`] — the signed offset of the table's left
//! edge from the band's left edge. Every placement site (the paginator's
//! atomic step, every page part of a row split, column balancing, note
//! bodies, cell content, header/footer bands) adds it to the band's own
//! x; hit-testing, caret geometry, paint and PDF export all read the
//! resulting `origin.x`, so they need no knowledge of it.
//!
//! Resolution rules:
//!
//! * `start` (also `left`, which the reader folds into `start` — the
//!   transitional reading, where `left`/`right` on a bidi object are the
//!   leading/trailing edges) and an absent `jc`: the table hugs the
//!   leading edge — left for LTR, right for `bidiVisual` — shifted
//!   inward by `tblInd` (a negative indent pushes it into the margin).
//! * `end` (also `right`): the trailing edge. `tblInd` is not applied —
//!   it is measured from the leading edge, and a trailing/centred table
//!   has no leading-edge anchor for it to shift.
//! * `center`: centred in the band; `tblInd` is not applied.
//! * `both` is not a valid table alignment — treated as `start`.
//!
//! A table that exactly fills the band (autofit, the common case) has
//! zero slack, so `end` / `center` / RTL `start` resolve to zero too; a
//! sub-`PLACEMENT_EPSILON` residue from the autofit distribution is
//! snapped to exactly zero so full-width tables keep a bit-identical
//! origin (pinned geometry fingerprints).

use crate::boxes::TableBox;
use engine::Alignment;

/// Offsets below this magnitude (layout units) are float noise from the
/// column-width distribution, not a placement: snapped to `0.0`.
const PLACEMENT_EPSILON: f32 = 0.01;

/// Resolve the signed x-offset of a table's left edge from the left edge
/// of its band. `indent` is `tblInd` in layout units; `band_width` the
/// width the table was laid out for; `table_width` the table's own width.
pub fn resolve_table_placement(
    alignment: Option<Alignment>,
    indent: f32,
    bidi_visual: bool,
    band_width: f32,
    table_width: f32,
) -> f32 {
    let slack = band_width - table_width;
    let dx = match alignment {
        Some(Alignment::Center) => slack / 2.0,
        Some(Alignment::End) => {
            if bidi_visual {
                0.0
            } else {
                slack
            }
        }
        None | Some(Alignment::Start) | Some(Alignment::Justify) => {
            if bidi_visual {
                slack - indent
            } else {
                indent
            }
        }
    };
    if !dx.is_finite() || dx.abs() < PLACEMENT_EPSILON {
        0.0
    } else {
        dx
    }
}

/// Stamp [`TableBox::placement_dx`] from the table properties. The
/// table's own `origin.x` is set to the same value, so a caller that
/// stacks the box in a band starting at `x = 0` (cell content, header /
/// footer bands) only has to keep `origin.x`; the paginator re-derives
/// `origin.x` as `column x + placement_dx` for every page part.
pub fn place_table(
    table: &mut TableBox,
    alignment: Option<Alignment>,
    indent: f32,
    bidi_visual: bool,
    band_width: f32,
) {
    let dx = resolve_table_placement(alignment, indent, bidi_visual, band_width, table.size.width);
    table.placement_dx = dx;
    table.origin.x = dx;
}

#[cfg(test)]
mod tests {
    use super::*;

    const BAND: f32 = 450.0;
    const W: f32 = 200.0;

    fn dx(a: Option<Alignment>, ind: f32, rtl: bool) -> f32 {
        resolve_table_placement(a, ind, rtl, BAND, W)
    }

    #[test]
    fn ltr_alignments_resolve_against_the_left_edge() {
        assert_eq!(dx(None, 0.0, false), 0.0);
        assert_eq!(dx(Some(Alignment::Start), 0.0, false), 0.0);
        assert_eq!(dx(Some(Alignment::Center), 0.0, false), 125.0);
        assert_eq!(dx(Some(Alignment::End), 0.0, false), 250.0);
        assert_eq!(dx(Some(Alignment::Justify), 0.0, false), 0.0);
    }

    #[test]
    fn indent_applies_from_the_leading_edge_only() {
        assert_eq!(dx(None, 36.0, false), 36.0);
        assert_eq!(dx(Some(Alignment::Start), -5.4, false), -5.4);
        /* Not applied to the trailing / centred alignments. */
        assert_eq!(dx(Some(Alignment::Center), 36.0, false), 125.0);
        assert_eq!(dx(Some(Alignment::End), 36.0, false), 250.0);
        /* RTL: the leading edge is the right one — indent pushes left. */
        assert_eq!(dx(None, 36.0, true), 214.0);
        assert_eq!(dx(Some(Alignment::End), 36.0, true), 0.0);
    }

    #[test]
    fn bidi_visual_mirrors_start_and_end() {
        assert_eq!(dx(None, 0.0, true), 250.0, "default start = right edge");
        assert_eq!(dx(Some(Alignment::Start), 0.0, true), 250.0);
        assert_eq!(dx(Some(Alignment::End), 0.0, true), 0.0);
        assert_eq!(dx(Some(Alignment::Center), 0.0, true), 125.0);
    }

    #[test]
    fn full_width_tables_resolve_to_exactly_zero() {
        for a in [
            None,
            Some(Alignment::Start),
            Some(Alignment::Center),
            Some(Alignment::End),
        ] {
            for rtl in [false, true] {
                let v = resolve_table_placement(a, 0.0, rtl, BAND, BAND - 0.0004);
                assert_eq!(v.to_bits(), 0.0f32.to_bits(), "{a:?} rtl={rtl}");
            }
        }
    }

    #[test]
    fn overflowing_tables_overhang_away_from_the_leading_edge() {
        /* Wider than the band: an LTR start table overhangs the right
        margin, a centred one both sides, an RTL start table grows
        leftward past the left margin. */
        assert_eq!(resolve_table_placement(None, 0.0, false, 100.0, 140.0), 0.0);
        assert_eq!(
            resolve_table_placement(Some(Alignment::Center), 0.0, false, 100.0, 140.0),
            -20.0
        );
        assert_eq!(
            resolve_table_placement(None, 0.0, true, 100.0, 140.0),
            -40.0
        );
    }

    /* ---------- paginator: every page part keeps the placement ---------- */

    use crate::boxes::{
        LayoutBlock, LineBox, ParagraphBox, Point, Size, TableCellBox, TableRowBox,
    };
    use crate::page::A4Page;
    use crate::paginate::{PageGeometry, Paginator};

    fn para(n: usize, line_h: f32) -> ParagraphBox {
        ParagraphBox {
            origin: Point::default(),
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
            direction: text_pipeline::ShapingDirection::Ltr,
            marker: None,
            source_paragraph_id: ParagraphBox::NO_SOURCE_ID,
            fields: Vec::new(),
            page_break_after_line: Vec::new(),
            borders: None,
            shading: None,
            keep_next: false,
            flow: crate::boxes::ParaFlow::default(),
        }
    }

    fn row(n_lines: usize, line_h: f32, header: bool) -> TableRowBox {
        let h = n_lines as f32 * line_h;
        TableRowBox {
            origin: Point::default(),
            size: Size {
                width: W,
                height: h,
            },
            cells: vec![TableCellBox {
                origin: Point::default(),
                size: Size {
                    width: W,
                    height: h,
                },
                grid_span: 1,
                v_merge: engine::VMergeRole::None,
                borders: engine::CellBorders::default(),
                shading: None,
                content: vec![LayoutBlock::Paragraph(para(n_lines, line_h))],
                padding_left: 0.0,
                padding_top: 0.0,
                padding_right: 0.0,
                padding_bottom: 0.0,
                content_offset: 0,
            }],
            header,
            cant_split: false,
            source_row: 0,
            exact_height: false,
        }
    }

    fn table(rows: Vec<TableRowBox>) -> TableBox {
        let mut y = 0.0;
        let rows: Vec<TableRowBox> = rows
            .into_iter()
            .enumerate()
            .map(|(i, mut r)| {
                r.origin.y = y;
                r.source_row = i as u32;
                y += r.size.height;
                r
            })
            .collect();
        TableBox {
            origin: Point::default(),
            size: Size {
                width: W,
                height: y,
            },
            columns: vec![W],
            rows,
            outer_borders: engine::CellBorders::default(),
            placement_dx: 0.0,
        }
    }

    fn geom() -> PageGeometry {
        let page = A4Page::a4();
        PageGeometry {
            width: page.width,
            height: page.height,
            margins: page.margin,
            header_offset: 36.0,
            footer_offset: 36.0,
        }
    }

    fn table_xs(pages: &[crate::boxes::PageBox]) -> Vec<(usize, f32)> {
        pages
            .iter()
            .enumerate()
            .flat_map(|(i, p)| {
                p.blocks
                    .iter()
                    .filter_map(LayoutBlock::as_table)
                    .map(move |t| (i, t.origin.x))
            })
            .collect()
    }

    /// A right-aligned table taller than a page: the row split (with a
    /// repeated header) and the in-cell continuation of a too-tall row
    /// put every page part at the same x.
    #[test]
    fn every_page_part_of_a_split_table_keeps_the_offset() {
        let g = geom();
        let band = g.width - g.margins.left - g.margins.right;
        let mut rows: Vec<TableRowBox> = (0..80).map(|i| row(1, 20.0, i == 0)).collect();
        rows.push(row(60, 20.0, false));
        let mut t = table(rows);
        place_table(&mut t, Some(Alignment::End), 0.0, false, band);
        let want = band - W;
        assert!(want > 100.0);
        let mut pag = Paginator::with_default_bands(g, None, None);
        pag.push_block(LayoutBlock::Paragraph(para(2, 20.0)), 0.0, 0.0);
        pag.push_block(LayoutBlock::Table(t), 0.0, 0.0);
        let pages = pag.finish();
        let xs = table_xs(&pages);
        assert!(xs.len() >= 3, "split into several parts: {xs:?}");
        assert!(xs.last().unwrap().0 >= 2, "{xs:?}");
        for (page, x) in &xs {
            assert_eq!(*x, want, "page {page}");
        }
        /* Paragraphs stay at the column edge. */
        assert_eq!(pages[0].blocks[0].origin().x, 0.0);
    }

    /// A multi-column section: a table snaking into column 2 sits at
    /// column 2's x plus its own offset.
    #[test]
    fn offset_is_relative_to_the_column_the_part_lands_in() {
        let mut pag = Paginator::with_default_bands(geom(), None, None);
        pag.set_columns(2, 12.0);
        let cw = pag.column_width();
        let w = cw / 2.0;
        let mut rows: Vec<TableRowBox> = (0..60).map(|_| row(1, 20.0, false)).collect();
        for r in &mut rows {
            r.size.width = w;
            r.cells[0].size.width = w;
        }
        let mut t = table(rows);
        t.size.width = w;
        place_table(&mut t, Some(Alignment::Center), 0.0, false, cw);
        let dx = t.placement_dx;
        assert!((dx - w / 2.0).abs() < 1e-3);
        pag.push_block(LayoutBlock::Table(t), 0.0, 0.0);
        let pages = pag.finish();
        let xs = table_xs(&pages);
        assert!(xs.len() >= 2, "{xs:?}");
        let col1 = cw + 12.0;
        assert_eq!(xs[0], (0, dx));
        assert_eq!(xs[1], (0, col1 + dx));
    }

    /// The pre-#173 placement: a start-aligned LTR table with no indent
    /// stays flush with the column edge (bit-identical `0.0`).
    #[test]
    fn default_placement_is_the_column_edge() {
        let mut t = table(vec![row(2, 20.0, false)]);
        place_table(&mut t, None, 0.0, false, 450.0);
        let mut pag = Paginator::with_default_bands(geom(), None, None);
        pag.push_block(LayoutBlock::Table(t), 0.0, 0.0);
        let pages = pag.finish();
        assert_eq!(table_xs(&pages), vec![(0, 0.0)]);
    }

    #[test]
    fn place_table_stamps_offset_and_origin() {
        let mut t = TableBox {
            origin: crate::boxes::Point { x: 0.0, y: 7.0 },
            size: crate::boxes::Size {
                width: W,
                height: 10.0,
            },
            columns: vec![W],
            rows: Vec::new(),
            outer_borders: engine::CellBorders::default(),
            placement_dx: 0.0,
        };
        place_table(&mut t, Some(Alignment::Center), 0.0, false, BAND);
        assert_eq!(
            (t.placement_dx, t.origin.x, t.origin.y),
            (125.0, 125.0, 7.0)
        );
    }
}
