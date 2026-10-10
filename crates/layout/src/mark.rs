//! Issue #370 — the paragraph MARK of an empty paragraph.
//!
//! Word sizes an empty paragraph's one line from its paragraph mark — the
//! pilcrow's own run properties (`<w:pPr><w:rPr>`, folded over the
//! paragraph style): a spacer paragraph whose mark is 2 pt is a thin line,
//! one whose mark is 28 pt a tall one, whatever the document default. The
//! mark itself is never painted.
//!
//! The layout contract every pipeline shares (engine-wasm, the corpus and
//! differential harnesses):
//!
//! * the caller passes an empty paragraph ONE zero-width style span
//!   `[0, 0)` — the resolved mark style — as `ParagraphConfig::spans`;
//! * it resolves the line pitch for the mark with [`empty_mark_pitch`]
//!   when the paragraph's line rule is font-relative (no `w:line`
//!   override, or an `auto` multiple; `exact` / `atLeast` are absolute
//!   and stay as resolved);
//! * [`empty_line_extents`] then sizes the placeholder line: that pitch,
//!   grown to the mark glyph's own ascent + descent when the glyph is
//!   taller.
//!
//! **The pitch rule.** The nominal pitch a caller resolves is the floor
//! every line of text gets — a document-wide value standing for one line
//! of the document's DEFAULT run style (the `base` span); a line of text
//! grows past it only by its glyph envelope (`line_extents`). A mark
//! SMALLER than the default scales that floor down with it — by the ratio
//! of the two faces' single-line metrics ([`mark_line_ratio`]), which is
//! how much shorter Word makes the mark's line than a default one. A
//! LARGER mark never raises the floor: like a line of text in its face it
//! grows only to its glyph extents, so an empty paragraph is never taller
//! than the same paragraph holding one character of the mark's
//! formatting.
//!
//! A mark that formats like the default (the common case: an unstyled
//! paragraph with no mark properties) gives exactly the nominal pitch and
//! — while its glyph fits it — the pre-#370 placeholder, bit for bit, so
//! only documents whose marks really differ move.

use crate::boxes::StyleSpan;
use text_pipeline::{FontStack, LoadedFont, Script};

/// The face, size and font id one piece of `span` shapes with — the
/// complex-script twins for an RTL / complex paragraph — exactly as the
/// line builder resolves a run (`StyleSpan::face_for` + `FontStack::resolve`).
fn mark_face<'a>(
    fonts: &'a FontStack,
    span: &StyleSpan,
    complex: bool,
) -> Option<(&'a str, &'a LoadedFont, f32)> {
    let sf = span.face_for(complex);
    /* An empty paragraph has no characters to classify: an RTL
    paragraph's mark is complex script — Arabic, the same fallback the
    engine's font-slot resolver uses for a run with no complex-script
    character. */
    let script = if complex {
        Script::Arabic
    } else {
        Script::Latin
    };
    let (id, face, _) = fonts.resolve(script, sf.font_family, sf.bold, sf.italic)?;
    Some((id.as_str(), face, sf.px_size))
}

/// Single-line height of `face` at `px`: ascent + descent + line gap.
fn single_line(face: &LoadedFont, px: f32) -> f32 {
    let m = face.metrics(px);
    m.ascent + m.descent.abs() + m.leading.max(0.0)
}

/// Issue #370 — how tall a single line set in the paragraph `mark`'s face
/// is relative to one set in the document default's `base` face (both resolved
/// through `fonts`; `complex` selects the complex-script twins, as for an
/// RTL paragraph). Exactly `1.0` when both resolve to the same face at
/// the same size, or when either cannot be measured.
pub fn mark_line_ratio(
    fonts: &FontStack,
    base: &StyleSpan,
    mark: &StyleSpan,
    complex: bool,
) -> f32 {
    let (Some((base_id, base_face, base_px)), Some((mark_id, mark_face, mark_px))) = (
        mark_face(fonts, base, complex),
        mark_face(fonts, mark, complex),
    ) else {
        return 1.0;
    };
    if base_id == mark_id && base_px.to_bits() == mark_px.to_bits() {
        return 1.0;
    }
    let base_line = single_line(base_face, base_px);
    let mark_line = single_line(mark_face, mark_px);
    if !(base_line.is_finite() && mark_line.is_finite()) || base_line <= 0.0 || mark_line <= 0.0 {
        return 1.0;
    }
    mark_line / base_line
}

/// Issue #370 — the line pitch of an empty paragraph whose nominal
/// (document default) line pitch is `nominal` (see the module docs'
/// pitch rule): `nominal` scaled down by [`mark_line_ratio`] for a mark
/// whose single line is shorter than the default's, `nominal` itself
/// otherwise (a taller
/// mark grows the line through its glyph extents in
/// [`empty_line_extents`], never through the floor).
pub fn empty_mark_pitch(
    fonts: &FontStack,
    nominal: f32,
    base: &StyleSpan,
    mark: &StyleSpan,
    complex: bool,
) -> f32 {
    let ratio = mark_line_ratio(fonts, base, mark, complex);
    if ratio < 1.0 {
        nominal * ratio
    } else {
        nominal
    }
}

/// Issue #370 — `(baseline, height)` of an EMPTY paragraph's placeholder
/// line. `mark` is the paragraph's zero-width mark span (`None` for a
/// caller that passes none: the nominal line, as before #370),
/// `line_height` the pitch the caller resolved for the mark
/// ([`empty_mark_pitch`]), `exact` the `w:lineRule="exact"` flag (the
/// line box is the pitch, whatever the glyph).
///
/// The pitch wins while the mark glyph fits it — baseline on the line's
/// bottom edge, the pre-#370 placeholder shape. A taller glyph grows the
/// line to its ascent + descent with the baseline at the ascent, the
/// big-glyph rule of a line of text.
pub fn empty_line_extents(
    fonts: &FontStack,
    mark: Option<&StyleSpan>,
    complex: bool,
    line_height: f32,
    exact: bool,
) -> (f32, f32) {
    let nominal = (line_height, line_height);
    if exact {
        return nominal;
    }
    let Some((_, face, px)) = mark.and_then(|m| mark_face(fonts, m, complex)) else {
        return nominal;
    };
    let m = face.metrics(px);
    let (ascent, descent) = (m.ascent, m.descent.abs());
    if ascent + descent > line_height {
        (ascent, ascent + descent)
    } else {
        nominal
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Arc;

    fn stack() -> FontStack {
        let mut faces: HashMap<String, Arc<LoadedFont>> = HashMap::new();
        for (id, bytes) in [
            (
                "liberation",
                &include_bytes!("../../../ts/fonts/LiberationSans-Regular.ttf")[..],
            ),
            (
                "amiri",
                &include_bytes!("../../../ts/fonts/Amiri-Regular.ttf")[..],
            ),
        ] {
            let face = LoadedFont::parse(id.to_string(), bytes.to_vec()).expect("font parses");
            faces.insert(id.to_string(), Arc::new(face));
        }
        FontStack::from_faces(faces, "liberation")
    }

    fn span(px: f32) -> StyleSpan {
        StyleSpan {
            start: 0,
            end: 0,
            px_size: px,
            color: [0, 0, 0, 255],
            bold: false,
            italic: false,
            underline: engine::UnderlineStyle::None,
            strike: false,
            bg_color: None,
            font_family: None,
            caps_transform: false,
            baseline_shift_px: 0.0,
            cs: None,
        }
    }

    #[test]
    fn a_mark_like_the_base_has_ratio_exactly_one() {
        let fonts = stack();
        assert_eq!(
            mark_line_ratio(&fonts, &span(12.0), &span(12.0), false),
            1.0
        );
        assert_eq!(mark_line_ratio(&fonts, &span(12.0), &span(12.0), true), 1.0);
        assert_eq!(
            empty_mark_pitch(&fonts, 26.0, &span(12.0), &span(12.0), false).to_bits(),
            26.0_f32.to_bits()
        );
    }

    #[test]
    fn a_mark_in_the_same_face_scales_with_its_size() {
        let fonts = stack();
        let half = mark_line_ratio(&fonts, &span(12.0), &span(6.0), false);
        assert!((half - 0.5).abs() < 1e-4, "{half}");
        let double = mark_line_ratio(&fonts, &span(12.0), &span(24.0), false);
        assert!((double - 2.0).abs() < 1e-4, "{double}");
    }

    #[test]
    fn only_a_smaller_mark_moves_the_pitch() {
        let fonts = stack();
        let small = empty_mark_pitch(&fonts, 26.0, &span(12.0), &span(3.0), false);
        assert!((small - 6.5).abs() < 1e-3, "{small}");
        /* A larger mark keeps the floor; its glyph grows the line. */
        assert_eq!(
            empty_mark_pitch(&fonts, 26.0, &span(12.0), &span(48.0), false),
            26.0
        );
    }

    #[test]
    fn an_rtl_mark_measures_its_complex_script_twin() {
        let fonts = stack();
        let mark = span(12.0).with_cs(crate::boxes::ComplexScriptAttrs {
            px_size: 6.0,
            baseline_shift_px: 0.0,
            bold: false,
            italic: false,
            font_family: None,
            whole_span: false,
        });
        /* LTR: the Latin size (equal to the base) — no change. */
        assert_eq!(mark_line_ratio(&fonts, &span(12.0), &mark, false), 1.0);
        /* RTL: the 6 px complex-script twin against the 12 px base. */
        let rtl = mark_line_ratio(&fonts, &span(12.0), &mark, true);
        assert!((rtl - 0.5).abs() < 1e-4, "{rtl}");
    }

    #[test]
    fn the_placeholder_keeps_the_nominal_shape_while_the_mark_fits() {
        let fonts = stack();
        assert_eq!(
            empty_line_extents(&fonts, Some(&span(12.0)), false, 26.0, false),
            (26.0, 26.0)
        );
        assert_eq!(
            empty_line_extents(&fonts, None, false, 26.0, false),
            (26.0, 26.0)
        );
    }

    #[test]
    fn a_tall_mark_grows_the_placeholder_to_its_glyph_extents() {
        let fonts = stack();
        let (baseline, height) = empty_line_extents(&fonts, Some(&span(72.0)), false, 26.0, false);
        assert!(height > 26.0, "{height}");
        assert!(
            baseline < height && baseline > height / 2.0,
            "{baseline} / {height}"
        );
        /* Exact spacing never grows. */
        assert_eq!(
            empty_line_extents(&fonts, Some(&span(72.0)), false, 26.0, true),
            (26.0, 26.0)
        );
    }
}
