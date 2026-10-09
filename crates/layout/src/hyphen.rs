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
use std::ops::Range;
use text_pipeline::{
    FontStack, Hyphenator, LoadedFont, ShapingDirection, is_complex_script, segment_by_script_class,
};

/// Issue #326 — automatic hyphenation for one paragraph
/// (`<w:autoHyphenation/>` on, `<w:suppressAutoHyphens/>` off).
///
/// The composer consults it only when a word overflows the line: hyphenation
/// points never join the normal break opportunities (their width sums would
/// cut through kerning), they are offered for the overflowing word alone.
#[derive(Debug, Clone)]
pub struct AutoHyphenation<'a> {
    /// Byte ranges of the paragraph text in a language with patterns
    /// (sorted, disjoint), each with its hyphenator. Text outside every
    /// range is never hyphenated.
    pub ranges: &'a [(Range<u32>, &'static Hyphenator)],
    /// `<w:hyphenationZone>` in layout px: a word is hyphenated only when
    /// moving it to the next line would leave a wider gap than this.
    pub zone_px: f32,
    /// `<w:consecutiveHyphenLimit>`: the most consecutive hyphenated lines;
    /// `0` = no limit.
    pub consecutive_limit: u32,
    /// `<w:doNotHyphenateCaps/>`: words in all capitals stay whole.
    pub no_caps: bool,
}

/// Issue #326 — where to hyphenate the word overflowing the line
/// `[line_start, …)` at the segment `[seg_from, seg_end)` (the text after
/// the last break opportunity that fit, `line_width` wide so far), or
/// `None` to leave it to the ordinary greedy break.
///
/// Rules, in order: the consecutive-hyphen limit (every hyphenated line
/// counts, author soft hyphens included); the segment's first word must be
/// letters only (leading / trailing punctuation allowed, never an inner
/// apostrophe, digit or hyphen), not the paragraph's last word, not complex
/// script (Arabic never hyphenates), not all capitals under
/// `doNotHyphenateCaps`, and inside one hyphenation range; the gap the
/// word would leave if it moved down must exceed the hyphenation zone;
/// then the LATEST pattern point whose prefix + the drawn hyphen still
/// fits wins.
pub(crate) fn auto_hyphen_point(
    cfg: &ParagraphConfig<'_>,
    hy: &AutoHyphenation<'_>,
    previous: &[(LineBox, bool)],
    line_start: usize,
    seg_from: usize,
    seg_end: usize,
    line_width: f32,
) -> Option<usize> {
    if hy.consecutive_limit > 0 {
        let run = previous
            .iter()
            .rev()
            .take_while(|(l, _)| !l.hyphen.is_none())
            .count();
        if run >= hy.consecutive_limit as usize {
            return None;
        }
    }
    if cfg.max_width - line_width <= hy.zone_px {
        return None;
    }
    let seg = cfg.text.get(seg_from..seg_end)?;
    let lead = seg.find(|c: char| c.is_alphabetic())?;
    let word_start = seg_from + lead;
    let word_len = cfg.text[word_start..seg_end]
        .find(|c: char| !c.is_alphabetic())
        .unwrap_or(seg_end - word_start);
    let word_end = word_start + word_len;
    let word = &cfg.text[word_start..word_end];
    /* Only trailing punctuation / spaces after the word inside its
    segment: an apostrophe, digit or hyphen joins a compound. */
    let tail = &cfg.text[word_end..seg_end];
    let tail_word = tail.split(char::is_whitespace).next().unwrap_or("");
    if tail_word
        .chars()
        .any(|c| c.is_alphanumeric() || c == '\'' || c == '\u{2019}')
    {
        return None;
    }
    /* Never the paragraph's last word. */
    if !cfg.text[word_end..].chars().any(char::is_alphanumeric) {
        return None;
    }
    if word.chars().any(is_complex_script) {
        return None;
    }
    if hy.no_caps && !word.chars().any(char::is_lowercase) {
        return None;
    }
    let (_, hyphenator) = hy
        .ranges
        .iter()
        .find(|(r, _)| r.start as usize <= word_start && word_end <= r.end as usize)?;
    let char_starts: Vec<usize> = word.char_indices().map(|(i, _)| i).collect();
    for &ci in hyphenator.hyphenate(word).iter().rev() {
        let at = word_start + *char_starts.get(ci)?;
        let prefix = crate::paragraph::measure_text(
            cfg.fonts,
            &cfg.text[seg_from..at],
            seg_from as u32,
            cfg.spans,
            cfg.base_direction,
            cfg.inline_objects,
        );
        let hyphen = break_hyphen_advance(cfg, line_start, at);
        if line_width + prefix + hyphen <= cfg.max_width {
            return Some(at);
        }
    }
    None
}

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
