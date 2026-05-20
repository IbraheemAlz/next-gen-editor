//! Paragraph layout: greedy logical-order line break, per-line BiDi reorder,
//! per-line shaping, optional justify. Produces a [`ParagraphBox`] — the
//! hierarchical box tree of PHASE_3_RENDER_RTL.md §5.
//!
//! `layout_paragraph` owns all geometry: it positions every line within the
//! paragraph (`origin.y` stacks top-down, `origin.x` carries the alignment
//! offset) so the renderer only has to accumulate parent origins.
//!
//! Cost: O(N) calls to `shape_text` where N = number of line-break
//! opportunities. Acceptable for the PoC; Phase 3 will cache widths.

use crate::boxes::{
    LineBox, ParagraphBox, Point, PositionedGlyph, Size, StyleSpan, TextAttrs, VisualRun,
};
use std::mem::take;
use text_pipeline::{
    Alignment, FontStack, JustifyMode, ShapingDirection, analyze_bidi, break_opportunities,
    justify::is_arabic_codepoint,
    justify_kashida::{JoinRole, KashidaPriority, join_role, kashida_point},
    segment_by_script, shape_text,
};

pub struct ParagraphConfig<'a> {
    pub text: &'a str,
    /// Per-script font resolver — Latin and Arabic runs each shape against a
    /// covering face (PHASE_3_RENDER_RTL.md §13.A).
    pub fonts: &'a FontStack,
    /// Resolved style spans covering `[0, text.len())` with no gaps. Runs split
    /// at span boundaries and shape at the span's `px_size` (rich text).
    pub spans: &'a [StyleSpan],
    pub base_direction: ShapingDirection,
    pub max_width: f32,
    pub line_height: f32,
    pub alignment: Alignment,
}

/// Lay out `cfg.text` into a [`ParagraphBox`] with positioned lines.
///
/// The returned box has `origin == (0, 0)`; the page assembler sets the
/// paragraph's position when stacking it onto a `PageBox`.
pub fn layout_paragraph(cfg: ParagraphConfig<'_>) -> ParagraphBox {
    let mut composed = compose_lines(&cfg);

    /* Justify every line except the last and any hard-broken (overflow) line. */
    if cfg.alignment == Alignment::Justify {
        let last = composed.len().saturating_sub(1);
        for (i, (line, broke)) in composed.iter_mut().enumerate() {
            if i == last || !*broke {
                continue;
            }
            justify_line(line, cfg.max_width, cfg.text);
        }
    }

    /* Position each line within the paragraph: `origin.y` stacks top-down,
    `origin.x` carries the alignment offset so the renderer stays a pure
    origin accumulator. */
    let mut lines: Vec<LineBox> = Vec::with_capacity(composed.len());
    for (i, (mut line, _)) in composed.into_iter().enumerate() {
        line.origin = Point {
            x: alignment_origin_x(
                line.width,
                cfg.max_width,
                line.alignment,
                cfg.base_direction,
            ),
            y: i as f32 * cfg.line_height,
        };
        line.baseline = cfg.line_height;
        line.height = cfg.line_height;
        lines.push(line);
    }

    let height = lines.len() as f32 * cfg.line_height;
    ParagraphBox {
        origin: Point::default(),
        size: Size {
            width: cfg.max_width,
            height,
        },
        lines,
        direction: cfg.base_direction,
    }
}

/// Greedy line breaking. Returns each line paired with whether it ended at a
/// break opportunity (`true`) rather than the end of the paragraph (`false`) —
/// only opportunity-broken non-final lines are justified.
fn compose_lines(cfg: &ParagraphConfig<'_>) -> Vec<(LineBox, bool)> {
    if cfg.text.is_empty() {
        return vec![];
    }
    let breaks = break_opportunities(cfg.text);

    let mut lines: Vec<(LineBox, bool)> = vec![];
    let mut start = 0_usize;
    let mut last_fit_end = start;

    for &b in breaks.iter() {
        if b <= start {
            continue;
        }
        let candidate = &cfg.text[start..b];
        let probe = measure_text(
            cfg.fonts,
            candidate,
            start as u32,
            cfg.spans,
            cfg.base_direction,
        );

        if probe <= cfg.max_width {
            last_fit_end = b;
        } else {
            /* Overflow. Commit whatever fit so far. */
            if last_fit_end > start {
                lines.push((build_line(cfg, start, last_fit_end), true));
            } else {
                /* Single segment doesn't fit — force-break here. */
                lines.push((build_line(cfg, start, b), true));
                last_fit_end = b;
            }
            start = last_fit_end;
        }
    }

    if start < cfg.text.len() {
        lines.push((build_line(cfg, start, cfg.text.len()), false));
    }
    lines
}

/// Shape one logical byte range into a [`LineBox`]. Runs UAX #9 BiDi on the
/// slice and shapes each visual run with its resolved direction; one
/// [`VisualRun`] per BiDi run. Geometry (`origin`/`baseline`/`height`) is left
/// zeroed for [`layout_paragraph`] to fill.
fn build_line(cfg: &ParagraphConfig<'_>, start: usize, end: usize) -> LineBox {
    let line_text = &cfg.text[start..end];
    let bidi = analyze_bidi(line_text, cfg.base_direction);

    let mut runs: Vec<VisualRun> = Vec::new();
    for brun in &bidi.visual_runs {
        let brun_text = &line_text[brun.range.clone()];
        let brun_abs = (start + brun.range.start) as u32;

        /* Sub-segment the BiDi run by script (font) and by style span (size +
        colour) — both are hard splits. Collect in logical order, then reverse
        to visual (left-to-right) order for an RTL run. */
        let mut subs = Vec::new();
        for (srange, script) in segment_by_script(brun_text) {
            let mut cursor = brun_abs + srange.start as u32;
            let seg_end = brun_abs + srange.end as u32;
            while cursor < seg_end {
                let Some(span) = style_at(cfg.spans, cursor) else {
                    break;
                };
                let piece_end = span.end.min(seg_end);
                subs.push((
                    (cursor - brun_abs) as usize,
                    (piece_end - brun_abs) as usize,
                    script,
                    span,
                ));
                cursor = piece_end;
            }
        }
        if matches!(brun.direction, ShapingDirection::Rtl) {
            subs.reverse();
        }

        for (rel_start, rel_end, script, span) in subs {
            let Some((font_id, face)) = cfg.fonts.resolve(script) else {
                continue;
            };
            let sub_text = &brun_text[rel_start..rel_end];
            let shaped = shape_text(face, sub_text, brun.direction, span.px_size);
            let glyphs: Vec<PositionedGlyph> = shaped
                .glyphs
                .iter()
                .map(|g| PositionedGlyph {
                    id: g.glyph_id as u16,
                    cluster: g.cluster,
                    x_advance: g.x_advance,
                    y_advance: g.y_advance,
                    x_offset: g.x_offset,
                    y_offset: g.y_offset,
                })
                .collect();
            runs.push(VisualRun {
                glyphs,
                font: font_id.clone(),
                direction: brun.direction,
                source_range: brun_abs + rel_start as u32..brun_abs + rel_end as u32,
                attrs: TextAttrs {
                    px_size: span.px_size,
                    color: span.color,
                },
            });
        }
    }

    LineBox {
        width: line_advance(&runs),
        origin: Point::default(),
        baseline: 0.0,
        height: 0.0,
        runs,
        alignment: cfg.alignment,
    }
}

/// Sum of every glyph advance across a line's runs.
fn line_advance(runs: &[VisualRun]) -> f32 {
    runs.iter()
        .flat_map(|r| &r.glyphs)
        .map(|g| g.x_advance)
        .sum()
}

/// The style span covering byte `offset`. `cfg.spans` covers the whole
/// paragraph with no gaps, so a miss only happens past the text end.
fn style_at(spans: &[StyleSpan], offset: u32) -> Option<StyleSpan> {
    spans
        .iter()
        .copied()
        .find(|s| offset >= s.start && offset < s.end)
}

/// Total advance of `text` shaped with per-script fonts and per-span sizes —
/// the greedy probe's width estimate. `abs_start` is `text`'s byte offset in
/// the paragraph. Mirrors [`build_line`]'s segmentation so a fitted candidate
/// measures consistently with the line eventually built.
fn measure_text(
    fonts: &FontStack,
    text: &str,
    abs_start: u32,
    spans: &[StyleSpan],
    direction: ShapingDirection,
) -> f32 {
    let mut total = 0.0_f32;
    for (srange, script) in segment_by_script(text) {
        let Some((_, face)) = fonts.resolve(script) else {
            continue;
        };
        let mut cursor = abs_start + srange.start as u32;
        let seg_end = abs_start + srange.end as u32;
        while cursor < seg_end {
            let Some(span) = style_at(spans, cursor) else {
                break;
            };
            let piece_end = span.end.min(seg_end);
            let sub = &text[(cursor - abs_start) as usize..(piece_end - abs_start) as usize];
            total += shape_text(face, sub, direction, span.px_size).total_advance;
            cursor = piece_end;
        }
    }
    total
}

/// Horizontal offset of a line within its paragraph's content width. Mirrors
/// the Phase 1 inline rule: centre when centred, right-align a short RTL line,
/// otherwise flush-left.
fn alignment_origin_x(
    line_width: f32,
    content_width: f32,
    alignment: Alignment,
    base: ShapingDirection,
) -> f32 {
    if alignment == Alignment::Center {
        (content_width - line_width) / 2.0
    } else if matches!(base, ShapingDirection::Rtl) && line_width < content_width - 0.5 {
        content_width - line_width
    } else {
        0.0
    }
}

/// A glyph located within a line's run tree, tagged with the source character
/// it maps to — the working unit for justification.
struct JustifySlot {
    run: usize,
    glyph: usize,
    ch: char,
    arabic: bool,
}

/// Flatten a line's glyphs into [`JustifySlot`]s in visual order, resolving the
/// source character of each via its run's `source_range` + `cluster`.
fn line_glyph_slots(line: &LineBox, source_text: &str) -> Vec<JustifySlot> {
    let mut slots: Vec<JustifySlot> = Vec::new();
    for (ri, run) in line.runs.iter().enumerate() {
        for (gi, g) in run.glyphs.iter().enumerate() {
            let offset = (run.source_range.start + g.cluster) as usize;
            let ch = source_text[offset..].chars().next().unwrap_or(' ');
            slots.push(JustifySlot {
                run: ri,
                glyph: gi,
                ch,
                arabic: is_arabic_codepoint(ch),
            });
        }
    }
    slots
}

/// Pick a justify strategy for the line — Kashida for Arabic, spaces for Latin,
/// a weighted split for mixed — and stretch glyph advances to `target_width`.
fn justify_line(line: &mut LineBox, target_width: f32, source_text: &str) {
    let extra = target_width - line.width;
    if extra <= 0.0 {
        return;
    }
    let slots = line_glyph_slots(line, source_text);
    if slots.is_empty() {
        return;
    }

    let arabic_count = slots.iter().filter(|s| s.arabic).count();
    let mode = if arabic_count == 0 {
        JustifyMode::Space
    } else if arabic_count == slots.len() {
        JustifyMode::Kashida
    } else {
        JustifyMode::Mixed
    };

    match mode {
        JustifyMode::None => {}
        JustifyMode::Space => distribute_to_spaces(line, &slots, extra),
        JustifyMode::Kashida => distribute_to_kashida_points(line, &slots, extra),
        JustifyMode::Mixed => {
            let space_count = slots
                .iter()
                .filter(|s| !s.arabic && s.ch.is_whitespace())
                .count();
            let arabic_share = if space_count == 0 {
                extra
            } else {
                extra * (arabic_count as f32) / (arabic_count + space_count * 2) as f32
            };
            distribute_to_kashida_points(line, &slots, arabic_share);
            distribute_to_spaces(line, &slots, extra - arabic_share);
        }
    }

    line.width = line_advance(&line.runs);
}

/// Distribute `extra` width evenly across the line's inter-word spaces.
fn distribute_to_spaces(line: &mut LineBox, slots: &[JustifySlot], extra: f32) {
    let targets: Vec<usize> = slots
        .iter()
        .enumerate()
        .filter(|(_, s)| !s.arabic && s.ch.is_whitespace())
        .map(|(i, _)| i)
        .collect();
    if targets.is_empty() {
        return;
    }
    let per = extra / targets.len() as f32;
    for i in targets {
        let s = &slots[i];
        line.runs[s.run].glyphs[s.glyph].x_advance += per;
    }
}

/// Distribute `extra` width across Kashida elongation points.
///
/// Candidates are cursive-joining boundaries identified by Unicode
/// `Joining_Type` (PHASE_3_RENDER_RTL.md §6): the logically-earlier letter must
/// carry a left-joining form and the next a right-joining form. Each candidate
/// is scored into a Microsoft priority band, and one kashida per word — at the
/// word's highest-priority stroke — receives an equal share of `extra`.
fn distribute_to_kashida_points(line: &mut LineBox, slots: &[JustifySlot], extra: f32) {
    /* Walk glyph slots in visual order, grouping cursive letters into words —
    a non-joining glyph ends a word, combining marks are transparent. Within a
    word each connecting boundary is a candidate scored into a priority band.
    Arabic shapes right-to-left, so of two adjacent letter glyphs the
    right-hand one is logically earlier; the kashida widens the left glyph. */
    let mut words: Vec<Vec<(usize, KashidaPriority)>> = Vec::new();
    let mut word: Vec<(usize, KashidaPriority)> = Vec::new();
    let mut prev_letter: Option<usize> = None;
    for vi in 0..slots.len() {
        match join_role(slots[vi].ch) {
            JoinRole::Transparent => {}
            JoinRole::NonJoining => {
                if !word.is_empty() {
                    words.push(take(&mut word));
                }
                prev_letter = None;
            }
            JoinRole::Letter => {
                if let Some(pvi) = prev_letter
                    && let Some(priority) = kashida_point(slots[vi].ch, slots[pvi].ch)
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
    evenly across those points. */
    let mut targets: Vec<usize> = Vec::new();
    for w in &words {
        if let Some(&(slot, _)) = w.iter().min_by_key(|&&(_, p)| p) {
            targets.push(slot);
        }
    }
    if targets.is_empty() {
        return;
    }
    let per = extra / targets.len() as f32;
    for i in targets {
        let s = &slots[i];
        line.runs[s.run].glyphs[s.glyph].x_advance += per;
    }
}
