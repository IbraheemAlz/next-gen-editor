//! Paragraph layout: greedy logical-order line break, per-line BiDi reorder,
//! per-line shaping, optional justify.
//!
//! Earlier draft flattened paragraph-wide visual_runs and packed them by
//! advance width — that mixes content from non-adjacent logical positions
//! onto the same line. UAX #9 requires BiDi to be applied **per line**
//! after line break positions are known. This implementation does that.
//!
//! Cost: O(N) calls to `shape_text` where N = number of line-break
//! opportunities (per greedy candidate). Acceptable for PoC; Phase 3 will
//! cache widths and consider Knuth-Plass.

use crate::line_box::{LineBox, PaintedGlyph};
use text_pipeline::{
    Alignment, JustifyMode, LoadedFont, ShapingDirection, analyze_bidi, break_opportunities,
    justify::is_arabic_codepoint,
    justify_kashida::{JoinRole, KashidaPriority, join_role, kashida_point},
    shape_text,
};

pub struct ParagraphConfig<'a> {
    pub text: &'a str,
    pub font: &'a LoadedFont,
    pub base_direction: ShapingDirection,
    pub px_size: f32,
    pub max_width: f32,
    pub line_height: f32,
    pub alignment: Alignment,
}

pub fn layout_paragraph(cfg: ParagraphConfig<'_>) -> Vec<LineBox> {
    if cfg.text.is_empty() {
        return vec![];
    }

    let breaks = break_opportunities(cfg.text);

    /* Greedy: walk through break opportunities; keep extending the line
    while the candidate range fits in max_width; commit the longest fit
    when the next opportunity would overflow. */
    let mut lines: Vec<LineBox> = vec![];
    let mut start = 0_usize;
    let mut last_fit_end = start;

    for &b in breaks.iter() {
        if b <= start {
            continue;
        }
        let candidate = &cfg.text[start..b];
        let probe = shape_text(cfg.font, candidate, cfg.base_direction, cfg.px_size);

        if probe.total_advance <= cfg.max_width {
            last_fit_end = b;
        } else {
            /* Overflow. Commit whatever fit so far. */
            if last_fit_end > start {
                lines.push(build_line(&cfg, start, last_fit_end, true));
            } else {
                /* Single segment from start..b doesn't fit — force-break at
                this opportunity. (PoC: no mid-word hyphenation.) */
                lines.push(build_line(&cfg, start, b, true));
                last_fit_end = b;
            }
            start = last_fit_end;
        }
    }

    if start < cfg.text.len() {
        lines.push(build_line(&cfg, start, cfg.text.len(), false));
    }

    /* Justify all but the last line (and only lines that ended at an
    opportunity, not a forced hard break — covered already by
    `broken_at_opportunity`). */
    if cfg.alignment == Alignment::Justify {
        let last = lines.len().saturating_sub(1);
        for (i, line) in lines.iter_mut().enumerate() {
            if i == last || !line.broken_at_opportunity {
                continue;
            }
            justify_line(line, cfg.max_width, cfg.text);
        }
    }

    /* Baselines stacked from the top of the paragraph. */
    for (i, line) in lines.iter_mut().enumerate() {
        line.baseline_y = (i as f32 + 1.0) * cfg.line_height;
    }

    lines
}

/// Shape one logical byte range as a single visual line.
/// Runs UAX #9 BiDi on the slice, shapes each visual run with its resolved
/// direction, concatenates glyphs left-to-right.
fn build_line(cfg: &ParagraphConfig<'_>, start: usize, end: usize, broken: bool) -> LineBox {
    let line_text = &cfg.text[start..end];
    let bidi = analyze_bidi(line_text, cfg.base_direction);

    let mut glyphs: Vec<PaintedGlyph> = Vec::new();
    let mut x = 0.0_f32;
    for run in &bidi.visual_runs {
        let substr = &line_text[run.range.clone()];
        let shaped = shape_text(cfg.font, substr, run.direction, cfg.px_size);
        for g in shaped.glyphs {
            glyphs.push(PaintedGlyph {
                glyph_id: g.glyph_id,
                source_offset: (start + run.range.start) as u32 + g.cluster,
                x,
                x_offset: g.x_offset,
                y_offset: g.y_offset,
                x_advance: g.x_advance,
            });
            x += g.x_advance;
        }
    }

    LineBox {
        glyphs,
        direction: Some(bidi.paragraph_direction),
        natural_width: x,
        baseline_y: 0.0,
        broken_at_opportunity: broken,
    }
}

/// Pick justify strategy per line: Kashida-style for Arabic-heavy, space for Latin.
fn justify_line(line: &mut LineBox, target_width: f32, source_text: &str) {
    let extra = target_width - line.natural_width;
    if extra <= 0.0 || line.glyphs.is_empty() {
        return;
    }

    let kinds: Vec<(bool, char)> = line
        .glyphs
        .iter()
        .map(|g| {
            let c = source_text[g.source_offset as usize..]
                .chars()
                .next()
                .unwrap_or(' ');
            (is_arabic_codepoint(c), c)
        })
        .collect();

    let arabic_count = kinds.iter().filter(|k| k.0).count();
    let mode = if arabic_count == 0 {
        JustifyMode::Space
    } else if arabic_count == line.glyphs.len() {
        JustifyMode::Kashida
    } else {
        JustifyMode::Mixed
    };

    match mode {
        JustifyMode::None => {}
        JustifyMode::Space => distribute_to_spaces(line, &kinds, extra),
        JustifyMode::Kashida => distribute_to_kashida_points(line, &kinds, extra),
        JustifyMode::Mixed => {
            let space_count = kinds
                .iter()
                .filter(|(is_ar, c)| !*is_ar && c.is_whitespace())
                .count();
            let arabic_share = if space_count == 0 {
                extra
            } else {
                extra * (arabic_count as f32) / (arabic_count + space_count * 2) as f32
            };
            let space_share = extra - arabic_share;
            distribute_to_kashida_points(line, &kinds, arabic_share);
            distribute_to_spaces(line, &kinds, space_share);
        }
    }

    /* After bumping advances, re-emit x positions in visual order. */
    let mut x = 0.0;
    for g in line.glyphs.iter_mut() {
        g.x = x;
        x += g.x_advance;
    }
    line.natural_width = x;
}

fn distribute_to_spaces(line: &mut LineBox, kinds: &[(bool, char)], extra: f32) {
    let idx: Vec<usize> = kinds
        .iter()
        .enumerate()
        .filter(|(_, (is_ar, c))| !*is_ar && c.is_whitespace())
        .map(|(i, _)| i)
        .collect();
    if idx.is_empty() {
        return;
    }
    let per = extra / idx.len() as f32;
    for &i in &idx {
        line.glyphs[i].x_advance += per;
    }
}

/// Distribute `extra` width across Kashida elongation points.
///
/// Candidates are cursive-joining boundaries identified by Unicode
/// `Joining_Type` (PHASE_3_RENDER_RTL.md §6), not by Unicode block: the
/// logically-earlier letter must carry a left-joining form and the next a
/// right-joining form. Each candidate is scored into a Microsoft priority
/// band (P1 best … P5 last resort).
///
/// One kashida per word, placed at the word's highest-priority connecting
/// stroke. §6's pseudocode selects a band per line, but on a line whose top
/// band is sparse that dumps all the slack into one gap; choosing a single
/// stroke per word — as Uniscribe does — spreads the elongation so every word
/// stretches at its own best joint instead of one word tearing apart.
fn distribute_to_kashida_points(line: &mut LineBox, kinds: &[(bool, char)], extra: f32) {
    /* Walk visual glyphs, grouping cursive letters into words — a non-joining
    glyph (space, Latin) ends a word, combining marks are transparent. Arabic
    shapes right-to-left, so of two adjacent letter glyphs the right-hand one
    is logically earlier (the letter the stroke runs *after*); the kashida
    widens the left glyph's advance, opening the gap between the pair. */
    let mut words: Vec<Vec<(usize, KashidaPriority)>> = Vec::new();
    let mut word: Vec<(usize, KashidaPriority)> = Vec::new();
    let mut prev_letter: Option<usize> = None;
    for vi in 0..line.glyphs.len() {
        match join_role(kinds[vi].1) {
            JoinRole::Transparent => {}
            JoinRole::NonJoining => {
                if !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
                prev_letter = None;
            }
            JoinRole::Letter => {
                if let Some(pvi) = prev_letter
                    && let Some(priority) = kashida_point(kinds[vi].1, kinds[pvi].1)
                {
                    word.push((pvi, priority));
                }
                prev_letter = Some(vi);
            }
        }
    }
    if !word.is_empty() {
        words.push(word);
    }

    /* One kashida per word, at its highest-priority stroke; `extra` splits
    evenly across those points so each word stretches by the same amount. */
    let mut points: Vec<usize> = Vec::new();
    for w in &words {
        if let Some(&(idx, _)) = w.iter().min_by_key(|&&(_, p)| p) {
            points.push(idx);
        }
    }
    if points.is_empty() {
        return;
    }
    let per = extra / points.len() as f32;
    for i in points {
        line.glyphs[i].x_advance += per;
    }
}
