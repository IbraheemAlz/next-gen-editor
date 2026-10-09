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
///
/// Issue #422 — output coverage is the max over the `(radius + 1)²` box of
/// source pixels up and to the left. A box max is separable (rows, then
/// columns) and each pass is a sliding-window max, so the cost is
/// `O(width × height)` whatever the radius. The direct four-nested-loop
/// form was `O(width × height × radius²)`: ~6 × 10¹¹ steps for a 4096 px
/// glyph (radius 186), a hang. The output is identical
/// (`embolden_matches_the_direct_box_dilation`).
pub fn embolden(g: &RasterizedGlyph, px_size: f32) -> RasterizedGlyph {
    if g.width == 0 || g.height == 0 {
        return g.clone();
    }
    let radius = ((px_size / 22.0).round() as u32).max(1);
    let (w, h) = (g.width as usize, g.height as usize);
    let r = radius as usize;
    let (nw, nh) = (w + r, h + r);
    /* Rows: `rows[y][x]` = max of source row `y` over columns `x-r ..= x`. */
    let mut rows = vec![0u8; nw * h];
    let mut window = std::collections::VecDeque::new();
    for y in 0..h {
        window_max(
            &g.alpha[y * w..(y + 1) * w],
            r,
            &mut rows[y * nw..(y + 1) * nw],
            &mut window,
        );
    }
    /* Columns: `alpha[y][x]` = max of `rows` column `x` over rows `y-r ..= y`. */
    let mut alpha = vec![0u8; nw * nh];
    let mut col = vec![0u8; h];
    let mut out = vec![0u8; nh];
    for x in 0..nw {
        for (y, c) in col.iter_mut().enumerate() {
            *c = rows[y * nw + x];
        }
        window_max(&col, r, &mut out, &mut window);
        for (y, v) in out.iter().enumerate() {
            alpha[y * nw + x] = *v;
        }
    }
    RasterizedGlyph {
        width: nw as u32,
        height: nh as u32,
        left: g.left,
        top: g.top,
        alpha,
    }
}

/// `out[i]` = the max of `src[j]` for `j` in `i - r ..= i` within `src`
/// (0 when that window holds no source sample); `out.len()` is
/// `src.len() + r`. A monotonic deque of `src` indices whose values
/// decrease front to back — every index enters and leaves once.
fn window_max(
    src: &[u8],
    r: usize,
    out: &mut [u8],
    window: &mut std::collections::VecDeque<usize>,
) {
    window.clear();
    for (i, o) in out.iter_mut().enumerate() {
        if let Some(&v) = src.get(i) {
            while window.back().is_some_and(|&b| src[b] <= v) {
                window.pop_back();
            }
            window.push_back(i);
        }
        while window.front().is_some_and(|&f| f + r < i) {
            window.pop_front();
        }
        *o = window.front().map_or(0, |&f| src[f]);
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

    /// The pre-#422 direct form — the reference [`embolden`] must match.
    fn embolden_direct(g: &RasterizedGlyph, px_size: f32) -> RasterizedGlyph {
        if g.width == 0 || g.height == 0 {
            return g.clone();
        }
        let radius = ((px_size / 22.0).round() as u32).max(1);
        let (w, h) = (g.width, g.height);
        let (nw, nh) = (w + radius, h + radius);
        let mut alpha = vec![0u8; (nw * nh) as usize];
        for y in 0..nh {
            for x in 0..nw {
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

    /// Issue #422 — the separable sliding-window dilation is byte-identical
    /// to the direct box dilation on noisy masks, every shape and radius.
    #[test]
    fn embolden_matches_the_direct_box_dilation() {
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for (w, h) in [(1, 1), (1, 7), (7, 1), (5, 9), (13, 4), (31, 29)] {
            for px in [1.0, 22.0, 33.0, 44.0, 90.0, 300.0] {
                let alpha: Vec<u8> = (0..w * h)
                    .map(|_| match next() % 4 {
                        0 => 0,
                        1 => 255,
                        _ => (next() % 256) as u8,
                    })
                    .collect();
                let g = RasterizedGlyph {
                    width: w,
                    height: h,
                    left: -2,
                    top: 5,
                    alpha,
                };
                let (a, b) = (embolden(&g, px), embolden_direct(&g, px));
                assert_eq!(
                    (a.width, a.height, a.left, a.top),
                    (b.width, b.height, b.left, b.top)
                );
                assert!(a.alpha == b.alpha, "{w}x{h} at {px}px");
            }
        }
    }

    /// Issue #422 — a cap-sized glyph emboldens in linear time.
    #[test]
    fn embolden_is_linear_in_the_mask_size() {
        let g = solid(2048, 2048);
        let started = std::time::Instant::now();
        let bold = embolden(&g, 4096.0);
        assert_eq!(bold.width, 2048 + 186);
        assert!(started.elapsed() < std::time::Duration::from_secs(20));
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
