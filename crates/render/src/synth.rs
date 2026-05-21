//! Synthetic (faux) bold + italic for fonts that ship no real variant
//! (Backlog #1). Both transform a rasterized alpha mask: [`embolden`] dilates
//! the coverage, [`slant`] shears it. Cheap enough to run once per glyph, and
//! [`crate::atlas::GlyphAtlas`] caches the result keyed by style.

use text_pipeline::RasterizedGlyph;

/// Shear factor for faux italic — a row's horizontal shift is its distance
/// above the baseline times this. ~12.4 degrees, a conventional oblique angle.
const SHEAR: f32 = 0.22;

/// Faux bold — dilate the alpha mask down-and-right by `radius` pixels so
/// every stroke thickens. `radius` scales with `px_size` (~1 px at 22 pt). The
/// mask grows `radius` on the right and bottom; `left` / `top` (measured to
/// the top-left origin) are unchanged.
pub fn embolden(g: &RasterizedGlyph, px_size: f32) -> RasterizedGlyph {
    if g.width == 0 || g.height == 0 {
        return g.clone();
    }
    let radius = ((px_size / 22.0).round() as u32).max(1);
    let (w, h) = (g.width, g.height);
    let (nw, nh) = (w + radius, h + radius);
    let mut alpha = vec![0u8; (nw * nh) as usize];
    for y in 0..nh {
        for x in 0..nw {
            /* Output coverage is the max over the `radius`-sized box of source
            pixels up and to the left — a morphological dilation. */
            let mut cover = 0u8;
            for dy in 0..=radius {
                for dx in 0..=radius {
                    if x >= dx && y >= dy {
                        let (sx, sy) = (x - dx, y - dy);
                        if sx < w && sy < h {
                            cover = cover.max(g.alpha[(sy * w + sx) as usize]);
                        }
                    }
                }
            }
            alpha[(y * nw + x) as usize] = cover;
        }
    }
    RasterizedGlyph {
        width: nw,
        height: nh,
        left: g.left,
        top: g.top,
        alpha,
    }
}

/// Faux italic — shear the alpha mask so rows above the baseline lean right
/// and rows below lean left, pivoting on the baseline. The mask widens to hold
/// the lean; `left` shifts so the pen origin still lands correctly.
pub fn slant(g: &RasterizedGlyph) -> RasterizedGlyph {
    if g.width == 0 || g.height == 0 {
        return g.clone();
    }
    let (w, h) = (g.width as i32, g.height as i32);
    /* Bitmap row `top` sits on the baseline — `top` counts rows above it. */
    let dx_of = |y: i32| -> i32 { (((g.top - y) as f32) * SHEAR).round() as i32 };
    let mut min_dx = 0;
    let mut max_dx = 0;
    for y in 0..h {
        let d = dx_of(y);
        min_dx = min_dx.min(d);
        max_dx = max_dx.max(d);
    }
    let nw = w + max_dx - min_dx;
    let mut alpha = vec![0u8; (nw * h) as usize];
    for y in 0..h {
        let shift = dx_of(y) - min_dx; // always >= 0
        for x in 0..w {
            let v = g.alpha[(y * w + x) as usize];
            alpha[(y * nw + x + shift) as usize] = v;
        }
    }
    RasterizedGlyph {
        width: nw as u32,
        height: g.height,
        left: g.left + min_dx,
        top: g.top,
        alpha,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32) -> RasterizedGlyph {
        RasterizedGlyph {
            width: w,
            height: h,
            left: 0,
            top: h as i32,
            alpha: vec![255; (w * h) as usize],
        }
    }

    #[test]
    fn embolden_grows_the_mask() {
        let bold = embolden(&solid(10, 10), 24.0);
        assert!(bold.width > 10 && bold.height > 10);
        /* A fully-covered glyph stays fully covered. */
        assert!(bold.alpha.iter().all(|&a| a == 255));
    }

    #[test]
    fn slant_widens_and_shifts_left() {
        let g = solid(10, 20);
        let it = slant(&g);
        /* Shearing a 20-tall mask widens it; no descender here, so the lean
        is rightward only and `left` is unchanged. */
        assert!(it.width > 10);
        assert_eq!(it.height, 20);
        assert_eq!(it.left, 0);
    }

    #[test]
    fn empty_glyph_is_untouched() {
        let empty = RasterizedGlyph {
            width: 0,
            height: 0,
            left: 0,
            top: 0,
            alpha: Vec::new(),
        };
        assert_eq!(embolden(&empty, 24.0).width, 0);
        assert_eq!(slant(&empty).width, 0);
    }
}
