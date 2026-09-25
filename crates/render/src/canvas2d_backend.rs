//! Canvas2D backend — interprets a [`DisplayList`] onto an `OffscreenCanvas`.
//!
//! `paint_alpha_glyph` (the low-level alpha-mask blit) is retained as the
//! shared primitive: [`render_canvas2d`] calls it, and the Phase 1 PoC paths
//! (`RasterizeGlyph` / `ShapeAndRasterize`) still call it directly.

use crate::atlas::{GlyphAtlas, GlyphKey};
use crate::scene::{DisplayCmd, DisplayList, GlyphRun, Paint};
use kurbo::Rect;
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
///
/// When `bg` is set the glyph is composited over that background colour and
/// the output is fully opaque — `put_image_data` replaces pixels wholesale, so
/// a transparent surround would punch a hole through a background highlight
/// (Backlog #1). With `bg == None` the surround stays transparent as before.
pub fn paint_alpha_glyph(
    ctx: &web_sys::OffscreenCanvasRenderingContext2d,
    glyph: &RasterizedGlyph,
    origin_x: f64,
    origin_y: f64,
    color: [u8; 3],
    bg: Option<[u8; 4]>,
) -> Result<(), JsValue> {
    paint_alpha_glyph_cropped(ctx, glyph, origin_x, origin_y, color, bg, None)
}

/// [`paint_alpha_glyph`] cropped to `crop` (canvas pixels). Issue #169 —
/// `put_image_data` ignores the canvas clip path, so a glyph straddling a
/// [`DisplayCmd::PushClip`] rect (an exact-height table row, a text box)
/// would overflow it; the dirty-rect form of `putImageData` writes only
/// the part inside the clip. `None` is the historical uncropped blit,
/// byte-identical to [`paint_alpha_glyph`].
fn paint_alpha_glyph_cropped(
    ctx: &web_sys::OffscreenCanvasRenderingContext2d,
    glyph: &RasterizedGlyph,
    origin_x: f64,
    origin_y: f64,
    color: [u8; 3],
    bg: Option<[u8; 4]>,
    crop: Option<Rect>,
) -> Result<(), JsValue> {
    if glyph.width == 0 || glyph.height == 0 {
        return Ok(());
    }
    let dx = origin_x + glyph.left as f64;
    let dy = origin_y - glyph.top as f64;
    let dirty = match crop {
        None => None,
        Some(c) => {
            let hit = c.intersect(Rect::new(
                dx,
                dy,
                dx + f64::from(glyph.width),
                dy + f64::from(glyph.height),
            ));
            if hit.width() <= 0.0 || hit.height() <= 0.0 {
                return Ok(());
            }
            Some(Rect::new(
                hit.x0 - dx,
                hit.y0 - dy,
                hit.x1 - dx,
                hit.y1 - dy,
            ))
        }
    };
    let pixel_count = (glyph.width as usize) * (glyph.height as usize);
    let mut rgba = Vec::with_capacity(pixel_count * 4);
    match bg {
        Some([br, bg_g, bb, _]) => {
            for &a in &glyph.alpha {
                let t = f32::from(a) / 255.0;
                let mix =
                    |fg: u8, bk: u8| (f32::from(fg) * t + f32::from(bk) * (1.0 - t)).round() as u8;
                rgba.push(mix(color[0], br));
                rgba.push(mix(color[1], bg_g));
                rgba.push(mix(color[2], bb));
                rgba.push(255);
            }
        }
        None => {
            for &a in &glyph.alpha {
                rgba.push(color[0]);
                rgba.push(color[1]);
                rgba.push(color[2]);
                rgba.push(a);
            }
        }
    }
    let image_data = web_sys::ImageData::new_with_u8_clamped_array_and_sh(
        Clamped(&rgba),
        glyph.width,
        glyph.height,
    )?;
    match dirty {
        None => ctx.put_image_data(&image_data, dx, dy)?,
        Some(d) => ctx.put_image_data_with_dirty_x_and_dirty_y_and_dirty_width_and_dirty_height(
            &image_data,
            dx,
            dy,
            d.x0,
            d.y0,
            d.width(),
            d.height(),
        )?,
    }
    Ok(())
}

/// Interpret a [`DisplayList`] onto a Canvas2D context, clipped to the dirty
/// `clip` region (D3.8). `resolve_font` maps a `FontId` to its `LoadedFont`.
///
/// Fills and strokes are bounded by the canvas clip path. Glyphs are blitted
/// with `put_image_data`, which the Canvas2D spec exempts from the clip — so a
/// glyph run whose bounding box misses `clip` is culled in the loop instead.
///
/// A `clip` covering the whole page is a no-op: every run intersects it and
/// nothing is excluded, so a full repaint is byte-identical to the unclipped
/// path.
pub fn render_canvas2d(
    ctx: &web_sys::OffscreenCanvasRenderingContext2d,
    list: &DisplayList,
    atlas: &mut GlyphAtlas,
    resolve_font: impl Fn(&str) -> Option<Arc<LoadedFont>>,
    resolve_image: impl Fn(&str) -> Option<web_sys::ImageBitmap>,
    clip: Rect,
) -> Result<(), JsValue> {
    /* Phase 6c multi-canvas — each canvas is exactly one page card.
    The CSS `.editor-page` element draws the white background + drop
    shadow; the canvas just paints page content over a transparent
    surface. Clearing to a colour here would mask the CSS bg's
    rounded corners / shadow ring; clearing to transparent keeps
    them visible. */
    let canvas = ctx.canvas();
    ctx.clear_rect(
        0.0,
        0.0,
        f64::from(canvas.width()),
        f64::from(canvas.height()),
    );

    ctx.save();
    ctx.begin_path();
    ctx.rect(clip.x0, clip.y0, clip.width(), clip.height());
    ctx.clip();

    /* Issue #169 — the active `PushClip` rects, intersected, so glyph
    blits (which ignore the canvas clip) can be cropped to them. Under a
    `PushTransform` the clip lives in transformed space the untransformed
    blit cannot follow, so cropping is skipped there (no emitter nests
    them today). */
    let mut push_clips: Vec<Rect> = Vec::new();
    let mut transform_depth: u32 = 0;

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
                /* `put_image_data` ignores the clip — cull off-region runs. */
                if let Some(bbox) = run_bbox(run) {
                    let hit = clip.intersect(bbox);
                    if hit.width() <= 0.0 || hit.height() <= 0.0 {
                        continue;
                    }
                }
                let Some(font) = resolve_font(&run.font) else {
                    continue;
                };
                let rgb = paint_rgb(&run.paint);
                let crop = if transform_depth == 0 {
                    push_clips.last().copied()
                } else {
                    None
                };
                for g in &run.glyphs {
                    let key = GlyphKey::new(
                        run.font.clone(),
                        g.glyph_id,
                        run.px_size,
                        run.faux_bold,
                        run.faux_italic,
                    );
                    if let Some(raster) = atlas.get_or_rasterize(&key, &font, run.px_size) {
                        paint_alpha_glyph_cropped(ctx, raster, g.x, g.y, rgb, run.bg_color, crop)?;
                    }
                }
            }
            DisplayCmd::DrawPageCard { rect } => {
                /* Phase 6c multi-canvas — the page card's white fill +
                drop shadow are now drawn by the wrapping
                `.editor-page` CSS element (each page is its own DOM
                node). The canvas paints content over a transparent
                surface; we still emit a white fill here as a safety
                net in case the CSS bg ever fails to apply. No
                shadow — Canvas2D's `shadow*` would blur outside the
                canvas bounds (which are sized exactly to the page)
                and clip invisibly. */
                ctx.set_fill_style_str("#ffffff");
                ctx.fill_rect(rect.x0, rect.y0, rect.width(), rect.height());
            }
            DisplayCmd::DrawImage { rect, rel_id } => {
                /* Phase 7 — inline image. If the worker has already
                decoded the bytes into an `ImageBitmap`, draw it at
                `rect` (Canvas2D's `drawImage` respects clip + transform).
                Cache miss falls back to a gray placeholder rectangle so
                the layout still shows the image's footprint. */
                if let Some(bitmap) = resolve_image(rel_id) {
                    ctx.draw_image_with_image_bitmap_and_dw_and_dh(
                        &bitmap,
                        rect.x0,
                        rect.y0,
                        rect.width(),
                        rect.height(),
                    )?;
                } else {
                    ctx.set_fill_style_str("#dddddd");
                    ctx.fill_rect(rect.x0, rect.y0, rect.width(), rect.height());
                    ctx.set_stroke_style_str("#999999");
                    ctx.set_line_width(1.0);
                    ctx.stroke_rect(rect.x0, rect.y0, rect.width(), rect.height());
                }
            }
            DisplayCmd::PushClip { rect } => {
                ctx.save();
                ctx.begin_path();
                ctx.rect(rect.x0, rect.y0, rect.width(), rect.height());
                ctx.clip();
                let top = push_clips.last().map_or(*rect, |c| c.intersect(*rect));
                push_clips.push(top);
            }
            DisplayCmd::PopClip => {
                push_clips.pop();
                ctx.restore();
            }
            DisplayCmd::PushTransform(affine) => {
                ctx.save();
                transform_depth += 1;
                let [a, b, c, d, e, f] = affine.as_coeffs();
                ctx.transform(a, b, c, d, e, f)?;
            }
            DisplayCmd::PopTransform => {
                transform_depth = transform_depth.saturating_sub(1);
                ctx.restore();
            }
        }
    }

    ctx.restore();
    Ok(())
}

/// Conservative bounding box of a glyph run — the pen extents padded by the
/// run's pixel size to cover ascenders, descenders, and side bearings. `None`
/// for an empty run.
fn run_bbox(run: &GlyphRun) -> Option<Rect> {
    if run.glyphs.is_empty() {
        return None;
    }
    let mut x0 = f64::MAX;
    let mut y0 = f64::MAX;
    let mut x1 = f64::MIN;
    let mut y1 = f64::MIN;
    for g in &run.glyphs {
        x0 = x0.min(g.x);
        y0 = y0.min(g.y);
        x1 = x1.max(g.x);
        y1 = y1.max(g.y);
    }
    let pad = f64::from(run.px_size);
    Some(Rect::new(x0 - pad, y0 - 2.0 * pad, x1 + pad, y1 + pad))
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
