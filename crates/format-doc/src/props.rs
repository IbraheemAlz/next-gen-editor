// Modified from WordCraft (https://github.com/storytold/wordcraft) at commit
// 3a4770ef8ce037d2650c93e2d5764536549357c7, files crates/docbin/src/fmt.rs
// (character / paragraph sprms) and crates/docbin/src/sections.rs (section
// sprms). Copyright (c) 2026 ArtCraft Team and the WordCraft contributors.
// Licensed under the MIT License or the Apache License, Version 2.0, at your
// option. Changes: re-expressed onto this engine's model (`SpanStyle`,
// `ParaProperties`, `PageGeometry` in twips / points); adds the right-to-left
// sprms (sprmCFBiDi, sprmPFBiDi) and the complex-script twins (sprmCFBoldBi,
// sprmCFItalicBi, sprmCHpsBi); physical (sprmPJc80) vs logical (sprmPJc)
// justification resolved against the paragraph's CASCADED direction;
// toggle operands kept as read and resolved against the style chain once
// the run's paragraph and character styles are known (issue #575); font
// slots kept as `ftc` indices into the font table; every measure is
// clamped to the engine's Word bounds (`engine::point_bounds`,
// `engine::MIN/MAX_FONT_SIZE_PT`); borders, shading, tabs, hidden and
// raised text are not applied yet (issue #626). See THIRD-PARTY-NOTICES.md
// at the repository root.

//! Single property modifiers onto the engine model ([MS-DOC] 2.6.1
//! Character Properties, 2.6.2 Paragraph Properties, 2.6.4 Section
//! Properties).
//!
//! The same appliers read DIRECT formatting (the FKPs' `Chpx` /
//! `PapxInFkp`, a piece's `Prm`) and a style's own formatting (its
//! `UpxChpx` / `UpxPapx`, [`crate::styles`]): a style contributes its own
//! sprms only and the engine's one cascade (`engine::StyleCascade`) folds
//! the chain, exactly like a `.docx` style's `<w:pPr>` / `<w:rPr>`.

use engine::{
    Alignment, GrabBag, LineHeight, MAX_POINT_FIELD_TWIPS, PageGeometry, ParaProperties,
    SectionType, SpanStyle, TextDirection, UnderlineStyle, VertAlign, fit_margins_to_text_area,
};

use crate::ffn::FontTable;
use crate::sprm::Prl;

// Character sprms (2.6.1).
const C_F_BOLD: u16 = 0x0835;
const C_F_ITALIC: u16 = 0x0836;
const C_F_STRIKE: u16 = 0x0837;
const C_F_SMALL_CAPS: u16 = 0x083A;
const C_F_CAPS: u16 = 0x083B;
const C_F_D_STRIKE: u16 = 0x2A53;
const C_KUL: u16 = 0x2A3E;
const C_ICO: u16 = 0x2A42;
const C_CV: u16 = 0x6870;
const C_HPS: u16 = 0x4A43;
const C_ISS: u16 = 0x2A48;
const C_HIGHLIGHT: u16 = 0x2A0C;
const C_ISTD: u16 = 0x4A30;
const C_RG_FTC0: u16 = 0x4A4F;
const C_RG_FTC2: u16 = 0x4A51;
const C_FTC_BI: u16 = 0x4A5E;
const C_F_BIDI: u16 = 0x085A;
const C_F_BOLD_BI: u16 = 0x085C;
const C_F_ITALIC_BI: u16 = 0x085D;
const C_HPS_BI: u16 = 0x4A61;

// Paragraph sprms (2.6.2).
const P_ISTD: u16 = 0x4600;
const P_JC80: u16 = 0x2403;
const P_JC: u16 = 0x2461;
const P_F_KEEP: u16 = 0x2405;
const P_F_KEEP_FOLLOW: u16 = 0x2406;
const P_F_PAGE_BREAK_BEFORE: u16 = 0x2407;
const P_F_WIDOW_CONTROL: u16 = 0x2431;
const P_OUT_LVL: u16 = 0x2640;
const P_DXA_LEFT80: u16 = 0x840F;
const P_DXA_LEFT: u16 = 0x845E;
const P_DXA_RIGHT80: u16 = 0x840E;
const P_DXA_RIGHT: u16 = 0x845D;
const P_DXA_LEFT1_80: u16 = 0x8411;
const P_DXA_LEFT1: u16 = 0x8460;
const P_DYA_BEFORE: u16 = 0xA413;
const P_DYA_AFTER: u16 = 0xA414;
const P_DYA_LINE: u16 = 0x6412;
const P_F_BIDI: u16 = 0x2441;
/// Table / list membership — read only to report what is not imported.
pub(crate) const P_F_IN_TABLE: u16 = 0x2416;
pub(crate) const P_F_TTP: u16 = 0x2417;
pub(crate) const P_F_INNER_TTP: u16 = 0x244C;
pub(crate) const P_ITAP: u16 = 0x6649;
pub(crate) const P_ILFO: u16 = 0x460B;

// Section sprms (2.6.4).
const S_BKC: u16 = 0x3009;
const S_DYA_HDR_TOP: u16 = 0xB017;
const S_DYA_HDR_BOTTOM: u16 = 0xB018;
const S_XA_PAGE: u16 = 0xB01F;
const S_YA_PAGE: u16 = 0xB020;
const S_DXA_LEFT: u16 = 0xB021;
const S_DXA_RIGHT: u16 = 0xB022;
const S_DYA_TOP: u16 = 0x9023;
const S_DYA_BOTTOM: u16 = 0x9024;

/// Word's 16-colour `Ico` palette (2.9.119 Ico); index 0 is "automatic".
const ICO: [[u8; 3]; 17] = [
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 255],
    [0, 255, 255],
    [0, 255, 0],
    [255, 0, 255],
    [255, 0, 0],
    [255, 255, 0],
    [255, 255, 255],
    [0, 0, 128],
    [0, 128, 128],
    [0, 128, 0],
    [128, 0, 128],
    [128, 0, 0],
    [128, 128, 0],
    [128, 128, 128],
    [192, 192, 192],
];

/// An `Ico` (2.9.119) as an opaque colour; `None` for 0 ("automatic", or
/// no highlight) and for an index past the palette.
fn ico(i: u8) -> Option<[u8; 4]> {
    match i {
        0 => None,
        i => ICO.get(usize::from(i)).map(|[r, g, b]| [*r, *g, *b, 255]),
    }
}

fn i16_of(p: &Prl) -> Option<i16> {
    crate::le::u16_at(p.operand, 0).map(|v| v as i16)
}

fn u16_of(p: &Prl) -> Option<u16> {
    crate::le::u16_at(p.operand, 0)
}

/// A `Bool8` operand: any nonzero byte is on.
fn bool8(p: &Prl) -> Option<bool> {
    p.operand.first().map(|&v| v != 0)
}

/// A ToggleOperand (2.9.327) as read: 0x00 off, 0x01 on, 0x80 "the
/// style's value", 0x81 "the opposite of the style's value". The two
/// style-relative values can only be resolved once the run's paragraph
/// and character styles are known ([`CharProps::resolve`]). Any other
/// byte is invalid; it reads as on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Toggle {
    Off,
    On,
    Style,
    NotStyle,
}

impl Toggle {
    fn of(p: &Prl) -> Option<Toggle> {
        Some(match *p.operand.first()? {
            0x00 => Toggle::Off,
            0x80 => Toggle::Style,
            0x81 => Toggle::NotStyle,
            _ => Toggle::On,
        })
    }

    /// The value at this level: `style` is what the styles under this
    /// level give the property (`None`: inherited from further down),
    /// `shown` what the text would show without this sprm.
    fn resolve(self, style: Option<bool>, shown: bool) -> Option<bool> {
        match self {
            Toggle::Off => Some(false),
            Toggle::On => Some(true),
            Toggle::Style => style,
            Toggle::NotStyle => Some(!shown),
        }
    }
}

/// The character properties ToggleOperands drive, by index.
const T_BOLD: usize = 0;
const T_ITALIC: usize = 1;
const T_STRIKE: usize = 2;
const T_BOLD_CS: usize = 3;
const T_ITALIC_CS: usize = 4;
const T_CAPS: usize = 5;
const T_SMALL_CAPS: usize = 6;
const TOGGLED: usize = 7;

/// The engine slot of toggled property `i`.
fn toggled(s: &mut SpanStyle, i: usize) -> &mut Option<bool> {
    match i {
        T_BOLD => &mut s.bold,
        T_ITALIC => &mut s.italic,
        T_STRIKE => &mut s.strike,
        T_BOLD_CS => &mut s.bold_cs,
        T_ITALIC_CS => &mut s.italic_cs,
        T_CAPS => &mut s.caps,
        _ => &mut s.small_caps,
    }
}

/// Half-points → points, held to Word's character size bounds.
fn half_points(hps: u16) -> f32 {
    (f32::from(hps) / 2.0).clamp(engine::MIN_FONT_SIZE_PT, engine::MAX_FONT_SIZE_PT)
}

/// The character formatting of one level (a run's direct `Chpx` and its
/// piece's `Prm`, or a style's `UpxChpx`) as read: plain values already
/// on `style`; the toggles, the fonts and the character style still as
/// their operands, which [`Self::resolve`] turns into the engine style.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct CharProps {
    style: SpanStyle,
    /// The last ToggleOperand of each toggled property ([`toggled`]).
    toggles: [Option<Toggle>; TOGGLED],
    /// `sprmCFBiDi`: kept as the `<w:rtl/>` grab-bag fragment the model
    /// knows (`SpanStyle::forces_complex_script`).
    rtl: Option<Toggle>,
    /// `sprmCIstd`: the character style (an `istd` into the STSH).
    pub(crate) istd: Option<u16>,
    /// `sprmCRgFtc0` (ASCII) / `sprmCRgFtc2` (other, high-ANSI) /
    /// `sprmCFtcBi` (complex script): `ftc` indices into the font table.
    /// (`sprmCRgFtc1`, the East Asian slot, has no model field.)
    ftc_ascii: Option<u16>,
    ftc_other: Option<u16>,
    ftc_bi: Option<u16>,
}

impl CharProps {
    /// Apply one character sprm (any other sprm is ignored).
    pub(crate) fn apply(&mut self, p: &Prl) {
        let toggle = Toggle::of(p);
        let s = &mut self.style;
        match p.op {
            C_F_BOLD => self.toggles[T_BOLD] = toggle,
            C_F_ITALIC => self.toggles[T_ITALIC] = toggle,
            C_F_STRIKE | C_F_D_STRIKE => self.toggles[T_STRIKE] = toggle,
            C_F_BOLD_BI => self.toggles[T_BOLD_CS] = toggle,
            C_F_ITALIC_BI => self.toggles[T_ITALIC_CS] = toggle,
            C_F_CAPS => self.toggles[T_CAPS] = toggle,
            C_F_SMALL_CAPS => self.toggles[T_SMALL_CAPS] = toggle,
            C_F_BIDI => self.rtl = toggle,
            /* Kul (2.9.135), mapped by meaning onto the variants the
            renderer draws — the same folding the .docx reader applies to
            `w:u/@w:val`. */
            C_KUL => {
                s.underline = Some(match p.operand.first().copied().unwrap_or(0) {
                    0x00 => UnderlineStyle::None,
                    0x03 | 0x2B => UnderlineStyle::Double,
                    0x04 | 0x14 => UnderlineStyle::Dotted,
                    0x07 | 0x09 | 0x0A | 0x17 | 0x19 | 0x1A | 0x27 | 0x37 => UnderlineStyle::Dashed,
                    0x0B | 0x1B => UnderlineStyle::Wavy,
                    _ => UnderlineStyle::Single,
                });
            }
            C_ICO => s.color = ico(p.operand.first().copied().unwrap_or(0)),
            /* COLORREF (2.9.43): red, green, blue, fAuto (0xFF = automatic). */
            C_CV => {
                if let [r, g, b, auto, ..] = p.operand {
                    s.color = (*auto != 0xFF).then_some([*r, *g, *b, 255]);
                }
            }
            /* sprmCHighlight: an Ico, 0 = no highlight (the `.docx`
            reader's `<w:highlight>` lands on `bg_color` too). */
            C_HIGHLIGHT => s.bg_color = ico(p.operand.first().copied().unwrap_or(0)),
            /* sprmCIss: 0 normal, 1 superscript, 2 subscript. */
            C_ISS => {
                s.vert_align = Some(match p.operand.first().copied() {
                    Some(1) => VertAlign::Superscript,
                    Some(2) => VertAlign::Subscript,
                    _ => VertAlign::Baseline,
                });
            }
            C_HPS => {
                if let Some(h) = u16_of(p) {
                    s.font_size = Some(half_points(h));
                }
            }
            C_HPS_BI => {
                if let Some(h) = u16_of(p) {
                    s.font_size_cs = Some(half_points(h));
                }
            }
            C_ISTD => self.istd = u16_of(p),
            C_RG_FTC0 => self.ftc_ascii = u16_of(p),
            C_RG_FTC2 => self.ftc_other = u16_of(p),
            C_FTC_BI => self.ftc_bi = u16_of(p),
            _ => {}
        }
    }

    /// The engine span style of this level on top of `base`, the
    /// character-style chain the level starts from (what 0x80, "the
    /// style's value", means; a run's carries its id in `char_style`).
    /// `shown` is everything the text shows without this level (document
    /// defaults → paragraph style chain → `base`): what 0x81 turns
    /// around. A run's span is therefore `.docx`'s: the character style
    /// chain plus the direct formatting, the paragraph style folded at
    /// layout (`build_style_spans`).
    pub(crate) fn resolve(
        &self,
        base: &SpanStyle,
        shown: &SpanStyle,
        fonts: &FontTable,
    ) -> SpanStyle {
        let mut own = self.style.clone();
        let (mut base_view, mut shown_view) = (base.clone(), shown.clone());
        for (i, t) in self.toggles.iter().enumerate() {
            if let Some(t) = t {
                let style = *toggled(&mut base_view, i);
                let on = toggled(&mut shown_view, i).unwrap_or(false);
                *toggled(&mut own, i) = t.resolve(style, on);
            }
        }
        /* The Latin slot: the ASCII font, else the high-ANSI one (the
        `.docx` reader's `w:ascii`, else `w:hAnsi`). */
        if let Some(fam) = self
            .ftc_ascii
            .or(self.ftc_other)
            .and_then(|f| fonts.family(f))
        {
            own.font_family = Some(fam);
        }
        if let Some(fam) = self.ftc_bi.and_then(|f| fonts.family(f)) {
            own.font_family_cs = Some(fam);
        }
        /* The flag rides a grab-bag fragment, and bags never cascade: a
        style's value is "off" here. */
        if self.rtl.and_then(|t| t.resolve(None, false)) == Some(true) {
            GrabBag::push_into(&mut own.grab_bag, b"<w:rtl/>".to_vec());
        }
        base.clone().merged_with(own)
    }
}

/// The paragraph formatting of one level (a paragraph's `PapxInFkp` and
/// its piece's `Prm`, or a style's `UpxPapx`) while its sprms are applied.
#[derive(Debug, Clone, Default)]
pub(crate) struct ParaProps {
    pub(crate) props: ParaProperties,
    /// The paragraph style: `PapxInFkp`'s `istd` (2.9.112 GrpPrlAndIstd),
    /// or a later `sprmPIstd`.
    pub(crate) istd: Option<u16>,
    /// The last justification sprm: `(jc, physical)`. Resolved once the
    /// paragraph's direction is known ([`Self::finish`]): a physical
    /// value depends on it, and it may come from a later sprm or from a
    /// style.
    jc: Option<(u8, bool)>,
    /// Membership the importer does not model, reported instead.
    pub(crate) in_table: bool,
    pub(crate) row_end: bool,
    /// `sprmPIlfo`: `Some(true)` a list paragraph, `Some(false)` an
    /// explicit "no list" (it cancels a style's list), `None` unset.
    pub(crate) list: Option<bool>,
}

impl ParaProps {
    /// Apply one paragraph sprm (any other sprm is ignored).
    pub(crate) fn apply(&mut self, p: &Prl) {
        let props = &mut self.props;
        let signed = |p: &Prl| {
            i16_of(p).map(|v| i32::from(v).clamp(-MAX_POINT_FIELD_TWIPS, MAX_POINT_FIELD_TWIPS))
        };
        let unsigned = |p: &Prl| u16_of(p).map(|v| i32::from(v).min(MAX_POINT_FIELD_TWIPS));
        match p.op {
            P_ISTD => self.istd = u16_of(p),
            P_JC80 => self.jc = p.operand.first().map(|&v| (v, true)),
            P_JC => self.jc = p.operand.first().map(|&v| (v, false)),
            P_F_KEEP => props.keep_lines = bool8(p),
            P_F_KEEP_FOLLOW => props.keep_next = bool8(p),
            P_F_PAGE_BREAK_BEFORE => props.page_break_before = bool8(p).unwrap_or(false),
            P_F_WIDOW_CONTROL => props.widow_control = bool8(p),
            /* sprmPOutLvl: 0-8 an outline level, 9 body text — the values
            OOXML's `w:outlineLvl` takes. */
            P_OUT_LVL => props.outline_level = p.operand.first().map(|&v| v.min(9)),
            /* The logical (leading / trailing) indents; the 80 forms are
            Word 97's spelling of the same values. */
            P_DXA_LEFT | P_DXA_LEFT80 => {
                if let Some(v) = signed(p) {
                    props.indent.start_twips = v;
                }
            }
            P_DXA_RIGHT | P_DXA_RIGHT80 => {
                if let Some(v) = signed(p) {
                    props.indent.end_twips = v;
                }
            }
            /* dxaLeft1: the first line's offset from the left indent; a
            negative value is a hanging indent. */
            P_DXA_LEFT1 | P_DXA_LEFT1_80 => {
                if let Some(v) = signed(p) {
                    props.indent.first_line_twips = v.max(0);
                    props.indent.hanging_twips = (-v).max(0);
                }
            }
            P_DYA_BEFORE => {
                if let Some(v) = unsigned(p) {
                    props.spacing.before_twips = v;
                }
            }
            P_DYA_AFTER => {
                if let Some(v) = unsigned(p) {
                    props.spacing.after_twips = v;
                }
            }
            /* LSPD (2.9.150): dyaLine (i16) then fMultLinespace (i16). A
            multiple is in 240ths of a line, exactly OOXML's `auto` rule;
            otherwise twips, "at least" when positive, "exactly" (the
            magnitude) when negative. */
            P_DYA_LINE => {
                if let Some(v) = i16_of(p) {
                    let mult = crate::le::u16_at(p.operand, 2) == Some(1);
                    let v = i32::from(v);
                    props.line_height = Some(if mult {
                        LineHeight::Auto {
                            twips: v.abs().clamp(15, MAX_POINT_FIELD_TWIPS),
                        }
                    } else if v < 0 {
                        LineHeight::Exact {
                            twips: v.abs().min(MAX_POINT_FIELD_TWIPS),
                        }
                    } else {
                        LineHeight::AtLeast {
                            twips: v.min(MAX_POINT_FIELD_TWIPS),
                        }
                    });
                }
            }
            P_F_BIDI => {
                props.direction = Some(match p.operand.first().copied() {
                    Some(0) | None => TextDirection::Ltr,
                    Some(_) => TextDirection::Rtl,
                });
            }
            P_F_IN_TABLE => self.in_table = p.operand.first().is_some_and(|&v| v != 0),
            P_ITAP => self.in_table |= crate::le::u32_at(p.operand, 0).is_some_and(|v| v != 0),
            P_F_TTP | P_F_INNER_TTP => self.row_end = p.operand.first().is_some_and(|&v| v != 0),
            P_ILFO => self.list = i16_of(p).map(|v| v != 0 && v != -2047),
            _ => {}
        }
    }

    /// The final properties of this level. `inherited_rtl` is the
    /// direction the styles under it give the paragraph when the level
    /// sets none itself. Jc (2.9.130): 0 left, 1 center, 2 right, 3 both,
    /// 4 distributed, 5 / 7 / 8 the Kashida justifications, 9 Thai
    /// distributed. A logical value (sprmPJc) is relative to the reading
    /// order, like the engine's `Start` / `End`; a physical one
    /// (sprmPJc80) is turned around for a right-to-left paragraph.
    pub(crate) fn finish(mut self, inherited_rtl: bool) -> ParaProps {
        if let Some((jc, physical)) = self.jc {
            let rtl = self
                .props
                .direction
                .map_or(inherited_rtl, |d| d == TextDirection::Rtl);
            let flip = physical && rtl;
            self.props.alignment = Some(match jc {
                1 => Alignment::Center,
                0 if flip => Alignment::End,
                2 if flip => Alignment::Start,
                2 => Alignment::End,
                3..=9 => Alignment::Justify,
                _ => Alignment::Start,
            });
        }
        self
    }
}

/// The section properties of one `Sepx` (2.9.253) over the default SEP.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct SectionGeometry {
    pub(crate) geometry: PageGeometry,
    pub(crate) section_type: SectionType,
}

impl Default for SectionGeometry {
    /// The SEP defaults a `Sepx` modifies (2.6.4: a section property a
    /// Sepx does not set keeps its default): US Letter, 1.25 in left and
    /// right margins, 1 in top and bottom, header / footer 0.5 in, a new
    /// page.
    fn default() -> Self {
        SectionGeometry {
            geometry: PageGeometry::from_twips(12240, 15840, 1440, 1800, 1440, 1800, 720, 720),
            section_type: SectionType::NextPage,
        }
    }
}

impl SectionGeometry {
    /// Apply one section sprm (any other sprm is ignored). Measures are
    /// twips; held to Word's page bounds (`engine::point_bounds`).
    pub(crate) fn apply(&mut self, p: &Prl) {
        let g = &mut self.geometry;
        let side = |p: &Prl| {
            u16_of(p).map(|v| f32::from(v.clamp(144, MAX_POINT_FIELD_TWIPS as u16)) / 20.0)
        };
        /* Margins are held to 0..=22 in: a negative top / bottom margin
        (legal in the format: the body does not move for the header) is
        read by its magnitude, as Word lays it out. */
        let margin = |p: &Prl| {
            i16_of(p).map(|v| (i32::from(v).abs().min(MAX_POINT_FIELD_TWIPS)) as f32 / 20.0)
        };
        match p.op {
            S_XA_PAGE => g.width = side(p).unwrap_or(g.width),
            S_YA_PAGE => g.height = side(p).unwrap_or(g.height),
            S_DXA_LEFT => g.margin_left = margin(p).unwrap_or(g.margin_left),
            S_DXA_RIGHT => g.margin_right = margin(p).unwrap_or(g.margin_right),
            S_DYA_TOP => g.margin_top = margin(p).unwrap_or(g.margin_top),
            S_DYA_BOTTOM => g.margin_bottom = margin(p).unwrap_or(g.margin_bottom),
            S_DYA_HDR_TOP => g.header_offset = margin(p).unwrap_or(g.header_offset),
            S_DYA_HDR_BOTTOM => g.footer_offset = margin(p).unwrap_or(g.footer_offset),
            /* SBkcOperand (2.6.4 sprmSBkc): 0 continuous, 1 new column,
            2 new page, 3 even page, 4 odd page. A column break has no
            model equivalent: it starts a new page. */
            S_BKC => {
                self.section_type = match p.operand.first().copied() {
                    Some(0) => SectionType::Continuous,
                    Some(3) => SectionType::EvenPage,
                    Some(4) => SectionType::OddPage,
                    _ => SectionType::NextPage,
                };
            }
            _ => {}
        }
    }

    /// Margins that leave no text area are scaled back, the same repair
    /// the `.docx` reader applies (`fit_margins_to_text_area`).
    pub(crate) fn finish(mut self) -> SectionGeometry {
        fit_margins_to_text_area(&mut self.geometry);
        self
    }
}
