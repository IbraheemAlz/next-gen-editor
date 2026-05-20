//! Arabic Kashida (cursive letter-elongation) justification — candidate
//! detection and the Microsoft Arabic-typography priority bands.
//!
//! Replaces the Phase 1 Unicode-block heuristic (`is_arabic_codepoint` used as
//! a candidate filter) with the Unicode `Joining_Type` property looked up via
//! `icu_properties`. See PHASE_3_RENDER_RTL.md §6.
//!
//! This module is the pure-Unicode half of the algorithm: it classifies
//! characters and scores elongation sites. Applying the result to glyph
//! advances stays in the `layout` crate — `LineBox` cannot be named here
//! without a `layout` → `text-pipeline` → `layout` dependency cycle.

use icu_properties::CodePointMapData;
use icu_properties::props::JoiningType;

/// Microsoft Arabic-typography Kashida priority band (PHASE_3_RENDER_RTL.md §6).
///
/// `P1` is the most preferred elongation site, `P5` the least. Ordering is
/// load-bearing: the justifier stretches only the highest band present on a
/// line, so `P1 < P2 < … < P5` must hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum KashidaPriority {
    /// After seen / sheen (س ش).
    P1,
    /// After sad / dad (ص ض).
    P2,
    /// After taa / thaa (ط ث).
    P3,
    /// After lam (ل).
    P4,
    /// Any other valid cursive-joining boundary.
    P5,
}

/// Coarse cursive-joining role of a character. Lets a caller walk a glyph run
/// while skipping combining marks and breaking the cursive chain on spaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinRole {
    /// A joining letter — `Joining_Type` Dual, Right, Left, or Join_Causing.
    Letter,
    /// A combining mark — `Joining_Type` Transparent; invisible to joining.
    Transparent,
    /// Breaks the cursive chain — space, punctuation, non-cursive script.
    NonJoining,
}

/// Classify a character's cursive-joining role from its `Joining_Type`.
pub fn join_role(c: char) -> JoinRole {
    let jt = CodePointMapData::<JoiningType>::new().get(c);
    if jt == JoiningType::Transparent {
        JoinRole::Transparent
    } else if jt == JoiningType::NonJoining {
        JoinRole::NonJoining
    } else {
        JoinRole::Letter
    }
}

/// Score the cursive stroke that runs *after* `after` (joining it to the
/// following letter `next`) as a Kashida elongation site.
///
/// `after` and `next` are in logical (reading) order. The site is valid only
/// when the two letters genuinely connect: `after` must carry a left-joining
/// form and `next` a right-joining form. Returns the priority band, or `None`
/// when the pair does not connect — which is how isolated or one-sided letters
/// (e.g. ا د ر) are kept out of the candidate set.
pub fn kashida_point(after: char, next: char) -> Option<KashidaPriority> {
    let data = CodePointMapData::<JoiningType>::new();
    let connects = connects_left(data.get(after)) && connects_right(data.get(next));
    connects.then_some(priority_of(after))
}

/// A letter extends a stroke on its left when it has an initial/medial form —
/// `Joining_Type` Dual (or Left, or the Join_Causing tatweel). Right-joining
/// letters such as ا د ر have no left connector and cannot be elongated after.
fn connects_left(jt: JoiningType) -> bool {
    jt == JoiningType::DualJoining
        || jt == JoiningType::LeftJoining
        || jt == JoiningType::JoinCausing
}

/// A letter accepts a stroke on its right when it has a medial/final form —
/// `Joining_Type` Dual or Right (or the Join_Causing tatweel).
fn connects_right(jt: JoiningType) -> bool {
    jt == JoiningType::DualJoining
        || jt == JoiningType::RightJoining
        || jt == JoiningType::JoinCausing
}

/// Microsoft priority band for the stroke that follows `after` (§6). Bands key
/// off the base Arabic letter; presentation forms never reach here because the
/// classifier runs on source characters, not shaped glyphs.
fn priority_of(after: char) -> KashidaPriority {
    match after {
        '\u{0633}' | '\u{0634}' => KashidaPriority::P1, // seen, sheen
        '\u{0635}' | '\u{0636}' => KashidaPriority::P2, // sad, dad
        '\u{0637}' | '\u{062B}' => KashidaPriority::P3, // taa, thaa
        '\u{0644}' => KashidaPriority::P4,              // lam
        _ => KashidaPriority::P5,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_roles() {
        assert_eq!(join_role('\u{0628}'), JoinRole::Letter); // beh
        assert_eq!(join_role(' '), JoinRole::NonJoining);
        assert_eq!(join_role('\u{064E}'), JoinRole::Transparent); // fatha
    }

    #[test]
    fn kashida_after_seen_is_p1() {
        // seen has a left form, beh has a right form → valid, top band.
        assert_eq!(
            kashida_point('\u{0633}', '\u{0628}'),
            Some(KashidaPriority::P1)
        );
    }

    #[test]
    fn no_kashida_after_right_joining_alef() {
        // alef has no left-joining form; nothing elongates after it.
        assert_eq!(kashida_point('\u{0627}', '\u{0628}'), None);
    }

    #[test]
    fn kashida_into_alef_is_valid() {
        // beh has a left form, alef has a right form → valid, default band.
        assert_eq!(
            kashida_point('\u{0628}', '\u{0627}'),
            Some(KashidaPriority::P5)
        );
    }

    #[test]
    fn priority_bands_rank() {
        assert_eq!(
            kashida_point('\u{0635}', '\u{0628}'), // sad
            Some(KashidaPriority::P2)
        );
        assert_eq!(
            kashida_point('\u{0644}', '\u{0645}'), // lam → meem
            Some(KashidaPriority::P4)
        );
        assert!(KashidaPriority::P1 < KashidaPriority::P5);
    }
}
