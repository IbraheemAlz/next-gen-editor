//! Issue #355 — the document's DrawingML theme (`word/theme/theme1.xml`,
//! ECMA-376 Part 1 §20.1.6 `a:theme`) plus the two `word/settings.xml`
//! elements that steer how WordprocessingML resolves against it:
//! `<w:themeFontLang>` (§17.15.1.88) and `<w:clrSchemeMapping>`
//! (§17.15.1.20).
//!
//! Read-only. The `.docx` reader parses the theme part into this model;
//! the part itself still rides the source package byte for byte (the
//! writer never regenerates it). The model lives on
//! [`crate::DocumentTree::theme`] so the live editor (which lays out and
//! saves from the tree alone) and crash-recovery snapshots carry it.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::BTreeMap;
use std::sync::Arc;

/// The parsed theme plus the settings that select into it.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Hash, Default)]
#[serde(default)]
pub struct DocumentTheme {
    /// `<a:theme name="…">` — informational only.
    pub name: String,
    /// `<a:themeElements><a:fontScheme>` (§20.1.4.1.18).
    pub fonts: FontScheme,
    /// `<a:themeElements><a:clrScheme>` (§20.1.6.2).
    pub colors: ColorScheme,
    /// `word/settings.xml` `<w:themeFontLang>`: the languages whose script
    /// picks the supplemental `<a:font script=…>` entry of a theme font.
    pub font_lang: ThemeFontLang,
    /// `word/settings.xml` `<w:clrSchemeMapping>`: which scheme slot each
    /// logical WordprocessingML theme colour (`text1`, `background1`, …)
    /// names.
    pub color_map: ColorSchemeMapping,
}

/// `<a:fontScheme>` — the heading (`major`) and body (`minor`) font sets.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Hash, Default)]
#[serde(default)]
pub struct FontScheme {
    /// `<a:majorFont>` — what `majorAscii` / `majorHAnsi` / `majorBidi` /
    /// `majorEastAsia` name ("+Headings").
    pub major: ThemeFonts,
    /// `<a:minorFont>` — what the `minor*` references name ("+Body").
    pub minor: ThemeFonts,
}

/// One `<a:majorFont>` / `<a:minorFont>` (`CT_FontCollection`,
/// §20.1.4.1.24 / §20.1.4.1.25).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Hash, Default)]
#[serde(default)]
pub struct ThemeFonts {
    /// `<a:latin typeface>`; empty when absent.
    pub latin: String,
    /// `<a:ea typeface>` — Word's stock themes leave it empty and name the
    /// East Asian faces per script instead.
    pub ea: String,
    /// `<a:cs typeface>` — likewise usually empty (`Arab`, `Hebr`, … carry
    /// the complex-script faces).
    pub cs: String,
    /// The supplemental `<a:font script="Arab" typeface="…"/>` entries,
    /// keyed by the ISO 15924 script tag exactly as written.
    pub by_script: BTreeMap<String, String>,
}

/// The twelve `<a:clrScheme>` slots (§20.1.4.1.9 … §20.1.6.2), in schema
/// order — also the `ColorScheme::slots` index.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SchemeColor {
    Dark1,
    Light1,
    Dark2,
    Light2,
    Accent1,
    Accent2,
    Accent3,
    Accent4,
    Accent5,
    Accent6,
    Hyperlink,
    FollowedHyperlink,
}

impl SchemeColor {
    pub const ALL: [SchemeColor; 12] = [
        SchemeColor::Dark1,
        SchemeColor::Light1,
        SchemeColor::Dark2,
        SchemeColor::Light2,
        SchemeColor::Accent1,
        SchemeColor::Accent2,
        SchemeColor::Accent3,
        SchemeColor::Accent4,
        SchemeColor::Accent5,
        SchemeColor::Accent6,
        SchemeColor::Hyperlink,
        SchemeColor::FollowedHyperlink,
    ];

    /// Index into [`ColorScheme::slots`].
    pub fn index(self) -> usize {
        self as usize
    }

    /// The `<a:clrScheme>` child local name (`dk1`, `accent3`, `folHlink`).
    pub fn scheme_element(self) -> &'static str {
        match self {
            SchemeColor::Dark1 => "dk1",
            SchemeColor::Light1 => "lt1",
            SchemeColor::Dark2 => "dk2",
            SchemeColor::Light2 => "lt2",
            SchemeColor::Accent1 => "accent1",
            SchemeColor::Accent2 => "accent2",
            SchemeColor::Accent3 => "accent3",
            SchemeColor::Accent4 => "accent4",
            SchemeColor::Accent5 => "accent5",
            SchemeColor::Accent6 => "accent6",
            SchemeColor::Hyperlink => "hlink",
            SchemeColor::FollowedHyperlink => "folHlink",
        }
    }

    /// Parse a `<a:clrScheme>` child local name.
    pub fn from_scheme_element(name: &str) -> Option<SchemeColor> {
        SchemeColor::ALL
            .into_iter()
            .find(|c| c.scheme_element() == name)
    }

    /// Parse an `ST_WmlColorSchemeIndex` value (§17.18.104) — what a
    /// `<w:clrSchemeMapping>` attribute names (`dark1`, `accent3`,
    /// `followedHyperlink`); also the scheme-direct `ST_ThemeColor` values.
    pub fn from_wml_index(name: &str) -> Option<SchemeColor> {
        Some(match name {
            "dark1" => SchemeColor::Dark1,
            "light1" => SchemeColor::Light1,
            "dark2" => SchemeColor::Dark2,
            "light2" => SchemeColor::Light2,
            "accent1" => SchemeColor::Accent1,
            "accent2" => SchemeColor::Accent2,
            "accent3" => SchemeColor::Accent3,
            "accent4" => SchemeColor::Accent4,
            "accent5" => SchemeColor::Accent5,
            "accent6" => SchemeColor::Accent6,
            "hyperlink" => SchemeColor::Hyperlink,
            "followedHyperlink" => SchemeColor::FollowedHyperlink,
            _ => return None,
        })
    }
}

/// `<a:clrScheme>` — one sRGB value per [`SchemeColor`] slot; `None` for a
/// slot the part does not define (or defines in a form we do not model:
/// `a:scrgbClr`, `a:hslClr`, `a:prstClr`, `a:schemeClr`).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Hash, Default)]
#[serde(default)]
pub struct ColorScheme {
    /// `<a:clrScheme name>` — informational only.
    pub name: String,
    pub slots: [Option<[u8; 3]>; 12],
}

impl ColorScheme {
    pub fn get(&self, slot: SchemeColor) -> Option<[u8; 3]> {
        self.slots[slot.index()]
    }

    pub fn set(&mut self, slot: SchemeColor, rgb: [u8; 3]) {
        self.slots[slot.index()] = Some(rgb);
    }
}

/// `<w:themeFontLang w:val w:eastAsia w:bidi/>` — BCP 47 tags verbatim.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Hash, Default)]
#[serde(default)]
pub struct ThemeFontLang {
    /// `w:val` — the Latin-script language.
    pub latin: Option<String>,
    /// `w:eastAsia` — selects `Jpan` / `Hans` / `Hant` / `Hang`.
    pub east_asia: Option<String>,
    /// `w:bidi` — selects `Arab` / `Hebr` / … for the `*Bidi` references.
    pub bidi: Option<String>,
}

/// `<w:clrSchemeMapping>` — attribute local name (`bg1`, `t1`, `accent1`,
/// `hyperlink`, …) → `ST_WmlColorSchemeIndex` value, verbatim. An absent
/// attribute maps to its identity default (`t1` → `dark1`, `bg1` →
/// `light1`, `accent1` → `accent1`, …).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Hash, Default)]
#[serde(default)]
pub struct ColorSchemeMapping {
    pub entries: BTreeMap<String, String>,
}

/// Serde for `Option<Arc<DocumentTheme>>` (the workspace builds serde
/// without its `rc` feature) — the snapshot encodes the theme inline.
pub mod arc_option {
    use super::*;

    pub fn serialize<S: Serializer>(
        value: &Option<Arc<DocumentTheme>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        value.as_deref().serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Arc<DocumentTheme>>, D::Error> {
        Ok(Option::<DocumentTheme>::deserialize(deserializer)?.map(Arc::new))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scheme_slots_round_trip_their_names() {
        for (i, c) in SchemeColor::ALL.into_iter().enumerate() {
            assert_eq!(c.index(), i);
            assert_eq!(
                SchemeColor::from_scheme_element(c.scheme_element()),
                Some(c)
            );
        }
        assert_eq!(
            SchemeColor::from_wml_index("followedHyperlink"),
            Some(SchemeColor::FollowedHyperlink)
        );
        assert_eq!(SchemeColor::from_wml_index("text1"), None);
    }

    fn sample_theme() -> DocumentTheme {
        let mut t = DocumentTheme {
            name: "Office Theme".into(),
            ..Default::default()
        };
        t.fonts.minor.latin = "Calibri".into();
        t.fonts
            .minor
            .by_script
            .insert("Arab".into(), "Arial".into());
        t.fonts
            .major
            .by_script
            .insert("Hebr".into(), "Times New Roman".into());
        t.colors.set(SchemeColor::Accent1, [0x44, 0x72, 0xC4]);
        t.font_lang.bidi = Some("ar-SA".into());
        t.color_map.entries.insert("t1".into(), "dark1".into());
        t
    }

    /// Issue #355 — the theme rides the tree through a snapshot; a
    /// theme-less tree encodes exactly as before the field existed (the
    /// key is skipped), and equal states encode byte-identically.
    #[test]
    fn theme_survives_a_snapshot_and_a_theme_less_tree_is_unchanged() {
        let plain = crate::DocumentTree::from_text("x");
        let bytes = crate::snapshot::encode(&plain).expect("encode");
        /* MessagePack fixstr(5) "theme" — the map key itself (a bare
        substring search would also hit `font_theme`). */
        let key = b"\xa5theme";
        assert!(
            !bytes.windows(key.len()).any(|w| w == key),
            "no theme key without a theme"
        );
        let mut themed = plain.clone();
        themed.theme = Some(Arc::new(sample_theme()));
        let a = crate::snapshot::encode(&themed).expect("encode");
        let b = crate::snapshot::encode(&themed.clone()).expect("encode");
        assert_eq!(a, b, "deterministic");
        let back: crate::snapshot::Decoded<crate::DocumentTree> =
            crate::snapshot::decode(&a).expect("decode");
        assert_eq!(back.payload.theme, themed.theme);
        let again = crate::snapshot::encode(&back.payload).expect("encode");
        assert_eq!(again, a, "byte-stable through a round trip");
    }
}
