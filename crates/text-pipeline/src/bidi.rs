//! Unicode Bidirectional Algorithm wrapper around the `unicode-bidi` crate.
//!
//! Splits a mixed-direction paragraph into homogeneous runs in **visual
//! order** (left-to-right on screen), each tagged with its resolved
//! direction so the shaper can apply the right `Direction` per run.

use crate::shape::ShapingDirection;
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
