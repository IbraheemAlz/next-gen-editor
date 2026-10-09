//! Issue #436 — glyph outlines as paths, for glyphs the mask rasterizer
//! refuses.
//!
//! [`text_pipeline::MAX_RASTER_PX`] (issue #422) bounds the alpha-mask
//! rasterizer, so a legitimately huge glyph (a 400 pt display font at 4×
//! zoom on a 3× display is 4800 device px) used to be skipped on Canvas2D.
//! Past the cap the Canvas2D backend now fills the glyph's outline as a
//! path instead: memory is the point count, not the area, so no cap on the
//! mask applies. This module is the backend-neutral builder (a `kurbo`
//! [`BezPath`] in display-list space); the Canvas2D interpreter replays the
//! path elements and any other path backend can do the same.

use kurbo::{BezPath, Point};
use text_pipeline::{FontError, LoadedFont, MAX_RASTER_PX, OutlineCmd};

/// Whether a glyph at `px_size` is drawn from its outline rather than a
/// cached alpha mask. Non-finite / non-positive sizes are not outline
/// glyphs either — they are refused by both paths.
pub fn use_outline(px_size: f32) -> bool {
    px_size.is_finite() && px_size > MAX_RASTER_PX
}

/// The glyph's outline as a path in display-list space (y down), with the
/// pen/baseline origin at `(origin_x, origin_y)`. `italic` applies the same
/// shear as the mask path's faux italic.
pub fn glyph_path(
    font: &LoadedFont,
    glyph_id: u16,
    px_size: f32,
    origin: (f64, f64),
    italic: bool,
) -> Result<BezPath, FontError> {
    let cmds = font.glyph_outline(glyph_id, px_size)?;
    Ok(path_from_outline(&cmds, origin, italic))
}

/// Convert scaled y-up outline commands to a y-down [`BezPath`] at `origin`.
pub fn path_from_outline(cmds: &[OutlineCmd], origin: (f64, f64), italic: bool) -> BezPath {
    let shear = if italic {
        f64::from(crate::synth::SHEAR)
    } else {
        0.0
    };
    let pt = |p: &[f32; 2]| {
        let (x, y) = (f64::from(p[0]), f64::from(p[1]));
        Point::new(origin.0 + x + shear * y, origin.1 - y)
    };
    let mut path = BezPath::new();
    for c in cmds {
        match c {
            OutlineCmd::MoveTo(p) => path.move_to(pt(p)),
            OutlineCmd::LineTo(p) => path.line_to(pt(p)),
            OutlineCmd::QuadTo(a, b) => path.quad_to(pt(a), pt(b)),
            OutlineCmd::CurveTo(a, b, c) => path.curve_to(pt(a), pt(b), pt(c)),
            OutlineCmd::Close => path.close_path(),
        }
    }
    path
}

/// Faux-bold stroke width (px) for the outline path — the dilation radius
/// the mask path uses, applied as a stroke centred on the outline and
/// shifted half a radius down-right by the caller.
pub fn faux_bold_stroke(px_size: f32) -> f64 {
    f64::from(crate::synth::embolden_radius(px_size))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::{DisplayCmd, DisplayList, GlyphRun, Paint, RunGlyph};
    use kurbo::Shape;

    fn liberation() -> LoadedFont {
        let bytes = include_bytes!("../../../ts/fonts/LiberationSans-Regular.ttf").to_vec();
        LoadedFont::parse("lib".to_string(), bytes).expect("font")
    }

    /// What the Canvas2D interpreter decides for one glyph of a run:
    /// `Some(path)` = filled outline, `None` = mask path.
    fn outline_for(font: &LoadedFont, run: &GlyphRun) -> Vec<Option<BezPath>> {
        run.glyphs
            .iter()
            .map(|g| {
                use_outline(run.px_size).then(|| {
                    glyph_path(font, g.glyph_id, run.px_size, (g.x, g.y), run.faux_italic)
                        .expect("outline")
                })
            })
            .collect()
    }

    /* Issue #436 — a 400 pt glyph at zoom 4 on a 3x display (4800 device
    px) is above the mask cap: it is drawn, as an outline whose box matches
    the font's own cap height, at the glyph's baseline origin. */
    #[test]
    fn a_400pt_glyph_at_zoom_4_is_drawn_as_an_outline() {
        let font = liberation();
        let gid = font.glyph_id('H').expect("H");
        let px = 400.0_f32 * 4.0 * 3.0;
        assert!(px > MAX_RASTER_PX);
        assert!(font.rasterize_glyph(gid, px).is_err(), "mask path refuses");
        let run = GlyphRun {
            font: "lib".to_string(),
            px_size: px,
            paint: Paint::solid(peniko::Color::from_rgba8(0, 0, 0, 255)),
            glyphs: vec![RunGlyph {
                glyph_id: gid,
                x: 100.0,
                y: 6000.0,
            }],
            faux_bold: false,
            faux_italic: false,
            bg_color: None,
        };
        let mut list = DisplayList::default();
        list.cmds.push(DisplayCmd::DrawGlyphRun(run));
        let DisplayCmd::DrawGlyphRun(run) = &list.cmds[0] else {
            unreachable!()
        };
        let paths = outline_for(&font, run);
        let path = paths[0].as_ref().expect("drawn as an outline");
        let bb = path.bounding_box();
        assert!(bb.width() > 0.0 && bb.height() > f64::from(px) * 0.6);
        /* Sits on the baseline (y down) and starts right of the origin. */
        assert!((bb.y1 - 6000.0).abs() < 1.0, "{bb:?}");
        assert!(bb.y0 < 6000.0 - f64::from(px) * 0.6);
        assert!(bb.x0 > 100.0);
    }

    /* Nominal sizes stay on the mask path (goldens unchanged). */
    #[test]
    fn nominal_sizes_keep_the_mask_path() {
        for px in [8.0, 16.0, 96.0, MAX_RASTER_PX] {
            assert!(!use_outline(px), "{px}");
        }
        assert!(use_outline(MAX_RASTER_PX + 1.0));
        assert!(!use_outline(f32::NAN) && !use_outline(f32::INFINITY));
    }

    /* A hostile size is refused with a typed error, never a huge path. */
    #[test]
    fn hostile_sizes_are_refused() {
        let font = liberation();
        let gid = font.glyph_id('H').expect("H");
        assert!(glyph_path(&font, gid, f32::INFINITY, (0.0, 0.0), false).is_err());
        assert!(glyph_path(&font, gid, 1.0e12, (0.0, 0.0), false).is_err());
    }

    #[test]
    fn italic_shears_the_top_to_the_right() {
        let font = liberation();
        let gid = font.glyph_id('H').expect("H");
        let upright = glyph_path(&font, gid, 5000.0, (0.0, 0.0), false).expect("p");
        let slanted = glyph_path(&font, gid, 5000.0, (0.0, 0.0), true).expect("p");
        assert!(slanted.bounding_box().x1 > upright.bounding_box().x1);
    }
}
