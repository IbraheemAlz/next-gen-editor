//! Issue #392 — the `/FontDescriptor` values (ISO 32000-1 §9.8, Tables 122
//! and 123), derived from the face's own tables instead of the constants
//! every face used to share (`/FontBBox [-500 -500 1500 1500]`, `/StemV
//! 80`, `/ItalicAngle 0`).
//!
//! Every value is in PDF glyph space — 1000 units per em — so a font-unit
//! value `v` is written as `round(v × 1000 / unitsPerEm)`. ONE derivation
//! ([`descriptor_metrics`]) serves both outline flavours (`CIDFontType2`
//! and `CIDFontType0`) and both embeddings (subset and full):
//!
//! - **`/FontBBox`** — the `head` table's `xMin yMin xMax yMax`: the WHOLE
//!   face's box, not the subset's. The subsetter copies `head` (TrueType)
//!   and the Top DICT `FontBBox` (CFF) verbatim, so the embedded program
//!   itself declares the whole-face box — the descriptor then agrees with
//!   it. A box that encloses more glyphs than the subset holds is never
//!   wrong (nothing is clipped by it), the value does not depend on which
//!   glyphs a document shows (re-exports stay byte-stable), and it costs
//!   no outline walk. A degenerate `head` box (empty or inverted) falls
//!   back to `[0 Descent maxAdvance Ascent]`.
//! - **`/Ascent` `/Descent`** — the typographic ascender / descender when
//!   `OS/2` (version ≥ 4) sets `USE_TYPO_METRICS`, else `hhea`'s; a zero
//!   (or, for the ascent, negative) value takes the `head` box's top /
//!   bottom instead. The descent is written negative whatever sign the
//!   font stored. (The win metrics are never read: `ttf_parser` negates
//!   `usWinDescent` as an `i16`, which overflows on a hostile `0x8000` —
//!   see `text_pipeline::fonts::word_line_metrics`.)
//! - **`/CapHeight`** — `OS/2.sCapHeight` (version ≥ 2), else the top of
//!   the `H` glyph's outline, else the ascent. Always written: PDF 1.4 —
//!   which PDF/A-1 is based on — lists it as required.
//! - **`/XHeight`** — `OS/2.sxHeight`, else the top of the `x` glyph;
//!   omitted when neither exists (it is optional).
//! - **`/ItalicAngle`** — `post.italicAngle` (both count degrees
//!   counter-clockwise from the vertical), to 0.01°.
//! - **`/StemV`** — a CFF face's own `StdVW` (Private DICT; the first font
//!   DICT that has one in a CID-keyed CFF), else an estimate from
//!   `OS/2.usWeightClass` ([`stem_v_for_weight`]). No sfnt table carries a
//!   stem width for TrueType outlines, so those always take the estimate.
//! - **`/Flags`** — see [`flags`]: FixedPitch from `post.isFixedPitch`,
//!   Serif / Script from the `OS/2` family class (else PANOSE), Italic from
//!   the italic angle or the `fsSelection` italic / oblique bits, and
//!   always **Symbolic**, never Nonsymbolic.

use pdf_writer::Rect;
use pdf_writer::types::FontFlags;
use rustybuzz::ttf_parser::{Face, GlyphId, Tag};

/// What one face's `/FontDescriptor` says, in 1000-unit glyph space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct DescriptorMetrics {
    /// The `/Flags` bits ([`FontFlags`] is neither `Copy` nor `PartialEq`).
    pub(crate) flags: u32,
    /// `[llx lly urx ury]`.
    pub(crate) bbox: [f32; 4],
    pub(crate) italic_angle: f32,
    pub(crate) ascent: f32,
    /// Never positive.
    pub(crate) descent: f32,
    pub(crate) cap_height: f32,
    /// `None` — no source; the optional key is omitted.
    pub(crate) x_height: Option<f32>,
    pub(crate) stem_v: f32,
}

impl DescriptorMetrics {
    pub(crate) fn font_flags(&self) -> FontFlags {
        FontFlags::from_bits_truncate(self.flags)
    }

    pub(crate) fn bbox_rect(&self) -> Rect {
        let [x1, y1, x2, y2] = self.bbox;
        Rect::new(x1, y1, x2, y2)
    }
}

/// The descriptor values for `face` (see the module docs for each rule).
pub(crate) fn descriptor_metrics(face: &Face<'_>) -> DescriptorMetrics {
    /* ttf-parser refuses a `head` whose unitsPerEm is outside 16..=16384,
    so the divisor is never zero; the guard only keeps the math total. */
    let scale = 1000.0 / f32::from(face.units_per_em().max(1));
    let em = |v: f32| (v * scale).round();
    let head = face.global_bounding_box();

    let tables = face.tables();
    let (ascender, descender) = match tables.os2 {
        Some(os2) if os2.use_typographic_metrics() => {
            (os2.typographic_ascender(), os2.typographic_descender())
        }
        _ => (tables.hhea.ascender, tables.hhea.descender),
    };
    let ascent = if ascender > 0 {
        em(f32::from(ascender))
    } else {
        em(f32::from(head.y_max.max(0)))
    };
    let descent = if descender != 0 {
        -em(f32::from(descender)).abs()
    } else {
        em(f32::from(head.y_min.min(0)))
    };
    let bbox = if head.x_min < head.x_max && head.y_min < head.y_max {
        [
            em(f32::from(head.x_min)),
            em(f32::from(head.y_min)),
            em(f32::from(head.x_max)),
            em(f32::from(head.y_max)),
        ]
    } else {
        let max_advance = (0..face.number_of_glyphs())
            .filter_map(|g| face.glyph_hor_advance(GlyphId(g)))
            .max()
            .unwrap_or(0);
        [0.0, descent, em(f32::from(max_advance)), ascent]
    };
    let os2_or_glyph_top = |os2: Option<i16>, probe: char| {
        os2.filter(|&v| v > 0)
            .or_else(|| glyph_top(face, probe))
            .map(|v| em(f32::from(v)))
    };
    let cap_height = os2_or_glyph_top(face.capital_height(), 'H').unwrap_or(ascent);
    let x_height = os2_or_glyph_top(face.x_height(), 'x');
    let italic_angle = (face.italic_angle() * 100.0).round() / 100.0;
    let stem_v = face
        .raw_face()
        .table(Tag::from_bytes(b"CFF "))
        .and_then(cff_std_vw)
        .filter(|&v| v > 0.0 && v < f32::from(face.units_per_em()))
        .map(em)
        .unwrap_or_else(|| stem_v_for_weight(face.weight().to_number()));
    DescriptorMetrics {
        flags: flags(face, italic_angle).bits(),
        bbox,
        italic_angle,
        ascent,
        descent,
        cap_height,
        x_height,
        stem_v,
    }
}

/// The top of `ch`'s outline in font units — `None` when the face has no
/// such glyph or it is empty.
fn glyph_top(face: &Face<'_>, ch: char) -> Option<i16> {
    let gid = face.glyph_index(ch)?;
    face.glyph_bounding_box(gid)
        .map(|b| b.y_max)
        .filter(|&top| top > 0)
}

/// `/StemV` from the `OS/2` weight class when the face carries no stem
/// width of its own: a straight line through the Base-14 AFM `StdVW`
/// values — Helvetica 88 / Times-Roman 84 at weight 400, Helvetica-Bold
/// 140 / Times-Bold 139 at weight 700 — i.e. 86 at 400 rising 53.5 per
/// 300 weight units. A weight class outside the OS/2 range 1..=1000 (0
/// in a broken table) reads as 400.
fn stem_v_for_weight(weight_class: u16) -> f32 {
    let w = if (1..=1000).contains(&weight_class) {
        f32::from(weight_class)
    } else {
        400.0
    };
    (86.0 + (w - 400.0) * 53.5 / 300.0).round()
}

/// The descriptor flags (ISO 32000-1 Table 123).
///
/// **Symbolic, never Nonsymbolic.** Nonsymbolic promises that the font's
/// character set is the Adobe standard Latin set (or a subset) under its
/// standard glyph names, so a consumer may pick glyphs through a standard
/// Latin encoding. These fonts are `Identity-H` CIDFonts: a code is a
/// CID — the glyph id in the embedded program — never a name, and the
/// shaped runs show glyphs no standard encoding can reach (Arabic
/// positional forms, ligatures, Kashida). Symbolic is therefore the only
/// accurate value for every face, Latin ones included (§9.8.2: exactly one
/// of the two is set). PDF/A does not decide it: veraPDF's 1b / 2u / UA-1
/// profiles test the symbolic state only for simple `TrueType` fonts (the
/// ISO 19005-1 §6.3.7 / 19005-2 §6.2.11.6 `Encoding` and `cmap` rules),
/// and veraPDF 1.30 passes these exports with either flag (checked when
/// #392 landed), so the choice rests on §9.8.2 alone.
///
/// - FixedPitch — `post.isFixedPitch`.
/// - Serif — [`serif_class`]: the `OS/2` family class, else PANOSE.
/// - Script — `OS/2` family class 10 (Scripts), else PANOSE family kind 3
///   (Latin Hand Written).
/// - Italic — a non-zero italic angle or the `fsSelection` italic /
///   oblique bit.
/// - AllCap / SmallCap / ForceBold — no table states them; never set.
fn flags(face: &Face<'_>, italic_angle: f32) -> FontFlags {
    let os2 = face.raw_face().table(Tag::from_bytes(b"OS/2"));
    let class = os2.and_then(|t| t.get(30)).copied().unwrap_or(0);
    let panose = os2.and_then(|t| t.get(32..42));
    let panose_family = panose.map_or(0, |p| p[0]);
    let mut flags = FontFlags::SYMBOLIC;
    flags.set(FontFlags::FIXED_PITCH, face.is_monospaced());
    flags.set(FontFlags::SERIF, serif_class(class, panose) == Some(true));
    flags.set(FontFlags::SCRIPT, class == 10 || panose_family == 3);
    flags.set(
        FontFlags::ITALIC,
        italic_angle != 0.0 || face.is_italic() || face.is_oblique(),
    );
    flags
}

/// Whether the face has serifs: `Some(true / false)` when its tables say,
/// `None` when they do not.
///
/// The `OS/2` family class (`sFamilyClass` high byte) decides first:
/// 1-5 and 7 are the serif classes (Oldstyle, Transitional, Modern,
/// Clarendon, Slab, Freeform), 8 is Sans Serif. Any other class (0 = no
/// classification, ornamentals, scripts, symbolic) defers to PANOSE, read
/// only for Latin Text (family kind 2): serif styles 2-10 (cove …
/// triangle) are serifs, 11-15 (normal / obtuse / perpendicular sans,
/// flared, rounded) are not, 0-1 (any / no fit) say nothing.
fn serif_class(class: u8, panose: Option<&[u8]>) -> Option<bool> {
    match class {
        1..=5 | 7 => Some(true),
        8 => Some(false),
        _ => match panose {
            Some(&[2, serif, ..]) => match serif {
                2..=10 => Some(true),
                11..=15 => Some(false),
                _ => None,
            },
            _ => None,
        },
    }
}

/// The `StdVW` (dominant vertical stem width, font units) of a CFF
/// program: the Top DICT's Private DICT for a name-keyed font, else the
/// first font DICT in the `FDArray` whose Private DICT has one (CID-keyed).
/// `None` on any structural problem — every read is bounds-checked, and
/// the caller falls back to [`stem_v_for_weight`].
fn cff_std_vw(cff: &[u8]) -> Option<f32> {
    /* Header: major, minor, hdrSize, offSize. */
    let header_size = usize::from(*cff.get(2)?);
    let (_, after_names) = cff_index(cff, header_size)?;
    let (top_dicts, _) = cff_index(cff, after_names)?;
    let top = *top_dicts.first()?;
    if let Some(std_vw) = private_std_vw(cff, top) {
        return Some(std_vw);
    }
    let fd_array = dict_operand(top, FD_ARRAY, 0)?;
    let (font_dicts, _) = cff_index(cff, usize::try_from(fd_array as i64).ok()?)?;
    font_dicts.iter().find_map(|fd| private_std_vw(cff, fd))
}

/// DICT operators (CFF spec, Tables 9 / 23): one byte, or `12 x` encoded
/// here as `1200 + x`.
const PRIVATE: u16 = 18;
const STD_VW: u16 = 11;
const FD_ARRAY: u16 = 1236;

/// `StdVW` of the Private DICT a Top / font DICT points at.
fn private_std_vw(cff: &[u8], dict: &[u8]) -> Option<f32> {
    let size = usize::try_from(dict_operand(dict, PRIVATE, 0)? as i64).ok()?;
    let offset = usize::try_from(dict_operand(dict, PRIVATE, 1)? as i64).ok()?;
    let private = cff.get(offset..offset.checked_add(size)?)?;
    dict_operand(private, STD_VW, 0).map(|v| v as f32)
}

/// The items of the CFF INDEX at `at`, and the offset just past it.
fn cff_index(cff: &[u8], at: usize) -> Option<(Vec<&[u8]>, usize)> {
    /* Positions past a byte just read cannot overflow; everything built
    from an offset is checked — a 4-byte offset reaches 2^32 - 1, past
    `usize` on wasm32. */
    let count = usize::from(u16::from_be_bytes([
        *cff.get(at)?,
        *cff.get(at.checked_add(1)?)?,
    ]));
    if count == 0 {
        return Some((Vec::new(), at + 2));
    }
    let off_size = usize::from(*cff.get(at + 2)?);
    if !(1..=4).contains(&off_size) {
        return None;
    }
    let offsets_at = at + 3;
    let offset = |i: usize| -> Option<usize> {
        let start = offsets_at.checked_add(i * off_size)?;
        let bytes = cff.get(start..start.checked_add(off_size)?)?;
        Some(bytes.iter().fold(0usize, |v, &b| (v << 8) | usize::from(b)))
    };
    /* Offsets are 1-based from the byte before the data. */
    let data_base = offsets_at
        .checked_add((count + 1) * off_size)?
        .checked_sub(1)?;
    let at_data = |offset: usize| data_base.checked_add(offset);
    let mut items = Vec::with_capacity(count.min(cff.len()));
    for i in 0..count {
        let (start, end) = (offset(i)?, offset(i + 1)?);
        if start == 0 || end < start {
            return None;
        }
        items.push(cff.get(at_data(start)?..at_data(end)?)?);
    }
    Some((items, at_data(offset(count)?)?))
}

/// Operand `n` of the first `op` entry in a CFF DICT; `None` when the
/// operator is absent or the DICT is malformed before it.
fn dict_operand(dict: &[u8], op: u16, n: usize) -> Option<f64> {
    /* The CFF limit on a DICT's operand stack. */
    const MAX_OPERANDS: usize = 48;
    let mut operands: Vec<f64> = Vec::new();
    let mut i = 0;
    while let Some(&b0) = dict.get(i) {
        i += 1;
        let operand = match b0 {
            0..=21 => {
                let this = if b0 == 12 {
                    let b1 = *dict.get(i)?;
                    i += 1;
                    1200 + u16::from(b1)
                } else {
                    u16::from(b0)
                };
                if this == op {
                    return operands.get(n).copied();
                }
                operands.clear();
                continue;
            }
            28 => {
                let v = i16::from_be_bytes([*dict.get(i)?, *dict.get(i + 1)?]);
                i += 2;
                f64::from(v)
            }
            29 => {
                let b = dict.get(i..i + 4)?;
                i += 4;
                f64::from(i32::from_be_bytes([b[0], b[1], b[2], b[3]]))
            }
            30 => {
                let (v, used) = dict_real(dict.get(i..)?)?;
                i += used;
                v
            }
            32..=246 => f64::from(i32::from(b0) - 139),
            247..=250 => {
                let b1 = *dict.get(i)?;
                i += 1;
                f64::from((i32::from(b0) - 247) * 256 + i32::from(b1) + 108)
            }
            251..=254 => {
                let b1 = *dict.get(i)?;
                i += 1;
                f64::from(-(i32::from(b0) - 251) * 256 - i32::from(b1) - 108)
            }
            /* 22-27, 31, 255: reserved. */
            _ => return None,
        };
        if operands.len() == MAX_OPERANDS {
            return None;
        }
        operands.push(operand);
    }
    None
}

/// A nibble-packed real operand (after its `30` prefix): the value and the
/// bytes it used.
fn dict_real(data: &[u8]) -> Option<(f64, usize)> {
    let mut text = String::new();
    for (used, &byte) in data.iter().enumerate() {
        for nibble in [byte >> 4, byte & 0xf] {
            match nibble {
                0..=9 => text.push(char::from(b'0' + nibble)),
                0xa => text.push('.'),
                0xb => text.push('E'),
                0xc => text.push_str("E-"),
                0xe => text.push('-'),
                0xf => return Some((text.parse().ok()?, used + 1)),
                _ => return None,
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn face(data: &[u8]) -> Face<'_> {
        Face::parse(data, 0).expect("font parses")
    }

    fn bits(flags: FontFlags) -> u32 {
        flags.bits()
    }

    fn metrics(data: &[u8]) -> DescriptorMetrics {
        descriptor_metrics(&face(data))
    }

    const LIBERATION_SANS: &[u8] = include_bytes!("../../../ts/fonts/LiberationSans-Regular.ttf");
    const AMIRI: &[u8] = include_bytes!("../../../ts/fonts/Amiri-Regular.ttf");
    const NOTO_NASKH: &[u8] = include_bytes!("../../../ts/fonts/NotoNaskhArabic-Regular.ttf");
    const LIBERATION_SERIF: &[u8] =
        include_bytes!("../../../ts/public/fonts/LiberationSerif-Regular.ttf");
    const LIBERATION_MONO: &[u8] =
        include_bytes!("../../../ts/public/fonts/LiberationMono-Regular.ttf");
    const CALADEA: &[u8] = include_bytes!("../../../ts/public/fonts/Caladea-Regular.ttf");
    const CARLITO: &[u8] = include_bytes!("../../../ts/public/fonts/Carlito-Regular.ttf");

    /// A copy of `data` with `bytes` written at `offset` inside table `tag`.
    /// ttf-parser verifies no checksums, so a patched copy parses.
    fn patched(data: &[u8], tag: &[u8; 4], offset: usize, bytes: &[u8]) -> Vec<u8> {
        let table = crate::font_program::sfnt_table(data, *tag).expect("table");
        let at = table.as_ptr() as usize - data.as_ptr() as usize + offset;
        let mut out = data.to_vec();
        out[at..at + bytes.len()].copy_from_slice(bytes);
        out
    }

    /* ---- Liberation Sans (upem 2048, hhea metrics, OS/2 v3) ----------- */

    #[test]
    fn liberation_sans_bbox_is_the_head_box_in_1000_units() {
        /* head: -1114 -621 2666 2007 at 2048/em. */
        assert_eq!(
            metrics(LIBERATION_SANS).bbox,
            [-544.0, -303.0, 1302.0, 980.0]
        );
    }

    #[test]
    fn liberation_sans_ascent_and_descent_are_hhea() {
        /* fsSelection lacks USE_TYPO_METRICS: hhea 1854 / -434, not typo
        1491 / -431. */
        let m = metrics(LIBERATION_SANS);
        assert_eq!((m.ascent, m.descent), (905.0, -212.0));
    }

    #[test]
    fn liberation_sans_cap_and_x_height_are_os2() {
        /* sCapHeight 1409, sxHeight 1082. */
        let m = metrics(LIBERATION_SANS);
        assert_eq!((m.cap_height, m.x_height), (688.0, Some(528.0)));
    }

    #[test]
    fn liberation_sans_is_upright_symbolic_sans() {
        let m = metrics(LIBERATION_SANS);
        assert_eq!(m.italic_angle, 0.0);
        /* Family class 8 (sans serif), not fixed pitch, upright. */
        assert_eq!(m.flags, bits(FontFlags::SYMBOLIC));
    }

    #[test]
    fn liberation_sans_stem_v_is_the_weight_400_estimate() {
        /* No stem width in a TrueType face: usWeightClass 400. */
        assert_eq!(metrics(LIBERATION_SANS).stem_v, 86.0);
    }

    /* ---- Amiri (upem 1000, typo metrics) ------------------------------ */

    #[test]
    fn amiri_bbox_is_the_head_box() {
        assert_eq!(metrics(AMIRI).bbox, [-438.0, -900.0, 11467.0, 1815.0]);
    }

    #[test]
    fn amiri_ascent_and_descent_are_typo() {
        /* USE_TYPO_METRICS: 1124 / -634 (win would be 1850 / 910). */
        let m = metrics(AMIRI);
        assert_eq!((m.ascent, m.descent), (1124.0, -634.0));
    }

    #[test]
    fn amiri_cap_and_x_height_are_os2() {
        let m = metrics(AMIRI);
        assert_eq!((m.cap_height, m.x_height), (646.0, Some(433.0)));
    }

    #[test]
    fn amiri_flags_and_stem() {
        let m = metrics(AMIRI);
        /* Family class 0 and PANOSE family kind 0: no serif / script
        claim either way. */
        assert_eq!(m.flags, bits(FontFlags::SYMBOLIC));
        assert_eq!(m.italic_angle, 0.0);
        assert_eq!(m.stem_v, 86.0);
    }

    /* ---- Noto Naskh Arabic (OS/2 v4 with zero cap / x heights) -------- */

    #[test]
    fn noto_naskh_bbox_ascent_descent() {
        let m = metrics(NOTO_NASKH);
        assert_eq!(m.bbox, [-249.0, -610.0, 6092.0, 1056.0]);
        assert_eq!((m.ascent, m.descent), (1069.0, -634.0));
    }

    #[test]
    fn noto_naskh_cap_and_x_height_fall_back_past_the_zero_os2_values() {
        let f = face(NOTO_NASKH);
        assert_eq!(f.capital_height(), Some(0));
        assert_eq!(f.x_height(), Some(0));
        /* An Arabic-only face: no H, no x to measure either. */
        assert_eq!((glyph_top(&f, 'H'), glyph_top(&f, 'x')), (None, None));
        let m = metrics(NOTO_NASKH);
        assert_eq!(m.cap_height, 1069.0, "the ascent");
        assert_eq!(m.x_height, None, "/XHeight omitted");
    }

    #[test]
    fn noto_naskh_flags_and_stem() {
        let m = metrics(NOTO_NASKH);
        /* PANOSE Latin Text, serif style 11 (normal sans). */
        assert_eq!(
            serif_class(0, Some(&[2, 11, 5, 2, 4, 5, 4, 2, 2, 4])),
            Some(false)
        );
        assert_eq!(m.flags, bits(FontFlags::SYMBOLIC));
        assert_eq!(m.stem_v, 86.0);
    }

    /* ---- Flags across the editor's other shipped faces ---------------- */

    #[test]
    fn serif_comes_from_the_family_class() {
        /* Liberation Serif: class 1 (oldstyle serifs). */
        assert_eq!(
            metrics(LIBERATION_SERIF).flags,
            bits(FontFlags::SYMBOLIC | FontFlags::SERIF)
        );
    }

    #[test]
    fn fixed_pitch_comes_from_post_and_the_class_outranks_panose() {
        /* Liberation Mono: isFixedPitch; class 8 (sans) although its PANOSE
        serif style is 7 (thin). */
        assert_eq!(
            metrics(LIBERATION_MONO).flags,
            bits(FontFlags::SYMBOLIC | FontFlags::FIXED_PITCH)
        );
    }

    #[test]
    fn panose_decides_when_the_class_is_unset() {
        /* Caladea: class 0, PANOSE serif style 4 (square cove). Carlito:
        class 0, serif style 15 (rounded). */
        assert_eq!(
            metrics(CALADEA).flags,
            bits(FontFlags::SYMBOLIC | FontFlags::SERIF)
        );
        assert_eq!(metrics(CARLITO).flags, bits(FontFlags::SYMBOLIC));
    }

    #[test]
    fn serif_class_table() {
        for class in [1, 2, 3, 4, 5, 7] {
            assert_eq!(serif_class(class, None), Some(true), "class {class}");
        }
        assert_eq!(
            serif_class(8, Some(&[2, 2, 0, 0, 0, 0, 0, 0, 0, 0])),
            Some(false)
        );
        for serif in 2..=10 {
            assert_eq!(
                serif_class(0, Some(&[2, serif, 0, 0, 0, 0, 0, 0, 0, 0])),
                Some(true)
            );
        }
        for serif in 11..=15 {
            assert_eq!(
                serif_class(0, Some(&[2, serif, 0, 0, 0, 0, 0, 0, 0, 0])),
                Some(false)
            );
        }
        /* Any / no fit, or not Latin Text: no claim. */
        assert_eq!(serif_class(0, Some(&[2, 0, 0, 0, 0, 0, 0, 0, 0, 0])), None);
        assert_eq!(serif_class(0, Some(&[2, 1, 0, 0, 0, 0, 0, 0, 0, 0])), None);
        assert_eq!(serif_class(0, Some(&[3, 2, 0, 0, 0, 0, 0, 0, 0, 0])), None);
        assert_eq!(serif_class(10, None), None);
    }

    #[test]
    fn italic_angle_and_italic_flag_come_from_post() {
        /* post.italicAngle is a 16.16 Fixed at offset 4: -12.5° =
        -12.5 × 65536. */
        let font = patched(LIBERATION_SANS, b"post", 4, &(-819_200i32).to_be_bytes());
        let m = metrics(&font);
        assert_eq!(m.italic_angle, -12.5);
        assert_eq!(m.flags, bits(FontFlags::SYMBOLIC | FontFlags::ITALIC));
    }

    #[test]
    fn italic_flag_comes_from_fs_selection_too() {
        /* fsSelection (offset 62) bit 0, upright angle. */
        let font = patched(LIBERATION_SANS, b"OS/2", 62, &0x41u16.to_be_bytes());
        let m = metrics(&font);
        assert_eq!(m.italic_angle, 0.0);
        assert_eq!(m.flags, bits(FontFlags::SYMBOLIC | FontFlags::ITALIC));
    }

    #[test]
    fn script_flag_comes_from_the_family_class_or_panose() {
        /* sFamilyClass high byte (offset 30) = 10, Scripts. */
        let font = patched(LIBERATION_SANS, b"OS/2", 30, &[10, 0]);
        assert_eq!(
            metrics(&font).flags,
            bits(FontFlags::SYMBOLIC | FontFlags::SCRIPT)
        );
        /* Class unset, PANOSE (offset 32) family kind 3, hand written. */
        let font = patched(LIBERATION_SANS, b"OS/2", 30, &[0, 0, 3]);
        assert_eq!(
            metrics(&font).flags,
            bits(FontFlags::SYMBOLIC | FontFlags::SCRIPT)
        );
    }

    #[test]
    fn fixed_pitch_flag_comes_from_post() {
        /* post.isFixedPitch (offset 12). */
        let font = patched(LIBERATION_SANS, b"post", 12, &1u32.to_be_bytes());
        assert_eq!(
            metrics(&font).flags,
            bits(FontFlags::SYMBOLIC | FontFlags::FIXED_PITCH)
        );
    }

    #[test]
    fn stem_v_follows_the_weight_class() {
        assert_eq!(stem_v_for_weight(400), 86.0);
        assert_eq!(stem_v_for_weight(700), 140.0);
        assert_eq!(stem_v_for_weight(100), 33.0);
        assert_eq!(stem_v_for_weight(900), 175.0);
        /* Out of the OS/2 range: regular. */
        assert_eq!(stem_v_for_weight(0), 86.0);
        assert_eq!(stem_v_for_weight(5000), 86.0);
        /* Through the face: usWeightClass at OS/2 offset 4. */
        let bold = patched(LIBERATION_SANS, b"OS/2", 4, &700u16.to_be_bytes());
        assert_eq!(metrics(&bold).stem_v, 140.0);
    }

    #[test]
    fn typo_metrics_win_when_use_typo_metrics_is_set() {
        /* Liberation Sans as an OS/2 v4 table with fsSelection bit 7:
        typo 1491 / -431. */
        let v4 = patched(LIBERATION_SANS, b"OS/2", 0, &4u16.to_be_bytes());
        let font = patched(&v4, b"OS/2", 62, &0xC0u16.to_be_bytes());
        let m = metrics(&font);
        assert_eq!((m.ascent, m.descent), (728.0, -210.0));
        /* The bit means nothing before version 4 (it was reserved). */
        let v3 = patched(LIBERATION_SANS, b"OS/2", 62, &0xC0u16.to_be_bytes());
        assert_eq!(metrics(&v3).ascent, 905.0);
    }

    #[test]
    fn zero_line_metrics_take_the_head_box() {
        /* hhea ascender / descender (offset 4) zeroed. */
        let font = patched(LIBERATION_SANS, b"hhea", 4, &[0; 4]);
        let m = metrics(&font);
        assert_eq!((m.ascent, m.descent), (980.0, -303.0));
    }

    #[test]
    fn os2_cap_height_zero_measures_the_h_glyph() {
        /* sCapHeight (OS/2 offset 88) zeroed: the H outline's top, 1409
        in Liberation Sans as well. */
        let font = patched(LIBERATION_SANS, b"OS/2", 88, &[0, 0]);
        let f = face(&font);
        assert_eq!(glyph_top(&f, 'H'), Some(1409));
        assert_eq!(metrics(&font).cap_height, 688.0);
        /* sxHeight (offset 86) zeroed: the x outline's top. */
        let font = patched(LIBERATION_SANS, b"OS/2", 86, &[0, 0]);
        let top = glyph_top(&face(&font), 'x').expect("x");
        assert_eq!(
            metrics(&font).x_height,
            Some((f32::from(top) * 1000.0 / 2048.0).round())
        );
    }

    #[test]
    fn a_degenerate_head_box_falls_back_to_the_metrics() {
        /* head xMin..yMax (offset 36) all zero. */
        let font = patched(LIBERATION_SANS, b"head", 36, &[0; 8]);
        let m = metrics(&font);
        /* hhea.advanceWidthMax is 2740: the widest hmtx advance. */
        let f = face(&font);
        let widest = (0..f.number_of_glyphs())
            .filter_map(|g| f.glyph_hor_advance(GlyphId(g)))
            .max();
        assert_eq!(widest, Some(2740));
        assert_eq!(m.bbox, [0.0, -212.0, 1338.0, 905.0]);
    }

    /* ---- CFF (the synthesized `OTTO` twin, issue #361) ---------------- */

    #[test]
    fn synthesized_cff_shares_the_truetype_values_and_estimates_its_stem() {
        let otf = crate::cff_test_font::cff_from_truetype(LIBERATION_SANS, "NGE Synth Sans");
        let cff = metrics(&otf);
        let tt = metrics(LIBERATION_SANS);
        /* head / hhea / OS/2 / post are copied: the same values. */
        assert_eq!(cff.bbox, tt.bbox);
        assert_eq!((cff.ascent, cff.descent), (tt.ascent, tt.descent));
        assert_eq!((cff.cap_height, cff.x_height), (tt.cap_height, tt.x_height));
        assert_eq!(cff.italic_angle, 0.0);
        assert_eq!(cff.flags, bits(FontFlags::SYMBOLIC));
        /* Its Private DICT carries no StdVW: the weight estimate. */
        let table = crate::font_program::sfnt_table(&otf, *b"CFF ").expect("CFF ");
        assert_eq!(cff_std_vw(table), None);
        assert_eq!(cff.stem_v, 86.0);
    }

    #[test]
    fn cff_std_vw_is_the_stem_v() {
        /* StdVW 180 font units at 2048/em → 87.89 → 88. */
        let otf = crate::cff_test_font::cff_from_truetype_with_std_vw(
            LIBERATION_SANS,
            "NGE Synth Sans",
            Some(180),
        );
        let table = crate::font_program::sfnt_table(&otf, *b"CFF ").expect("CFF ");
        assert_eq!(cff_std_vw(table), Some(180.0));
        assert_eq!(metrics(&otf).stem_v, 88.0);
    }

    #[test]
    fn cid_keyed_cff_std_vw_comes_from_the_fd_array() {
        /* The subsetter writes every subset CID-keyed (ROS + FDArray), its
        Private DICT copied: the FDArray path finds the same StdVW. */
        let otf = crate::cff_test_font::cff_from_truetype_with_std_vw(
            LIBERATION_SANS,
            "NGE Synth Sans",
            Some(180),
        );
        let mut remapper = subsetter::GlyphRemapper::new();
        remapper.remap(face(&otf).glyph_index('H').expect("H").0);
        let program =
            crate::font_program::subset_program("synth", &otf, &remapper).expect("subsets");
        let top = cff_index(&program.data, usize::from(program.data[2]))
            .and_then(|(_, after)| cff_index(&program.data, after))
            .expect("top dict index")
            .0[0];
        assert_eq!(dict_operand(top, PRIVATE, 0), None, "no top-level Private");
        assert!(dict_operand(top, FD_ARRAY, 0).is_some(), "CID-keyed");
        assert_eq!(cff_std_vw(&program.data), Some(180.0));
    }

    #[test]
    fn an_implausible_std_vw_takes_the_weight_estimate() {
        /* Zero / a stem wider than the em is no stem width. */
        for bogus in [0, 4096] {
            let otf = crate::cff_test_font::cff_from_truetype_with_std_vw(
                LIBERATION_SANS,
                "NGE Synth Sans",
                Some(bogus),
            );
            assert_eq!(metrics(&otf).stem_v, 86.0, "StdVW {bogus}");
        }
    }

    #[test]
    fn cff_dict_operands_decode_every_encoding() {
        /* 1-byte, 2-byte +/-, 3-byte (28), 5-byte (29), real (30). */
        let dict = [
            139, 247, 0, 251, 0, 28, 0x12, 0x34, 29, 0, 1, 0, 0, 30, 0x1a, 0x5f, 12, 7,
        ];
        let at = |n| dict_operand(&dict, 1207, n);
        assert_eq!(at(0), Some(0.0));
        assert_eq!(at(1), Some(108.0));
        assert_eq!(at(2), Some(-108.0));
        assert_eq!(at(3), Some(f64::from(0x1234)));
        assert_eq!(at(4), Some(65536.0));
        assert_eq!(at(5), Some(1.5));
        assert_eq!(at(6), None);
        /* "-2.5E-3" */
        assert_eq!(dict_real(&[0xe2, 0xa5, 0xc3, 0xff]), Some((-0.0025, 4)));
        /* Operands reset at each operator; an absent operator is None. */
        assert_eq!(dict_operand(&[140, 10, 141, 11], STD_VW, 0), Some(2.0));
        assert_eq!(dict_operand(&[140, 10], STD_VW, 0), None);
    }

    #[test]
    fn malformed_cff_is_none_never_a_panic() {
        assert_eq!(cff_std_vw(&[]), None);
        assert_eq!(cff_std_vw(&[1, 0, 4, 4]), None);
        assert_eq!(cff_std_vw(&[1, 0, 200, 4]), None);
        /* An INDEX with offSize 0 / 9, and offsets past the end. */
        assert_eq!(cff_index(&[0, 1, 0], 0), None);
        assert_eq!(cff_index(&[0, 1, 9], 0), None);
        assert_eq!(cff_index(&[0, 1, 1, 1, 200], 0), None);
        /* Reserved operand bytes, a truncated operand, a stack overflow. */
        assert_eq!(dict_operand(&[22, 11], STD_VW, 0), None);
        assert_eq!(dict_operand(&[28, 1], STD_VW, 0), None);
        assert_eq!(dict_operand(&[139; 49], STD_VW, 0), None);
        /* Every truncation of a real CFF program. */
        let otf = crate::cff_test_font::cff_from_truetype_with_std_vw(
            LIBERATION_SANS,
            "NGE Synth Sans",
            Some(180),
        );
        let table = crate::font_program::sfnt_table(&otf, *b"CFF ").expect("CFF ");
        for len in (0..table.len()).step_by(97) {
            let _ = cff_std_vw(&table[..len]);
        }
    }
}
