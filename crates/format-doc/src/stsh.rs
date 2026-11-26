//! The style sheet as stored ([MS-DOC] 2.9.271 STSH), at `fcStshf` in the
//! table stream: `LPStshi` (`cbStshi` + the `STSHI`, whose `Stshif` gives
//! the style count, the size of each style's fixed header and the default
//! fonts), then `rglpstd` — one `LPStd` (`cbStd` + an `STD`) per style
//! index (`istd`), an empty one (`cbStd` 0) where no style is defined.
//!
//! An `STD` is `Stdf` (`StdfBase`, 10 bytes, optionally followed by
//! `StdfPost2000`, 8 bytes — `Stshif.cbSTDBaseInFile` says which), the name
//! (`xstzName`: a 16-bit count, the UTF-16 name, a 16-bit terminator), and
//! `grLPUpxSw`: the style's own property modifiers by kind — a paragraph
//! style has an `LPUpxPapx` (`istd` + paragraph grpprl) then an
//! `LPUpxChpx` (character grpprl), a character style an `LPUpxChpx`; each
//! `LPUpx*` is a 16-bit size, the bytes, and one pad byte when the size is
//! odd. Table and numbering styles are read for their place in the index
//! only: the engine models neither.
//!
//! Written from the specification (not borrowed). Hostile input: every
//! read is bounded by the `STSH`'s own `lcb`; a record that runs past it
//! ends the walk (the styles read so far stay), and the result says the
//! sheet was damaged.

use crate::ffn::clean_name;
use crate::le::{slice, u16_at};

/// `istdBase` / `istdNext` "no style" (2.9.271 family: `StdfBase`).
pub(crate) const ISTD_NIL: u16 = 0x0FFF;
/// `Stshif.cstd` is below 0x0FFE.
const MAX_STYLES: usize = 0x0FFE;
/// `StdfBase` is 10 bytes; `StdfPost2000` adds 8.
const STDF_BASE: usize = 10;
/// A name longer than this is cut (Word's own limit is 253 characters).
const MAX_NAME_UNITS: usize = 255;

/// `StdfBase.stk`: what a style formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stk {
    Paragraph,
    Character,
    /// A table or numbering style, or an unknown kind: not modelled.
    Other,
}

/// One defined style, as stored.
#[derive(Debug, Clone)]
pub(crate) struct Std {
    /// `StdfBase.sti`: the built-in style this is (`STI_USER` for a
    /// user-defined one).
    pub(crate) sti: u16,
    pub(crate) stk: Stk,
    /// `StdfBase.istdBase`: the parent style ([`ISTD_NIL`]: none).
    pub(crate) istd_base: u16,
    /// `StdfBase.istdNext`: the style of the paragraph Enter creates after
    /// one in this style.
    pub(crate) istd_next: u16,
    /// The primary name: `xstzName` up to its first comma (the rest are
    /// aliases), cleaned for the model; `None` when empty.
    pub(crate) name: Option<String>,
    /// `UpxPapx.grpprlPapx` (paragraph styles).
    pub(crate) papx: Vec<u8>,
    /// `UpxChpx.grpprlChpx` (paragraph and character styles).
    pub(crate) chpx: Vec<u8>,
}

/// The parsed style sheet.
#[derive(Debug, Clone, Default)]
pub(crate) struct Stsh {
    /// `Stshif.ftcAsci`: the default ASCII font (an `ftc`).
    pub(crate) ftc_ascii: Option<u16>,
    /// `Stshif.ftcOther`: the default high-ANSI font.
    pub(crate) ftc_other: Option<u16>,
    /// `STSHI.ftcBi`: the default complex-script font.
    pub(crate) ftc_bi: Option<u16>,
    /// The styles by `istd`; `None` where none is defined.
    pub(crate) styles: Vec<Option<Std>>,
}

/// `LPUpx*` at `at` in `std`: its bytes and the offset just past it (and
/// its pad byte).
fn upx(std: &[u8], at: usize) -> Option<(&[u8], usize)> {
    let cb = usize::from(u16_at(std, at)?);
    let data = std.get(at + 2..at + 2 + cb)?;
    Some((data, at + 2 + cb + (cb & 1)))
}

/// Parse one `STD` (`cb_base` = `Stshif.cbSTDBaseInFile`). `None` when it
/// is too short to be a style.
fn parse_std(std: &[u8], cb_base: usize) -> Option<Std> {
    let w0 = u16_at(std, 0)?;
    let w1 = u16_at(std, 2)?;
    let w2 = u16_at(std, 4)?;
    /* StdfBase: sti (12 bits) + four flags; stk (4 bits) + istdBase
    (12 bits); cupx (4 bits) + istdNext (12 bits); bchUpe; grfstd. */
    let sti = w0 & 0x0FFF;
    let stk = match w1 & 0x000F {
        1 => Stk::Paragraph,
        2 => Stk::Character,
        _ => Stk::Other,
    };
    let istd_base = w1 >> 4;
    let cupx = usize::from(w2 & 0x000F);
    let istd_next = w2 >> 4;
    /* xstzName: cch, cch UTF-16 units, a 16-bit terminator. */
    let mut at = cb_base;
    let cch = usize::from(u16_at(std, at)?);
    let units: Vec<u16> = std
        .get(at + 2..at + 2 + 2 * cch)?
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .take(MAX_NAME_UNITS)
        .collect();
    at += 2 + 2 * cch + 2;
    let primary = units.split(|&u| u == u16::from(b',')).next().unwrap_or(&[]);
    let name = clean_name(primary);
    /* grLPUpxSw: the property modifiers this style's kind carries, as far
    as `cupx` says they are present. */
    let mut papx = Vec::new();
    let mut chpx = Vec::new();
    match stk {
        Stk::Paragraph if cupx >= 1 => {
            if let Some((data, next)) = upx(std, at) {
                /* UpxPapx: istd (2 bytes, this style's own) + grpprl. */
                papx = data.get(2..).unwrap_or(&[]).to_vec();
                if cupx >= 2
                    && let Some((data, _)) = upx(std, next)
                {
                    chpx = data.to_vec();
                }
            }
        }
        Stk::Character if cupx >= 1 => {
            if let Some((data, _)) = upx(std, at) {
                chpx = data.to_vec();
            }
        }
        _ => {}
    }
    Some(Std {
        sti,
        stk,
        istd_base,
        istd_next,
        name,
        papx,
        chpx,
    })
}

impl Stsh {
    /// Parse the `STSH` at `table[fc .. fc + lcb]`. `lcb` 0 is "no style
    /// sheet" (`None`). The flag is `true` when the sheet lies outside the
    /// stream or a record runs past its end: what was read stays usable.
    pub(crate) fn parse(table: &[u8], fc: u32, lcb: u32) -> (Option<Stsh>, bool) {
        if lcb == 0 {
            return (None, false);
        }
        let Some(stsh) = slice(table, fc, lcb) else {
            return (None, true);
        };
        let Some(cb_stshi) = u16_at(stsh, 0).map(usize::from) else {
            return (None, true);
        };
        let Some(stshi) = stsh.get(2..2 + cb_stshi) else {
            return (None, true);
        };
        /* Stshif (18 bytes): cstd, cbSTDBaseInFile, flags,
        stiMaxWhenSaved, istdMaxFixedWhenSaved, nVerBuiltInNamesWhenSaved,
        ftcAsci, ftcFE, ftcOther; then STSHI.ftcBi. */
        let (Some(cstd), Some(cb_base)) = (u16_at(stshi, 0), u16_at(stshi, 2)) else {
            return (None, true);
        };
        /* "MUST be 0x000A or 0x0012": a larger header (a later writer's)
        is skipped over, a smaller one cannot hold StdfBase. */
        let cb_base = usize::from(cb_base).max(STDF_BASE);
        let mut out = Stsh {
            ftc_ascii: u16_at(stshi, 12),
            ftc_other: u16_at(stshi, 16),
            ftc_bi: u16_at(stshi, 18),
            styles: Vec::new(),
        };
        let mut at = 2 + cb_stshi;
        let mut damaged = false;
        for _ in 0..usize::from(cstd).min(MAX_STYLES) {
            let Some(cb_std) = u16_at(stsh, at).map(usize::from) else {
                damaged = true;
                break;
            };
            let Some(std) = stsh.get(at + 2..at + 2 + cb_std) else {
                damaged = true;
                break;
            };
            at += 2 + cb_std;
            out.styles.push(if cb_std == 0 {
                None
            } else {
                parse_std(std, cb_base)
            });
        }
        (Some(out), damaged)
    }
}
