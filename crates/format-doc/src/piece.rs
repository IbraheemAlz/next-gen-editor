// Modified from WordCraft (https://github.com/storytold/wordcraft) at commit
// 3a4770ef8ce037d2650c93e2d5764536549357c7, file crates/docbin/src/piece.rs.
// Copyright (c) 2026 ArtCraft Team and the WordCraft contributors.
// Licensed under the MIT License or the Apache License, Version 2.0, at your
// option. Changes: pieces are exposed whole (the reader walks them in CP
// order instead of binary-searching every character); each piece carries
// its true byte offset; a CP table that is not ascending is a typed error
// instead of being clamped; the Prm0 table moved to `sprm.rs`. See
// THIRD-PARTY-NOTICES.md at the repository root.

//! The piece table ([MS-DOC] 2.9.38 Clx, 2.9.178 Pcdt, 2.8.35 PlcPcd,
//! 2.9.177 Pcd): which bytes of the `WordDocument` stream hold each range of
//! character positions, and in which encoding (2.4.1 Retrieving Text).
//!
//! A fast-saved document (`FibBase.fComplex`) is exactly a piece table whose
//! pieces are out of physical order and may carry property modifiers
//! (`Prm`, 2.9.215 Prm0 / 2.9.216 Prm1) that apply on top of the FKP
//! formatting of their text; a fully saved one has a single piece or a few
//! contiguous ones. The reader treats both the same way.

use crate::error::{DocError, malformed};
use crate::le::{slice, u16_at, u32_at};

const PART: &str = "Table: Clx";

/// Most pieces accepted (Word's own documents stay far below; the table
/// stream would have to be 12 MB to describe this many).
const MAX_PIECES: usize = 1_000_000;

/// One piece: CPs `[cp_start, cp_end)` stored from byte `fc` of the
/// `WordDocument` stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Piece {
    pub(crate) cp_start: u32,
    pub(crate) cp_end: u32,
    /// The byte offset of the piece's first character. For a compressed
    /// piece this is `FcCompressed.fc / 2` (2.9.73), for an uncompressed
    /// one `FcCompressed.fc` itself.
    pub(crate) fc: u32,
    /// `FcCompressed.fCompressed`: 8-bit Windows-1252 text (one byte per
    /// character) instead of UTF-16LE (two bytes per character).
    pub(crate) compressed: bool,
    /// The piece's `Prm` (2.9.177 Pcd.prm).
    pub(crate) prm: u16,
}

impl Piece {
    /// The byte offset of `cp` (inside this piece) in the `WordDocument`
    /// stream — the space the FKP bin tables index by.
    pub(crate) fn fc_of(&self, cp: u32) -> Option<u32> {
        let k = cp.checked_sub(self.cp_start)?;
        let step = if self.compressed { 1 } else { 2 };
        self.fc.checked_add(k.checked_mul(step)?)
    }
}

/// What a piece's `Prm` refers to.
#[derive(Debug, Clone, Copy)]
pub(crate) enum PrmRef<'a> {
    None,
    /// Prm0 (2.9.215): an index into the fixed `isprm` table with a
    /// one-byte operand.
    Short {
        isprm: u8,
        val: u8,
    },
    /// Prm1 (2.9.216): the grpprl of one of the Clx's `Prc`s.
    Grpprl(&'a [u8]),
}

/// The parsed `Clx`: the pieces in CP order plus the `Prc` grpprls Prm1
/// values reference.
#[derive(Debug)]
pub(crate) struct PieceTable {
    pieces: Vec<Piece>,
    prcs: Vec<Vec<u8>>,
}

impl PieceTable {
    /// Parse the `Clx` at `table[fc .. fc + lcb]`.
    pub(crate) fn parse(table: &[u8], fc: u32, lcb: u32) -> Result<PieceTable, DocError> {
        if lcb == 0 {
            return Err(malformed(PART, "the FIB names no piece table"));
        }
        let clx = slice(table, fc, lcb)
            .ok_or_else(|| malformed(PART, "the piece table lies outside the table stream"))?;
        /* 2.9.38: zero or more Prc (clxt 0x01, cbGrpprl i16, grpprl), then
        exactly one Pcdt (clxt 0x02, lcb u32, PlcPcd). */
        let mut prcs = Vec::new();
        let mut at = 0usize;
        let plc = loop {
            match clx.get(at).copied() {
                Some(0x01) => {
                    let cb = u16_at(clx, at + 1)
                        .ok_or_else(|| malformed(PART, "a Prc is cut short"))?
                        as i16;
                    let cb = usize::try_from(cb)
                        .map_err(|_| malformed(PART, "a Prc has a negative size"))?;
                    let start = at + 3;
                    let g = clx
                        .get(start..start + cb)
                        .ok_or_else(|| malformed(PART, "a Prc runs past the piece table"))?;
                    if prcs.len() >= MAX_PIECES {
                        return Err(DocError::TooLarge {
                            what: "the number of Prc property groups",
                            max: MAX_PIECES as u64,
                        });
                    }
                    prcs.push(g.to_vec());
                    at = start + cb;
                }
                Some(0x02) => {
                    let lcb_plc = u32_at(clx, at + 1)
                        .ok_or_else(|| malformed(PART, "the Pcdt is cut short"))?;
                    break slice(clx, (at + 5) as u32, lcb_plc)
                        .ok_or_else(|| malformed(PART, "the PlcPcd runs past the piece table"))?;
                }
                Some(other) => {
                    return Err(malformed(
                        PART,
                        format!("unexpected Clx entry type 0x{other:02X} at byte {at}"),
                    ));
                }
                None => return Err(malformed(PART, "the piece table has no Pcdt")),
            }
        };
        /* 2.8.35 PlcPcd: aCp[n + 1] (u32) then aPcd[n] (8 bytes each). */
        if plc.len() < 4 || (plc.len() - 4) % 12 != 0 {
            return Err(malformed(
                PART,
                format!("the PlcPcd is {} bytes, not 4 + 12n", plc.len()),
            ));
        }
        let n = (plc.len() - 4) / 12;
        if n > MAX_PIECES {
            return Err(DocError::TooLarge {
                what: "the number of pieces",
                max: MAX_PIECES as u64,
            });
        }
        let cp = |i: usize| u32_at(plc, 4 * i).unwrap_or(0);
        let mut pieces = Vec::with_capacity(n);
        for i in 0..n {
            let (cp_start, cp_end) = (cp(i), cp(i + 1));
            /* 2.8.35: "aCp … MUST be sorted in ascending order" — and no
            two pieces may share a CP. */
            if cp_end < cp_start || pieces.last().is_some_and(|p: &Piece| p.cp_end > cp_start) {
                return Err(malformed(PART, "the piece CPs are not in ascending order"));
            }
            /* Pcd (2.9.177): 2 bytes of flags, FcCompressed (4), Prm (2). */
            let pcd = (n + 1) * 4 + i * 8;
            let raw = u32_at(plc, pcd + 2).unwrap_or(0);
            let prm = u16_at(plc, pcd + 6).unwrap_or(0);
            let compressed = raw & 0x4000_0000 != 0;
            let fc30 = raw & 0x3FFF_FFFF;
            pieces.push(Piece {
                cp_start,
                cp_end,
                fc: if compressed { fc30 / 2 } else { fc30 },
                compressed,
                prm,
            });
        }
        Ok(PieceTable { pieces, prcs })
    }

    /// The pieces, in ascending CP order.
    pub(crate) fn pieces(&self) -> &[Piece] {
        &self.pieces
    }

    /// What `piece.prm` refers to. Bit 0 (`fComplex`) tells the two forms
    /// apart: Prm0 keeps a 7-bit `isprm` in bits 1-7 and its operand in
    /// bits 8-15; Prm1 keeps the index of a `Prc` in bits 1-15. An
    /// out-of-range Prc index refers to nothing.
    pub(crate) fn prm(&self, piece: &Piece) -> PrmRef<'_> {
        let prm = piece.prm;
        if prm & 1 == 0 {
            let isprm = ((prm >> 1) & 0x7F) as u8;
            if isprm == 0 {
                return PrmRef::None;
            }
            PrmRef::Short {
                isprm,
                val: (prm >> 8) as u8,
            }
        } else {
            match self.prcs.get(usize::from(prm >> 1)) {
                Some(g) => PrmRef::Grpprl(g),
                None => PrmRef::None,
            }
        }
    }
}

/// The character at index `k` of `piece` and how many CPs it covers (2 for
/// a UTF-16 surrogate pair whose halves are both in this piece), or `None`
/// when the piece's bytes end before it (a truncated stream).
pub(crate) fn char_in_piece(word: &[u8], piece: &Piece, k: u32) -> Option<(char, u32)> {
    let len = piece.cp_end - piece.cp_start;
    if piece.compressed {
        let at = piece.fc.checked_add(k)?;
        return word
            .get(at as usize)
            .map(|&b| (crate::cp1252::decode(b), 1));
    }
    let unit = |i: u32| -> Option<u16> {
        let at = piece.fc.checked_add(i.checked_mul(2)?)?;
        u16_at(word, at as usize)
    };
    let u = unit(k)?;
    if (0xD800..0xDC00).contains(&u)
        && k + 1 < len
        && let Some(lo) = unit(k + 1).filter(|lo| (0xDC00..0xE000).contains(lo))
        && let Some(Ok(c)) = char::decode_utf16([u, lo]).next()
    {
        return Some((c, 2));
    }
    /* A lone surrogate is not a character: U+FFFD, as the wasm string glue
    would make it anyway. */
    Some((char::from_u32(u32::from(u)).unwrap_or('\u{FFFD}'), 1))
}
