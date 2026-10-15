//! Issue #328 — page-level decorations: the `<w:pgBorders>` box and the
//! `<w:lnNumType>` line numbers. Both are pure functions of a laid-out
//! page (they never feed back into the flow), so the engine runs them
//! once per page after pagination and the renderer / PDF exporter paint
//! the result ([`crate::PageDecor`]).

use crate::boxes::{
    LayoutBlock, MarkerBox, PageBorderBox, PageBox, Point, PositionedGlyph, Size, TextAttrs,
    VisualRun,
};
use crate::page::Margins;
use text_pipeline::{FontStack, Script, ShapingDirection, shape_text};

/// The page border box of a `size` page with `margins` (both already in
/// layout units, i.e. multiplied by `scale`), or `None` when no edge
/// paints.
///
/// Each edge's `w:space` (points) is measured per `w:offsetFrom`: from
/// the page edge inward (`page`) or from the text margin outward
/// (`text`). The stroke is centred on the resulting rectangle.
pub fn page_border_box(
    borders: &engine::PageBorders,
    size: Size,
    margins: Margins,
    scale: f32,
) -> Option<PageBorderBox> {
    if !borders.is_visible() {
        return None;
    }
    let space = |e: &Option<engine::BorderStroke>| {
        e.as_ref()
            .and_then(|s| s.space_pt)
            .map_or(0.0, |v| f32::from(v.min(31)) * scale)
    };
    let (x0, y0, x1, y1) = match borders.offset_from {
        engine::PageBorderOffset::Page => (
            space(&borders.left),
            space(&borders.top),
            size.width - space(&borders.right),
            size.height - space(&borders.bottom),
        ),
        engine::PageBorderOffset::Text => (
            margins.left - space(&borders.left),
            margins.top - space(&borders.top),
            size.width - margins.right + space(&borders.right),
            size.height - margins.bottom + space(&borders.bottom),
        ),
    };
    let (x0, x1) = (x0.clamp(0.0, size.width), x1.clamp(0.0, size.width));
    let (y0, y1) = (y0.clamp(0.0, size.height), y1.clamp(0.0, size.height));
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    Some(PageBorderBox {
        origin: Point { x: x0, y: y0 },
        size: Size {
            width: x1 - x0,
            height: y1 - y0,
        },
        edges: engine::CellBorders {
            top: borders.top.clone(),
            left: borders.left.clone(),
            bottom: borders.bottom.clone(),
            right: borders.right.clone(),
            ..Default::default()
        },
        behind: borders.z_back,
    })
}

/// Shape `text` in the default Latin face at `px_size` into one
/// left-to-right run. Returns the run and its advance.
pub fn shape_gutter_run(
    fonts: &FontStack,
    family: Option<&str>,
    text: &str,
    px_size: f32,
    color: [u8; 4],
) -> Option<(VisualRun, f32)> {
    let (font_id, face, _) = fonts
        .resolve(Script::Latin, family, false, false)
        .or_else(|| fonts.resolve(Script::Latin, None, false, false))?;
    let shaped = shape_text(face, text, ShapingDirection::Ltr, px_size);
    let glyphs: Vec<PositionedGlyph> = shaped
        .glyphs
        .iter()
        .map(|g| PositionedGlyph {
            id: g.glyph_id as u16,
            cluster: g.cluster,
            x_advance: g.x_advance,
            y_advance: g.y_advance,
            x_offset: g.x_offset,
            y_offset: g.y_offset,
            synthetic: false,
            inline_image_rel_id: None,
            inline_footnote_marker: None,
            inline_note_anchor: None,
            inline_object_height: 0.0,
            float: None,
            leader: None,
        })
        .collect();
    let width = glyphs.iter().map(|g| g.x_advance).sum::<f32>();
    Some((
        VisualRun {
            glyphs,
            font: font_id.clone(),
            direction: ShapingDirection::Ltr,
            source_range: 0..0,
            attrs: TextAttrs {
                px_size,
                color,
                bg_color: None,
                underline: engine::UnderlineStyle::None,
                strike: false,
                faux_bold: false,
                faux_italic: false,
                baseline_shift_px: 0.0,
            },
            metrics_font: None,
        },
        width,
    ))
}

/// How the numbers look: the face family (`None` = the default Latin
/// face), the size and colour — the `LineNumber` character style.
#[derive(Debug, Clone, Copy)]
pub struct LineNumberStyle<'a> {
    pub family: Option<&'a str>,
    pub px_size: f32,
    pub color: [u8; 4],
}

/// Number the lines of `page`'s top-level paragraphs into
/// `page.decor.line_numbers`. `counted[j]` says whether block `j` takes
/// part (a table, a paragraph with `w:suppressLineNumbers` or a frame
/// does not — its lines are neither counted nor shown). `counter` is
/// the number the next counted line gets; every line advances it, only
/// multiples of `count_by` are shown. Each number's right edge sits
/// `distance` (layout units) left of the text column's leading edge, on
/// the line's baseline.
pub fn number_page_lines(
    page: &mut PageBox,
    counted: &[bool],
    numbering: &engine::LineNumbering,
    counter: &mut u32,
    distance: f32,
    fonts: &FontStack,
    style: LineNumberStyle<'_>,
) {
    let count_by = numbering.count_by.max(1);
    let (left, top) = (page.margins.left, page.margins.top);
    let mut out = Vec::new();
    for (j, block) in page.blocks.iter().enumerate() {
        let LayoutBlock::Paragraph(para) = block else {
            continue;
        };
        if !counted.get(j).copied().unwrap_or(false) {
            continue;
        }
        for line in &para.lines {
            let n = *counter;
            *counter = counter.saturating_add(1);
            if n % count_by != 0 {
                continue;
            }
            let text = n.to_string();
            let Some((run, width)) =
                shape_gutter_run(fonts, style.family, &text, style.px_size, style.color)
            else {
                continue;
            };
            let right = left + para.origin.x - distance;
            out.push(MarkerBox {
                origin: Point {
                    x: (right - width).max(0.0),
                    y: top + para.origin.y + line.origin.y,
                },
                baseline: line.baseline,
                run,
                width,
                text,
            });
        }
    }
    page.decor.line_numbers.extend(out);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stroke(space: u16) -> Option<engine::BorderStroke> {
        Some(engine::BorderStroke {
            style: engine::BorderStyle::Single,
            size_eighth_pt: 12,
            color: None,
            space_pt: Some(space),
            shadow: false,
            frame: false,
        })
    }

    fn borders(offset: engine::PageBorderOffset) -> engine::PageBorders {
        engine::PageBorders {
            top: stroke(24),
            left: stroke(24),
            bottom: stroke(24),
            right: stroke(24),
            offset_from: offset,
            ..Default::default()
        }
    }

    const PAGE: Size = Size {
        width: 600.0,
        height: 800.0,
    };
    const MARGINS: Margins = Margins {
        top: 72.0,
        right: 72.0,
        bottom: 72.0,
        left: 72.0,
    };

    #[test]
    fn page_offset_measures_inward_from_the_page_edge() {
        let b = page_border_box(&borders(engine::PageBorderOffset::Page), PAGE, MARGINS, 1.0)
            .expect("visible");
        assert_eq!(b.origin, Point { x: 24.0, y: 24.0 });
        assert_eq!(
            b.size,
            Size {
                width: 552.0,
                height: 752.0
            }
        );
    }

    #[test]
    fn text_offset_measures_outward_from_the_margins() {
        let b = page_border_box(&borders(engine::PageBorderOffset::Text), PAGE, MARGINS, 2.0)
            .expect("visible");
        /* 24 pt × scale 2 outward of the 72-unit margin. */
        assert_eq!(b.origin, Point { x: 24.0, y: 24.0 });
        assert_eq!(b.size.width, 552.0);
    }

    #[test]
    fn invisible_borders_paint_nothing() {
        let mut b = borders(engine::PageBorderOffset::Page);
        for e in [&mut b.top, &mut b.left, &mut b.bottom, &mut b.right] {
            *e = None;
        }
        assert!(page_border_box(&b, PAGE, MARGINS, 1.0).is_none());
    }
}
