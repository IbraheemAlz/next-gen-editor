// Modified from WordCraft (https://github.com/storytold/wordcraft) at commit
// 3a4770ef8ce037d2650c93e2d5764536549357c7, file crates/docbin/src/sprm.rs.
// Copyright (c) 2026 ArtCraft Team and the WordCraft contributors.
// Licensed under the MIT License or the Apache License, Version 2.0, at your
// option. Changes: the Prm0 table maps isprm 0x05 to sprmPJc80 (the Word 97
// justification sprm Prm0 was defined for) and lists only the sprms this
// importer applies; the toggle helper moved into `props.rs`. See
// THIRD-PARTY-NOTICES.md at the repository root.

//! Property modifiers ([MS-DOC] 2.2.5.1 Sprm, 2.2.5.2 Prl, 2.2.5.3
//! Grpprl). A `Prl` is a 2-byte `Sprm` followed by an operand whose size
//! the sprm's `spra` field gives, with two documented exceptions whose
//! size is spelled inside the operand.

/// `sprmPChgTabs`: its operand's size byte can be 255 ("compute it",
/// 2.6.2 PChgTabsOperand).
const P_CHG_TABS: u16 = 0xC615;
/// `sprmTDefTable`: a 2-byte size that counts itself minus one (2.6.3
/// TDefTableOperand).
const T_DEF_TABLE: u16 = 0xD608;

/// One property modifier: the sprm and its operand bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Prl<'a> {
    pub(crate) op: u16,
    pub(crate) operand: &'a [u8],
}

impl Prl<'_> {
    /// `Sprm.sgc` — which kind of properties it modifies: 1 paragraph,
    /// 2 character, 3 picture, 4 section, 5 table.
    pub(crate) fn sgc(&self) -> u8 {
        ((self.op >> 10) & 7) as u8
    }
}

/// Sprm groups (`Sprm.sgc`, 2.2.5.1).
pub(crate) const SGC_PARAGRAPH: u8 = 1;
pub(crate) const SGC_CHARACTER: u8 = 2;
pub(crate) const SGC_SECTION: u8 = 4;
pub(crate) const SGC_TABLE: u8 = 5;

/// The operand length of `op` given the bytes after it, `None` when the
/// bytes cannot hold it.
fn operand_len(op: u16, after: &[u8]) -> Option<usize> {
    Some(match op {
        P_CHG_TABS => match *after.first()? {
            /* cb 255: cb, cTabsDel, rgdxaDel (2 × del), rgdxaClose (2 ×
            del), cTabsAdd, rgdxaAdd (2 × add), rgtbdAdd (add). */
            255 => {
                let del = usize::from(*after.get(1)?);
                let add = usize::from(*after.get(2 + 4 * del)?);
                1 + 1 + 4 * del + 1 + 3 * add
            }
            cb => 1 + usize::from(cb),
        },
        T_DEF_TABLE => 1 + usize::from(crate::le::u16_at(after, 0)?),
        /* spra (2.2.5.1): 0 / 1 → 1 byte, 2 / 4 / 5 → 2, 3 → 4, 7 → 3,
        6 → variable: the first operand byte is the size of the rest. */
        _ => match op >> 13 {
            0 | 1 => 1,
            2 | 4 | 5 => 2,
            3 => 4,
            7 => 3,
            _ => 1 + usize::from(*after.first()?),
        },
    })
}

/// The `Prl`s of a grpprl, in order. A truncated last `Prl` ends the
/// iteration (the earlier ones still apply).
pub(crate) fn prls(grpprl: &[u8]) -> impl Iterator<Item = Prl<'_>> {
    let mut rest = grpprl;
    std::iter::from_fn(move || {
        let op = crate::le::u16_at(rest, 0)?;
        let after = &rest[2..];
        let len = operand_len(op, after)?;
        let operand = after.get(..len)?;
        rest = &after[len..];
        Some(Prl { op, operand })
    })
}

/// The sprm a Prm0 `isprm` stands for (2.9.215 Prm0, its isprm table),
/// for the sprms this importer applies; `None` for every other entry.
pub(crate) fn sprm_of_isprm(isprm: u8) -> Option<u16> {
    Some(match isprm {
        0x05 => 0x2403, // sprmPJc80
        0x55 => 0x0835, // sprmCFBold
        0x56 => 0x0836, // sprmCFItalic
        0x57 => 0x0837, // sprmCFStrike
        0x5E => 0x2A3E, // sprmCKul
        0x62 => 0x2A42, // sprmCIco
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operand_sizes_follow_spra_and_the_two_exceptions() {
        let g = [
            0x35, 0x08, 0x01, // sprmCFBold, spra 0 → 1 byte
            0x43, 0x4A, 0x18, 0x00, // sprmCHps, spra 2 → 2 bytes
            0x70, 0x68, 1, 2, 3, 0, // sprmCCv, spra 3 → 4 bytes
            0x15, 0xC6, 0x02, 0xAA, 0xBB, // sprmPChgTabs, cb = 2
            0x08, 0xD6, 0x03, 0x00, 0x01, 0x02, // sprmTDefTable, cb 3 → 4 bytes
            0x35, 0x08, // truncated tail
        ];
        let ops: Vec<(u16, usize)> = prls(&g).map(|p| (p.op, p.operand.len())).collect();
        assert_eq!(
            ops,
            vec![
                (0x0835, 1),
                (0x4A43, 2),
                (0x6870, 4),
                (0xC615, 3),
                (0xD608, 4)
            ]
        );
        assert_eq!(
            Prl {
                op: 0x0835,
                operand: &[]
            }
            .sgc(),
            SGC_CHARACTER
        );
        assert_eq!(
            Prl {
                op: 0x2461,
                operand: &[]
            }
            .sgc(),
            SGC_PARAGRAPH
        );
        assert_eq!(
            Prl {
                op: 0xB01F,
                operand: &[]
            }
            .sgc(),
            SGC_SECTION
        );
    }

    #[test]
    fn a_computed_tab_operand_size_is_derived_from_its_counts() {
        /* cb 255, one stop removed (2 + 2 bytes), two added (4 + 2). */
        let g = [0x15, 0xC6, 255, 1, 0, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0];
        let p: Vec<Prl> = prls(&g).collect();
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].operand.len(), 13);
    }
}
