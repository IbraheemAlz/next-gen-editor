//! Issue #327 — the font program a PDF embeds for each used face, subset to
//! the glyphs the document actually shows.
//!
//! Subsetting is delegated to the [`subsetter`] crate (MIT OR Apache-2.0 —
//! the subsetter behind Typst / krilla), built with `default-features =
//! false`: its only dependency is then `rustc-hash`. The `variable-fonts`
//! feature (skrifa + write-fonts + kurbo, CFF2 → TrueType instancing) stays
//! off — variable-font instancing is out of scope, and a face the subsetter
//! cannot handle simply falls back to full embedding. Measured wasm cost
//! (`wasm-pack build --release`): +67,058 B raw (9,369,160 → 9,436,218 B,
//! still ~6.0 MiB under the 15 MiB gate) / +25,604 B `gzip -9`.
//!
//! # Glyph renumbering
//!
//! A subset font has a fresh, contiguous glyph-id space (`.notdef` stays 0),
//! so every place the PDF names a glyph has to speak the NEW ids: the
//! 2-byte `Tj` codes in the content streams, the `/W` widths array, the
//! `/ToUnicode` CMap and (PDF/A-1b) the `/CIDSet`. [`GlyphCodes`] is the
//! one place that mapping lives — the content emitters ask it for a code
//! as they show a glyph, which assigns new ids in first-use order and so
//! records the exact set of glyphs the subset needs as a side effect.
//! `CIDToGIDMap /Identity` keeps CID == new glyph id, so the code *is* the
//! CID.
//!
//! When a face cannot be subset it falls back to the pre-#327 full
//! embedding: codes are the shaped glyph ids unchanged and the whole font
//! file is embedded — byte-identical to the old output for that face.

use std::collections::BTreeMap;
use subsetter::GlyphRemapper;

#[cfg(test)]
thread_local! {
    /// Test hook: make [`subset_program`] reject every face on this thread,
    /// to drive the full-embedding fallback with a font the subsetter would
    /// otherwise accept.
    pub(crate) static REJECT_SUBSET: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// How shaped glyph ids become content-stream codes for one font.
#[derive(Debug, Clone)]
pub(crate) enum GlyphCodes {
    /// Full embedding — the code is the shaped glyph id.
    Identity,
    /// Subset embedding — glyphs renumbered in first-use order.
    Subset(GlyphRemapper),
}

impl GlyphCodes {
    /// A fresh subsetting map (only `.notdef` assigned, to 0).
    pub(crate) fn subset() -> Self {
        Self::Subset(GlyphRemapper::new())
    }

    /// The code for a glyph that is about to be SHOWN — for a subset this
    /// assigns the next new id on first use, which is how the used-glyph
    /// set is collected.
    pub(crate) fn code_for_show(&mut self, gid: u16) -> u16 {
        match self {
            Self::Identity => gid,
            Self::Subset(r) => r.remap(gid),
        }
    }

    /// Re-key a glyph-id → Unicode map by code. A subset drops every glyph
    /// that never reached a content stream (it is not in the font program
    /// at all) and `.notdef` (never shown — `show_run` skips glyph id 0).
    /// Identity keeps the map exactly as harvested.
    pub(crate) fn rekey_unicode(
        &self,
        map: Option<&BTreeMap<u16, Vec<char>>>,
    ) -> Option<BTreeMap<u16, Vec<char>>> {
        let map = map?;
        Some(match self {
            Self::Identity => map.clone(),
            Self::Subset(r) => map
                .iter()
                .filter(|&(&gid, _)| gid != 0)
                .filter_map(|(&gid, chars)| r.get(gid).map(|code| (code, chars.clone())))
                .collect(),
        })
    }
}

/// The program embedded for one face, plus what the font dictionaries need
/// to describe it.
pub(crate) struct FontProgram {
    /// The `FontFile2` payload (an sfnt).
    pub(crate) data: Vec<u8>,
    /// Glyph count of the embedded program. For a subset this is what the
    /// `/CIDSet` must cover: the requested glyphs plus every composite
    /// component the subsetter's closure pulled in (CID == GID).
    pub(crate) num_glyphs: u16,
    /// Six-letter `ABCDEF` subset tag (ISO 32000-1 §9.6.4) — `None` for a
    /// full embedding, which keeps its plain name.
    pub(crate) subset_tag: Option<String>,
}

/// Subset `face_data` to the glyphs `remapper` recorded. `Err` when the
/// face can't be subset (the caller falls back to full embedding).
pub(crate) fn subset_program(
    id: &str,
    face_data: &[u8],
    remapper: &GlyphRemapper,
) -> Result<FontProgram, subsetter::Error> {
    #[cfg(test)]
    if REJECT_SUBSET.with(std::cell::Cell::get) {
        return Err(subsetter::Error::Unimplemented);
    }
    let data = subsetter::subset(face_data, 0, remapper)?;
    let num_glyphs = sfnt_table(&data, *b"maxp")
        .and_then(|maxp| maxp.get(4..6))
        .map(|b| u16::from_be_bytes([b[0], b[1]]))
        .ok_or(subsetter::Error::SubsetError)?;
    Ok(FontProgram {
        data,
        num_glyphs,
        subset_tag: Some(subset_tag(id, remapper)),
    })
}

/// The pre-#327 full embedding: the whole font file, byte-identical to the
/// old output.
pub(crate) fn full_program(face_data: &[u8]) -> FontProgram {
    FontProgram {
        data: face_data.to_vec(),
        num_glyphs: 0,
        subset_tag: None,
    }
}

/// Deterministic subset tag: six uppercase letters from an FNV-1a hash of
/// the face id and the glyph order. Different subsets of one face in one
/// file get different tags (they differ in glyph order); re-exporting the
/// same document reproduces the same tag, keeping exports byte-stable.
fn subset_tag(id: &str, remapper: &GlyphRemapper) -> String {
    let mut bytes = id.as_bytes().to_vec();
    for gid in remapper.remapped_gids() {
        bytes.extend_from_slice(&gid.to_be_bytes());
    }
    let mut h = crate::fnv1a64(&bytes);
    (0..6)
        .map(|_| {
            let c = b'A' + (h % 26) as u8;
            h /= 26;
            c as char
        })
        .collect()
}

/// The `/CIDSet` bitmap for CIDs `0..num_glyphs`: bit `7 - cid % 8` of
/// byte `cid / 8` set for every CID present (ISO 32000-1 Table 124).
pub(crate) fn cid_set_bytes(num_glyphs: u16) -> Vec<u8> {
    let n = usize::from(num_glyphs);
    let mut bits = vec![0u8; n.div_ceil(8)];
    for cid in 0..n {
        bits[cid / 8] |= 0x80 >> (cid % 8);
    }
    bits
}

/// The table `tag` of an sfnt font (the first face of a collection) —
/// a bounds-checked table-directory lookup, nothing more.
pub(crate) fn sfnt_table(data: &[u8], tag: [u8; 4]) -> Option<&[u8]> {
    let slice = |at: usize, len: usize| data.get(at..at.checked_add(len)?);
    let be32 = |at: usize| -> Option<usize> {
        slice(at, 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize)
    };
    let dir = if slice(0, 4)? == b"ttcf" {
        be32(12)?
    } else {
        0
    };
    let count = slice(dir.checked_add(4)?, 2).map(|b| u16::from_be_bytes([b[0], b[1]]))?;
    for i in 0..usize::from(count) {
        let rec = dir.checked_add(12 + i * 16)?;
        if slice(rec, 4)? == tag {
            return slice(be32(rec + 8)?, be32(rec + 12)?);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn liberation() -> &'static [u8] {
        include_bytes!("../../../ts/fonts/LiberationSans-Regular.ttf")
    }

    #[test]
    fn sfnt_table_lookup_is_bounds_checked() {
        assert!(sfnt_table(liberation(), *b"glyf").is_some());
        assert!(sfnt_table(liberation(), *b"zzzz").is_none());
        assert!(sfnt_table(b"garbage", *b"glyf").is_none());
        assert!(sfnt_table(&[], *b"glyf").is_none());
        /* A table record pointing past the end of the data. */
        let mut bogus = liberation()[..12 + 16].to_vec();
        bogus[4..6].copy_from_slice(&1u16.to_be_bytes());
        bogus[12..16].copy_from_slice(b"glyf");
        bogus[20..24].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(sfnt_table(&bogus, *b"glyf").is_none());
    }

    #[test]
    fn cid_set_covers_exactly_the_glyph_count() {
        assert_eq!(cid_set_bytes(0), Vec::<u8>::new());
        assert_eq!(cid_set_bytes(1), vec![0x80]);
        assert_eq!(cid_set_bytes(8), vec![0xFF]);
        assert_eq!(cid_set_bytes(10), vec![0xFF, 0xC0]);
    }

    #[test]
    fn subset_renumbers_and_shrinks() {
        let mut codes = GlyphCodes::subset();
        /* First-use order: 40 → 1, 2 → 2, 40 again → 1. */
        assert_eq!(codes.code_for_show(40), 1);
        assert_eq!(codes.code_for_show(2), 2);
        assert_eq!(codes.code_for_show(40), 1);
        let GlyphCodes::Subset(r) = &codes else {
            unreachable!("built as a subset")
        };
        let p = subset_program("liberation", liberation(), r).expect("liberation subsets");
        assert!(p.data.len() * 20 < liberation().len(), "{}", p.data.len());
        assert!(p.num_glyphs >= 3);
        let tag = p.subset_tag.expect("subset carries a tag");
        assert_eq!(tag.len(), 6);
        assert!(tag.bytes().all(|b| b.is_ascii_uppercase()));
        /* Unicode map: unused glyph 7 and .notdef drop out, used re-key. */
        let mut map = BTreeMap::new();
        map.insert(0u16, vec!['\u{c}']);
        map.insert(2u16, vec!['b']);
        map.insert(7u16, vec!['z']);
        map.insert(40u16, vec!['a']);
        let rekeyed = codes.rekey_unicode(Some(&map)).expect("map");
        assert_eq!(
            rekeyed.into_iter().collect::<Vec<_>>(),
            vec![(1, vec!['a']), (2, vec!['b'])]
        );
        assert_eq!(
            GlyphCodes::Identity.rekey_unicode(Some(&map)),
            Some(map.clone())
        );
    }

    #[test]
    fn garbage_does_not_subset() {
        let r = GlyphRemapper::new();
        assert!(subset_program("x", b"not a font at all", &r).is_err());
    }
}
