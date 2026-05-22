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
    /// Render with synthetic bold / italic — no real face variant exists
    /// (Backlog #1).
    pub faux_bold: bool,
    pub faux_italic: bool,
    /// Highlight colour painted behind the run. The glyph blit composites
    /// over it, since `put_image_data` would otherwise punch holes through a
    /// background rect (Backlog #1).
    pub bg_color: Option<[u8; 4]>,
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
            let baseline = (para_y + line.origin.y + line.baseline) as f64;
            let line_top = (para_y + line.origin.y) as f64;
            let line_bottom = line_top + line.height as f64;
            /* One pen across the whole line; runs lie left-to-right in
            visual order, each glyph placed at the cumulative advance. */
            let mut pen = 0.0_f32;
            for run in &line.runs {
                let [r, g, b, a] = run.attrs.color;
                let text_color = Color::from_rgba8(r, g, b, a);
                let run_x0 = (line_x as f64) + (pen as f64);
                let mut glyphs: Vec<RunGlyph> = Vec::with_capacity(run.glyphs.len());
                for glyph in &run.glyphs {
                    /* glyph id 0 is .notdef — advance the pen, draw nothing. */
                    if glyph.id != 0 {
                        glyphs.push(RunGlyph {
                            glyph_id: glyph.id,
                            x: (line_x as f64) + (pen as f64) + (glyph.x_offset as f64),
                            y: baseline - (glyph.y_offset as f64),
                        });
                    }
                    pen += glyph.x_advance;
                }
                let run_x1 = (line_x as f64) + (pen as f64);

                /* Background highlight — emitted before the glyphs so it sits
                behind them, spanning the run's advance and the line's full
                height so adjacent highlights tile seamlessly (Backlog #1). */
                if let Some([br, bgc, bb, ba]) = run.attrs.bg_color
                    && run_x1 > run_x0
                {
                    cmds.push(DisplayCmd::FillRect {
                        rect: Rect::new(run_x0, line_top, run_x1, line_bottom),
                        paint: Paint::solid(Color::from_rgba8(br, bgc, bb, ba)),
                    });
                }
                if !glyphs.is_empty() {
                    cmds.push(DisplayCmd::DrawGlyphRun(GlyphRun {
                        font: run.font.clone(),
                        px_size: run.attrs.px_size,
                        paint: Paint::solid(text_color),
                        glyphs,
                        faux_bold: run.attrs.faux_bold,
                        faux_italic: run.attrs.faux_italic,
                        bg_color: run.attrs.bg_color,
                    }));
                }
                /* Decoration strokes — thin `FillRect`s drawn over the glyphs.
                Y positions are px-size-relative approximations (Backlog #1):
                the underline sits just below the baseline, the strikethrough
                is centred ~one quarter em above it (≈ x-height / 2). */
                if (run.attrs.underline || run.attrs.strike) && run_x1 > run_x0 {
                    let px = run.attrs.px_size as f64;
                    let thickness = (px * 0.06).max(1.0);
                    if run.attrs.underline {
                        let top = baseline + px * 0.10;
                        cmds.push(DisplayCmd::FillRect {
                            rect: Rect::new(run_x0, top, run_x1, top + thickness),
                            paint: Paint::solid(text_color),
                        });
                    }
                    if run.attrs.strike {
                        let mid = baseline - px * 0.25;
                        cmds.push(DisplayCmd::FillRect {
                            rect: Rect::new(
                                run_x0,
                                mid - thickness / 2.0,
                                run_x1,
                                mid + thickness / 2.0,
                            ),
                            paint: Paint::solid(text_color),
                        });
                    }
                }
            }
        }
    }

    DisplayList { cmds }
}
