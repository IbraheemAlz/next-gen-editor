//! Issue #329 — metric-compatible font substitution.
//!
//! A `.docx` names the faces it was authored with (Calibri, Cambria, Times
//! New Roman, Simplified Arabic, …). When the editor does not have a named
//! face, [`crate::FontStack`] consults [`SUBSTITUTIONS`] before it falls back
//! to the per-script default: a **metric-compatible** substitute (Carlito
//! for Calibri, Liberation Sans for Arial, …) has the same advance widths,
//! so line breaks and page counts follow the original document instead of
//! drifting with whatever face the script fallback happens to pick.
//!
//! The table is keyed on **(family, script class)**, not the family alone:
//! the Latin metric clones (Liberation, Carlito, Selawik, …) carry no Arabic
//! glyphs, so Arabic text in a run named "Arial" or "Times New Roman" (the
//! stock Office theme's complex-script fonts) needs an Arabic substitute of
//! its own. None of the Arabic rows is metric-compatible — no OFL clone of
//! Simplified Arabic / Traditional Arabic exists — they pick the closest
//! shipped style (Naskh for the simplified faces, the classical Amiri for
//! the traditional ones).
//!
//! Sources: the rows are public metric-compatibility facts — each clone's
//! own documentation states the face it is metric-compatible with
//! (Liberation Sans / Serif / Mono ↔ Arial / Times New Roman / Courier New,
//! Carlito ↔ Calibri, Caladea ↔ Cambria, Gelasio ↔ Georgia, Selawik ↔
//! Segoe UI). All substitutes are SIL OFL 1.1.
//!
//! Matching is by [`family_key`]: case-, quote- and separator-insensitive,
//! so `"Times New Roman"`, `times-new-roman` (the engine's slugged
//! resolution id) and `'TIMES  NEW ROMAN'` are the same family.

use crate::script::Script;

/// The script class a substitution row applies to — which `<w:rFonts>`
/// slot's text the substitute must cover (ECMA-376 Part 1 §17.3.2.26).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScriptClass {
    /// Latin / Greek / Cyrillic / symbols — the `ascii` / `hAnsi` slots.
    Latin,
    /// Arabic — the complex-script (`cs`) slot the Arabic rows serve.
    ComplexScript,
}

impl ScriptClass {
    /// The class a coarse [`Script`] is substituted under: Arabic is the
    /// only complex script the shipped substitutes cover; every other
    /// script (Latin, the script-neutral `Common`, `Other`) takes the
    /// Latin rows.
    pub fn of(script: Script) -> ScriptClass {
        match script {
            Script::Arabic => ScriptClass::ComplexScript,
            _ => ScriptClass::Latin,
        }
    }

    /// A character a substitute must map for this class (the same probes
    /// [`crate::FontStack::from_faces`] classifies faces with): BEH for
    /// Arabic, `A` for Latin.
    pub fn probe(self) -> char {
        match self {
            ScriptClass::ComplexScript => '\u{0628}',
            ScriptClass::Latin => 'A',
        }
    }
}

/// One row of [`SUBSTITUTIONS`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Substitution {
    /// The family a document names (display spelling; matched through
    /// [`family_key`]).
    pub family: &'static str,
    /// The script class whose text the row substitutes.
    pub class: ScriptClass,
    /// Substitute family names in preference order; the first one the
    /// font stack holds (and that covers the class's script) wins.
    pub substitutes: &'static [&'static str],
    /// `true` when the substitutes are metric-compatible with `family`
    /// (same advance widths — line breaks follow the original). `false`
    /// marks a closest-style pick (every Arabic row).
    pub metric_compatible: bool,
    /// The family whose line metrics stand in for the ORIGINAL face's when
    /// the substitute's own differ: Arabic text in a run named "Arial" is
    /// shaped with an Arabic face, but Word lays it out with Arial's line
    /// height — which Liberation Sans (Arial's metric clone) carries. `None`
    /// uses the substitute's own metrics. Ignored when that face is not
    /// loaded.
    pub metrics_from: Option<&'static str>,
}

const NASKH: &[&str] = &["Noto Naskh Arabic", "Amiri"];
const TRADITIONAL: &[&str] = &["Amiri", "Scheherazade New"];

const fn latin(family: &'static str, substitutes: &'static [&'static str]) -> Substitution {
    Substitution {
        family,
        class: ScriptClass::Latin,
        substitutes,
        metric_compatible: true,
        metrics_from: None,
    }
}

const fn arabic(
    family: &'static str,
    substitutes: &'static [&'static str],
    metrics_from: Option<&'static str>,
) -> Substitution {
    Substitution {
        family,
        class: ScriptClass::ComplexScript,
        substitutes,
        metric_compatible: false,
        metrics_from,
    }
}

/// Issue #329 — the substitution table, consulted by
/// [`crate::FontStack::resolve_family`] after an exact / name match and
/// before the per-script fallback. Keyed on (family, script class).
pub static SUBSTITUTIONS: &[Substitution] = &[
    /* Latin — metric-compatible clones. */
    latin("Calibri", &["Carlito"]),
    /* Word's stock heading face ("+Headings" in the Office 2013–2022
    theme). Carlito ships no Light weight; the regular face keeps the
    Calibri family's widths closer than any script fallback. */
    Substitution {
        metric_compatible: false,
        ..latin("Calibri Light", &["Carlito"])
    },
    latin("Cambria", &["Caladea"]),
    latin("Arial", &["Liberation Sans"]),
    latin("Helvetica", &["Liberation Sans"]),
    latin("Arimo", &["Liberation Sans"]),
    latin("Times New Roman", &["Liberation Serif"]),
    latin("Times", &["Liberation Serif"]),
    latin("Tinos", &["Liberation Serif"]),
    latin("Courier New", &["Liberation Mono"]),
    latin("Courier", &["Liberation Mono"]),
    latin("Cousine", &["Liberation Mono"]),
    latin("Georgia", &["Gelasio"]),
    latin("Segoe UI", &["Selawik"]),
    /* Arabic faces Word documents name (none has an OFL metric clone). */
    arabic("Simplified Arabic", NASKH, None),
    arabic("Sakkal Majalla", NASKH, None),
    arabic("Traditional Arabic", TRADITIONAL, None),
    arabic("Arabic Typesetting", TRADITIONAL, None),
    /* Latin-named faces in Arabic runs: Office's theme complex-script
    fonts (Arial body, Times New Roman headings) and the UI faces. The
    Latin clones above carry no Arabic glyphs; the line metrics come from
    the original's Latin clone, which shares its vertical metrics. */
    arabic("Arial", NASKH, Some("Liberation Sans")),
    arabic("Times New Roman", NASKH, Some("Liberation Serif")),
    arabic("Tahoma", NASKH, None),
    arabic("Segoe UI", NASKH, Some("Selawik")),
    arabic("Calibri", NASKH, Some("Carlito")),
    arabic("Cambria", NASKH, Some("Caladea")),
    arabic("Courier New", NASKH, Some("Liberation Mono")),
];

/// Normalize a font family name (or a slugged resolution id) to its
/// matching key: trimmed, surrounding quotes dropped, ASCII-lowercased,
/// whitespace / `-` / `_` runs collapsed to one `-`. `"Times New Roman"`,
/// `"times-new-roman"` and `" 'TIMES_NEW  ROMAN' "` all key as
/// `times-new-roman`. Empty for a blank name.
pub fn family_key(name: &str) -> String {
    let trimmed = name.trim().trim_matches(['"', '\'']).trim();
    trimmed
        .split(|c: char| c.is_whitespace() || c == '-' || c == '_')
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>()
        .join("-")
}

/// The [`SUBSTITUTIONS`] row for `family` (any spelling — see
/// [`family_key`]) in `class`, if the table has one.
pub fn substitution_for(family: &str, class: ScriptClass) -> Option<&'static Substitution> {
    let key = family_key(family);
    if key.is_empty() {
        return None;
    }
    SUBSTITUTIONS
        .iter()
        .find(|row| row.class == class && family_key(row.family) == key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn family_key_folds_case_quotes_and_separators() {
        assert_eq!(family_key("Times New Roman"), "times-new-roman");
        assert_eq!(family_key("times-new-roman"), "times-new-roman");
        assert_eq!(family_key(" 'TIMES_NEW  ROMAN' "), "times-new-roman");
        assert_eq!(family_key("\"Calibri\""), "calibri");
        assert_eq!(family_key("   "), "");
    }

    #[test]
    fn rows_key_on_family_and_script_class() {
        let arial = substitution_for("Arial", ScriptClass::Latin).expect("Arial latin");
        assert_eq!(arial.substitutes, &["Liberation Sans"]);
        assert!(arial.metric_compatible);
        let arial_cs =
            substitution_for("arial", ScriptClass::ComplexScript).expect("Arial complex script");
        assert_eq!(arial_cs.substitutes, NASKH);
        assert_eq!(arial_cs.metrics_from, Some("Liberation Sans"));
        assert!(!arial_cs.metric_compatible);
        /* An Arabic face has no Latin row: its Latin text keeps the
        script fallback. */
        assert!(substitution_for("Simplified Arabic", ScriptClass::Latin).is_none());
        assert_eq!(
            substitution_for("simplified-arabic", ScriptClass::ComplexScript)
                .map(|r| r.substitutes),
            Some(NASKH)
        );
        assert_eq!(
            substitution_for("Traditional Arabic", ScriptClass::ComplexScript)
                .map(|r| r.substitutes),
            Some(TRADITIONAL)
        );
        assert!(substitution_for("Comic Sans MS", ScriptClass::Latin).is_none());
        assert!(substitution_for("", ScriptClass::Latin).is_none());
    }

    #[test]
    fn every_row_is_unique_per_class() {
        for (i, a) in SUBSTITUTIONS.iter().enumerate() {
            for b in &SUBSTITUTIONS[i + 1..] {
                assert!(
                    !(a.class == b.class && family_key(a.family) == family_key(b.family)),
                    "duplicate row {} / {:?}",
                    a.family,
                    a.class
                );
            }
            assert!(!a.substitutes.is_empty(), "{} has no substitute", a.family);
        }
    }
}
