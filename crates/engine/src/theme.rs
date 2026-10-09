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

/* ============================================================
Theme fonts (ECMA-376 §17.3.2.26 `w:rFonts`, §17.18.96 `ST_Theme`).
============================================================ */

/// Which typeface of a font collection an `ST_Theme` value names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeTypeface {
    /// `*Ascii` / `*HAnsi` → `<a:latin>`.
    Latin,
    /// `*EastAsia` → the East Asian script entry, else `<a:ea>`.
    EastAsian,
    /// `*Bidi` → the complex-script entry, else `<a:cs>`.
    ComplexScript,
}

/// A parsed `ST_Theme` value (`minorHAnsi`, `majorBidi`, …).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThemeFontRef {
    /// `major*` (headings) vs `minor*` (body).
    pub major: bool,
    pub typeface: ThemeTypeface,
}

impl ThemeFontRef {
    /// Parse an `ST_Theme` value. Case-insensitive (the schema spells it
    /// in camelCase; a lenient read costs nothing); `None` for anything
    /// else, which then simply does not resolve.
    pub fn parse(value: &str) -> Option<ThemeFontRef> {
        let v = value.trim().to_ascii_lowercase();
        let (major, rest) = if let Some(r) = v.strip_prefix("major") {
            (true, r)
        } else {
            (false, v.strip_prefix("minor")?)
        };
        let typeface = match rest {
            "ascii" | "hansi" => ThemeTypeface::Latin,
            "eastasia" => ThemeTypeface::EastAsian,
            "bidi" => ThemeTypeface::ComplexScript,
            _ => return None,
        };
        Some(ThemeFontRef { major, typeface })
    }
}

/// The ISO 15924 tag a theme's supplemental `<a:font script>` list uses
/// for a BCP 47 language (`ar-SA` → `Arab`, `zh-TW` → `Hant`). An explicit
/// script subtag wins (`pa-Arab-PK` → `Arab`). `None` for a language whose
/// script has no supplemental entry in Word's themes (Latin, Cyrillic, …).
pub fn script_tag_for_lang(lang: &str) -> Option<&'static str> {
    const SCRIPTS: &[&str] = &[
        "Arab", "Hebr", "Thai", "Jpan", "Hang", "Hans", "Hant", "Syrc", "Thaa", "Deva", "Beng",
        "Guru", "Gujr", "Orya", "Taml", "Telu", "Knda", "Mlym", "Sinh", "Khmr", "Laoo", "Mymr",
        "Tibt", "Ethi", "Geor", "Armn", "Mong", "Viet", "Uigh", "Cher", "Cans", "Yiii",
    ];
    let mut parts = lang.trim().split(['-', '_']).filter(|p| !p.is_empty());
    let primary = parts.next()?.to_ascii_lowercase();
    let rest: Vec<&str> = parts.collect();
    if let Some(script) = rest
        .iter()
        .find(|p| p.len() == 4 && p.chars().all(|c| c.is_ascii_alphabetic()))
        && let Some(tag) = SCRIPTS.iter().find(|t| t.eq_ignore_ascii_case(script))
    {
        return Some(tag);
    }
    Some(match primary.as_str() {
        "ar" | "fa" | "ur" | "ps" | "sd" | "ckb" | "prs" | "ks" => "Arab",
        "ug" => "Uigh",
        "he" | "yi" => "Hebr",
        "th" => "Thai",
        "ja" => "Jpan",
        "ko" => "Hang",
        "zh" => {
            let traditional = rest
                .iter()
                .any(|r| matches!(r.to_ascii_uppercase().as_str(), "TW" | "HK" | "MO"));
            if traditional { "Hant" } else { "Hans" }
        }
        "syr" => "Syrc",
        "dv" => "Thaa",
        "hi" | "mr" | "ne" | "sa" | "kok" => "Deva",
        "bn" | "as" => "Beng",
        "pa" => "Guru",
        "gu" => "Gujr",
        "or" => "Orya",
        "ta" => "Taml",
        "te" => "Telu",
        "kn" => "Knda",
        "ml" => "Mlym",
        "si" => "Sinh",
        "km" => "Khmr",
        "lo" => "Laoo",
        "my" => "Mymr",
        "bo" => "Tibt",
        "am" | "ti" => "Ethi",
        "ka" => "Geor",
        "hy" => "Armn",
        "vi" => "Viet",
        "chr" => "Cher",
        "iu" => "Cans",
        "ii" => "Yiii",
        _ => return None,
    })
}

impl DocumentTheme {
    /// The typeface `r` names, or `None` when the theme leaves it empty.
    ///
    /// * Latin (`*Ascii` / `*HAnsi`) → `<a:latin>`.
    /// * Complex script (`*Bidi`) / East Asian (`*EastAsia`) → the
    ///   supplemental `<a:font script>` entry for the script of the
    ///   `<w:themeFontLang>` language (`w:bidi` / `w:eastAsia`) when the
    ///   settings name one, else for `script_hint` (the script of the text
    ///   being laid out — `Arab` for Arabic runs); then the generic
    ///   `<a:cs>` / `<a:ea>` typeface. Word's stock themes leave the
    ///   generic slots empty and name every complex-script / East Asian
    ///   face per script, so the per-script entry is the more specific
    ///   statement and is consulted first.
    pub fn theme_font(&self, r: ThemeFontRef, script_hint: Option<&str>) -> Option<&str> {
        let fonts = if r.major {
            &self.fonts.major
        } else {
            &self.fonts.minor
        };
        let (lang, generic) = match r.typeface {
            ThemeTypeface::Latin => return nonempty(&fonts.latin),
            ThemeTypeface::EastAsian => (self.font_lang.east_asia.as_deref(), fonts.ea.as_str()),
            ThemeTypeface::ComplexScript => (self.font_lang.bidi.as_deref(), fonts.cs.as_str()),
        };
        lang.and_then(script_tag_for_lang)
            .or(script_hint)
            .and_then(|s| fonts.by_script.get(s))
            .and_then(|f| nonempty(f))
            .or_else(|| nonempty(generic))
    }
}

/// `Some(s)` unless `s` is blank — an empty `typeface=""` names nothing.
fn nonempty(s: &str) -> Option<&str> {
    (!s.trim().is_empty()).then_some(s)
}

/// Issue #355 — what one `<w:rFonts>` slot was bound to at the level that
/// declared it.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Hash)]
pub enum FontBinding {
    /// An explicit family name (`w:ascii` / `w:hAnsi` / `w:eastAsia` /
    /// `w:cs`) with no theme attribute for the slot. The name itself rides
    /// [`crate::SpanStyle::font_family`] / `raw_font_family`; the binding
    /// only records that this level claimed the slot, so an inherited
    /// theme binding yields to it.
    Name,
    /// A theme reference (`w:asciiTheme="minorHAnsi"`, …), verbatim. Per
    /// §17.3.2.26 it supersedes a name for the same slot on the same
    /// element.
    Theme(String),
}

/// Issue #355 — the four `<w:rFonts>` slots of one formatting level
/// (§17.3.2.26): `ascii` (U+0000–U+007F), `hAnsi` (other Latin and
/// everything not claimed by the other two), `eastAsia`, `cs` (complex
/// script — Arabic, Hebrew, Thai, …). `None` = the level does not mention
/// the slot, so the cascade below decides it.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Hash, Default)]
#[serde(default)]
pub struct RunFontBindings {
    pub ascii: Option<FontBinding>,
    pub h_ansi: Option<FontBinding>,
    pub east_asia: Option<FontBinding>,
    pub cs: Option<FontBinding>,
}

impl RunFontBindings {
    pub fn is_empty(&self) -> bool {
        self.ascii.is_none()
            && self.h_ansi.is_none()
            && self.east_asia.is_none()
            && self.cs.is_none()
    }

    /// Every theme binding as `(attribute, ST_Theme value)`, in the order
    /// Word writes them.
    pub fn theme_attrs(&self) -> impl Iterator<Item = (&'static str, &str)> {
        [
            ("w:asciiTheme", &self.ascii),
            ("w:eastAsiaTheme", &self.east_asia),
            ("w:hAnsiTheme", &self.h_ansi),
            ("w:cstheme", &self.cs),
        ]
        .into_iter()
        .filter_map(|(k, v)| match v {
            Some(FontBinding::Theme(t)) => Some((k, t.as_str())),
            _ => None,
        })
    }
}

/// Issue #355 — the cascade rule for [`RunFontBindings`], applied by
/// [`crate::SpanStyle::merged_with`] at every level (docDefaults → style
/// chain → paragraph mark → character style → direct formatting →
/// engine edits). A level that mentions a slot replaces it whole, name or
/// theme — Word's own behaviour: picking Arial for text in a theme-fonted
/// document writes `w:ascii="Arial"` and the text shows Arial, although
/// the inherited `w:asciiTheme` was never removed. A patch without
/// bindings that still names a family (the toolbar, `ModifyStyle`, HTML
/// paste) claims the three slots the writer spells for it — ascii, hAnsi,
/// cs — so their inherited theme bindings yield.
pub(crate) fn merge_font_bindings(
    base: Option<Box<RunFontBindings>>,
    patch: Option<Box<RunFontBindings>>,
    patch_names_family: bool,
) -> Option<Box<RunFontBindings>> {
    match (base, patch) {
        (base, Some(p)) => {
            let b = base.map(|b| *b).unwrap_or_default();
            let merged = RunFontBindings {
                ascii: p.ascii.or(b.ascii),
                h_ansi: p.h_ansi.or(b.h_ansi),
                east_asia: p.east_asia.or(b.east_asia),
                cs: p.cs.or(b.cs),
            };
            (!merged.is_empty()).then(|| Box::new(merged))
        }
        (Some(mut b), None) if patch_names_family => {
            for slot in [&mut b.ascii, &mut b.h_ansi, &mut b.cs] {
                if matches!(slot, Some(FontBinding::Theme(_))) {
                    *slot = Some(FontBinding::Name);
                }
            }
            Some(b)
        }
        (base, None) => base,
    }
}

/// The text class a family is resolved for — which `<w:rFonts>` slot
/// applies (§17.3.2.26).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontClass {
    /// ASCII text (U+0000–U+007F): `ascii`, then `hAnsi` when the
    /// cascade never binds `ascii`.
    Latin,
    /// Every other non-complex, non-East-Asian character (accented
    /// Latin, Greek, Cyrillic, symbol-font code points): `hAnsi`, then
    /// `ascii`. A run naming a symbol face only in `w:hAnsi` over
    /// theme-bound docDefaults shows its private-use symbols in that face
    /// while its ASCII stays on the theme font.
    HighAnsi,
    /// `eastAsia`.
    EastAsian,
    /// `cs`.
    ComplexScript,
}

/// A resolved run family and where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedFont {
    pub family: crate::FontFamily,
    /// `true` when a theme binding produced it — before issue #355 the
    /// run had no family for this class and fell to the font stack.
    pub from_theme: bool,
}

impl FontClass {
    /// The Latin-slot class for a run of `text`: [`FontClass::HighAnsi`]
    /// when it holds any non-ASCII character outside the complex scripts
    /// (Arabic, which the complex-script slot serves), else
    /// [`FontClass::Latin`]. Layout segments by script, not by the ASCII
    /// boundary, so a run mixing both takes one slot — they only differ
    /// when a level binds `ascii` and `hAnsi` apart.
    pub fn latin_for(text: &str) -> FontClass {
        let high = text.chars().any(|c| {
            !c.is_ascii()
                && !matches!(c, '\u{0590}'..='\u{08FF}' | '\u{FB1D}'..='\u{FDFF}' | '\u{FE70}'..='\u{FEFF}')
        });
        if high {
            FontClass::HighAnsi
        } else {
            FontClass::Latin
        }
    }
}

impl crate::SpanStyle {
    /// Issue #355 — the family this (cascade-merged) style lays out text of
    /// `class` in: the slot's theme binding resolved through `theme` (with
    /// `script_hint` choosing the supplemental script entry, see
    /// [`DocumentTheme::theme_font`]) — else the explicit family — else
    /// `None`, which leaves the choice to the font stack's per-script
    /// fallback. A theme reference that does not resolve (no theme part,
    /// an empty typeface, an unknown value) falls through to the explicit
    /// name, which is how a producer that writes both caches the
    /// resolved face.
    ///
    /// The explicit family is the reader's single name slot (`w:ascii`,
    /// else `w:hAnsi`, else `w:cs`) for every class until the per-slot
    /// names land (issue #249).
    pub fn resolve_font(
        &self,
        theme: Option<&DocumentTheme>,
        class: FontClass,
        script_hint: Option<&str>,
    ) -> Option<ResolvedFont> {
        let binding = self.font_bindings.as_deref().and_then(|b| match class {
            FontClass::Latin => b.ascii.as_ref().or(b.h_ansi.as_ref()),
            FontClass::HighAnsi => b.h_ansi.as_ref().or(b.ascii.as_ref()),
            FontClass::EastAsian => b.east_asia.as_ref(),
            FontClass::ComplexScript => b.cs.as_ref(),
        });
        if let Some(FontBinding::Theme(value)) = binding
            && let Some(r) = ThemeFontRef::parse(value)
            && let Some(name) = theme.and_then(|t| t.theme_font(r, script_hint))
            && let Some(family) = crate::FontFamily::from_display_name(name)
        {
            return Some(ResolvedFont {
                family,
                from_theme: true,
            });
        }
        self.font_family
            .clone()
            .or_else(|| {
                self.raw_font_family
                    .as_deref()
                    .and_then(crate::FontFamily::from_display_name)
            })
            .map(|family| ResolvedFont {
                family,
                from_theme: false,
            })
    }
}

/* ============================================================
Theme colours for text (§17.3.2.6 `w:color`, §17.18.97 `ST_ThemeColor`).
============================================================ */

/// Issue #355 — the theme half of a run's `<w:color>`: `w:themeColor`
/// plus `w:themeTint` / `w:themeShade`, verbatim (the writer re-emits the
/// attributes as read). Per §17.3.2.6 the theme colour supersedes the
/// `w:val` RGB, which producers cache as the resolved value.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Hash, Default)]
#[serde(default)]
pub struct ThemeColorRef {
    /// `ST_ThemeColor`: `dark1` … `followedHyperlink`, `text1` /
    /// `background1` / `text2` / `background2`, or `none`.
    pub color: String,
    /// `w:themeTint` — hex byte (`"99"`).
    pub tint: Option<String>,
    /// `w:themeShade` — hex byte (`"BF"`).
    pub shade: Option<String>,
}

impl DocumentTheme {
    /// The scheme slot an `ST_ThemeColor` names: the logical colours
    /// (`text1`, `background1`, …, and the accents / hyperlink colours)
    /// through `<w:clrSchemeMapping>` (absent attribute → the identity
    /// mapping Word writes), the scheme-direct ones (`dark1` …) as is.
    fn scheme_slot(&self, color: &str) -> Option<SchemeColor> {
        let mapped = |key: &str, default: &'static str| {
            self.color_map
                .entries
                .get(key)
                .map_or(default, String::as_str)
                .to_string()
        };
        let index = match color {
            "text1" => mapped("t1", "dark1"),
            "background1" => mapped("bg1", "light1"),
            "text2" => mapped("t2", "dark2"),
            "background2" => mapped("bg2", "light2"),
            "accent1" => mapped("accent1", "accent1"),
            "accent2" => mapped("accent2", "accent2"),
            "accent3" => mapped("accent3", "accent3"),
            "accent4" => mapped("accent4", "accent4"),
            "accent5" => mapped("accent5", "accent5"),
            "accent6" => mapped("accent6", "accent6"),
            "hyperlink" => mapped("hyperlink", "hyperlink"),
            "followedHyperlink" => mapped("followedHyperlink", "followedHyperlink"),
            direct => direct.to_string(),
        };
        SchemeColor::from_wml_index(&index)
    }

    /// Issue #355 — the RGBA a `w:themeColor` (+ tint / shade) resolves
    /// to; `None` for `none`, an unknown value or a slot the scheme does
    /// not define (the caller then keeps `w:val`). Shade wins when both
    /// modifiers appear. See [`apply_theme_tint`] / [`apply_theme_shade`]
    /// for the formula.
    pub fn resolve_color(&self, r: &ThemeColorRef) -> Option<[u8; 4]> {
        let rgb = self.colors.get(self.scheme_slot(&r.color)?)?;
        let byte = |v: &Option<String>| {
            v.as_deref()
                .and_then(|s| u8::from_str_radix(s.trim(), 16).ok())
        };
        let [red, green, blue] = match (byte(&r.shade), byte(&r.tint)) {
            (Some(s), _) => apply_theme_shade(rgb, s),
            (None, Some(t)) => apply_theme_tint(rgb, t),
            (None, None) => rgb,
        };
        Some([red, green, blue, 255])
    }
}

/// `w:themeShade` (§17.3.2.6): darken toward black by scaling the HSL
/// lightness — `L' = L · s`, `s = shade / 255` — computed on the sRGB
/// values (not linear light), channels truncated.
///
/// Measured against the `w:val` Word caches next to the attributes in the
/// real-document corpus (`/data/corpus/files`, 147 shaded and 76 tinted
/// runs over 30 documents): every tint exact, 96 / 147 shades exact and
/// every shade within ±1 per channel. The off-by-one cases are all the
/// Office 2007 palette (`4F81BD` / `5B9BD5`), where Word's own caches
/// disagree with each other for the same input (`4F81BD` + `BF` cached
/// as both `365F91` and `376092`). Linear-light scaling misses by up to
/// 42 (shade) / 87 (tint) levels, rounding instead of truncating matches
/// only 34 / 147 shades.
pub fn apply_theme_shade(rgb: [u8; 3], shade: u8) -> [u8; 3] {
    let s = f64::from(shade) / 255.0;
    scale_lightness(rgb, |l| l * s)
}

/// `w:themeTint` (§17.3.2.6): lighten toward white — `L' = L · t + (1 −
/// t)`, `t = tint / 255` — same space and truncation as
/// [`apply_theme_shade`].
pub fn apply_theme_tint(rgb: [u8; 3], tint: u8) -> [u8; 3] {
    let t = f64::from(tint) / 255.0;
    scale_lightness(rgb, |l| l * t + (1.0 - t))
}

fn scale_lightness(rgb: [u8; 3], f: impl Fn(f64) -> f64) -> [u8; 3] {
    let [r, g, b] = rgb.map(|c| f64::from(c) / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    let d = max - min;
    let (h, s) = if d == 0.0 {
        (0.0, 0.0)
    } else {
        let s = if l > 0.5 {
            d / (2.0 - max - min)
        } else {
            d / (max + min)
        };
        let h = if max == r {
            (g - b) / d + if g < b { 6.0 } else { 0.0 }
        } else if max == g {
            (b - r) / d + 2.0
        } else {
            (r - g) / d + 4.0
        };
        (h / 6.0, s)
    };
    let l = f(l).clamp(0.0, 1.0);
    let channel = |t: f64| -> u8 {
        let v = if s == 0.0 {
            l
        } else {
            let q = if l < 0.5 {
                l * (1.0 + s)
            } else {
                l + s - l * s
            };
            let p = 2.0 * l - q;
            let t = t.rem_euclid(1.0);
            if t < 1.0 / 6.0 {
                p + (q - p) * 6.0 * t
            } else if t < 0.5 {
                q
            } else if t < 2.0 / 3.0 {
                p + (q - p) * (2.0 / 3.0 - t) * 6.0
            } else {
                p
            }
        };
        /* Truncate; the epsilon absorbs float noise on exact levels. */
        (v * 255.0 + 1e-9).floor().clamp(0.0, 255.0) as u8
    };
    [channel(h + 1.0 / 3.0), channel(h), channel(h - 1.0 / 3.0)]
}

impl crate::SpanStyle {
    /// Issue #355 — the colour this (cascade-merged) style paints text
    /// in: the theme colour when it resolves through `theme`, else the
    /// `w:val` RGB, else `None` (the caller's default).
    pub fn resolve_color(&self, theme: Option<&DocumentTheme>) -> Option<[u8; 4]> {
        self.color_theme
            .as_deref()
            .zip(theme)
            .and_then(|(r, t)| t.resolve_color(r))
            .or(self.color)
    }
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

    #[test]
    fn st_theme_values_parse() {
        let p = |v| ThemeFontRef::parse(v);
        assert_eq!(
            p("minorHAnsi"),
            Some(ThemeFontRef {
                major: false,
                typeface: ThemeTypeface::Latin
            })
        );
        assert_eq!(
            p("majorAscii").map(|r| (r.major, r.typeface)),
            Some((true, ThemeTypeface::Latin))
        );
        assert_eq!(
            p("minorBidi").map(|r| r.typeface),
            Some(ThemeTypeface::ComplexScript)
        );
        assert_eq!(
            p("majoreastasia").map(|r| r.typeface),
            Some(ThemeTypeface::EastAsian)
        );
        assert_eq!(p("minor"), None);
        assert_eq!(p("bodyLatin"), None);
    }

    #[test]
    fn languages_map_to_theme_script_tags() {
        assert_eq!(script_tag_for_lang("ar-SA"), Some("Arab"));
        assert_eq!(script_tag_for_lang("fa-IR"), Some("Arab"));
        assert_eq!(script_tag_for_lang("he-IL"), Some("Hebr"));
        assert_eq!(script_tag_for_lang("zh-CN"), Some("Hans"));
        assert_eq!(script_tag_for_lang("zh-TW"), Some("Hant"));
        assert_eq!(script_tag_for_lang("ja-JP"), Some("Jpan"));
        assert_eq!(
            script_tag_for_lang("pa-Arab-PK"),
            Some("Arab"),
            "script subtag wins"
        );
        assert_eq!(script_tag_for_lang("en-US"), None);
        assert_eq!(script_tag_for_lang(""), None);
    }

    /// Word's stock layout: empty generic `a:cs`, the face per script.
    fn word_theme() -> DocumentTheme {
        let mut t = DocumentTheme::default();
        t.fonts.minor.latin = "Calibri".into();
        t.fonts.major.latin = "Calibri Light".into();
        for (fonts, arab, hebr) in [
            (&mut t.fonts.minor, "Arial", "Arial"),
            (&mut t.fonts.major, "Times New Roman", "Times New Roman"),
        ] {
            fonts.by_script.insert("Arab".into(), arab.into());
            fonts.by_script.insert("Hebr".into(), hebr.into());
            fonts.by_script.insert("Jpan".into(), "Yu Mincho".into());
        }
        t
    }

    fn r(v: &str) -> ThemeFontRef {
        ThemeFontRef::parse(v).unwrap()
    }

    #[test]
    fn theme_font_picks_the_typeface_and_script_entry() {
        let mut t = word_theme();
        assert_eq!(t.theme_font(r("minorHAnsi"), None), Some("Calibri"));
        assert_eq!(
            t.theme_font(r("majorAscii"), Some("Arab")),
            Some("Calibri Light")
        );
        assert_eq!(t.theme_font(r("minorBidi"), Some("Arab")), Some("Arial"));
        assert_eq!(
            t.theme_font(r("majorBidi"), Some("Arab")),
            Some("Times New Roman")
        );
        assert_eq!(
            t.theme_font(r("minorBidi"), None),
            None,
            "no script, empty a:cs"
        );
        assert_eq!(
            t.theme_font(r("minorEastAsia"), Some("Jpan")),
            Some("Yu Mincho")
        );
        /* `<w:themeFontLang>` selects the entry over the text's script. */
        t.font_lang.bidi = Some("he-IL".into());
        t.fonts
            .minor
            .by_script
            .insert("Hebr".into(), "David".into());
        assert_eq!(t.theme_font(r("minorBidi"), Some("Arab")), Some("David"));
        /* A missing / empty entry falls back to the generic typeface. */
        t.font_lang.bidi = Some("ur-PK".into());
        t.fonts.minor.by_script.insert("Arab".into(), String::new());
        assert_eq!(t.theme_font(r("minorBidi"), Some("Arab")), None);
        t.fonts.minor.cs = "Noto Naskh Arabic".into();
        assert_eq!(
            t.theme_font(r("minorBidi"), Some("Arab")),
            Some("Noto Naskh Arabic")
        );
    }

    fn theme_b(v: &str) -> Option<FontBinding> {
        Some(FontBinding::Theme(v.into()))
    }

    /// docDefaults as Word writes them.
    fn defaults() -> crate::SpanStyle {
        crate::SpanStyle {
            font_theme: Some("minorHAnsi".into()),
            font_bindings: Some(Box::new(RunFontBindings {
                ascii: theme_b("minorHAnsi"),
                h_ansi: theme_b("minorHAnsi"),
                east_asia: theme_b("minorEastAsia"),
                cs: theme_b("minorBidi"),
            })),
            ..Default::default()
        }
    }

    fn family(style: &crate::SpanStyle, t: &DocumentTheme, class: FontClass) -> Option<String> {
        style
            .resolve_font(Some(t), class, Some("Arab"))
            .map(|f| f.family.display_name().to_string())
    }

    #[test]
    fn resolve_font_goes_theme_then_explicit_then_fallback() {
        let t = word_theme();
        let d = defaults();
        let latin = d.resolve_font(Some(&t), FontClass::Latin, None).unwrap();
        assert_eq!(latin.family.display_name(), "Calibri");
        assert_eq!(latin.family.id(), "calibri");
        assert!(latin.from_theme);
        assert_eq!(
            family(&d, &t, FontClass::ComplexScript).as_deref(),
            Some("Arial")
        );
        /* No theme part → nothing to resolve, nothing explicit → fallback. */
        assert_eq!(d.resolve_font(None, FontClass::Latin, None), None);
        /* An explicit family is used when the theme cannot answer. */
        let cached = crate::SpanStyle {
            font_family: crate::FontFamily::from_display_name("Cached Face"),
            ..d.clone()
        };
        let got = cached.resolve_font(None, FontClass::Latin, None).unwrap();
        assert_eq!(
            (got.family.display_name(), got.from_theme),
            ("Cached Face", false)
        );
        /* …but on the same element the theme attribute wins (§17.3.2.26). */
        assert_eq!(
            family(&cached, &t, FontClass::Latin).as_deref(),
            Some("Calibri")
        );
        /* Unknown ST_Theme value → the name. */
        let odd = crate::SpanStyle {
            font_bindings: Some(Box::new(RunFontBindings {
                ascii: theme_b("bodyLatin"),
                ..Default::default()
            })),
            ..cached.clone()
        };
        assert_eq!(
            family(&odd, &t, FontClass::Latin).as_deref(),
            Some("Cached Face")
        );
    }

    /// Word's own behaviour: a run naming a Latin face over theme-bound
    /// docDefaults shows that face for Latin text while Arabic text keeps
    /// the complex-script theme binding; a toolbar family claims every
    /// slot its writer spells (not eastAsia).
    #[test]
    fn a_more_specific_level_claims_the_slots_it_mentions() {
        let t = word_theme();
        let direct = crate::SpanStyle {
            font_family: crate::FontFamily::from_display_name("Arial"),
            font_bindings: Some(Box::new(RunFontBindings {
                ascii: Some(FontBinding::Name),
                h_ansi: Some(FontBinding::Name),
                ..Default::default()
            })),
            ..Default::default()
        };
        let eff = defaults().merged_with(direct);
        assert_eq!(family(&eff, &t, FontClass::Latin).as_deref(), Some("Arial"));
        let cs = eff
            .resolve_font(Some(&t), FontClass::ComplexScript, Some("Arab"))
            .unwrap();
        assert!(cs.from_theme, "cs still bound to minorBidi");
        assert_eq!(cs.family.display_name(), "Arial");

        /* A run-level theme rebinding wins over the defaults. */
        let heading = crate::SpanStyle {
            font_bindings: Some(Box::new(RunFontBindings {
                cs: theme_b("majorBidi"),
                ..Default::default()
            })),
            ..Default::default()
        };
        let eff = defaults().merged_with(heading);
        assert_eq!(
            family(&eff, &t, FontClass::ComplexScript).as_deref(),
            Some("Times New Roman")
        );
        assert_eq!(
            family(&eff, &t, FontClass::Latin).as_deref(),
            Some("Calibri")
        );

        /* The toolbar (no bindings) claims ascii / hAnsi / cs. */
        let toolbar = crate::SpanStyle {
            font_family: Some(crate::FontFamily::Amiri),
            ..Default::default()
        };
        let eff = defaults().merged_with(toolbar);
        assert_eq!(family(&eff, &t, FontClass::Latin).as_deref(), Some("Amiri"));
        assert_eq!(
            family(&eff, &t, FontClass::ComplexScript).as_deref(),
            Some("Amiri")
        );
        let b = eff.font_bindings.as_deref().unwrap();
        assert_eq!(b.ascii, Some(FontBinding::Name));
        assert_eq!(b.cs, Some(FontBinding::Name));
        assert_eq!(b.east_asia, theme_b("minorEastAsia"), "eastAsia untouched");
        assert_eq!(eff.font_theme, None, "the legacy single binding goes too");
        assert_eq!(
            b.theme_attrs().collect::<Vec<_>>(),
            [("w:eastAsiaTheme", "minorEastAsia")]
        );

        /* A patch without a family leaves the bindings alone; an
        engine-authored style never grows bindings. */
        let bold = crate::SpanStyle {
            bold: Some(true),
            ..Default::default()
        };
        assert_eq!(
            defaults().merged_with(bold.clone()).font_bindings,
            defaults().font_bindings
        );
        let plain = crate::SpanStyle {
            font_family: Some(crate::FontFamily::Amiri),
            ..Default::default()
        };
        assert_eq!(plain.clone().merged_with(bold).font_bindings, None);
        assert_eq!(
            crate::SpanStyle::default().merged_with(plain).font_bindings,
            None
        );
    }

    fn hex(v: &str) -> [u8; 3] {
        let d = |i: usize| u8::from_str_radix(&v[i..i + 2], 16).unwrap();
        [d(0), d(2), d(4)]
    }

    /// The `(kind, base, modifier, w:val Word cached)` combinations found
    /// in the real-document corpus (see [`apply_theme_shade`]): exact for
    /// every tint and the shades listed first; within ±1 per channel for
    /// the Office 2007 palette, where Word's own caches disagree.
    #[test]
    fn tint_and_shade_match_the_values_word_caches() {
        let exact = [
            ("shade", "4472C4", 0xBF, "2F5496"),
            ("shade", "156082", 0xBF, "0F4761"),
            ("shade", "1F497D", 0xBF, "17365D"),
            ("shade", "001E4E", 0xBF, "00163A"),
            ("shade", "9BBB59", 0xBF, "76923C"),
            ("shade", "000000", 0xBF, "000000"),
            ("tint", "000000", 0xA6, "595959"),
            ("tint", "000000", 0xD8, "272727"),
            ("tint", "000000", 0x80, "7F7F7F"),
            ("tint", "000000", 0xBF, "404040"),
            ("tint", "1F497D", 0x99, "548DD4"),
            /* Word's "Lighter 80% / 40%" swatches of 4472C4. */
            ("tint", "4472C4", 0x33, "D9E2F3"),
            ("tint", "4472C4", 0x66, "B4C6E7"),
        ];
        let near = [
            ("shade", "4F81BD", 0xBF, "365F91"),
            ("shade", "4F81BD", 0xBF, "376092"),
            ("shade", "4F81BD", 0x7F, "243F60"),
            ("shade", "4F81BD", 0xB5, "345A8A"),
            ("shade", "5B9BD5", 0x7F, "1F4D78"),
            ("shade", "5B9BD5", 0xBF, "2E74B5"),
        ];
        let apply = |kind: &str, base: &str, m: u8| match kind {
            "shade" => apply_theme_shade(hex(base), m),
            _ => apply_theme_tint(hex(base), m),
        };
        for (kind, base, m, want) in exact {
            assert_eq!(apply(kind, base, m), hex(want), "{kind} {base} {m:02X}");
        }
        for (kind, base, m, want) in near {
            let got = apply(kind, base, m);
            let dev = (0..3)
                .map(|i| (i16::from(got[i]) - i16::from(hex(want)[i])).abs())
                .max()
                .unwrap();
            assert!(dev <= 1, "{kind} {base} {m:02X}: {got:02X?} vs {want}");
        }
        assert_eq!(
            apply_theme_tint([0x12, 0x34, 0x56], 0xFF),
            [0x12, 0x34, 0x56]
        );
        assert_eq!(apply_theme_shade([0xFF, 0xFF, 0xFF], 0), [0, 0, 0]);
    }

    #[test]
    fn theme_colours_resolve_through_the_mapping() {
        let mut t = DocumentTheme::default();
        t.colors.set(SchemeColor::Dark1, [0, 0, 0]);
        t.colors.set(SchemeColor::Light1, [255, 255, 255]);
        t.colors.set(SchemeColor::Dark2, [0x44, 0x54, 0x6A]);
        t.colors.set(SchemeColor::Accent1, [0x44, 0x72, 0xC4]);
        t.colors.set(SchemeColor::Accent2, [0xED, 0x7D, 0x31]);
        fn resolve(
            t: &DocumentTheme,
            color: &str,
            tint: Option<&str>,
            shade: Option<&str>,
        ) -> Option<[u8; 4]> {
            t.resolve_color(&ThemeColorRef {
                color: color.into(),
                tint: tint.map(Into::into),
                shade: shade.map(Into::into),
            })
        }
        assert_eq!(resolve(&t, "text1", None, None), Some([0, 0, 0, 255]));
        assert_eq!(
            resolve(&t, "background1", None, None),
            Some([255, 255, 255, 255])
        );
        assert_eq!(
            resolve(&t, "dark2", None, None),
            Some([0x44, 0x54, 0x6A, 255])
        );
        assert_eq!(
            resolve(&t, "accent1", None, Some("BF")),
            Some([0x2F, 0x54, 0x96, 255])
        );
        assert_eq!(
            resolve(&t, "accent1", Some("33"), Some("BF")),
            Some([0x2F, 0x54, 0x96, 255]),
            "shade wins"
        );
        assert_eq!(resolve(&t, "none", None, None), None);
        assert_eq!(
            resolve(&t, "accent3", None, None),
            None,
            "slot not in the scheme"
        );
        assert_eq!(resolve(&t, "bogus", None, None), None);
        /* A remapped document (dark background): text1 → light1. */
        t.color_map.entries.insert("t1".into(), "light1".into());
        t.color_map
            .entries
            .insert("accent1".into(), "accent2".into());
        assert_eq!(resolve(&t, "text1", None, None), Some([255, 255, 255, 255]));
        assert_eq!(
            resolve(&t, "accent1", None, None),
            Some([0xED, 0x7D, 0x31, 255])
        );
        assert_eq!(
            resolve(&t, "dark1", None, None),
            Some([0, 0, 0, 255]),
            "direct"
        );
    }

    /// The theme colour supersedes the cached `w:val`; it travels with
    /// the colour through the cascade, and the colour picker drops it.
    #[test]
    fn span_colour_prefers_the_theme_and_travels_with_the_colour() {
        let mut t = DocumentTheme::default();
        t.colors.set(SchemeColor::Accent1, [0x44, 0x72, 0xC4]);
        let themed = crate::SpanStyle {
            color: Some([1, 2, 3, 255]),
            color_theme: Some(Box::new(ThemeColorRef {
                color: "accent1".into(),
                ..Default::default()
            })),
            ..Default::default()
        };
        assert_eq!(
            themed.resolve_color(Some(&t)),
            Some([0x44, 0x72, 0xC4, 255])
        );
        assert_eq!(
            themed.resolve_color(None),
            Some([1, 2, 3, 255]),
            "no theme: w:val"
        );
        let bold = crate::SpanStyle {
            bold: Some(true),
            ..Default::default()
        };
        assert_eq!(
            themed.clone().merged_with(bold).color_theme,
            themed.color_theme
        );
        let picked = crate::SpanStyle {
            color: Some([200, 0, 0, 255]),
            ..Default::default()
        };
        let eff = themed.merged_with(picked);
        assert_eq!(eff.color_theme, None);
        assert_eq!(eff.resolve_color(Some(&t)), Some([200, 0, 0, 255]));
    }
}
