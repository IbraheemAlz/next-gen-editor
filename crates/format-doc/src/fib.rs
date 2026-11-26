// Modified from WordCraft (https://github.com/storytold/wordcraft) at commit
// 3a4770ef8ce037d2650c93e2d5764536549357c7, file crates/docbin/src/fib.rs.
// Copyright (c) 2026 ArtCraft Team and the WordCraft contributors.
// Licensed under the MIT License or the Apache License, Version 2.0, at your
// option. Changes: re-expressed against this crate's error type; the
// encryption check runs on the cleartext FibBase before anything else is
// read; only the FibRgFcLcb97 pairs this importer reads are named; a short
// RgFcLcb blob is read as far as the stream holds it. See
// THIRD-PARTY-NOTICES.md at the repository root.

//! The File Information Block at the start of the `WordDocument` stream
//! ([MS-DOC] 2.5.1 Fib): `FibBase` (2.5.2), `FibRgW97` (2.5.3),
//! `FibRgLw97` (2.5.4) and the `FibRgFcLcb` blob (2.5.5 / 2.5.6).

use crate::error::{DocError, malformed};
use crate::le::{u16_at, u32_at};

const PART: &str = "WordDocument: FIB";

/// `FibBase.wIdent` of a Word 97-2003 document (2.5.2).
const W_IDENT_97: u16 = 0xA5EC;
/// `FibBase.wIdent` of a Word 6.0 / 95 document.
const W_IDENT_95: u16 = 0xA5DC;
/// The oldest `nFib` of the Word 97 format (2.5.2: "MUST be 0x00C1"; the
/// Word 97 betas wrote 0x00C0).
const N_FIB_97: u16 = 0x00C0;

/// Byte offsets inside the FIB (2.5.1: `FibBase` is 32 bytes, then `csw`,
/// `FibRgW97` (14 × 2 bytes), `cslw`, `FibRgLw97` (22 × 4 bytes),
/// `cbRgFcLcb` and the blob).
const OFF_N_FIB: usize = 0x02;
const OFF_FLAGS: usize = 0x0A;
const OFF_CSW: usize = 0x20;
const OFF_CSLW: usize = 0x3E;
const OFF_RG_LW: usize = 0x40;
const OFF_CB_RG_FC_LCB: usize = 0x98;
const OFF_RG_FC_LCB: usize = 0x9A;

/// `FibBase` flag bits (2.5.2).
const F_ENCRYPTED: u16 = 0x0100;
const F_WHICH_TBL_STM: u16 = 0x0200;

/// Indices of the `(fc, lcb)` pairs of `FibRgFcLcb97` (2.5.6) this
/// importer reads.
pub(crate) mod pair {
    /// `fcStshf` / `lcbStshf` — the style sheet (only its presence is
    /// read: styles are not imported yet).
    pub(crate) const STSHF: usize = 1;
    /// `fcPlcfSed` — the section descriptors.
    pub(crate) const PLCF_SED: usize = 6;
    /// `fcPlcfBteChpx` — the character-formatting bin table.
    pub(crate) const PLCF_BTE_CHPX: usize = 12;
    /// `fcPlcfBtePapx` — the paragraph-formatting bin table.
    pub(crate) const PLCF_BTE_PAPX: usize = 13;
    /// `fcClx` — the piece table.
    pub(crate) const CLX: usize = 33;
}

/// Character counts of the subdocuments (`FibRgLw97`, 2.5.4). The main
/// document is CPs `[0, text)`; the others follow it in the CP space.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Ccp {
    pub(crate) text: u32,
    pub(crate) ftn: u32,
    pub(crate) hdd: u32,
    pub(crate) atn: u32,
    pub(crate) edn: u32,
    pub(crate) txbx: u32,
    pub(crate) hdr_txbx: u32,
}

/// The parsed FIB.
#[derive(Debug, Clone)]
pub(crate) struct Fib {
    /// `fWhichTblStm`: the table stream is `1Table` (true) or `0Table`.
    /// (`fComplex`, "the last save was a fast save", is not read: a
    /// fast-saved document is just a piece table with more pieces, which
    /// the reader handles the same way.)
    pub(crate) which_tbl_stm: bool,
    pub(crate) ccp: Ccp,
    /// The `FibRgFcLcb` blob as `fc0, lcb0, fc1, lcb1, …`.
    pairs: Vec<u32>,
}

impl Fib {
    /// The `(fc, lcb)` pair at `FibRgFcLcb97` index `i`, `None` when the
    /// blob is shorter.
    pub(crate) fn pair(&self, i: usize) -> Option<(u32, u32)> {
        Some((*self.pairs.get(2 * i)?, *self.pairs.get(2 * i + 1)?))
    }

    /// Parse the FIB at the head of the `WordDocument` stream.
    ///
    /// The order matters: an encrypted document stores only its first 68
    /// bytes in the clear ([MS-DOC] 2.2.6), so `fEncrypted` is checked
    /// right after `FibBase` (32 bytes), before any later field is
    /// trusted.
    pub(crate) fn parse(word: &[u8]) -> Result<Fib, DocError> {
        let (Some(w_ident), Some(n_fib), Some(flags)) = (
            u16_at(word, 0),
            u16_at(word, OFF_N_FIB),
            u16_at(word, OFF_FLAGS),
        ) else {
            return Err(malformed(PART, "the stream is shorter than FibBase"));
        };
        if w_ident == W_IDENT_95 || (w_ident == W_IDENT_97 && n_fib < N_FIB_97) {
            return Err(DocError::UnsupportedVersion { w_ident, n_fib });
        }
        if w_ident != W_IDENT_97 {
            return Err(malformed(
                PART,
                format!("wIdent is 0x{w_ident:04X}, not a Word document"),
            ));
        }
        if flags & F_ENCRYPTED != 0 {
            return Err(DocError::Encrypted);
        }
        if word.len() < OFF_RG_FC_LCB {
            return Err(malformed(
                PART,
                format!("the stream is {} bytes, shorter than the FIB", word.len()),
            ));
        }
        /* `csw` and `cslw` are fixed by 2.5.1 ("MUST be 0x000E" /
        "MUST be 0x0016"); anything else means the offsets below are not
        where the spec puts them. */
        let csw = u16_at(word, OFF_CSW).unwrap_or(0);
        let cslw = u16_at(word, OFF_CSLW).unwrap_or(0);
        if csw != 0x000E || cslw != 0x0016 {
            return Err(malformed(
                PART,
                format!("csw 0x{csw:04X} / cslw 0x{cslw:04X} are not the Word 97 values"),
            ));
        }
        /* FibRgLw97 (2.5.4): cbMac, reserved1, reserved2, ccpText, ccpFtn,
        ccpHdd, reserved3, ccpAtn, ccpEdn, ccpTxbx, ccpHdrTxbx, … The ccp
        fields are signed and "MUST be 0 or greater": a negative one is
        read as 0. */
        let ccp_at = |i: usize| -> u32 {
            let v = u32_at(word, OFF_RG_LW + 4 * i).unwrap_or(0) as i32;
            v.max(0) as u32
        };
        let ccp = Ccp {
            text: ccp_at(3),
            ftn: ccp_at(4),
            hdd: ccp_at(5),
            atn: ccp_at(7),
            edn: ccp_at(8),
            txbx: ccp_at(9),
            hdr_txbx: ccp_at(10),
        };
        /* The blob: `cbRgFcLcb` 8-byte pairs. A writer newer than Word 97
        writes more pairs (fine, the first 93 keep their meaning); a blob
        cut short by the stream end is read as far as it goes, and a
        missing pair simply reads as absent. */
        let cb = usize::from(u16_at(word, OFF_CB_RG_FC_LCB).unwrap_or(0));
        let available = (word.len() - OFF_RG_FC_LCB) / 8;
        let n = cb.min(available);
        let mut pairs = Vec::with_capacity(2 * n);
        for i in 0..2 * n {
            pairs.push(u32_at(word, OFF_RG_FC_LCB + 4 * i).unwrap_or(0));
        }
        Ok(Fib {
            which_tbl_stm: flags & F_WHICH_TBL_STM != 0,
            ccp,
            pairs,
        })
    }
}
