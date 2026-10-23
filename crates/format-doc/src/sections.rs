// Modified from WordCraft (https://github.com/storytold/wordcraft) at commit
// 3a4770ef8ce037d2650c93e2d5764536549357c7, file crates/docbin/src/sections.rs.
// Copyright (c) 2026 ArtCraft Team and the WordCraft contributors.
// Licensed under the MIT License or the Apache License, Version 2.0, at your
// option. Changes: only the section table is read (header / footer stories
// are not imported); a malformed PlcfSed is a typed error; the sprms land in
// `props::SectionGeometry`. See THIRD-PARTY-NOTICES.md at the repository
// root.

//! Sections ([MS-DOC] 2.8.26 PlcfSed, 2.9.243 Sed, 2.9.253 Sepx): the CP
//! where each section ends and the grpprl of section sprms that modifies
//! its default properties.

use crate::error::{DocError, malformed};
use crate::le::{slice, u16_at, u32_at};
use crate::props::SectionGeometry;
use crate::sprm::{SGC_SECTION, prls};

const PART: &str = "Table: PlcfSed";

/// Most sections read (Word's documents rarely have more than a few
/// dozen; each one is a page-setup change).
const MAX_SECTIONS: usize = 10_000;

/// One section: it ends with the character before `end_cp`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Section {
    pub(crate) end_cp: u32,
    pub(crate) props: SectionGeometry,
}

/// Parse the `PlcfSed` at `table[fc .. fc + lcb]` and each section's
/// `Sepx` from the `WordDocument` stream. An absent table is no sections.
pub(crate) fn parse(
    word: &[u8],
    table: &[u8],
    fc: u32,
    lcb: u32,
) -> Result<Vec<Section>, DocError> {
    if lcb == 0 {
        return Ok(Vec::new());
    }
    let plc = slice(table, fc, lcb)
        .ok_or_else(|| malformed(PART, "the section table lies outside the table stream"))?;
    /* aCp[n + 1] (u32) then aSed[n], each 12 bytes: fn (2), fcSepx (4),
    fnMpr (2), fcMpr (4). */
    if plc.len() < 4 || (plc.len() - 4) % 16 != 0 {
        return Err(malformed(
            PART,
            format!("the section table is {} bytes, not 4 + 16n", plc.len()),
        ));
    }
    let n = (plc.len() - 4) / 16;
    if n > MAX_SECTIONS {
        return Err(DocError::TooLarge {
            what: "the number of sections",
            max: MAX_SECTIONS as u64,
        });
    }
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let end_cp = u32_at(plc, 4 * (i + 1)).unwrap_or(0);
        if out.last().is_some_and(|s: &Section| s.end_cp > end_cp) {
            return Err(malformed(
                PART,
                "the section CPs are not in ascending order",
            ));
        }
        let sed = 4 * (n + 1) + 12 * i;
        let fc_sepx = u32_at(plc, sed + 2).unwrap_or(u32::MAX);
        let mut props = SectionGeometry::default();
        /* fcSepx 0xFFFFFFFF: the section has the default properties.
        Sepx: cb (i16) then a grpprl of cb bytes. A Sepx outside the
        stream is read as no modifiers (the section keeps its defaults),
        not as a damaged document: nothing else depends on it. */
        if fc_sepx != u32::MAX
            && let Some(cb) = u16_at(word, fc_sepx as usize)
            && let Some(grpprl) = slice(word, fc_sepx.saturating_add(2), u32::from(cb))
        {
            for p in prls(grpprl).filter(|p| p.sgc() == SGC_SECTION) {
                props.apply(&p);
            }
        }
        out.push(Section {
            end_cp,
            props: props.finish(),
        });
    }
    Ok(out)
}
