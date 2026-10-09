//! Issues #335 / #357 — how run-content elements that carry no text of
//! their own are encoded in [`crate::Paragraph::text`].
//!
//! The `.docx` reader maps each element onto one character (or, for the
//! elements with attributes, a U+FFFC anchor plus an
//! [`crate::InlineObject`]); layout gives it its meaning; the writer turns
//! it back into the element (never the raw character, which Word treats
//! differently).
//!
//! | element                  | text                     | layout                         |
//! |--------------------------|--------------------------|--------------------------------|
//! | `<w:softHyphen/>`        | U+00AD                   | invisible break opportunity; a |
//! |                          |                          | synthetic hyphen when broken   |
//! | `<w:noBreakHyphen/>`     | U+2011                   | a hyphen that never breaks     |
//! | `<w:cr/>`                | U+000D                   | a line break (like `<w:br/>`)  |
//! | `<w:sym/>`               | U+FFFC + `Symbol`        | the symbol's Unicode glyph     |
//! | `<w:ptab/>`              | U+FFFC + `PositionalTab` | an absolute-position tab       |
//! | `<w:dir>` … `</w:dir>`   | LRE / RLE … PDF          | a UAX #9 embedding             |
//! | `<w:bdo>` … `</w:bdo>`   | LRO / RLO … PDF          | a UAX #9 override              |

use serde::{Deserialize, Serialize};

/// U+00AD SOFT HYPHEN — `<w:softHyphen/>` (ECMA-376 §17.3.3.29): an
/// optional hyphenation point. Invisible while the line does not break
/// there; a line that breaks right after it ends with a drawn hyphen that
/// is not part of the text.
pub const SOFT_HYPHEN: char = '\u{00AD}';

/// U+2011 NON-BREAKING HYPHEN — `<w:noBreakHyphen/>` (ECMA-376
/// §17.3.3.18): renders as a hyphen and is never a break opportunity
/// (UAX #14 class GL).
pub const NON_BREAKING_HYPHEN: char = '\u{2011}';

/// `true` for [`SOFT_HYPHEN`] / [`NON_BREAKING_HYPHEN`] — the characters
/// the two hyphen elements are read into.
pub fn is_hyphen_character(ch: char) -> bool {
    ch == SOFT_HYPHEN || ch == NON_BREAKING_HYPHEN
}

/// U+000D CARRIAGE RETURN — `<w:cr/>` (ECMA-376 §17.3.3.4): a line break
/// inside the paragraph, laid out exactly like `<w:br/>` (U+2028) but kept
/// apart so a regenerated run writes the element it was read from.
pub const CARRIAGE_RETURN: char = '\u{000D}';

/// UAX #9 explicit formatting characters the `<w:dir>` / `<w:bdo>`
/// wrappers (ECMA-376 §17.3.2.8 / §17.3.2.3) become in the text: the
/// opening character at the wrapper's start, [`POP_DIRECTIONAL`] at its
/// end. Unicode BiDi resolution then applies the embedding / override
/// exactly; the writer turns each balanced pair back into its wrapper.
pub const LRE: char = '\u{202A}';
/// See [`LRE`] — `<w:dir w:val="rtl">`.
pub const RLE: char = '\u{202B}';
/// U+202C POP DIRECTIONAL FORMATTING — closes [`LRE`] / [`RLE`] /
/// [`LRO`] / [`RLO`].
pub const POP_DIRECTIONAL: char = '\u{202C}';
/// See [`LRE`] — `<w:bdo w:val="ltr">`.
pub const LRO: char = '\u{202D}';
/// See [`LRE`] — `<w:bdo w:val="rtl">`.
pub const RLO: char = '\u{202E}';

/// Issue #357 — which wrapper a bidi control character opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BidiWrapper {
    /// `<w:dir w:val="…">` — an embedding (LRE / RLE).
    Dir { rtl: bool },
    /// `<w:bdo w:val="…">` — an override (LRO / RLO).
    Bdo { rtl: bool },
}

impl BidiWrapper {
    /// The opening character of this wrapper.
    pub fn opener(self) -> char {
        match self {
            BidiWrapper::Dir { rtl: false } => LRE,
            BidiWrapper::Dir { rtl: true } => RLE,
            BidiWrapper::Bdo { rtl: false } => LRO,
            BidiWrapper::Bdo { rtl: true } => RLO,
        }
    }

    /// The wrapper an opening character stands for (`None` for anything
    /// else, [`POP_DIRECTIONAL`] included).
    pub fn of_opener(ch: char) -> Option<BidiWrapper> {
        match ch {
            LRE => Some(BidiWrapper::Dir { rtl: false }),
            RLE => Some(BidiWrapper::Dir { rtl: true }),
            LRO => Some(BidiWrapper::Bdo { rtl: false }),
            RLO => Some(BidiWrapper::Bdo { rtl: true }),
            _ => None,
        }
    }
}

/// `true` for the embedding / override controls the `<w:dir>` /
/// `<w:bdo>` wrappers are encoded with (openers and the pop).
pub fn is_bidi_wrapper_control(ch: char) -> bool {
    ch == POP_DIRECTIONAL || BidiWrapper::of_opener(ch).is_some()
}

/// Issue #357 — `w:alignment` of a `<w:ptab>` (ECMA-376 §17.18.71
/// `ST_PTabAlignment`): where the text after the tab is aligned.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PTabAlignment {
    /// The following text starts at the leading edge.
    #[default]
    Left,
    /// The following text is centred between the edges.
    Center,
    /// The following text ends at the trailing edge.
    Right,
}

/// Issue #357 — `w:relativeTo` of a `<w:ptab>` (§17.18.73
/// `ST_PTabRelativeTo`): which edges the alignment is measured between.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PTabRelativeTo {
    /// The page margins (the column box).
    #[default]
    Margin,
    /// The paragraph's indents.
    Indent,
}

/// Issue #357 — `w:leader` of a `<w:ptab>` (§17.18.72 `ST_PTabLeader`).
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PTabLeader {
    #[default]
    None,
    Dot,
    Hyphen,
    Underscore,
    MiddleDot,
}

impl PTabAlignment {
    pub fn parse(v: &str) -> Self {
        match v.trim() {
            "center" => PTabAlignment::Center,
            "right" => PTabAlignment::Right,
            _ => PTabAlignment::Left,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            PTabAlignment::Left => "left",
            PTabAlignment::Center => "center",
            PTabAlignment::Right => "right",
        }
    }
}

impl PTabRelativeTo {
    pub fn parse(v: &str) -> Self {
        match v.trim() {
            "indent" => PTabRelativeTo::Indent,
            _ => PTabRelativeTo::Margin,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            PTabRelativeTo::Margin => "margin",
            PTabRelativeTo::Indent => "indent",
        }
    }
}

impl PTabLeader {
    pub fn parse(v: &str) -> Self {
        match v.trim() {
            "dot" => PTabLeader::Dot,
            "hyphen" => PTabLeader::Hyphen,
            "underscore" => PTabLeader::Underscore,
            "middleDot" => PTabLeader::MiddleDot,
            _ => PTabLeader::None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            PTabLeader::None => "none",
            PTabLeader::Dot => "dot",
            PTabLeader::Hyphen => "hyphen",
            PTabLeader::Underscore => "underscore",
            PTabLeader::MiddleDot => "middleDot",
        }
    }
}

/// U+25A1 WHITE SQUARE — what a `<w:sym>` whose font and code have no
/// known Unicode equivalent draws: a visible stand-in instead of the
/// invisible missing glyph of a symbol face the editor does not ship.
pub const UNKNOWN_SYMBOL: char = '\u{25A1}';

/// Issue #357 — the character code of a `<w:sym w:char>` (hex; Word
/// writes the symbol-font private-use form `F0xx`, older producers the
/// bare byte `xx`). `None` when unparsable.
pub fn symbol_code(char_attr: &str) -> Option<u32> {
    u32::from_str_radix(char_attr.trim(), 16).ok()
}

/// Issue #357 — the Unicode character a `<w:sym w:font w:char>` draws:
/// the font's published Unicode equivalent of the code (Symbol: Adobe's
/// Symbol encoding; Wingdings: the common bullets / boxes / marks), the
/// code itself when it is an ordinary (non-private-use) character, else
/// [`UNKNOWN_SYMBOL`]. A symbol font's code page lives in U+F000..U+F0FF;
/// only the low byte selects the glyph.
pub fn symbol_char(font: &str, char_attr: &str) -> char {
    let Some(code) = symbol_code(char_attr) else {
        return UNKNOWN_SYMBOL;
    };
    let low = (code & 0xFF) as u8;
    let pua = (0xF000..=0xF0FF).contains(&code) || code <= 0xFF;
    let font = font.trim();
    let mapped = if pua && font.eq_ignore_ascii_case("Symbol") {
        symbol_font_char(low)
    } else if pua && font.eq_ignore_ascii_case("Wingdings") {
        wingdings_char(low)
    } else {
        None
    };
    mapped
        .or_else(|| {
            /* A real character (not the symbol-font private-use page):
            draw it as itself. */
            (!(0xE000..=0xF8FF).contains(&code))
                .then(|| char::from_u32(code))
                .flatten()
                .filter(|c| !c.is_control())
        })
        .unwrap_or(UNKNOWN_SYMBOL)
}

/// The Symbol font's code page (Adobe Symbol encoding) → Unicode.
fn symbol_font_char(b: u8) -> Option<char> {
    let c = match b {
        0x20 => ' ',
        0x21 => '!',
        0x22 => '\u{2200}',
        0x23 => '#',
        0x24 => '\u{2203}',
        0x25 => '%',
        0x26 => '&',
        0x27 => '\u{220B}',
        0x28 => '(',
        0x29 => ')',
        0x2A => '\u{2217}',
        0x2B => '+',
        0x2C => ',',
        0x2D => '\u{2212}',
        0x2E => '.',
        0x2F => '/',
        0x30..=0x39 => char::from(b),
        0x3A => ':',
        0x3B => ';',
        0x3C => '<',
        0x3D => '=',
        0x3E => '>',
        0x3F => '?',
        0x40 => '\u{2245}',
        0x41 => '\u{0391}',
        0x42 => '\u{0392}',
        0x43 => '\u{03A7}',
        0x44 => '\u{0394}',
        0x45 => '\u{0395}',
        0x46 => '\u{03A6}',
        0x47 => '\u{0393}',
        0x48 => '\u{0397}',
        0x49 => '\u{0399}',
        0x4A => '\u{03D1}',
        0x4B => '\u{039A}',
        0x4C => '\u{039B}',
        0x4D => '\u{039C}',
        0x4E => '\u{039D}',
        0x4F => '\u{039F}',
        0x50 => '\u{03A0}',
        0x51 => '\u{0398}',
        0x52 => '\u{03A1}',
        0x53 => '\u{03A3}',
        0x54 => '\u{03A4}',
        0x55 => '\u{03A5}',
        0x56 => '\u{03C2}',
        0x57 => '\u{03A9}',
        0x58 => '\u{039E}',
        0x59 => '\u{03A8}',
        0x5A => '\u{0396}',
        0x5B => '[',
        0x5C => '\u{2234}',
        0x5D => ']',
        0x5E => '\u{22A5}',
        0x5F => '_',
        0x60 => '\u{203E}',
        0x61 => '\u{03B1}',
        0x62 => '\u{03B2}',
        0x63 => '\u{03C7}',
        0x64 => '\u{03B4}',
        0x65 => '\u{03B5}',
        0x66 => '\u{03C6}',
        0x67 => '\u{03B3}',
        0x68 => '\u{03B7}',
        0x69 => '\u{03B9}',
        0x6A => '\u{03D5}',
        0x6B => '\u{03BA}',
        0x6C => '\u{03BB}',
        0x6D => '\u{03BC}',
        0x6E => '\u{03BD}',
        0x6F => '\u{03BF}',
        0x70 => '\u{03C0}',
        0x71 => '\u{03B8}',
        0x72 => '\u{03C1}',
        0x73 => '\u{03C3}',
        0x74 => '\u{03C4}',
        0x75 => '\u{03C5}',
        0x76 => '\u{03D6}',
        0x77 => '\u{03C9}',
        0x78 => '\u{03BE}',
        0x79 => '\u{03C8}',
        0x7A => '\u{03B6}',
        0x7B => '{',
        0x7C => '|',
        0x7D => '}',
        0x7E => '\u{223C}',
        0xA0 => '\u{20AC}',
        0xA1 => '\u{03D2}',
        0xA2 => '\u{2032}',
        0xA3 => '\u{2264}',
        0xA4 => '\u{2044}',
        0xA5 => '\u{221E}',
        0xA6 => '\u{0192}',
        0xA7 => '\u{2663}',
        0xA8 => '\u{2666}',
        0xA9 => '\u{2665}',
        0xAA => '\u{2660}',
        0xAB => '\u{2194}',
        0xAC => '\u{2190}',
        0xAD => '\u{2191}',
        0xAE => '\u{2192}',
        0xAF => '\u{2193}',
        0xB0 => '\u{00B0}',
        0xB1 => '\u{00B1}',
        0xB2 => '\u{2033}',
        0xB3 => '\u{2265}',
        0xB4 => '\u{00D7}',
        0xB5 => '\u{221D}',
        0xB6 => '\u{2202}',
        0xB7 => '\u{2022}',
        0xB8 => '\u{00F7}',
        0xB9 => '\u{2260}',
        0xBA => '\u{2261}',
        0xBB => '\u{2248}',
        0xBC => '\u{2026}',
        0xBD => '\u{23D0}',
        0xBE => '\u{23AF}',
        0xBF => '\u{21B5}',
        0xC0 => '\u{2135}',
        0xC1 => '\u{2111}',
        0xC2 => '\u{211C}',
        0xC3 => '\u{2118}',
        0xC4 => '\u{2297}',
        0xC5 => '\u{2295}',
        0xC6 => '\u{2205}',
        0xC7 => '\u{2229}',
        0xC8 => '\u{222A}',
        0xC9 => '\u{2283}',
        0xCA => '\u{2287}',
        0xCB => '\u{2284}',
        0xCC => '\u{2282}',
        0xCD => '\u{2286}',
        0xCE => '\u{2208}',
        0xCF => '\u{2209}',
        0xD0 => '\u{2220}',
        0xD1 => '\u{2207}',
        0xD2 | 0xE2 => '\u{00AE}',
        0xD3 | 0xE3 => '\u{00A9}',
        0xD4 | 0xE4 => '\u{2122}',
        0xD5 => '\u{220F}',
        0xD6 => '\u{221A}',
        0xD7 => '\u{22C5}',
        0xD8 => '\u{00AC}',
        0xD9 => '\u{2227}',
        0xDA => '\u{2228}',
        0xDB => '\u{21D4}',
        0xDC => '\u{21D0}',
        0xDD => '\u{21D1}',
        0xDE => '\u{21D2}',
        0xDF => '\u{21D3}',
        0xE0 => '\u{25CA}',
        0xE1 => '\u{2329}',
        0xE5 => '\u{2211}',
        0xE6 => '\u{239B}',
        0xE7 => '\u{239C}',
        0xE8 => '\u{239D}',
        0xE9 => '\u{23A1}',
        0xEA => '\u{23A2}',
        0xEB => '\u{23A3}',
        0xEC => '\u{23A7}',
        0xED => '\u{23A8}',
        0xEE => '\u{23A9}',
        0xEF => '\u{23AA}',
        0xF1 => '\u{232A}',
        0xF2 => '\u{222B}',
        0xF3 => '\u{2320}',
        0xF4 => '\u{23AE}',
        0xF5 => '\u{2321}',
        0xF6 => '\u{239E}',
        0xF7 => '\u{239F}',
        0xF8 => '\u{23A0}',
        0xF9 => '\u{23A4}',
        0xFA => '\u{23A5}',
        0xFB => '\u{23A6}',
        0xFC => '\u{23AB}',
        0xFD => '\u{23AC}',
        0xFE => '\u{23AD}',
        _ => return None,
    };
    Some(c)
}

/// The Wingdings code page → Unicode, for the glyphs Word documents use
/// as bullets, check boxes and marks (the rest has no single agreed
/// Unicode equivalent and draws [`UNKNOWN_SYMBOL`]).
fn wingdings_char(b: u8) -> Option<char> {
    let c = match b {
        0x21 => '\u{270F}',
        0x22 => '\u{2702}',
        0x28 => '\u{260E}',
        0x2A => '\u{2709}',
        0x45 => '\u{261C}',
        0x46 => '\u{261E}',
        0x4A => '\u{263A}',
        0x4C => '\u{2639}',
        0x54 => '\u{2744}',
        0x58 => '\u{2720}',
        0x5B => '\u{262F}',
        0x6C => '\u{25CF}',
        0x6E => '\u{25A0}',
        0x6F => '\u{25A1}',
        0x71 => '\u{2751}',
        0x75 => '\u{25C6}',
        0x76 => '\u{2756}',
        0xA7 => '\u{25AA}',
        0xA8 => '\u{25FB}',
        0xD8 => '\u{27A2}',
        0xE8 => '\u{2794}',
        0xFB => '\u{2717}',
        0xFC => '\u{2713}',
        0xFD => '\u{2612}',
        0xFE => '\u{2611}',
        _ => return None,
    };
    Some(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbol_codes_map_to_unicode() {
        assert_eq!(symbol_char("Symbol", "F061"), 'α');
        assert_eq!(symbol_char("Symbol", "61"), 'α');
        assert_eq!(symbol_char("symbol", "F0B7"), '•');
        assert_eq!(symbol_char("Symbol", "F0E5"), '∑');
        assert_eq!(symbol_char("Wingdings", "F0FC"), '✓');
        assert_eq!(symbol_char("Wingdings", "F0D8"), '➢');
        /* Unknown code / font: the visible stand-in. */
        assert_eq!(symbol_char("Wingdings", "F0F0"), UNKNOWN_SYMBOL);
        assert_eq!(symbol_char("Webdings", "F061"), UNKNOWN_SYMBOL);
        assert_eq!(symbol_char("Symbol", "zz"), UNKNOWN_SYMBOL);
        /* A real character draws as itself. */
        assert_eq!(symbol_char("Arial", "2192"), '→');
    }

    #[test]
    fn ptab_attributes_round_trip() {
        for a in [
            PTabAlignment::Left,
            PTabAlignment::Center,
            PTabAlignment::Right,
        ] {
            assert_eq!(PTabAlignment::parse(a.as_str()), a);
        }
        for r in [PTabRelativeTo::Margin, PTabRelativeTo::Indent] {
            assert_eq!(PTabRelativeTo::parse(r.as_str()), r);
        }
        for l in [
            PTabLeader::None,
            PTabLeader::Dot,
            PTabLeader::Hyphen,
            PTabLeader::Underscore,
            PTabLeader::MiddleDot,
        ] {
            assert_eq!(PTabLeader::parse(l.as_str()), l);
        }
    }

    #[test]
    fn bidi_wrapper_controls() {
        for w in [
            BidiWrapper::Dir { rtl: false },
            BidiWrapper::Dir { rtl: true },
            BidiWrapper::Bdo { rtl: false },
            BidiWrapper::Bdo { rtl: true },
        ] {
            assert_eq!(BidiWrapper::of_opener(w.opener()), Some(w));
            assert!(is_bidi_wrapper_control(w.opener()));
        }
        assert!(is_bidi_wrapper_control(POP_DIRECTIONAL));
        assert!(!is_bidi_wrapper_control('a'));
    }
}
