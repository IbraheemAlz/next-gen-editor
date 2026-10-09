//! Unicode script detection + script-run segmentation (PHASE_3_RENDER_RTL.md
//! §13.A).
//!
//! Splits text into runs of a single script so the layout engine can resolve a
//! covering font per run. Script comes from the Unicode `Script` property via
//! `icu_properties`; font resolution only needs a coarse projection, so
//! `Common`/`Inherited` collapse to [`Script::Common`] and everything outside
//! Arabic/Latin to [`Script::Other`].

use icu_properties::CodePointMapData;
use icu_properties::props::Script as UnicodeScript;
use std::ops::Range;

/// Coarse script classification — the granularity font resolution needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Script {
    Arabic,
    Latin,
    /// Script-neutral — spaces, punctuation, digits, combining marks. Absorbed
    /// into an adjacent real-script run during segmentation.
    Common,
    /// Any script the font stack is not specialized for.
    Other,
}

/// Classify a character's script via its Unicode `Script` property.
pub fn script_of(c: char) -> Script {
    let s = CodePointMapData::<UnicodeScript>::new().get(c);
    if s == UnicodeScript::Arabic {
        Script::Arabic
    } else if s == UnicodeScript::Latin {
        Script::Latin
    } else if s == UnicodeScript::Common || s == UnicodeScript::Inherited {
        Script::Common
    } else {
        Script::Other
    }
}

/// Issues #359 / #104 / #249 — `true` when `c` belongs to one of Word's
/// **complex-script** ranges, i.e. the characters OOXML formats with the
/// complex-script twins of the run properties (`<w:szCs>`, `<w:bCs>`,
/// `<w:iCs>`, `<w:rFonts w:cs>`) instead of the Latin ones (ECMA-376
/// Part 1 §17.3.2.26: the right-to-left scripts plus the Brahmic /
/// South-East Asian scripts Word routes to its complex-script font slot).
/// Script-neutral characters (`Common` / `Inherited`) are never complex on
/// their own — segmentation absorbs them into the neighbouring run.
pub fn is_complex_script(c: char) -> bool {
    let s = CodePointMapData::<UnicodeScript>::new().get(c);
    matches!(
        s,
        UnicodeScript::Arabic
            | UnicodeScript::Hebrew
            | UnicodeScript::Syriac
            | UnicodeScript::Thaana
            | UnicodeScript::Nko
            | UnicodeScript::Samaritan
            | UnicodeScript::Mandaic
            | UnicodeScript::Thai
            | UnicodeScript::Lao
            | UnicodeScript::Khmer
            | UnicodeScript::Devanagari
            | UnicodeScript::Bengali
            | UnicodeScript::Gurmukhi
            | UnicodeScript::Gujarati
            | UnicodeScript::Oriya
            | UnicodeScript::Tamil
            | UnicodeScript::Telugu
            | UnicodeScript::Kannada
            | UnicodeScript::Malayalam
            | UnicodeScript::Sinhala
    )
}

/// Issues #359 / #104 / #249 — [`segment_by_script`] refined by the
/// complex-script class: maximal runs of one coarse [`Script`] AND one
/// [`is_complex_script`] class, in logical order, each tagged with its
/// class. `Script::Other` covers both classes (Hebrew and Han are both
/// "other" to the font stack), so a Hebrew word next to a Greek one is
/// cut here. Script-neutral characters absorb exactly as in
/// [`segment_by_script`]; an all-neutral text is one non-complex run.
pub fn segment_by_script_class(text: &str) -> Vec<(Range<usize>, Script, bool)> {
    let mut runs: Vec<(Range<usize>, Script, bool)> = Vec::new();
    let mut run_start = 0_usize;
    let mut run_key: Option<(Script, bool)> = None;
    for (i, c) in text.char_indices() {
        let s = script_of(c);
        if s == Script::Common {
            continue;
        }
        let key = (s, is_complex_script(c));
        match run_key {
            None => run_key = Some(key),
            Some(cur) if cur != key => {
                runs.push((run_start..i, cur.0, cur.1));
                run_start = i;
                run_key = Some(key);
            }
            Some(_) => {}
        }
    }
    if run_start < text.len() {
        let (s, cs) = run_key.unwrap_or((Script::Common, false));
        runs.push((run_start..text.len(), s, cs));
    }
    runs
}

/// Segment `text` into maximal single-script runs, in logical order.
///
/// `Common`/`Inherited` characters never open a run of their own: they absorb
/// into the run in progress, or — at the start of the text — into the first
/// real-script run that follows. Text made only of common characters yields a
/// single [`Script::Common`] run.
pub fn segment_by_script(text: &str) -> Vec<(Range<usize>, Script)> {
    let mut runs: Vec<(Range<usize>, Script)> = Vec::new();
    let mut run_start = 0_usize;
    let mut run_script: Option<Script> = None;
    for (i, c) in text.char_indices() {
        let s = script_of(c);
        if s == Script::Common {
            continue;
        }
        match run_script {
            None => run_script = Some(s),
            Some(cur) if cur != s => {
                runs.push((run_start..i, cur));
                run_start = i;
                run_script = Some(s);
            }
            Some(_) => {}
        }
    }
    if run_start < text.len() {
        runs.push((run_start..text.len(), run_script.unwrap_or(Script::Common)));
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_scripts() {
        assert_eq!(script_of('A'), Script::Latin);
        assert_eq!(script_of('\u{0627}'), Script::Arabic); // alef
        assert_eq!(script_of(' '), Script::Common);
        assert_eq!(script_of('.'), Script::Common);
    }

    #[test]
    fn splits_at_script_change() {
        // "ab" + Arabic jeem (2 bytes)
        let runs = segment_by_script("ab\u{062C}");
        assert_eq!(runs, vec![(0..2, Script::Latin), (2..4, Script::Arabic)]);
    }

    #[test]
    fn common_absorbs_into_preceding_run() {
        // The space rides with the Latin run; the Arabic run starts at jeem.
        let runs = segment_by_script("a \u{062C}");
        assert_eq!(runs, vec![(0..2, Script::Latin), (2..4, Script::Arabic)]);
    }

    #[test]
    fn leading_common_absorbs_into_following_run() {
        let runs = segment_by_script(" A");
        assert_eq!(runs, vec![(0..2, Script::Latin)]);
    }

    #[test]
    fn all_common_is_one_run() {
        let runs = segment_by_script("  .");
        assert_eq!(runs, vec![(0..3, Script::Common)]);
    }

    #[test]
    fn complex_script_class_covers_rtl_and_brahmic_scripts() {
        for c in [
            '\u{0627}', '\u{05D0}', '\u{0710}', '\u{0E01}', '\u{0915}', '\u{FEFB}',
        ] {
            assert!(is_complex_script(c), "{c:?} is complex script");
        }
        for c in [
            'A', '\u{00E9}', '\u{4E2D}', '\u{0391}', ' ', '1', '\u{0640}',
        ] {
            assert!(!is_complex_script(c), "{c:?} is not complex script");
        }
    }

    #[test]
    fn class_segmentation_matches_script_segmentation_for_latin_and_arabic() {
        let text = "Hello \u{0623}\u{0647}\u{0644}\u{0627} world 12";
        let coarse = segment_by_script(text);
        let classed = segment_by_script_class(text);
        assert_eq!(coarse.len(), classed.len());
        for ((r1, s1), (r2, s2, cs)) in coarse.iter().zip(&classed) {
            assert_eq!((r1, s1), (r2, s2));
            assert_eq!(*cs, *s1 == Script::Arabic);
        }
    }

    #[test]
    fn class_segmentation_cuts_other_scripts_by_class() {
        // Hebrew alef + Greek alpha: one coarse `Other` run, two classes.
        let text = "\u{05D0}\u{0391}";
        assert_eq!(segment_by_script(text), vec![(0..4, Script::Other)]);
        assert_eq!(
            segment_by_script_class(text),
            vec![(0..2, Script::Other, true), (2..4, Script::Other, false)]
        );
        assert_eq!(
            segment_by_script_class("  "),
            vec![(0..2, Script::Common, false)]
        );
        assert!(segment_by_script_class("").is_empty());
    }
}
