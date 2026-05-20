//! Backend-agnostic display list + page scene builder
//! (PHASE_3_RENDER_RTL.md §9.1).
//!
//! Layout produces the hierarchical box tree ([`layout::PageBox`]);
//! [`build_page_scene`] walks `PageBox` → `ParagraphBox` → `LineBox` →
//! `VisualRun`, accumulating parent-relative origins into absolute positions,
//! and lowers it to a linear [`DisplayList`] any backend interprets. `kurbo`
//! is the geometry vocabulary, `peniko` the paint vocabulary.

use kurbo::{Affine, Rect};
use layout::PageBox;
use peniko::{Brush, Color};

/// Font identifier — a key into the engine's font map.
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

/* Page chrome colours — fixed in the PoC; configurable in a later batch. */
fn page_color() -> Color {
    Color::from_rgba8(0xff, 0xff, 0xff, 0xff)
}
fn border_color() -> Color {
    Color::from_rgba8(0xcc, 0xcc, 0xcc, 0xff)
}

/// Lower a laid-out [`PageBox`] into a [`DisplayList`].
///
/// Pure traversal — layout already owns every coordinate. Emits a white page
/// fill and a 1px border inset 0.5px, then walks `PageBox` → `ParagraphBox` →
/// `LineBox` → `VisualRun`. Each level's `origin` is parent-relative, so the
/// accumulated origin reaches absolute glyph positions; font size and colour
/// come from each run's `attrs` (no `PaintConfig` side channel).
pub fn build_page_scene(page: &PageBox) -> DisplayList {
    let mut cmds: Vec<DisplayCmd> = Vec::new();

    let w = page.size.width as f64;
    let h = page.size.height as f64;
    cmds.push(DisplayCmd::FillRect {
        rect: Rect::new(0.0, 0.0, w, h),
        paint: Paint::solid(page_color()),
    });
    cmds.push(DisplayCmd::StrokeRect {
        rect: Rect::new(0.5, 0.5, w - 0.5, h - 0.5),
        paint: Paint::solid(border_color()),
        width: 1.0,
    });

    /* Page content origin — paragraph and line origins are relative to it. */
    let content_x = page.margins.left;
    let content_y = page.margins.top;

    for para in &page.paragraphs {
        let para_x = content_x + para.origin.x;
        let para_y = content_y + para.origin.y;
        for line in &para.lines {
            let line_x = para_x + line.origin.x;
            let baseline = para_y + line.origin.y + line.baseline;
            /* One pen across the whole line; runs lie left-to-right in
            visual order, each glyph placed at the cumulative advance. */
            let mut pen = 0.0_f32;
            for run in &line.runs {
                let [r, g, b, a] = run.attrs.color;
                let mut glyphs: Vec<RunGlyph> = Vec::with_capacity(run.glyphs.len());
                for glyph in &run.glyphs {
                    /* glyph id 0 is .notdef — advance the pen, draw nothing. */
                    if glyph.id != 0 {
                        glyphs.push(RunGlyph {
                            glyph_id: glyph.id,
                            x: (line_x as f64) + (pen as f64) + (glyph.x_offset as f64),
                            y: (baseline as f64) - (glyph.y_offset as f64),
                        });
                    }
                    pen += glyph.x_advance;
                }
                if !glyphs.is_empty() {
                    cmds.push(DisplayCmd::DrawGlyphRun(GlyphRun {
                        font: run.font.clone(),
                        px_size: run.attrs.px_size,
                        paint: Paint::solid(Color::from_rgba8(r, g, b, a)),
                        glyphs,
                    }));
                }
            }
        }
    }

    DisplayList { cmds }
}
