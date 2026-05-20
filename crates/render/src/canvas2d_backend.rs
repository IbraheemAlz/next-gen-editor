//! Canvas2D backend — interprets a [`DisplayList`] onto an `OffscreenCanvas`.
//!
//! `paint_alpha_glyph` (the low-level alpha-mask blit) is retained as the
//! shared primitive: [`render_canvas2d`] calls it, and the Phase 1 PoC paths
//! (`RasterizeGlyph` / `ShapeAndRasterize`) still call it directly.

use crate::atlas::{GlyphAtlas, GlyphKey};
use crate::scene::{DisplayCmd, DisplayList, Paint};
use std::sync::Arc;
use text_pipeline::{LoadedFont, RasterizedGlyph};
use wasm_bindgen::Clamped;
use wasm_bindgen::JsValue;

/// Paint the glyph's alpha mask onto the canvas at the given baseline origin.
///
/// `origin_x`, `origin_y` is the baseline pen position. `swash`'s `placement.left`
/// and `placement.top` are offsets relative to that origin (left → x, top is
/// distance above baseline → subtracted from y because canvas y grows down).
///
/// `color` is RGB applied uniformly; the per-pixel alpha comes from `glyph.alpha`.
pub fn paint_alpha_glyph(
    ctx: &web_sys::OffscreenCanvasRenderingContext2d,
    glyph: &RasterizedGlyph,
    origin_x: f64,
    origin_y: f64,
    color: [u8; 3],
) -> Result<(), JsValue> {
    if glyph.width == 0 || glyph.height == 0 {
        return Ok(());
    }
    let pixel_count = (glyph.width as usize) * (glyph.height as usize);
    let mut rgba = Vec::with_capacity(pixel_count * 4);
    for &a in &glyph.alpha {
        rgba.push(color[0]);
        rgba.push(color[1]);
        rgba.push(color[2]);
        rgba.push(a);
    }
    let image_data = web_sys::ImageData::new_with_u8_clamped_array_and_sh(
        Clamped(&rgba),
        glyph.width,
        glyph.height,
    )?;
    let dx = origin_x + glyph.left as f64;
    let dy = origin_y - glyph.top as f64;
    ctx.put_image_data(&image_data, dx, dy)?;
    Ok(())
}

/// Interpret a [`DisplayList`] onto a Canvas2D context. `resolve_font` maps a
/// `FontId` to its `LoadedFont`.
///
/// Lossless with respect to the Phase 1 inline paint loop: rects go through
/// `fill_rect`/`stroke_rect`, glyphs through `paint_alpha_glyph`.
pub fn render_canvas2d(
    ctx: &web_sys::OffscreenCanvasRenderingContext2d,
    list: &DisplayList,
    atlas: &mut GlyphAtlas,
    resolve_font: impl Fn(&str) -> Option<Arc<LoadedFont>>,
) -> Result<(), JsValue> {
    for cmd in &list.cmds {
        match cmd {
            DisplayCmd::FillRect { rect, paint } => {
                ctx.set_fill_style_str(&css_color(paint));
                ctx.fill_rect(rect.x0, rect.y0, rect.width(), rect.height());
            }
            DisplayCmd::StrokeRect { rect, paint, width } => {
                ctx.set_stroke_style_str(&css_color(paint));
                ctx.set_line_width(*width);
                ctx.stroke_rect(rect.x0, rect.y0, rect.width(), rect.height());
            }
            DisplayCmd::DrawGlyphRun(run) => {
                let Some(font) = resolve_font(&run.font) else {
                    continue;
                };
                let rgb = paint_rgb(&run.paint);
                for g in &run.glyphs {
                    let key = GlyphKey::new(run.font.clone(), g.glyph_id, run.px_size);
                    if let Some(raster) = atlas.get_or_rasterize(&key, &font, run.px_size) {
                        paint_alpha_glyph(ctx, raster, g.x, g.y, rgb)?;
                    }
                }
            }
            DisplayCmd::PushClip { rect } => {
                ctx.save();
                ctx.begin_path();
                ctx.rect(rect.x0, rect.y0, rect.width(), rect.height());
                ctx.clip();
            }
            DisplayCmd::PopClip => ctx.restore(),
            DisplayCmd::PushTransform(affine) => {
                ctx.save();
                let [a, b, c, d, e, f] = affine.as_coeffs();
                ctx.transform(a, b, c, d, e, f)?;
            }
            DisplayCmd::PopTransform => ctx.restore(),
        }
    }
    Ok(())
}

/// Solid-paint channels as `[r, g, b, a]` u8. Non-solid brushes fall back to
/// opaque black (batch 1 emits only solid paints).
fn solid_rgba8(paint: &Paint) -> [u8; 4] {
    match &paint.brush {
        peniko::Brush::Solid(color) => {
            let [r, g, b, a] = color.components;
            let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
            [q(r), q(g), q(b), q(a)]
        }
        _ => [0, 0, 0, 255],
    }
}

fn css_color(paint: &Paint) -> String {
    let [r, g, b, _] = solid_rgba8(paint);
    format!("#{r:02x}{g:02x}{b:02x}")
}

fn paint_rgb(paint: &Paint) -> [u8; 3] {
    let [r, g, b, _] = solid_rgba8(paint);
    [r, g, b]
}
