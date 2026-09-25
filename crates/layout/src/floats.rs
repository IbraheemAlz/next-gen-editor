//! Issue #69 — floating-object placement (`<wp:anchor>`).
//!
//! A floating image is anchored to a byte in a paragraph (the U+FFFC
//! sentinel glyph carries a [`FloatGlyph`]) but positioned against a
//! *reference frame* on the page: the page itself, the margins, the text
//! column, the anchor paragraph, the anchor line or the anchor character
//! (ECMA-376 §20.4.3.4 / §20.4.3.5). This module resolves every float on
//! a finished page into page-relative [`FloatBox`]es.
//!
//! **Pure, single pass.** A float's position is a function of the
//! already-placed blocks — this resolver never iterates. Text wrap (issue
//! #82) lives *around* it: `crate::wrap` turns the resolved boxes into
//! per-paragraph cutouts for the next layout pass and `WrapConvergence`
//! drives the anchor→position→wrap→reflow loop to a fixpoint with its own
//! termination rules and `DegradeReason`s (render-rules self-defense
//! doctrine). The wrap contract rides each box (`FloatBox::wrap`).

use crate::boxes::{
    CellAnchorRef, FloatAnchorRef, FloatBox, FloatGlyph, FloatOffsetPx, LayoutBlock, PageBox,
    ParagraphBox, Point, Size, TableBox,
};
use engine::{FloatAlign, HRelativeFrom, VRelativeFrom};

/// The column descriptor of the section the page belongs to — the
/// `relativeFrom="column"` frame. Single-column sections use
/// `count == 1`, which makes the column the whole content area.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColumnLayout {
    pub count: u8,
    /// Gutter between adjacent columns, in layout px.
    pub gutter: f32,
}

impl Default for ColumnLayout {
    fn default() -> Self {
        Self {
            count: 1,
            gutter: 0.0,
        }
    }
}

/// Where the anchor glyph of a float sits, in page-relative px, plus the
/// frames derived from its paragraph / line.
#[derive(Debug, Clone, Copy)]
struct AnchorSite {
    /// Left edge of the anchor glyph.
    glyph_x: f32,
    /// Top edge and height of the anchor line.
    line_top: f32,
    line_height: f32,
    /// Top edge and height of the anchor paragraph box.
    para_top: f32,
    para_height: f32,
    /// Left edge and width of the block the paragraph lays in — the
    /// column for body paragraphs, the cell content box inside a table.
    column_x: f32,
    column_w: f32,
}

/// Resolve every float anchored on `page` into positioned boxes. Pure:
/// reads the page's blocks + bands, never mutates them. Output order is
/// anchor order (blocks first, then header, then footer); the renderer
/// sorts by `z_order` itself.
pub fn resolve_page_floats(page: &PageBox, columns: ColumnLayout) -> Vec<FloatBox> {
    let mut out: Vec<FloatBox> = Vec::new();
    let content_x = page.margins.left;
    let content_y = page.margins.top;
    let content_w = page.size.width - page.margins.left - page.margins.right;
    let (col_count, col_gutter) = (columns.count.max(1) as f32, columns.gutter.max(0.0));
    let col_w = ((content_w - col_gutter * (col_count - 1.0)) / col_count).max(0.0);
    let column_of = |block_x: f32| -> (f32, f32) {
        /* The block's content-relative x tells which column it flows in
        (the paginator stamps `column_x_offset(idx)` onto `origin.x`);
        snap to the nearest column start so a paragraph indent never
        selects the wrong column. */
        let pitch = col_w + col_gutter;
        let idx = if pitch > 0.0 {
            (block_x / pitch).round().max(0.0).min(col_count - 1.0)
        } else {
            0.0
        };
        (content_x + idx * pitch, col_w)
    };

    for (block_idx, block) in page.blocks.iter().enumerate() {
        match block {
            LayoutBlock::Paragraph(p) => {
                let (column_x, column_w) = column_of(p.origin.x);
                collect_paragraph_floats(
                    page,
                    p,
                    content_x,
                    content_y,
                    column_x,
                    column_w,
                    FloatAnchorRef::Body {
                        block: block_idx,
                        cell: None,
                    },
                    &mut out,
                );
            }
            LayoutBlock::Table(t) => {
                collect_table_floats(page, t, content_x, content_y, block_idx, &mut out);
            }
        }
    }
    if let Some(hf) = &page.header {
        let band_top = page.header_band_top();
        for block in &hf.blocks {
            if let LayoutBlock::Paragraph(p) = block {
                collect_paragraph_floats(
                    page,
                    p,
                    content_x,
                    band_top,
                    content_x,
                    content_w,
                    FloatAnchorRef::Header,
                    &mut out,
                );
            }
        }
    }
    if let Some(hf) = &page.footer {
        let band_top = page.footer_band_top();
        for block in &hf.blocks {
            if let LayoutBlock::Paragraph(p) = block {
                collect_paragraph_floats(
                    page,
                    p,
                    content_x,
                    band_top,
                    content_x,
                    content_w,
                    FloatAnchorRef::Footer,
                    &mut out,
                );
            }
        }
    }
    out
}

/// Issue #165 — a text box's content rect as a margin-less pseudo page:
/// `size` is the content rect, `blocks` the laid-out story (origins
/// relative to the content rect). Feeding it to [`resolve_page_floats`]
/// places the boxes nested in the story against the PARENT box — the
/// page / margin / column frames all collapse onto the content rect,
/// and paragraph / line / character frames follow the story text — and
/// feeding it to `crate::wrap` (`derive_plan` / `WrapConvergence`)
/// wraps the story around them with the body's own bounded loop.
/// `page_number` keeps inside / outside parity with the real page.
pub fn story_frame_page(blocks: Vec<LayoutBlock>, size: Size, page_number: u32) -> PageBox {
    PageBox {
        size,
        margins: crate::page::Margins {
            top: 0.0,
            right: 0.0,
            bottom: 0.0,
            left: 0.0,
        },
        blocks,
        header: None,
        footer: None,
        header_offset: 0.0,
        footer_offset: 0.0,
        footnotes: Default::default(),
        endnotes: Default::default(),
        hf_role: crate::boxes::HeaderRole::Default,
        page_number,
        floats: Vec::new(),
    }
}

/// Floats anchored in a table's cell paragraphs. The cell content box
/// stands in for the "column" frame (Word's `layoutInCell` semantics);
/// page / margin frames are unchanged.
fn collect_table_floats(
    page: &PageBox,
    table: &TableBox,
    base_x: f32,
    base_y: f32,
    block_idx: usize,
    out: &mut Vec<FloatBox>,
) {
    let tx = base_x + table.origin.x;
    let ty = base_y + table.origin.y;
    for (r, row) in table.rows.iter().enumerate() {
        let row_x = tx + row.origin.x;
        let row_y = ty + row.origin.y;
        for (c, cell) in row.cells.iter().enumerate() {
            if matches!(cell.v_merge, engine::VMergeRole::Continue) {
                continue;
            }
            let cell_x = row_x + cell.origin.x + cell.padding_left;
            let cell_y = row_y + cell.origin.y + cell.padding_top;
            let cell_w = (cell.size.width - cell.padding_left - cell.padding_right).max(0.0);
            for (inner, content) in cell.content.iter().enumerate() {
                let LayoutBlock::Paragraph(p) = content else {
                    /* Nested tables: out of scope for the first cut —
                    their floats are neither positioned nor painted. */
                    continue;
                };
                collect_paragraph_floats(
                    page,
                    p,
                    cell_x,
                    cell_y,
                    cell_x,
                    cell_w,
                    FloatAnchorRef::Body {
                        block: block_idx,
                        cell: Some(CellAnchorRef {
                            row: r,
                            col: c,
                            inner,
                        }),
                    },
                    out,
                );
            }
        }
    }
}

/// Scan a paragraph's glyphs for float sentinels and resolve each one.
/// `base_x` / `base_y` is the paragraph's container origin in
/// page-relative px (content area for body blocks, band top for header /
/// footer paragraphs, cell content box for cell paragraphs).
#[allow(clippy::too_many_arguments)]
fn collect_paragraph_floats(
    page: &PageBox,
    para: &ParagraphBox,
    base_x: f32,
    base_y: f32,
    column_x: f32,
    column_w: f32,
    anchor: FloatAnchorRef,
    out: &mut Vec<FloatBox>,
) {
    let para_x = base_x + para.origin.x;
    let para_y = base_y + para.origin.y;
    for line in &para.lines {
        let line_x = para_x + line.origin.x;
        let mut pen = 0.0_f32;
        for run in &line.runs {
            for g in &run.glyphs {
                if let Some(f) = g.float.as_deref() {
                    let site = AnchorSite {
                        glyph_x: line_x + pen,
                        line_top: para_y + line.origin.y,
                        line_height: line.height,
                        para_top: para_y,
                        para_height: para.size.height,
                        column_x,
                        column_w,
                    };
                    let at = run.source_range.start + g.cluster;
                    out.push(place_float(page, f, &site, at, anchor));
                }
                pen += g.x_advance;
            }
        }
    }
}

/// `true` for a right-hand (odd-numbered) page — the side `inside` /
/// `outside` frames and alignments resolve against.
fn is_odd_page(page: &PageBox) -> bool {
    page.page_number % 2 == 1
}

/// Resolve one float against its reference frames. Every frame is a
/// `(start, extent)` pair along its axis in page-relative px; the offset
/// mode then picks the object's edge inside it.
fn place_float(
    page: &PageBox,
    f: &FloatGlyph,
    site: &AnchorSite,
    at: u32,
    anchor: FloatAnchorRef,
) -> FloatBox {
    let odd = is_odd_page(page);
    let pw = page.size.width;
    let ph = page.size.height;
    let ml = page.margins.left;
    let mr = page.margins.right;
    let mt = page.margins.top;
    let mb = page.margins.bottom;

    let (fx, fw) = match f.spec.h_frame {
        HRelativeFrom::Page => (0.0, pw),
        HRelativeFrom::Margin => (ml, pw - ml - mr),
        HRelativeFrom::LeftMargin => (0.0, ml),
        HRelativeFrom::RightMargin => (pw - mr, mr),
        HRelativeFrom::InsideMargin => {
            if odd {
                (0.0, ml)
            } else {
                (pw - mr, mr)
            }
        }
        HRelativeFrom::OutsideMargin => {
            if odd {
                (pw - mr, mr)
            } else {
                (0.0, ml)
            }
        }
        HRelativeFrom::Column => (site.column_x, site.column_w),
        HRelativeFrom::Character => (site.glyph_x, 0.0),
    };
    let (fy, fh) = match f.spec.v_frame {
        VRelativeFrom::Page => (0.0, ph),
        VRelativeFrom::Margin => (mt, ph - mt - mb),
        VRelativeFrom::TopMargin => (0.0, mt),
        VRelativeFrom::BottomMargin => (ph - mb, mb),
        /* Vertical inside / outside: Word treats them as the top margin
        on the page side that binds and the bottom margin otherwise —
        approximated as top / bottom here. */
        VRelativeFrom::InsideMargin => (0.0, mt),
        VRelativeFrom::OutsideMargin => (ph - mb, mb),
        VRelativeFrom::Paragraph => (site.para_top, site.para_height),
        VRelativeFrom::Line => (site.line_top, site.line_height),
    };

    let x = resolve_axis(fx, fw, f.width, f.spec.h_offset, odd);
    let y = resolve_axis(fy, fh, f.height, f.spec.v_offset, odd);
    let (origin, frame_origin) = match f.spec.simple_pos {
        Some(p) => (p, Point { x: 0.0, y: 0.0 }),
        None => (Point { x, y }, Point { x: fx, y: fy }),
    };
    FloatBox {
        origin,
        size: Size {
            width: f.width,
            height: f.height,
        },
        rel_id: f.rel_id.clone(),
        at,
        anchor,
        z_order: f.spec.z_order,
        behind_doc: f.spec.behind_doc,
        hidden: f.spec.hidden,
        frame_origin,
        wrap: f.wrap.clone(),
        /* Issue #83 — the story is laid out after pagination (engine-
        wasm), once the box's page and width are final. */
        text_box: f.text_box.as_ref().map(|tb| {
            Box::new(crate::boxes::TextBoxFrame {
                source: (**tb).clone(),
                blocks: Vec::new(),
                floats: Vec::new(),
            })
        }),
    }
}

/// Edge of the object along one axis, given the frame `(start, extent)`,
/// the object's own extent and the offset mode. `Left` / `Top` both mean
/// the frame start and `Right` / `Bottom` the far edge, so a keyword
/// from the other axis degrades gracefully instead of erroring.
fn resolve_axis(start: f32, extent: f32, object: f32, offset: FloatOffsetPx, odd: bool) -> f32 {
    match offset {
        FloatOffsetPx::Px(o) => start + o,
        FloatOffsetPx::Fraction(p) => start + p * extent,
        FloatOffsetPx::Align(a) => {
            let far = start + extent - object;
            let centre = start + (extent - object) / 2.0;
            match a {
                FloatAlign::Left | FloatAlign::Top => start,
                FloatAlign::Right | FloatAlign::Bottom => far,
                FloatAlign::Center => centre,
                /* Inside = the binding edge: the frame start (left /
                top) on odd pages, the far edge on even pages; outside
                is the mirror. Both axes follow the same page parity. */
                FloatAlign::Inside => {
                    if odd {
                        start
                    } else {
                        far
                    }
                }
                FloatAlign::Outside => {
                    if odd {
                        far
                    } else {
                        start
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::boxes::{FloatSpec, LineBox, PositionedGlyph, TextAttrs, VisualRun};
    use crate::page::Margins;
    use text_pipeline::{Alignment, ShapingDirection};

    fn attrs() -> TextAttrs {
        TextAttrs {
            px_size: 12.0,
            color: [0, 0, 0, 255],
            faux_bold: false,
            faux_italic: false,
            underline: engine::UnderlineStyle::None,
            strike: false,
            bg_color: None,
            baseline_shift_px: 0.0,
        }
    }

    fn glyph(cluster: u32, adv: f32, float: Option<FloatGlyph>) -> PositionedGlyph {
        PositionedGlyph {
            id: 1,
            cluster,
            x_advance: adv,
            y_advance: 0.0,
            x_offset: 0.0,
            y_offset: 0.0,
            synthetic: false,
            inline_image_rel_id: None,
            inline_footnote_marker: None,
            inline_note_anchor: None,
            inline_object_height: 0.0,
            float: float.map(Box::new),
            leader: None,
        }
    }

    fn spec(h: HRelativeFrom, ho: FloatOffsetPx, v: VRelativeFrom, vo: FloatOffsetPx) -> FloatSpec {
        FloatSpec {
            h_frame: h,
            h_offset: ho,
            v_frame: v,
            v_offset: vo,
            simple_pos: None,
            z_order: 7,
            behind_doc: false,
            hidden: false,
        }
    }

    /// One paragraph with two 10-px glyphs and a float sentinel between
    /// them; the paragraph sits at `origin` in the content area.
    fn para_with_float(origin: Point, spec: FloatSpec) -> ParagraphBox {
        let fg = FloatGlyph {
            rel_id: "rId9".into(),
            width: 100.0,
            height: 50.0,
            spec,
            wrap: crate::boxes::FloatWrap::default(),
            text_box: None,
        };
        let line = LineBox {
            origin: Point { x: 0.0, y: 0.0 },
            baseline: 12.0,
            height: 16.0,
            width: 20.0,
            runs: vec![VisualRun {
                glyphs: vec![
                    glyph(0, 10.0, None),
                    glyph(1, 0.0, Some(fg)),
                    glyph(4, 10.0, None),
                ],
                font: "f".into(),
                direction: ShapingDirection::Ltr,
                source_range: 0..5,
                attrs: attrs(),
            }],
            alignment: Alignment::Start,
            source_start: 0,
            segments: Vec::new(),
            segment: 0,
        };
        ParagraphBox {
            origin,
            size: Size {
                width: 400.0,
                height: 40.0,
            },
            lines: vec![line],
            direction: ShapingDirection::Ltr,
            marker: None,
            source_paragraph_id: 0,
            fields: Vec::new(),
            page_break_after_line: Vec::new(),
            borders: None,
            shading: None,
            keep_next: false,
            flow: crate::boxes::ParaFlow::default(),
            review_mark: None,
        }
    }

    fn page(blocks: Vec<LayoutBlock>, page_number: u32) -> PageBox {
        PageBox {
            size: Size {
                width: 600.0,
                height: 800.0,
            },
            margins: Margins {
                top: 50.0,
                right: 60.0,
                bottom: 70.0,
                left: 80.0,
            },
            blocks,
            header: None,
            footer: None,
            header_offset: 20.0,
            footer_offset: 20.0,
            footnotes: Default::default(),
            endnotes: Default::default(),
            hf_role: crate::boxes::HeaderRole::Default,
            page_number,
            floats: Vec::new(),
        }
    }

    #[test]
    fn column_and_paragraph_offsets_resolve_from_the_anchor_block() {
        let s = spec(
            HRelativeFrom::Column,
            FloatOffsetPx::Px(30.0),
            VRelativeFrom::Paragraph,
            FloatOffsetPx::Px(5.0),
        );
        let p = para_with_float(Point { x: 0.0, y: 100.0 }, s);
        let pg = page(vec![LayoutBlock::Paragraph(p)], 1);
        let floats = resolve_page_floats(&pg, ColumnLayout::default());
        assert_eq!(floats.len(), 1);
        let f = &floats[0];
        /* column = content area: x0 = 80; paragraph top = 50 + 100. */
        assert_eq!(f.origin, Point { x: 110.0, y: 155.0 });
        assert_eq!(f.frame_origin, Point { x: 80.0, y: 150.0 });
        assert_eq!(
            f.size,
            Size {
                width: 100.0,
                height: 50.0
            }
        );
        assert_eq!(f.at, 1, "sentinel byte offset");
        assert_eq!(f.rel_id, "rId9");
        assert_eq!(f.z_order, 7);
        assert_eq!(
            f.anchor,
            FloatAnchorRef::Body {
                block: 0,
                cell: None
            }
        );
    }

    #[test]
    fn page_and_margin_alignments_use_page_geometry_and_parity() {
        let s = spec(
            HRelativeFrom::Page,
            FloatOffsetPx::Align(FloatAlign::Right),
            VRelativeFrom::Margin,
            FloatOffsetPx::Align(FloatAlign::Bottom),
        );
        let p = para_with_float(Point { x: 0.0, y: 0.0 }, s);
        let pg = page(vec![LayoutBlock::Paragraph(p)], 1);
        let f = &resolve_page_floats(&pg, ColumnLayout::default())[0];
        /* right of page: 600 - 100; bottom of margin box: 800 - 70 - 50. */
        assert_eq!(f.origin, Point { x: 500.0, y: 680.0 });

        /* Inside on an odd page = left margin edge; on an even page it is
        the right side. */
        let inside = spec(
            HRelativeFrom::Margin,
            FloatOffsetPx::Align(FloatAlign::Inside),
            VRelativeFrom::Page,
            FloatOffsetPx::Fraction(0.5),
        );
        let odd = page(
            vec![LayoutBlock::Paragraph(para_with_float(
                Point::default(),
                inside,
            ))],
            1,
        );
        let even = page(
            vec![LayoutBlock::Paragraph(para_with_float(
                Point::default(),
                inside,
            ))],
            2,
        );
        let fo = &resolve_page_floats(&odd, ColumnLayout::default())[0];
        let fe = &resolve_page_floats(&even, ColumnLayout::default())[0];
        assert_eq!(fo.origin.x, 80.0);
        assert_eq!(fe.origin.x, 600.0 - 60.0 - 100.0);
        assert_eq!(fo.origin.y, 400.0, "50 % of the page height");
    }

    #[test]
    fn character_and_line_frames_follow_the_sentinel_glyph() {
        let s = spec(
            HRelativeFrom::Character,
            FloatOffsetPx::Px(2.0),
            VRelativeFrom::Line,
            FloatOffsetPx::Align(FloatAlign::Bottom),
        );
        let p = para_with_float(Point { x: 10.0, y: 20.0 }, s);
        let pg = page(vec![LayoutBlock::Paragraph(p)], 1);
        let f = &resolve_page_floats(&pg, ColumnLayout::default())[0];
        /* glyph x = 80 + 10 + 10 (one 10-px glyph before the sentinel). */
        assert_eq!(f.origin.x, 102.0);
        /* line: top 50 + 20, height 16 → bottom-aligned 50-px object. */
        assert_eq!(f.origin.y, 70.0 + 16.0 - 50.0);
    }

    #[test]
    fn simple_pos_overrides_both_axes() {
        let mut s = spec(
            HRelativeFrom::Page,
            FloatOffsetPx::Px(999.0),
            VRelativeFrom::Page,
            FloatOffsetPx::Px(999.0),
        );
        s.simple_pos = Some(Point { x: 12.0, y: 34.0 });
        let p = para_with_float(Point::default(), s);
        let pg = page(vec![LayoutBlock::Paragraph(p)], 1);
        let f = &resolve_page_floats(&pg, ColumnLayout::default())[0];
        assert_eq!(f.origin, Point { x: 12.0, y: 34.0 });
        assert_eq!(f.frame_origin, Point::default());
    }

    #[test]
    fn second_column_frame_snaps_to_the_block_column() {
        let cols = ColumnLayout {
            count: 2,
            gutter: 20.0,
        };
        /* content width 460 → columns of 220; second column starts at
        content-relative 240 (= 80 + 240 absolute). */
        let s = spec(
            HRelativeFrom::Column,
            FloatOffsetPx::Align(FloatAlign::Right),
            VRelativeFrom::Paragraph,
            FloatOffsetPx::Px(0.0),
        );
        let p = para_with_float(Point { x: 240.0, y: 0.0 }, s);
        let pg = page(vec![LayoutBlock::Paragraph(p)], 1);
        let f = &resolve_page_floats(&pg, cols)[0];
        assert_eq!(f.frame_origin.x, 320.0);
        assert_eq!(f.origin.x, 320.0 + 220.0 - 100.0);
    }

    #[test]
    fn paragraphs_without_sentinels_yield_no_floats() {
        let mut p = para_with_float(
            Point::default(),
            spec(
                HRelativeFrom::Page,
                FloatOffsetPx::Px(0.0),
                VRelativeFrom::Page,
                FloatOffsetPx::Px(0.0),
            ),
        );
        for run in &mut p.lines[0].runs {
            for g in &mut run.glyphs {
                g.float = None;
            }
        }
        let pg = page(vec![LayoutBlock::Paragraph(p)], 1);
        assert!(resolve_page_floats(&pg, ColumnLayout::default()).is_empty());
    }
}
