//! The font table ([MS-DOC] 2.9.293 `SttbfFfn`): an STTB (2.2.4) of `FFN`
//! records (2.9.82), one per font the document names. A character's
//! `sprmCRgFtc0` / `sprmCRgFtc2` / `sprmCFtcBi` (2.6.1) — and the style
//! sheet's default fonts (`Stshif.ftcAsci`, `STSHI.ftcBi`, under 2.9.271
//! STSH) — are indices (`ftc`) into it.
//!
//! Only the face NAME is read: the engine's model keeps a family name per
//! slot and resolves it at layout through `FontStack::resolve_family` and
//! the substitution table, exactly like a `.docx` `<w:rFonts>` name. The
//! other `FFN` fields (pitch, family, weight, character set, PANOSE, the
//! font signature) describe the face for a font matcher the engine does
//! not have.
//!
//! Written from the specification (not borrowed): the table is bounded by
//! its own `lcb` inside the table stream, every entry by its own length.

use crate::le::{slice, u16_at};

/// `FFN` bytes before `xszFfn`: `ffid` (1), `wWeight` (2), `chs` (1),
/// `ixchSzAlt` (1), `panose` (10), `fs` (24) — 2.9.82.
const FFN_HEADER: usize = 39;
/// A face name longer than this is not a Windows face name (`LF_FACESIZE`
/// is 32; the spec bounds `xszFfn` at 65 characters): the rest is cut.
const MAX_NAME_UNITS: usize = 65;
/// Most entries read (an `ftc` is 16 bits).
const MAX_FONTS: usize = 1 << 16;

/// The document's fonts by `ftc`.
#[derive(Debug, Clone, Default)]
pub(crate) struct FontTable {
    names: Vec<Option<String>>,
}

/// A face or style name made safe for the model: characters XML 1.0 cannot
/// carry (and every other C0 control) dropped, surrounding blanks trimmed.
/// `None` when nothing is left.
pub(crate) fn clean_name(units: &[u16]) -> Option<String> {
    let s: String = char::decode_utf16(units.iter().copied())
        .map(|c| c.unwrap_or(engine::text_input::REPLACEMENT))
        .filter(|&c| c >= ' ' && engine::text_input::is_xml_char(c))
        .collect();
    let trimmed = s.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

impl FontTable {
    /// Parse the `SttbfFfn` at `table[fc .. fc + lcb]`. `lcb` 0 is an empty
    /// table. The second value is `true` when the table lies outside the
    /// stream or ends inside an entry (what was read stays usable).
    pub(crate) fn parse(table: &[u8], fc: u32, lcb: u32) -> (FontTable, bool) {
        if lcb == 0 {
            return (FontTable::default(), false);
        }
        let Some(sttb) = slice(table, fc, lcb) else {
            return (FontTable::default(), true);
        };
        /* STTB (2.2.4): an optional `fExtend` 0xFFFF (16-bit character
        counts), `cData` (2 bytes), `cbExtra` (2 bytes), then `cData`
        entries of `cchData` + data + `cbExtra` bytes. The font table is
        written with 1-byte `cchData` counting bytes (the FFN's size); an
        extended one counts 16-bit units and is accepted too. */
        let Some(first) = u16_at(sttb, 0) else {
            return (FontTable::default(), true);
        };
        let extended = first == 0xFFFF;
        let head = if extended { 2 } else { 0 };
        let (Some(c_data), Some(cb_extra)) = (u16_at(sttb, head), u16_at(sttb, head + 2)) else {
            return (FontTable::default(), true);
        };
        let mut at = head + 4;
        let mut names = Vec::new();
        let mut damaged = false;
        for _ in 0..usize::from(c_data).min(MAX_FONTS) {
            let (len, data_at) = if extended {
                match u16_at(sttb, at) {
                    Some(units) => (2 * usize::from(units), at + 2),
                    None => {
                        damaged = true;
                        break;
                    }
                }
            } else {
                match sttb.get(at) {
                    Some(&b) => (usize::from(b), at + 1),
                    None => {
                        damaged = true;
                        break;
                    }
                }
            };
            let Some(ffn) = sttb.get(data_at..data_at + len) else {
                damaged = true;
                break;
            };
            names.push(ffn_name(ffn));
            at = data_at + len + usize::from(cb_extra);
        }
        (FontTable { names }, damaged)
    }

    /// The face name of font `ftc`, `None` when the table has no such
    /// entry (or its name is empty).
    pub(crate) fn name(&self, ftc: u16) -> Option<&str> {
        self.names.get(usize::from(ftc))?.as_deref()
    }

    /// The engine family of font `ftc`: the face name as the `.docx`
    /// reader takes a `<w:rFonts>` name (`family_from_docx`), so layout
    /// resolves it — substitutes included — the same way.
    pub(crate) fn family(&self, ftc: u16) -> Option<engine::FontFamily> {
        format_docx::schema::ct_rpr::family_from_docx(self.name(ftc)?)
    }

    /// The number of entries read.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.names.len()
    }
}

/// `FFN.xszFfn`: the primary face name, a NUL-terminated UTF-16 string
/// after the fixed header (an alternate name, `xszAlt`, may follow the NUL
/// and is not read).
fn ffn_name(ffn: &[u8]) -> Option<String> {
    let tail = ffn.get(FFN_HEADER..)?;
    let units: Vec<u16> = tail
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .take_while(|&u| u != 0)
        .take(MAX_NAME_UNITS)
        .collect();
    clean_name(&units)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ffn(name: &str, alt: Option<&str>) -> Vec<u8> {
        let mut v = vec![0u8; FFN_HEADER];
        v[0] = 0x26; // prq 2, fTrueType, ff 2 (swiss)
        if alt.is_some() {
            v[4] = (name.encode_utf16().count() + 1) as u8;
        }
        for u in name.encode_utf16().chain([0]) {
            v.extend_from_slice(&u.to_le_bytes());
        }
        if let Some(a) = alt {
            for u in a.encode_utf16().chain([0]) {
                v.extend_from_slice(&u.to_le_bytes());
            }
        }
        v
    }

    fn sttb(entries: &[Vec<u8>]) -> Vec<u8> {
        let mut v = (entries.len() as u16).to_le_bytes().to_vec();
        v.extend_from_slice(&0u16.to_le_bytes());
        for e in entries {
            v.push(e.len() as u8);
            v.extend_from_slice(e);
        }
        v
    }

    #[test]
    fn names_are_read_by_index_and_the_alternate_name_is_skipped() {
        let t = sttb(&[
            ffn("Times New Roman", None),
            ffn("Arial", Some("Helvetica")),
            ffn("Simplified Arabic", None),
        ]);
        let mut table = vec![0xAAu8; 6];
        table.extend_from_slice(&t);
        let (fonts, damaged) = FontTable::parse(&table, 6, t.len() as u32);
        assert!(!damaged);
        assert_eq!(fonts.len(), 3);
        assert_eq!(fonts.name(0), Some("Times New Roman"));
        assert_eq!(fonts.name(1), Some("Arial"));
        assert_eq!(fonts.name(2), Some("Simplified Arabic"));
        assert_eq!(fonts.name(3), None);
        assert_eq!(
            fonts.family(1).map(|f| f.display_name().to_string()),
            Some("Arial".to_string())
        );
    }

    #[test]
    fn a_damaged_table_keeps_what_it_read() {
        let t = sttb(&[ffn("Arial", None), ffn("Tahoma", None)]);
        /* Cut inside the second entry. */
        let cut = &t[..t.len() - 3];
        let (fonts, damaged) = FontTable::parse(cut, 0, cut.len() as u32);
        assert!(damaged);
        assert_eq!(fonts.name(0), Some("Arial"));
        assert_eq!(fonts.name(1), None);
        /* Outside the stream. */
        let (fonts, damaged) = FontTable::parse(&t, 4, t.len() as u32);
        assert!(damaged);
        assert_eq!(fonts.name(0), None);
        /* A short entry (no name) and a name of control characters. */
        let t = sttb(&[vec![0u8; 10], ffn("\u{1}\u{2}", None), ffn(" Arial ", None)]);
        let (fonts, damaged) = FontTable::parse(&t, 0, t.len() as u32);
        assert!(!damaged);
        assert_eq!((fonts.name(0), fonts.name(1)), (None, None));
        assert_eq!(fonts.name(2), Some("Arial"));
    }

    #[test]
    fn an_extended_table_counts_16_bit_units() {
        let entry = ffn("Tahoma", None);
        let mut t = vec![0xFF, 0xFF];
        t.extend_from_slice(&1u16.to_le_bytes());
        t.extend_from_slice(&0u16.to_le_bytes());
        t.extend_from_slice(&((entry.len() / 2) as u16).to_le_bytes());
        t.extend_from_slice(&entry[..entry.len() / 2 * 2]);
        let (fonts, _) = FontTable::parse(&t, 0, t.len() as u32);
        assert_eq!(fonts.name(0), Some("Tahoma"));
    }
}
