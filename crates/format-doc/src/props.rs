// Modified from WordCraft (https://github.com/storytold/wordcraft) at commit
// 3a4770ef8ce037d2650c93e2d5764536549357c7, files crates/docbin/src/fmt.rs
// (character / paragraph sprms) and crates/docbin/src/sections.rs (section
// sprms). Copyright (c) 2026 ArtCraft Team and the WordCraft contributors.
// Licensed under the MIT License or the Apache License, Version 2.0, at your
// option. Changes: re-expressed onto this engine's model (`SpanStyle`,
// `ParaProperties`, `PageGeometry` in twips / points); adds the right-to-left
// sprms (sprmCFBiDi, sprmPFBiDi) and the complex-script twins (sprmCFBoldBi,
// sprmCFItalicBi, sprmCHpsBi); physical (sprmPJc80) vs logical (sprmPJc)
// justification resolved against the paragraph direction; every measure is
// clamped to the engine's Word bounds (`engine::point_bounds`,
// `engine::MIN/MAX_FONT_SIZE_PT`); styles, borders, tabs and fonts are not
// applied here. See THIRD-PARTY-NOTICES.md at the repository root.

//! Single property modifiers onto the engine model ([MS-DOC] 2.6.1
//! Character Properties, 2.6.2 Paragraph Properties, 2.6.4 Section
//! Properties).
//!
//! Only DIRECT formatting is applied: styles (the `STSH`, 2.9.271) are not
//! imported yet, so a toggle operand's "the style's value" (2.9.327
//! ToggleOperand) reads as off.

use engine::{
    Alignment, GrabBag, LineHeight, MAX_POINT_FIELD_TWIPS, PageGeometry, ParaProperties,
    SectionType, SpanStyle, TextDirection, UnderlineStyle, fit_margins_to_text_area,
};

use crate::sprm::Prl;

// Character sprms (2.6.1).
const C_F_BOLD: u16 = 0x0835;
const C_F_ITALIC: u16 = 0x0836;
const C_F_STRIKE: u16 = 0x0837;
const C_F_D_STRIKE: u16 = 0x2A53;
const C_KUL: u16 = 0x2A3E;
const C_ICO: u16 = 0x2A42;
const C_CV: u16 = 0x6870;
const C_HPS: u16 = 0x4A43;
const C_F_BIDI: u16 = 0x085A;
const C_F_BOLD_BI: u16 = 0x085C;
const C_F_ITALIC_BI: u16 = 0x085D;
const C_HPS_BI: u16 = 0x4A61;

// Paragraph sprms (2.6.2).
const P_JC80: u16 = 0x2403;
const P_JC: u16 = 0x2461;
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

fn i16_of(p: &Prl) -> Option<i16> {
    crate::le::u16_at(p.operand, 0).map(|v| v as i16)
}

fn u16_of(p: &Prl) -> Option<u16> {
    crate::le::u16_at(p.operand, 0)
}

/// A ToggleOperand (2.9.327): 0x00 off, 0x01 on, 0x80 "the style's value"
/// (left as it is), 0x81 "the opposite of the style's value" — with no
/// styles imported, the style's value is off.
fn toggle(slot: &mut Option<bool>, p: &Prl) {
    match p.operand.first().copied() {
        Some(0x00) => *slot = Some(false),
        Some(0x80) | None => {}
        Some(_) => *slot = Some(true),
    }
}

/// Half-points → points, held to Word's character size bounds.
fn half_points(hps: u16) -> f32 {
    (f32::from(hps) / 2.0).clamp(engine::MIN_FONT_SIZE_PT, engine::MAX_FONT_SIZE_PT)
}

/// The direct character formatting of a run while it is being
/// accumulated: the engine style plus the right-to-left flag, which the
/// model keeps as an `<w:rtl/>` grab-bag fragment (`SpanStyle::
/// forces_complex_script`, written back by the `.docx` writer).
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct CharProps {
    pub(crate) style: SpanStyle,
    pub(crate) rtl: bool,
}

impl CharProps {
    /// Apply one character sprm (any other sprm is ignored).
    pub(crate) fn apply(&mut self, p: &Prl) {
        let s = &mut self.style;
        match p.op {
            C_F_BOLD => toggle(&mut s.bold, p),
            C_F_ITALIC => toggle(&mut s.italic, p),
            C_F_STRIKE | C_F_D_STRIKE => toggle(&mut s.strike, p),
            C_F_BOLD_BI => toggle(&mut s.bold_cs, p),
            C_F_ITALIC_BI => toggle(&mut s.italic_cs, p),
            C_F_BIDI => {
                let mut on = Some(self.rtl);
                toggle(&mut on, p);
                self.rtl = on.unwrap_or(false);
            }
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
            C_ICO => {
                s.color = match p.operand.first().copied().unwrap_or(0) {
                    0 => None,
                    i => ICO.get(usize::from(i)).map(|[r, g, b]| [*r, *g, *b, 255]),
                };
            }
            /* COLORREF (2.9.43): red, green, blue, fAuto (0xFF = automatic). */
            C_CV => {
                if let [r, g, b, auto, ..] = p.operand {
                    s.color = (*auto != 0xFF).then_some([*r, *g, *b, 255]);
                }
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
            _ => {}
        }
    }

    /// The engine span style of this run.
    pub(crate) fn into_span_style(self) -> SpanStyle {
        let mut style = self.style;
        if self.rtl {
            GrabBag::push_into(&mut style.grab_bag, b"<w:rtl/>".to_vec());
        }
        style
    }
}

/// The direct paragraph formatting while its sprms are applied.
#[derive(Debug, Clone, Default)]
pub(crate) struct ParaProps {
    pub(crate) props: ParaProperties,
    /// The last justification sprm: `(jc, physical)`. Resolved once every
    /// sprm is in, because a physical value depends on the direction a
    /// later sprm may set.
    jc: Option<(u8, bool)>,
    /// Membership the importer does not model, reported instead.
    pub(crate) in_table: bool,
    pub(crate) row_end: bool,
    pub(crate) in_list: bool,
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
            P_JC80 => self.jc = p.operand.first().map(|&v| (v, true)),
            P_JC => self.jc = p.operand.first().map(|&v| (v, false)),
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
            P_ILFO => self.in_list = i16_of(p).is_some_and(|v| v != 0 && v != -2047),
            _ => {}
        }
    }

    /// The final paragraph properties. Jc (2.9.130): 0 left, 1 center,
    /// 2 right, 3 both, 4 distributed, 5 / 7 / 8 the Kashida
    /// justifications, 9 Thai distributed. A logical value (sprmPJc) is
    /// relative to the reading order, like the engine's `Start` / `End`;
    /// a physical one (sprmPJc80) is turned around for a right-to-left
    /// paragraph.
    pub(crate) fn finish(mut self) -> ParaProps {
        if let Some((jc, physical)) = self.jc {
            let rtl = self.props.direction == Some(TextDirection::Rtl);
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
