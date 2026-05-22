//! Unicode Bidirectional Algorithm wrapper around the `unicode-bidi` crate.
//!
//! Splits a mixed-direction paragraph into homogeneous runs in **visual
//! order** (left-to-right on screen), each tagged with its resolved
//! direction so the shaper can apply the right `Direction` per run.

use crate::shape::ShapingDirection;
use icu_properties::CodePointMapData;
use icu_properties::props::BidiClass;
use std::ops::Range;
use unicode_bidi::{BidiInfo, Level};

#[derive(Debug, Clone)]
pub struct VisualRun {
    pub range: Range<usize>,
    pub level: u8,
    pub direction: ShapingDirection,
}

#[derive(Debug, Clone)]
pub struct BidiAnalysis {
    pub paragraph_direction: ShapingDirection,
    pub visual_runs: Vec<VisualRun>,
}

/// Run the Unicode Bidi Algorithm on `text` with the given base direction.
pub fn analyze_bidi(text: &str, base: ShapingDirection) -> BidiAnalysis {
    let base_level = match base {
        ShapingDirection::Ltr => Level::ltr(),
        ShapingDirection::Rtl => Level::rtl(),
    };
    let info = BidiInfo::new(text, Some(base_level));
    if info.paragraphs.is_empty() {
        return BidiAnalysis {
            paragraph_direction: base,
            visual_runs: vec![],
        };
    }
    let para = &info.paragraphs[0];
    let (levels, runs) = info.visual_runs(para, para.range.clone());

    let visual_runs: Vec<VisualRun> = runs
        .into_iter()
        .map(|r| {
            let level = levels[r.start];
            let direction = if level.is_rtl() {
                ShapingDirection::Rtl
            } else {
                ShapingDirection::Ltr
            };
            VisualRun {
                range: r,
                level: level.number(),
                direction,
            }
        })
        .collect();

    BidiAnalysis {
        paragraph_direction: if para.level.is_rtl() {
            ShapingDirection::Rtl
        } else {
            ShapingDirection::Ltr
        },
        visual_runs,
    }
}

/// Detect a paragraph's base direction from its first strong character
/// (UAX #9 rules P2/P3). The first character of BiDi class `L` resolves to
/// LTR; the first of class `R` or `AL` (Arabic letter) resolves to RTL.
/// Returns `None` when the text holds no strong character — digits, symbols
/// or whitespace only — so the caller can fall back to the document direction
/// (Backlog #6).
pub fn first_strong_direction(text: &str) -> Option<ShapingDirection> {
    let bidi_class = CodePointMapData::<BidiClass>::new();
    for c in text.chars() {
        let class = bidi_class.get(c);
        if class == BidiClass::LeftToRight {
            return Some(ShapingDirection::Ltr);
        }
        if class == BidiClass::RightToLeft || class == BidiClass::ArabicLetter {
            return Some(ShapingDirection::Rtl);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_strong_latin_is_ltr() {
        assert_eq!(
            first_strong_direction("Hello world"),
            Some(ShapingDirection::Ltr),
        );
    }

    #[test]
    fn first_strong_arabic_is_rtl() {
        assert_eq!(first_strong_direction("مرحبا"), Some(ShapingDirection::Rtl));
    }

    #[test]
    fn first_strong_hebrew_is_rtl() {
        /* Hebrew aleph + bet — BiDi class R. */
        assert_eq!(
            first_strong_direction("\u{05D0}\u{05D1}"),
            Some(ShapingDirection::Rtl),
        );
    }

    #[test]
    fn leading_neutrals_are_skipped() {
        /* Digits, dash and space are neutral — the first strong letter wins. */
        assert_eq!(
            first_strong_direction("123 — A"),
            Some(ShapingDirection::Ltr),
        );
        assert_eq!(
            first_strong_direction("123 — ع"),
            Some(ShapingDirection::Rtl),
        );
    }

    #[test]
    fn no_strong_character_is_none() {
        assert_eq!(first_strong_direction("123 .,!"), None);
        assert_eq!(first_strong_direction(""), None);
    }
}
