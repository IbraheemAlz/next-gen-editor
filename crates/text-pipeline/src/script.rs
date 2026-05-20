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
}
