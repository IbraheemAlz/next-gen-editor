//! Issues #335 / #326 — the hyphen a line draws when it breaks inside a
//! word: after a U+00AD SOFT HYPHEN (`<w:softHyphen/>`), or at an
//! automatic hyphenation point.
//!
//! The hyphen is never part of the source text. It is ONE `synthetic`
//! glyph appended at the line's logical end — exactly the Kashida tatweel
//! rule: drawn like any glyph, skipped by caret / hit-test slot emission
//! and by the accessibility mirror (which reads the text, not glyphs);
//! PDF export maps it to the character it draws in `/ToUnicode`.

use crate::boxes::{LineBox, LineHyphen, PositionedGlyph};
use crate::paragraph::ParagraphConfig;
use text_pipeline::{FontStack, LoadedFont, ShapingDirection, segment_by_script_class};

/// The characters a break hyphen may draw, in preference order: U+002D
/// HYPHEN-MINUS (what Word draws), else U+2010 HYPHEN (a face such as
/// Noto Naskh Arabic carries only the latter).
pub const BREAK_HYPHEN_CHARS: [char; 2] = ['-', '\u{2010}'];

/// The glyph a break hyphen draws in `face` at `px_size`: `(glyph id,
/// advance, character)`. `None` when the face has neither character —
/// the line then breaks without a drawn hyphen rather than painting a
/// missing-glyph box.
pub fn hyphen_glyph(face: &LoadedFont, px_size: f32) -> Option<(u16, f32, char)> {
    BREAK_HYPHEN_CHARS.iter().find_map(|&ch| {
        let gid = face.glyph_id(ch).filter(|g| *g != 0)?;
        let adv = face.glyph_metrics(ch, px_size).ok()?.advance_width;
        Some((gid, adv, ch))
    })
}

/// `true` when the text before byte `end` ends with a U+00AD SOFT HYPHEN.
pub fn ends_at_soft_hyphen(text: &str, end: usize) -> bool {
    text.get(..end)
        .is_some_and(|t| t.ends_with(engine::run_content::SOFT_HYPHEN))
}

/// The advance the break hyphen adds to a line ending at byte `end` — the
/// greedy fitter's estimate. Resolves the face exactly as `build_line`
/// shapes the character before the break (span at that byte, its script
/// class, the span's matching face set), so the estimate agrees with the
/// glyph [`append_break_hyphen`] later draws.
pub fn break_hyphen_advance(cfg: &ParagraphConfig<'_>, line_start: usize, end: usize) -> f32 {
    let Some(prev) = cfg.text.get(line_start..end).and_then(|t| t.chars().last()) else {
        return 0.0;
    };
    let prev_at = end - prev.len_utf8();
    let Some(span) = cfg
        .spans
        .iter()
        .find(|s| (prev_at as u32) >= s.start && (prev_at as u32) < s.end)
    else {
        return 0.0;
    };
    /* The script class of the word the break sits in: the last segment of
    the line's text (a Common soft hyphen joins the run before it). */
    let (script, complex) = segment_by_script_class(&cfg.text[line_start..end])
        .last()
        .map(|(_, s, c)| (*s, *c))
        .unwrap_or((text_pipeline::Script::Common, false));
    let sf = span.face_for(complex);
    let Some((_, face, _)) = cfg
        .fonts
        .resolve(script, sf.font_family, sf.bold, sf.italic)
    else {
        return 0.0;
    };
    hyphen_glyph(face, sf.px_size).map_or(0.0, |(_, adv, _)| adv)
}

/// Append the synthetic break hyphen to `line`, whose logical end is byte
/// `end`, and flag the line `kind`. The glyph joins the run holding the
/// line's last character, right after that character in reading order —
/// to its right in an LTR run, to its left in an RTL one — in the run's
/// own face and size. A no-op when no run ends at `end` or the face has
/// no hyphen glyph.
pub fn append_break_hyphen(line: &mut LineBox, fonts: &FontStack, end: usize, kind: LineHyphen) {
    let end = end as u32;
    let Some(run) = line
        .runs
        .iter_mut()
        .filter(|r| r.source_range.end == end && r.source_range.start < end)
        .last()
    else {
        return;
    };
    let Some(face) = fonts.face(&run.font) else {
        return;
    };
    let Some((gid, adv, _)) = hyphen_glyph(face, run.attrs.px_size) else {
        return;
    };
    /* The logically last character's glyphs: the largest cluster. */
    let Some(last_cluster) = run.glyphs.iter().map(|g| g.cluster).max() else {
        return;
    };
    let at = match run.direction {
        ShapingDirection::Ltr => run
            .glyphs
            .iter()
            .rposition(|g| g.cluster == last_cluster)
            .map_or(run.glyphs.len(), |i| i + 1),
        ShapingDirection::Rtl => run
            .glyphs
            .iter()
            .position(|g| g.cluster == last_cluster)
            .unwrap_or(0),
    };
    run.glyphs.insert(
        at,
        PositionedGlyph {
            id: gid,
            cluster: last_cluster,
            x_advance: adv,
            y_advance: 0.0,
            x_offset: 0.0,
            y_offset: 0.0,
            synthetic: true,
            inline_image_rel_id: None,
            inline_footnote_marker: None,
            inline_note_anchor: None,
            inline_object_height: 0.0,
            float: None,
            leader: None,
        },
    );
    line.width = line
        .runs
        .iter()
        .flat_map(|r| &r.glyphs)
        .map(|g| g.x_advance)
        .sum();
    line.hyphen = kind;
}

#[cfg(test)]
#[path = "hyphen_tests.rs"]
mod tests;
