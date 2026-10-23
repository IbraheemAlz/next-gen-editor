//! Issue #455 — the paragraph decorations stamped onto a laid-out
//! [`ParagraphBox`] AFTER layout: `<w:pBdr>` strokes, `<w:shd>` fill and
//! the tracked paragraph-mark pilcrow colour. They are paint-only, so the
//! paragraph LRU key (`paragraph_layout_key`) leaves them out and the
//! cached box carries none; every consumer that puts a paragraph on a
//! page stamps them from the model through [`stamp_paragraph_decor`] —
//! the body loop, the note / text-box story loop, the header / footer
//! band loop, the table-cell loop (before #455 the cell loop had no copy,
//! so a cell paragraph rendered bare; before #504 neither had the band
//! loop) and the post-pagination field reshape's swap
//! (`transplant_reshaped_paragraph`, issue #504: a PAGE paragraph lost its
//! rule and fill once its page number changed). A new decoration copies
//! here, nowhere else.
//!
//! The content-keyed table cache (issue #379) stores the STAMPED cell
//! paragraph boxes, so its key must see the decorations
//! ([`decor_key`]) and its verified hit must check them
//! ([`decor_matches`]); both read the same three fields as the stamp.

use super::*;
use std::hash::{Hash, Hasher};

/// Copy `para`'s paint-only decorations onto its laid-out box `pb`.
pub(crate) fn stamp_paragraph_decor(pb: &mut ParagraphBox, para: &engine::Paragraph) {
    /* Audit gap A.M4 — `<w:pBdr>` strokes; cloning `CellBorders` is a
    handful of `Option<BorderStroke>` slots. */
    pb.borders = para.props.borders.clone();
    /* Sprint 6 (UI Edition) — `<w:shd>` paragraph shading. */
    pb.shading = para.props.shading;
    /* Issue #262 — the pilcrow of a tracked mark. */
    pb.review_mark = review_mark_color(para);
}

/// A fingerprint of exactly what [`stamp_paragraph_decor`] reads, for a
/// cache key whose cached value carries the stamped box.
pub(crate) fn decor_key(para: &engine::Paragraph) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    format!("{:?}", para.props.borders).hash(&mut h);
    para.props.shading.hash(&mut h);
    review_mark_color(para).hash(&mut h);
    h.finish()
}

/// Whether `pb` carries the decorations [`stamp_paragraph_decor`] would
/// stamp for `para` (the verified-hit check of a cached cell box).
pub(crate) fn decor_matches(pb: &ParagraphBox, para: &engine::Paragraph) -> bool {
    pb.borders == para.props.borders
        && pb.shading == para.props.shading
        && pb.review_mark == review_mark_color(para)
}
