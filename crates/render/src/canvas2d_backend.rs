//! Canvas2D backend — paints a `RasterizedGlyph` alpha mask via `putImageData`.

use text_pipeline::RasterizedGlyph;
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
