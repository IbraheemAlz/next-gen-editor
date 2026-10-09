//! Issue #361 — a CFF-flavoured OpenType (`OTTO`) font, synthesized at
//! test time. No binary font fixture lands in the tree (the `build.rs`
//! ICC-profile precedent): [`cff_from_truetype`] converts one of the OFL
//! faces the repo already ships (`ts/fonts/*.ttf`) into its CFF twin.
//!
//! Every glyph keeps its id — its quadratic TrueType outline is re-encoded
//! as a Type 2 charstring (each quadratic segment as the exactly-equivalent
//! cubic, control points rounded to integer font units) — so the source's
//! `cmap`, `hmtx`/`hhea`, `OS/2` and OpenType layout tables (`GSUB`,
//! `GPOS`, `GDEF`, …) are copied verbatim and shaping is unchanged: Arabic
//! joins exactly as it does with the TrueType face. The units-per-em is
//! kept too (a non-1000 em gets an explicit `FontMatrix`), so layout values
//! in font units stay valid. The hinting / TrueType-outline tables are
//! dropped, `maxp` becomes version 0.5, `post` version 3.0, and the `name`
//! table is replaced with a synthetic family name — the derivative must not
//! reuse a Reserved Font Name (SIL OFL 1.1 §3).

use rustybuzz::ttf_parser::{self, GlyphId, OutlineBuilder};

/// Build the CFF-flavoured twin of the TrueType font `ttf`, named `family`.
pub(crate) fn cff_from_truetype(ttf: &[u8], family: &str) -> Vec<u8> {
    let face = ttf_parser::Face::parse(ttf, 0).expect("source TrueType face");
    let n = face.number_of_glyphs();
    let upem = face.units_per_em();
    let mut charstrings = Vec::with_capacity(usize::from(n));
    for gid in 0..n {
        let mut cs = Charstring::new(face.glyph_hor_advance(GlyphId(gid)).unwrap_or(0));
        face.outline_glyph(GlyphId(gid), &mut cs);
        charstrings.push(cs.finish());
    }
    let bbox = face.global_bounding_box();
    let ps_name: String = family.chars().filter(|c| !c.is_whitespace()).collect();
    let cff = cff_table(
        &format!("{ps_name}-Regular"),
        upem,
        [bbox.x_min, bbox.y_min, bbox.x_max, bbox.y_max],
        &charstrings,
    );

    /* Copy every table the CFF twin can share; replace the rest. */
    const DROP: [&[u8; 4]; 12] = [
        b"glyf", b"loca", b"cvt ", b"fpgm", b"prep", b"gasp", b"hdmx", b"LTSH", b"VDMX", b"DSIG",
        b"maxp", b"post",
    ];
    let mut tables: Vec<([u8; 4], Vec<u8>)> = Vec::new();
    for (tag, data) in sfnt_tables(ttf) {
        if DROP.contains(&&tag) || &tag == b"name" {
            continue;
        }
        tables.push((tag, data.to_vec()));
    }
    let mut maxp = 0x0000_5000u32.to_be_bytes().to_vec();
    maxp.extend_from_slice(&n.to_be_bytes());
    tables.push((*b"maxp", maxp));
    let mut post = crate::font_program::sfnt_table(ttf, *b"post")
        .map(|p| p[..32].to_vec())
        .unwrap_or_else(|| vec![0; 32]);
    post[..4].copy_from_slice(&0x0003_0000u32.to_be_bytes());
    tables.push((*b"post", post));
    tables.push((*b"name", name_table(family, &ps_name)));
    tables.push((*b"CFF ", cff));
    write_sfnt(tables)
}

/// The table records of an sfnt, in directory order.
fn sfnt_tables(data: &[u8]) -> Vec<([u8; 4], &[u8])> {
    let count = u16::from_be_bytes([data[4], data[5]]);
    (0..usize::from(count))
        .map(|i| {
            let rec = &data[12 + i * 16..12 + i * 16 + 16];
            let tag = [rec[0], rec[1], rec[2], rec[3]];
            let offset = u32::from_be_bytes([rec[8], rec[9], rec[10], rec[11]]) as usize;
            let len = u32::from_be_bytes([rec[12], rec[13], rec[14], rec[15]]) as usize;
            (tag, &data[offset..offset + len])
        })
        .collect()
}

/// Assemble an `OTTO` sfnt: sorted table directory, 4-byte-aligned tables,
/// per-table checksums and the `head.checkSumAdjustment`.
fn write_sfnt(mut tables: Vec<([u8; 4], Vec<u8>)>) -> Vec<u8> {
    tables.sort_by_key(|(tag, _)| *tag);
    let count = tables.len() as u16;
    let entry_selector = 15 - count.leading_zeros() as u16;
    let search_range = (1u16 << entry_selector) * 16;
    let mut out = b"OTTO".to_vec();
    for v in [
        count,
        search_range,
        entry_selector,
        count * 16 - search_range,
    ] {
        out.extend_from_slice(&v.to_be_bytes());
    }
    let mut offset = 12 + tables.len() * 16;
    let mut head_at = None;
    for (tag, data) in &mut tables {
        if tag == b"head" {
            data[8..12].fill(0);
            head_at = Some(offset);
        }
        out.extend_from_slice(tag);
        out.extend_from_slice(&checksum(data).to_be_bytes());
        out.extend_from_slice(&(offset as u32).to_be_bytes());
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        offset += data.len().div_ceil(4) * 4;
    }
    for (_, data) in &tables {
        out.extend_from_slice(data);
        out.resize(out.len().div_ceil(4) * 4, 0);
    }
    if let Some(at) = head_at {
        let adjust = 0xB1B0_AFBAu32.wrapping_sub(checksum(&out));
        out[at + 8..at + 12].copy_from_slice(&adjust.to_be_bytes());
    }
    out
}

fn checksum(data: &[u8]) -> u32 {
    data.chunks(4).fold(0u32, |sum, chunk| {
        let mut word = [0u8; 4];
        word[..chunk.len()].copy_from_slice(chunk);
        sum.wrapping_add(u32::from_be_bytes(word))
    })
}

/// A minimal Windows-Unicode `name` table (format 0).
fn name_table(family: &str, ps_name: &str) -> Vec<u8> {
    let records: [(u16, String); 6] = [
        (
            0,
            "Synthesized at test time from an SIL OFL 1.1 font; test use only.".into(),
        ),
        (1, family.into()),
        (2, "Regular".into()),
        (4, format!("{family} Regular")),
        (5, "Version 1.000".into()),
        (6, format!("{ps_name}-Regular")),
    ];
    let mut strings = Vec::new();
    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&(records.len() as u16).to_be_bytes());
    out.extend_from_slice(&((6 + 12 * records.len()) as u16).to_be_bytes());
    for (id, text) in &records {
        let utf16: Vec<u8> = text.encode_utf16().flat_map(u16::to_be_bytes).collect();
        for v in [
            3u16,
            1,
            0x0409,
            *id,
            utf16.len() as u16,
            strings.len() as u16,
        ] {
            out.extend_from_slice(&v.to_be_bytes());
        }
        strings.extend_from_slice(&utf16);
    }
    out.extend_from_slice(&strings);
    out
}

/// Type 2 charstring builder fed by `ttf_parser`'s outline callbacks.
struct Charstring {
    out: Vec<u8>,
    /// Advance width, emitted as the first stack-clearing operator's extra
    /// leading operand (`nominalWidthX` is 0) — `None` once written.
    width: Option<i32>,
    /// The pen in integer font units (what the deltas are relative to)…
    pen: (i32, i32),
    /// …and its exact position, for the quadratic → cubic conversion.
    exact: (f32, f32),
}

impl Charstring {
    fn new(advance: u16) -> Self {
        Self {
            out: Vec::new(),
            width: Some(i32::from(advance)),
            pen: (0, 0),
            exact: (0.0, 0.0),
        }
    }

    fn num(&mut self, v: i32) {
        type2_int(&mut self.out, v);
    }

    /// Emit the pending width ahead of the first operator.
    fn width(&mut self) {
        if let Some(w) = self.width.take() {
            self.num(w);
        }
    }

    /// Move the pen to (x, y), emitting the rounded delta.
    fn delta_to(&mut self, x: f32, y: f32) {
        let to = (x.round() as i32, y.round() as i32);
        self.num(to.0 - self.pen.0);
        self.num(to.1 - self.pen.1);
        self.pen = to;
        self.exact = (x, y);
    }

    fn finish(mut self) -> Vec<u8> {
        self.width();
        self.out.push(14); // endchar
        self.out
    }
}

impl OutlineBuilder for Charstring {
    fn move_to(&mut self, x: f32, y: f32) {
        self.width();
        self.delta_to(x, y);
        self.out.push(21); // rmoveto
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.delta_to(x, y);
        self.out.push(5); // rlineto
    }

    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        /* The exact cubic equivalent of a quadratic Bézier. */
        let (x0, y0) = self.exact;
        let c1 = (x0 + 2.0 / 3.0 * (x1 - x0), y0 + 2.0 / 3.0 * (y1 - y0));
        let c2 = (x + 2.0 / 3.0 * (x1 - x), y + 2.0 / 3.0 * (y1 - y));
        self.curve_to(c1.0, c1.1, c2.0, c2.1, x, y);
    }

    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.delta_to(x1, y1);
        self.delta_to(x2, y2);
        self.delta_to(x, y);
        self.out.push(8); // rrcurveto
    }

    /// CFF closes every subpath implicitly at the next `rmoveto`/`endchar`.
    fn close(&mut self) {}
}

/// A Type 2 charstring integer operand (CFF2/Type2 spec §3.2).
fn type2_int(out: &mut Vec<u8>, v: i32) {
    match v {
        -107..=107 => out.push((v + 139) as u8),
        108..=1131 => {
            let v = v - 108;
            out.extend_from_slice(&[(v / 256 + 247) as u8, (v % 256) as u8]);
        }
        -1131..=-108 => {
            let v = -v - 108;
            out.extend_from_slice(&[(v / 256 + 251) as u8, (v % 256) as u8]);
        }
        _ => {
            out.push(28);
            out.extend_from_slice(&(v as i16).to_be_bytes());
        }
    }
}

/// A DICT integer operand; `fixed` forces the 5-byte form, so an offset's
/// encoded size never depends on its value.
fn dict_int(out: &mut Vec<u8>, v: i32, fixed: bool) {
    if fixed || !(-1131..=1131).contains(&v) {
        out.push(29);
        out.extend_from_slice(&v.to_be_bytes());
    } else {
        type2_int(out, v);
    }
}

/// A DICT real operand (CFF spec §6, nibble-packed decimal).
fn dict_real(out: &mut Vec<u8>, v: f64) {
    let text = format!("{v:e}");
    let mut nibbles = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '0'..='9' => nibbles.push(c as u8 - b'0'),
            '.' => nibbles.push(0xa),
            '-' => nibbles.push(0xe),
            'e' if chars.peek() == Some(&'-') => {
                chars.next();
                nibbles.push(0xc);
            }
            'e' => nibbles.push(0xb),
            _ => unreachable!("{text}"),
        }
    }
    nibbles.push(0xf);
    if nibbles.len() % 2 == 1 {
        nibbles.push(0xf);
    }
    out.push(30);
    out.extend(nibbles.chunks(2).map(|p| (p[0] << 4) | p[1]));
}

/// A CFF INDEX (offSize 4 throughout — simple, and always enough).
fn index(items: &[Vec<u8>]) -> Vec<u8> {
    let mut out = (items.len() as u16).to_be_bytes().to_vec();
    if items.is_empty() {
        return out;
    }
    out.push(4);
    let mut offset = 1u32;
    out.extend_from_slice(&offset.to_be_bytes());
    for item in items {
        offset += item.len() as u32;
        out.extend_from_slice(&offset.to_be_bytes());
    }
    for item in items {
        out.extend_from_slice(item);
    }
    out
}

/// A name-keyed (SID) CFF font program: header, Name / Top DICT / String /
/// Global Subr INDEXes, a format-0 charset (custom glyph names `g1`, …),
/// the CharStrings INDEX and a Private DICT (`defaultWidthX` /
/// `nominalWidthX` 0 — every charstring carries its width explicitly).
///
/// The Top DICT carries a `Notice` and a `Copyright`, as every real font
/// does. That is load-bearing for veraPDF (1.30): the subset's Top DICT
/// then reads `ROS, Copyright, Notice, FontMatrix, …`, whereas without
/// them `FontMatrix` directly follows `ROS` and veraPDF scales every
/// glyph width by the `ROS` registry operand (≈ ×391 — it evidently does
/// not clear the operand stack after `ROS`), failing ISO 19005-1 §6.3.6 /
/// 19005-2 §6.2.11.5 on a correct file.
fn cff_table(ps_name: &str, upem: u16, bbox: [i16; 4], charstrings: &[Vec<u8>]) -> Vec<u8> {
    const STANDARD_STRINGS: u16 = 391;
    let n = charstrings.len();
    let header = [1u8, 0, 4, 4];
    let names = index(&[ps_name.as_bytes().to_vec()]);
    let mut strings: Vec<Vec<u8>> = (1..n).map(|g| format!("g{g}").into_bytes()).collect();
    let notice_sid = STANDARD_STRINGS + strings.len() as u16;
    strings.push(b"Synthesized at test time from an SIL OFL 1.1 font.".to_vec());
    let copyright_sid = notice_sid + 1;
    strings.push(b"SIL Open Font License 1.1; test use only.".to_vec());
    let string_index = index(&strings);
    let global_subrs = index(&[]);
    let mut charset = vec![0u8];
    for g in 1..n {
        charset.extend_from_slice(&(STANDARD_STRINGS + (g as u16 - 1)).to_be_bytes());
    }
    let char_strings = index(charstrings);
    let mut private = Vec::new();
    dict_int(&mut private, 0, false);
    private.push(20); // defaultWidthX
    dict_int(&mut private, 0, false);
    private.push(21); // nominalWidthX

    let top_dict = |charset_at: i32, charstrings_at: i32, private_at: i32| {
        let mut d = Vec::new();
        dict_int(&mut d, i32::from(notice_sid), false);
        d.push(1); // Notice
        dict_int(&mut d, i32::from(copyright_sid), false);
        d.extend_from_slice(&[12, 0]); // Copyright
        if upem != 1000 {
            let scale = 1.0 / f64::from(upem);
            for v in [scale, 0.0, 0.0, scale, 0.0, 0.0] {
                if v == 0.0 {
                    dict_int(&mut d, 0, false);
                } else {
                    dict_real(&mut d, v);
                }
            }
            d.extend_from_slice(&[12, 7]); // FontMatrix
        }
        for v in bbox {
            dict_int(&mut d, i32::from(v), false);
        }
        d.push(5); // FontBBox
        dict_int(&mut d, charset_at, true);
        d.push(15); // charset
        dict_int(&mut d, charstrings_at, true);
        d.push(17); // CharStrings
        dict_int(&mut d, private.len() as i32, true);
        dict_int(&mut d, private_at, true);
        d.push(18); // Private
        index(&[d])
    };
    /* Offsets are fixed-width, so the Top DICT's size is known up front. */
    let top_len = top_dict(0, 0, 0).len();
    let charset_at = header.len() + names.len() + top_len + string_index.len() + global_subrs.len();
    let charstrings_at = charset_at + charset.len();
    let private_at = charstrings_at + char_strings.len();

    let mut out = header.to_vec();
    out.extend_from_slice(&names);
    out.extend_from_slice(&top_dict(
        charset_at as i32,
        charstrings_at as i32,
        private_at as i32,
    ));
    out.extend_from_slice(&string_index);
    out.extend_from_slice(&global_subrs);
    out.extend_from_slice(&charset);
    out.extend_from_slice(&char_strings);
    out.extend_from_slice(&private);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use text_pipeline::LoadedFont;

    #[test]
    fn synthesized_cff_font_parses_with_the_same_glyphs() {
        let ttf = include_bytes!("../../../ts/fonts/LiberationSans-Regular.ttf");
        let otf = cff_from_truetype(ttf, "NGE Synth Sans");
        assert_eq!(&otf[..4], b"OTTO");
        let src = ttf_parser::Face::parse(ttf, 0).expect("ttf");
        let cff = ttf_parser::Face::parse(&otf, 0).expect("synthesized OTF parses");
        assert!(cff.tables().cff.is_some(), "CFF outlines");
        assert!(cff.tables().glyf.is_none(), "no TrueType outlines");
        assert_eq!(cff.number_of_glyphs(), src.number_of_glyphs());
        for ch in ['A', 'g', '@', 'é'] {
            let gid = src.glyph_index(ch).expect("source glyph");
            assert_eq!(cff.glyph_index(ch), Some(gid), "{ch:?}: cmap copied");
            assert_eq!(
                cff.glyph_hor_advance(gid),
                src.glyph_hor_advance(gid),
                "{ch:?}"
            );
            let a = src.glyph_bounding_box(gid).expect("ttf bbox");
            let b = cff
                .outline_glyph(gid, &mut NullOutline)
                .expect("cff outline");
            /* Rounded control points move a bbox by at most a unit or two. */
            for (p, q) in [
                (a.x_min, b.x_min),
                (a.y_min, b.y_min),
                (a.x_max, b.x_max),
                (a.y_max, b.y_max),
            ] {
                assert!((p - q).abs() <= 2, "{ch:?}: bbox {a:?} vs {b:?}");
            }
            /* The CFF charstring width agrees with hmtx. */
            assert_eq!(
                cff.tables().cff.expect("cff").glyph_width(gid),
                src.glyph_hor_advance(gid),
                "{ch:?}: charstring width"
            );
        }
        /* The engine's own loader accepts it (swash + rustybuzz). */
        let loaded = LoadedFont::parse("synth".into(), otf).expect("LoadedFont parses OTTO");
        assert!(loaded.covers('A'));
    }

    struct NullOutline;
    impl OutlineBuilder for NullOutline {
        fn move_to(&mut self, _: f32, _: f32) {}
        fn line_to(&mut self, _: f32, _: f32) {}
        fn quad_to(&mut self, _: f32, _: f32, _: f32, _: f32) {}
        fn curve_to(&mut self, _: f32, _: f32, _: f32, _: f32, _: f32, _: f32) {}
        fn close(&mut self) {}
    }

    #[test]
    fn dict_real_packs_nibbles() {
        let mut out = Vec::new();
        dict_real(&mut out, 1.0 / 2048.0);
        /* "4.8828125e-4" */
        assert_eq!(out, [30, 0x4a, 0x88, 0x28, 0x12, 0x5c, 0x4f]);
    }
}
