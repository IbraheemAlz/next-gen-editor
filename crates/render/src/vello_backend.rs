//! Vello (WebGPU) backend — encodes a `DisplayList` into a `vello::Scene`
//! (PHASE_3_RENDER_RTL.md §9.2).
//!
//! Phase 3 batch 2 (P3-4): scene encoding only. Presenting the scene to a
//! WebGPU surface (a `vello::Renderer` + `wgpu` device/queue) is wired in a
//! later batch; until then Canvas2D stays the active backend.

use crate::scene::DisplayCmd;
use kurbo::{Affine, Stroke};
use peniko::{Fill, FontData};
use vello::{Glyph, Scene};

/// Encode a slice of `DisplayCmd`s into a `vello::Scene`.
///
/// `resolve_font` maps a `FontId` to a `peniko::FontData` (the post-rename
/// vello font handle). Vello does its own GPU-side glyph caching, so there is
/// no `GlyphAtlas` on this path.
pub fn render_vello(
    scene: &mut Scene,
    cmds: &[DisplayCmd],
    resolve_font: impl Fn(&str) -> Option<FontData>,
) {
    let mut transform = Affine::IDENTITY;
    let mut transform_stack: Vec<Affine> = Vec::new();
    let mut clip_depth: u32 = 0;

    for cmd in cmds {
        match cmd {
            DisplayCmd::FillRect { rect, paint } => {
                scene.fill(Fill::NonZero, transform, &paint.brush, None, rect);
            }
            DisplayCmd::StrokeRect { rect, paint, width } => {
                scene.stroke(&Stroke::new(*width), transform, &paint.brush, None, rect);
            }
            DisplayCmd::DrawGlyphRun(run) => {
                let Some(font) = resolve_font(&run.font) else {
                    continue;
                };
                scene
                    .draw_glyphs(&font)
                    .font_size(run.px_size)
                    .transform(transform)
                    .brush(&run.paint.brush)
                    .draw(
                        Fill::NonZero,
                        run.glyphs.iter().map(|g| Glyph {
                            id: u32::from(g.glyph_id),
                            x: g.x as f32,
                            y: g.y as f32,
                        }),
                    );
            }
            DisplayCmd::PushTransform(affine) => {
                transform_stack.push(transform);
                transform *= *affine;
            }
            DisplayCmd::PopTransform => {
                transform = transform_stack.pop().unwrap_or(Affine::IDENTITY);
            }
            DisplayCmd::PushClip { rect } => {
                scene.push_layer(Fill::NonZero, peniko::Mix::Normal, 1.0, transform, rect);
                clip_depth += 1;
            }
            DisplayCmd::PopClip => {
                if clip_depth > 0 {
                    clip_depth -= 1;
                    scene.pop_layer();
                }
            }
        }
    }
}
