//! Backend-agnostic display list + page scene builder
//! (PHASE_3_RENDER_RTL.md §9.1).
//!
//! Layout produces geometry (`A4Page` + `LineBox`s); [`build_page_scene`]
//! lowers it to a linear [`DisplayList`] that any backend — Canvas2D today,
//! Vello next — interprets. `kurbo` is the geometry vocabulary, `peniko` the
//! paint vocabulary.

use kurbo::{Affine, Rect};
use layout::{A4Page, LineBox};
use peniko::{Brush, Color};
use text_pipeline::{Alignment, ShapingDirection};

/// Font identifier. A `String` id keyed into the engine's font map for now;
/// becomes a numeric handle when the Phase 3 `FontStack` lands.
pub type FontId = String;

/// A solid paint. Wraps a `peniko::Brush`; batch 1 only ever uses `Solid`.
#[derive(Debug, Clone)]
pub struct Paint {
    pub brush: Brush,
}

impl Paint {
    pub fn solid(color: Color) -> Self {
        Self {
            brush: Brush::Solid(color),
        }
    }
}

/// One positioned glyph in a run. `x`/`y` is the baseline pen position; the
/// backend adds the rasterized glyph's bearing.
#[derive(Debug, Clone, Copy)]
pub struct RunGlyph {
    pub glyph_id: u16,
    pub x: f64,
    pub y: f64,
}

/// A run of glyphs sharing one font, pixel size, and paint.
#[derive(Debug, Clone)]
pub struct GlyphRun {
    pub font: FontId,
    pub px_size: f32,
    pub paint: Paint,
    pub glyphs: Vec<RunGlyph>,
}

/// One backend-agnostic drawing command. Batch 1 freezes this set; paths,
/// images, and layers arrive with the features that need them.
#[derive(Debug, Clone)]
pub enum DisplayCmd {
    FillRect {
        rect: Rect,
        paint: Paint,
    },
    StrokeRect {
        rect: Rect,
        paint: Paint,
        width: f64,
    },
    DrawGlyphRun(GlyphRun),
    PushClip {
        rect: Rect,
    },
    PopClip,
    PushTransform(Affine),
    PopTransform,
}

/// An ordered list of drawing commands — the renderer's sole input.
#[derive(Debug, Clone, Default)]
pub struct DisplayList {
    pub cmds: Vec<DisplayCmd>,
}

/// Paint parameters the Phase 1 `LineBox` does not yet carry (font, size,
/// alignment). Dissolves into per-run `VisualRun` attributes once the §5 box
/// model lands.
#[derive(Debug, Clone)]
pub struct PaintConfig {
    pub font: FontId,
    pub px_size: f32,
    pub alignment: Alignment,
}

/* Page chrome colours — fixed in the PoC; configurable in a later batch. */
fn page_color() -> Color {
    Color::from_rgba8(0xff, 0xff, 0xff, 0xff)
}
fn border_color() -> Color {
    Color::from_rgba8(0xcc, 0xcc, 0xcc, 0xff)
}
fn glyph_color() -> Color {
    Color::from_rgba8(0x00, 0x00, 0x00, 0xff)
}

/// Lower a laid-out page into a [`DisplayList`].
///
/// `lines` must carry **page-absolute** `baseline_y` (the caller stacks
/// paragraphs). Reproduces the Phase 1 inline paint loop exactly: white page
/// fill, a 1px border inset 0.5px, then one glyph run per line.
pub fn build_page_scene(page: &A4Page, lines: &[LineBox], cfg: &PaintConfig) -> DisplayList {
    let mut cmds: Vec<DisplayCmd> = Vec::with_capacity(lines.len() + 2);

    let w = page.width as f64;
    let h = page.height as f64;
    cmds.push(DisplayCmd::FillRect {
        rect: Rect::new(0.0, 0.0, w, h),
        paint: Paint::solid(page_color()),
    });
    cmds.push(DisplayCmd::StrokeRect {
        rect: Rect::new(0.5, 0.5, w - 0.5, h - 0.5),
        paint: Paint::solid(border_color()),
        width: 1.0,
    });

    let margin_left = page.margin.left as f64;
    let content_width = page.content_width() as f64;

    for line in lines {
        let natural_w = line.natural_width as f64;
        let rtl = matches!(line.direction, Some(ShapingDirection::Rtl));
        let x_origin = if cfg.alignment == Alignment::Center {
            margin_left + (content_width - natural_w) / 2.0
        } else if rtl && natural_w < content_width - 0.5 {
            margin_left + (content_width - natural_w)
        } else {
            margin_left
        };
        let baseline_y = line.baseline_y as f64;

        let mut glyphs: Vec<RunGlyph> = Vec::with_capacity(line.glyphs.len());
        for g in &line.glyphs {
            if g.glyph_id == 0 {
                continue;
            }
            glyphs.push(RunGlyph {
                glyph_id: g.glyph_id as u16,
                x: x_origin + g.x as f64 + g.x_offset as f64,
                y: baseline_y - g.y_offset as f64,
            });
        }
        if !glyphs.is_empty() {
            cmds.push(DisplayCmd::DrawGlyphRun(GlyphRun {
                font: cfg.font.clone(),
                px_size: cfg.px_size,
                paint: Paint::solid(glyph_color()),
                glyphs,
            }));
        }
    }

    DisplayList { cmds }
}
