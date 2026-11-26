// Modified from WordCraft (https://github.com/storytold/wordcraft) at commit
// 3a4770ef8ce037d2650c93e2d5764536549357c7, file crates/docbin/src/fkp.rs.
// Copyright (c) 2026 ArtCraft Team and the WordCraft contributors.
// Licensed under the MIT License or the Apache License, Version 2.0, at your
// option. Changes: a lookup also returns the end of the FC run it found (the
// reader caches one run's formatting instead of re-searching per
// character) and a PAPX entry's paragraph style (`istd`, issue #575); a
// bin table that is not 4 + 8n bytes is a typed error; the `crun` caps are
// the spec's (0x65 / 0x1D). See THIRD-PARTY-NOTICES.md at the repository
// root.

//! Direct formatting lookup ([MS-DOC] 2.4.6.1 Direct Character Formatting,
//! 2.4.6.2 Direct Paragraph Formatting): a bin table in the table stream
//! (2.8.12 PlcBteChpx, 2.8.13 PlcBtePapx) maps byte ranges of the
//! `WordDocument` stream to 512-byte FKP pages in that same stream; a page
//! (2.9.33 ChpxFkp, 2.9.175 PapxFkp) maps finer byte ranges ("runs") to a
//! `Chpx` (2.9.32) or a `PapxInFkp` (2.9.179) — the grpprls of property
//! modifiers that apply to the text stored in that range.

use crate::error::{DocError, malformed};
use crate::le::{slice, u32_at};

/// Size of an FKP page (2.9.33 / 2.9.175).
const PAGE: usize = 512;
/// `ChpxFkp.crun` is at most 0x65 (2.9.33).
const MAX_CHPX_RUNS: usize = 0x65;
/// `PapxFkp.cpara` is at most 0x1D (2.9.175).
const MAX_PAPX_RUNS: usize = 0x1D;
/// `BxPap` is 13 bytes: `bOffset` + a 12-byte `PHE` (2.9.23).
const BX_PAP: usize = 13;
/// Most bin-table entries accepted (each names a 512-byte page, so a real
/// document needs one per 512 bytes of its WordDocument stream at most).
const MAX_BINS: usize = 1 << 20;

/// Which bin table: the page layouts differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BinKind {
    Chpx,
    Papx,
}

/// A parsed `PlcBteChpx` / `PlcBtePapx`.
#[derive(Debug)]
pub(crate) struct Bins {
    kind: BinKind,
    /// `aFc[n + 1]`: the byte ranges each page covers.
    fcs: Vec<u32>,
    /// `aPnBte*[n]`: the page numbers (× 512 = byte offset).
    pns: Vec<u32>,
}

/// The formatting a lookup found: the grpprl (empty = no direct
/// formatting), the paragraph style of a `PapxInFkp` and the byte range
/// of the run it applies to.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Run<'w> {
    pub(crate) grpprl: &'w [u8],
    /// `GrpPrlAndIstd.istd` (2.9.112): the paragraph style (`None` for a
    /// CHPX run, and for a PAPX entry with no properties at all).
    pub(crate) istd: Option<u16>,
    /// The run covers bytes `[fc_start, fc_end)`.
    pub(crate) fc_start: u32,
    pub(crate) fc_end: u32,
}

impl Bins {
    /// An empty table (the FIB names none): every lookup finds nothing.
    pub(crate) fn empty(kind: BinKind) -> Bins {
        Bins {
            kind,
            fcs: Vec::new(),
            pns: Vec::new(),
        }
    }

    /// Parse the bin table at `table[fc .. fc + lcb]` (aFc[n + 1] u32s,
    /// then aPnBte[n] u32s).
    pub(crate) fn parse(
        kind: BinKind,
        part: &'static str,
        table: &[u8],
        fc: u32,
        lcb: u32,
    ) -> Result<Bins, DocError> {
        if lcb == 0 {
            return Ok(Bins::empty(kind));
        }
        let plc = slice(table, fc, lcb)
            .ok_or_else(|| malformed(part, "the bin table lies outside the table stream"))?;
        if plc.len() < 4 || (plc.len() - 4) % 8 != 0 {
            return Err(malformed(
                part,
                format!("the bin table is {} bytes, not 4 + 8n", plc.len()),
            ));
        }
        let n = (plc.len() - 4) / 8;
        if n > MAX_BINS {
            return Err(DocError::TooLarge {
                what: "the number of formatting pages",
                max: MAX_BINS as u64,
            });
        }
        let at = |i: usize| u32_at(plc, 4 * i).unwrap_or(0);
        let fcs = (0..=n).map(at).collect();
        /* PnFkpChpx / PnFkpPapx (2.9.198 / 2.9.199): the page number is the
        low 22 bits; the rest is unused. */
        let pns = (0..n).map(|i| at(n + 1 + i) & 0x003F_FFFF).collect();
        Ok(Bins { kind, fcs, pns })
    }

    /// The direct formatting of the byte at `fc`, `None` when no page
    /// covers it (the text has no direct formatting of this kind).
    pub(crate) fn lookup<'w>(&self, word: &'w [u8], fc: u32) -> Option<Run<'w>> {
        let i = self.fcs.partition_point(|&f| f <= fc).checked_sub(1)?;
        if fc >= *self.fcs.get(i + 1)? {
            return None;
        }
        let at = (*self.pns.get(i)? as usize).checked_mul(PAGE)?;
        let page = word.get(at..at.checked_add(PAGE)?)?;
        let crun = usize::from(page[PAGE - 1]);
        let (crun, entry) = match self.kind {
            BinKind::Chpx => (crun.min(MAX_CHPX_RUNS), 1),
            BinKind::Papx => (crun.min(MAX_PAPX_RUNS), BX_PAP),
        };
        /* rgfc[crun + 1] (u32) opens the page; the per-run entries
        (rgb / rgbx) follow it. */
        let rgfc = |j: usize| u32_at(page, 4 * j);
        let j = (0..crun)
            .rev()
            .find(|&j| rgfc(j).is_some_and(|f| f <= fc))?;
        let fc_start = rgfc(j)?;
        let fc_end = rgfc(j + 1)?;
        if fc >= fc_end {
            return None;
        }
        let entry_at = 4 * (crun + 1) + j * entry;
        /* The first byte of an entry is a word offset into the page; 0
        means "no properties". */
        let off = usize::from(*page.get(entry_at)?) * 2;
        let empty = Run {
            grpprl: &[],
            istd: None,
            fc_start,
            fc_end,
        };
        if off == 0 {
            return Some(empty);
        }
        let run = match self.kind {
            BinKind::Chpx => {
                /* Chpx (2.9.32): cb (1 byte) then a grpprl of cb bytes. */
                let cb = usize::from(*page.get(off)?);
                let grpprl = page.get(off + 1..off + 1 + cb).unwrap_or(&[]);
                Run { grpprl, ..empty }
            }
            BinKind::Papx => {
                /* PapxInFkp (2.9.179): cb; when nonzero the GrpPrlAndIstd is
                2 × cb − 1 bytes, when zero the next byte cb' gives 2 × cb'
                bytes. GrpPrlAndIstd (2.9.112): istd (u16, the paragraph
                style) + grpprl. */
                let cb = usize::from(*page.get(off)?);
                let (data_at, len) = if cb != 0 {
                    (off + 1, 2 * cb - 1)
                } else {
                    (off + 2, 2 * usize::from(*page.get(off + 1)?))
                };
                if len < 2 {
                    return Some(empty);
                }
                let istd = crate::le::u16_at(page, data_at);
                let grpprl = page.get(data_at + 2..data_at + len).unwrap_or(&[]);
                Run {
                    grpprl,
                    istd,
                    ..empty
                }
            }
        };
        Some(run)
    }
}
