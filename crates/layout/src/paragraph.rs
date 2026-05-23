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
    LineBox, MarkerBox, ParagraphBox, Point, PositionedGlyph, Size, StyleSpan, TextAttrs, VisualRun,
};
use std::mem::take;
use text_pipeline::{
    Alignment, FontStack, JustifyMode, Script, ShapingDirection, analyze_bidi, break_opportunities,
    justify::is_arabic_codepoint,
    justify_kashida::{JoinRole, KashidaPriority, join_role, kashida_point},
    segment_by_script, shape_text,
};

/// Phase 7 — per-paragraph inline object metadata (image dimensions). The
/// engine populates this from `engine::Paragraph::inline_objects`; layout
/// looks the relevant entry up by `at == cluster` when shaping yields a
/// glyph for the U+FFFC OBJECT REPLACEMENT CHARACTER sentinel.
#[derive(Debug, Clone)]
pub struct InlineObjectInfo {
    /// `at` field of the paragraph's `engine::InlineObject`, in
    /// paragraph-byte coords.
    pub at: u32,
    /// Width / height in layout pixels (already scaled).
    pub width_px: f32,
    pub height_px: f32,
    /// What sits at this anchor.
    pub kind: InlineObjectInfoKind,
}

/// Phase 8a — discriminator for `InlineObjectInfo`. Image anchors carry a
/// rel id; footnote refs carry the marker text the renderer paints as a
/// superscript.
#[derive(Debug, Clone)]
pub enum InlineObjectInfoKind {
    Image { rel_id: String },
    FootnoteMarker { text: String },
}

pub struct ParagraphConfig<'a> {
    pub text: &'a str,
    /// Per-script font resolver — Latin and Arabic runs each shape against a
    /// covering face (PHASE_3_RENDER_RTL.md §13.A).
    pub fonts: &'a FontStack,
    /// Phase 7 — inline-object table. Indexed by sentinel byte (`at`).
    pub inline_objects: &'a [InlineObjectInfo],
    /// Resolved style spans covering `[0, text.len())` with no gaps. Runs split
    /// at span boundaries and shape at the span's `px_size` (rich text).
    pub spans: &'a [StyleSpan],
    pub base_direction: ShapingDirection,
    pub max_width: f32,
    pub line_height: f32,
    pub alignment: Alignment,
    /// `<w:ind w:start>` — distance every line is offset from the leading
    /// margin. In layout px (already DPR-scaled).
    pub indent_start_px: f32,
    /// `<w:ind w:end>` — distance every line is offset from the trailing
    /// margin (shrinks the available content width).
    pub indent_end_px: f32,
    /// `<w:ind w:firstLine>` — extra leading-edge offset on the first line
    /// only. Mutually exclusive with `hanging_indent_px` (one is 0).
    pub first_line_indent_px: f32,
    /// `<w:ind w:hanging>` — distance the first line shifts back from
    /// `indent_start_px` (so subsequent lines hang in).
    pub hanging_indent_px: f32,
    /// Phase 4 list marker text (`"1."`, `"a)"`, `"•"`). `None` for non-list
    /// paragraphs. Shaped against the first available font in
    /// [`Self::fonts`] at [`Self::px_size_for_marker`] and laid out in
    /// the leading-edge gutter (before the first line for LTR; after for
    /// RTL).
    pub marker_text: Option<String>,
    /// Pixel size for shaping the marker. Matches the body font size by
    /// default at the engine-wasm boundary.
    pub px_size_for_marker: f32,
}

/// Lay out `cfg.text` into a [`ParagraphBox`] with positioned lines.
///
/// The returned box has `origin == (0, 0)`; the page assembler sets the
/// paragraph's position when stacking it onto a `PageBox`.
pub fn layout_paragraph(cfg: ParagraphConfig<'_>) -> ParagraphBox {
    /* `<w:ind w:start|end>` shrinks the available content width. First-line
    indent is applied per-line below. The remaining alignment math operates
    in the shrunken content space — alignment_origin_x positions inside it
    — and the leading-edge shift drops onto the line origin. */
    let content_width = (cfg.max_width - cfg.indent_start_px - cfg.indent_end_px).max(0.0);
    let mut composed = compose_lines_with_width(&cfg, content_width);

    /* Justify every line except the last and any hard-broken (overflow) line. */
    if cfg.alignment == Alignment::Justify {
        let last = composed.len().saturating_sub(1);
        for (i, (line, broke)) in composed.iter_mut().enumerate() {
            if i == last || !*broke {
                continue;
            }
            justify_line(line, content_width, cfg.text, cfg.fonts);
        }
    }

    let rtl = matches!(cfg.base_direction, ShapingDirection::Rtl);
    /* Leading-edge offset: distance from the box's logical-left edge (in
    layout coords, `origin.x = 0`) to the line's leading content edge. For
    LTR that's `indent_start_px`; for RTL it's `indent_end_px` because
    "leading" is the right side. The trailing edge is mirrored. */
    let (leading_off, trailing_off) = if rtl {
        (cfg.indent_end_px, cfg.indent_start_px)
    } else {
        (cfg.indent_start_px, cfg.indent_end_px)
    };

    /* Position each line within the paragraph. `origin.y` stacks by the
    accumulated height of preceding lines; each line's height is its own max
    ascent + max descent over its runs (Backlog #5 — dynamic line height), so
    a line carrying a larger span grows to fit instead of clipping. `origin.x`
    carries the alignment offset + the leading-edge indent so the renderer
    stays a pure accumulator. */
    let mut lines: Vec<LineBox> = Vec::with_capacity(composed.len());
    let mut y = 0.0_f32;
    for (i, (mut line, _)) in composed.into_iter().enumerate() {
        let (ascent, descent) = line_extents(&line, cfg.fonts, cfg.line_height);
        /* First-line indent / hanging: hanging shifts the first line *back*
        toward the leading edge; firstLine shifts it *forward* into the body.
        Both already in layout px. Subsequent lines hug `leading_off`. */
        let first_line_extra = if i == 0 {
            cfg.first_line_indent_px - cfg.hanging_indent_px
        } else {
            0.0
        };
        let inner_origin = alignment_origin_x(
            line.width,
            (content_width - first_line_extra).max(0.0),
            line.alignment,
            cfg.base_direction,
        );
        line.origin = Point {
            x: leading_off + first_line_extra + inner_origin,
            y,
        };
        line.baseline = ascent;
        line.height = ascent + descent;
        y += line.height;
        lines.push(line);
        /* `trailing_off` participates only by shrinking `content_width`. */
        let _ = trailing_off;
    }

    /* An empty paragraph (no text → no lines) still occupies one
    line's worth of vertical space and carries one zero-width
    placeholder line so the hit-test geometry can locate it. Without
    the line the caret cannot find its host paragraph after pressing
    Enter — `caret_rect_geom` falls back to `geom.first()` and the
    caret visibly jumps to (0, 0). Without the height the cell
    containing the placeholder collapses to zero. */
    if lines.is_empty() {
        let inner_origin =
            alignment_origin_x(0.0, content_width, cfg.alignment, cfg.base_direction);
        lines.push(LineBox {
            origin: Point {
                x: leading_off + inner_origin,
                y: 0.0,
            },
            baseline: cfg.line_height,
            height: cfg.line_height,
            width: 0.0,
            runs: Vec::new(),
            alignment: cfg.alignment,
        });
        y = cfg.line_height;
    }
    let height = y;
    /* Phase 4 — list marker (`"1."`, `"a)"`, `"•"`). Shape against the
    base-direction script's preferred face at `px_size_for_marker`, then
    park it in the leading-edge gutter aligned to the first line's
    baseline. Positioned but not justified, not selectable, not part of
    line layout — purely a side-car run. */
    let marker = build_marker(&cfg, leading_off, lines.first());
    let _ = trailing_off;
    ParagraphBox {
        origin: Point::default(),
        size: Size {
            width: cfg.max_width,
            height,
        },
        lines,
        direction: cfg.base_direction,
        marker,
        source_paragraph_id: ParagraphBox::NO_SOURCE_ID,
    }
}

fn build_marker(
    cfg: &ParagraphConfig<'_>,
    leading_off: f32,
    first_line: Option<&LineBox>,
) -> Option<MarkerBox> {
    let text = cfg.marker_text.as_deref()?;
    if text.is_empty() {
        return None;
    }
    let first = first_line?;
    let direction = cfg.base_direction;
    let script = match direction {
        ShapingDirection::Rtl => Script::Arabic,
        ShapingDirection::Ltr => Script::Latin,
    };
    let (font_id, face, _synth) = cfg.fonts.resolve(script, None, false, false)?;
    let shaped = shape_text(face, text, direction, cfg.px_size_for_marker);
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
            synthetic: false,
            inline_image_rel_id: None,
            inline_footnote_marker: None,
            inline_object_height: 0.0,
        })
        .collect();
    let width = glyphs.iter().map(|g| g.x_advance).sum::<f32>();
    /* Small visual gap between the marker and the body text — the OOXML
    `<w:suff>` element controls this in Word; Phase 4 ships a fixed
    em-fraction so the marker doesn't kiss the first glyph. */
    let gap = cfg.px_size_for_marker * 0.5;
    let rtl = matches!(direction, ShapingDirection::Rtl);
    /* LTR: marker sits to the left of the first line's leading edge, with
    its trailing edge at `leading_off - gap`. RTL: mirror — marker sits to
    the right, leading edge at `max_width - leading_off + gap`. */
    let origin_x = if rtl {
        cfg.max_width - leading_off + gap
    } else {
        (leading_off - gap - width).max(0.0)
    };
    let origin_y = first.origin.y;
    Some(MarkerBox {
        origin: Point {
            x: origin_x,
            y: origin_y,
        },
        baseline: first.baseline,
        run: VisualRun {
            glyphs,
            font: font_id.clone(),
            direction,
            source_range: 0..0,
            attrs: TextAttrs {
                px_size: cfg.px_size_for_marker,
                color: [0, 0, 0, 255],
                bg_color: None,
                underline: false,
                strike: false,
                faux_bold: false,
                faux_italic: false,
            },
        },
        width,
    })
}

/// Adapter that lets the indent-aware path call the existing `compose_lines`
/// (which read `cfg.max_width`) with the shrunken content width. Phase 1
/// call sites passed `indent_start_px == indent_end_px == 0`, so the shrunk
/// width equals `max_width` and behaviour is preserved byte-for-byte.
fn compose_lines_with_width<'a>(
    cfg: &ParagraphConfig<'a>,
    content_width: f32,
) -> Vec<(LineBox, bool)> {
    let scoped = ParagraphConfig {
        text: cfg.text,
        fonts: cfg.fonts,
        spans: cfg.spans,
        base_direction: cfg.base_direction,
        max_width: content_width,
        line_height: cfg.line_height,
        alignment: cfg.alignment,
        indent_start_px: 0.0,
        indent_end_px: 0.0,
        first_line_indent_px: 0.0,
        hanging_indent_px: 0.0,
        marker_text: None,
        px_size_for_marker: cfg.px_size_for_marker,
        inline_objects: cfg.inline_objects,
    };
    compose_lines(&scoped)
}

/// Maximum ascent and descent across a line's runs, from each run's font
/// metrics at its pixel size (Backlog #5). A line carrying a larger font size
/// or a taller script grows to fit the tallest run. A line with no resolvable
/// run metrics falls back to the configured line height as pure ascent.
fn line_extents(line: &LineBox, fonts: &FontStack, fallback: f32) -> (f32, f32) {
    let mut ascent = 0.0_f32;
    let mut descent = 0.0_f32;
    for run in &line.runs {
        if let Some(face) = fonts.face(&run.font) {
            let m = face.metrics(run.attrs.px_size);
            ascent = ascent.max(m.ascent);
            descent = descent.max(m.descent.abs());
        }
        /* Phase 7 — inline objects (images) push the ascent so the line
        grows to host the image without clipping. The bottom of the
        object sits on the baseline; the image extends `height_px`
        above it. */
        for g in &run.glyphs {
            if g.inline_image_rel_id.is_some() {
                ascent = ascent.max(g.inline_object_height);
            }
        }
    }
    if ascent + descent <= 0.0 {
        (fallback, 0.0)
    } else {
        (ascent, descent)
    }
}

/// Greedy line breaking. Returns each line paired with whether it ended at a
/// break opportunity (`true`) rather than the end of the paragraph (`false`) —
/// only opportunity-broken non-final lines are justified.
///
/// Each inter-break segment is measured exactly once and its width accumulated
/// into a running line total — so the greedy walk is O(breaks) per paragraph,
/// not the O(breaks²) of re-measuring every growing prefix. Segment widths
/// sum cleanly because break opportunities fall on spaces, where no ligature,
/// kerning or cursive join crosses the boundary.
fn compose_lines(cfg: &ParagraphConfig<'_>) -> Vec<(LineBox, bool)> {
    if cfg.text.is_empty() {
        return vec![];
    }
    let breaks = break_opportunities(cfg.text);

    let mut lines: Vec<(LineBox, bool)> = vec![];
    let mut start = 0_usize;
    let mut last_fit_end = start;
    /* `line_width` is the accumulated width of `[start..seg_from]` — the run
    of segments already accepted onto the current line. */
    let mut seg_from = start;
    let mut line_width = 0.0_f32;

    for &b in breaks.iter() {
        if b <= start {
            continue;
        }
        let seg_width = measure_text(
            cfg.fonts,
            &cfg.text[seg_from..b],
            seg_from as u32,
            cfg.spans,
            cfg.base_direction,
        );

        if line_width + seg_width <= cfg.max_width {
            last_fit_end = b;
            line_width += seg_width;
            seg_from = b;
        } else {
            /* Overflow. Commit whatever fit so far. */
            if last_fit_end > start {
                lines.push((build_line(cfg, start, last_fit_end), true));
                start = last_fit_end;
                seg_from = start;
                line_width = 0.0;
            } else {
                /* Single segment doesn't fit on a fresh line. Force a
                character-level break so the word wraps instead of
                overflowing the page (CSS `overflow-wrap: anywhere`). */
                let force = char_break_fit(cfg, start, b);
                lines.push((build_line(cfg, start, force), true));
                start = force;
                last_fit_end = force;
                seg_from = force;
                line_width = 0.0;
                /* The unconsumed remainder `[force..b]` rides onto the
                next outer iteration's segment measurement — when `b'`
                advances past `b`, the prefix `[force..b']` is
                re-measured and broken again if needed. */
            }
        }
    }

    if start < cfg.text.len() {
        /* The final tail may itself overflow (e.g. a giant trailing
        URL) — keep force-breaking until it fits. */
        while start < cfg.text.len() {
            let end = cfg.text.len();
            let w = measure_text(
                cfg.fonts,
                &cfg.text[start..end],
                start as u32,
                cfg.spans,
                cfg.base_direction,
            );
            if w <= cfg.max_width {
                lines.push((build_line(cfg, start, end), false));
                break;
            }
            let force = char_break_fit(cfg, start, end);
            lines.push((build_line(cfg, start, force), true));
            start = force;
        }
    }
    lines
}

/// Largest character-boundary position in `[start..hard_end]` whose
/// rendered text fits within `cfg.max_width`. When even one character
/// is too wide, returns the first char boundary past `start` so the
/// loop always makes progress (a single huge glyph then overflows the
/// page by itself — better than an infinite loop).
fn char_break_fit(cfg: &ParagraphConfig<'_>, start: usize, hard_end: usize) -> usize {
    let mut accept: Option<usize> = None;
    let mut first_char_end: Option<usize> = None;
    let mut iter = cfg.text[start..hard_end].char_indices();
    iter.next();
    for (offset, _) in iter {
        let abs = start + offset;
        if first_char_end.is_none() {
            first_char_end = Some(abs);
        }
        let w = measure_text(
            cfg.fonts,
            &cfg.text[start..abs],
            start as u32,
            cfg.spans,
            cfg.base_direction,
        );
        if w <= cfg.max_width {
            accept = Some(abs);
        } else {
            break;
        }
    }
    accept.or(first_char_end).unwrap_or_else(|| {
        /* Single-char segment that overflows — push it whole and
        let it overshoot rather than emit a zero-width line. */
        cfg.text[start..]
            .char_indices()
            .nth(1)
            .map(|(o, _)| start + o)
            .unwrap_or(hard_end)
    })
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
            let Some((font_id, face, synth)) =
                cfg.fonts
                    .resolve(script, span.font_family.as_deref(), span.bold, span.italic)
            else {
                continue;
            };
            let sub_text = &brun_text[rel_start..rel_end];
            let shaped = shape_text(face, sub_text, brun.direction, span.px_size);
            let brun_abs_start = brun_abs + rel_start as u32;
            let glyphs: Vec<PositionedGlyph> = shaped
                .glyphs
                .iter()
                .map(|g| {
                    /* Phase 7 — paragraph-absolute byte offset of this
                    glyph's cluster. The shaper reports cluster relative
                    to the input string we passed (`sub_text`), so we
                    re-anchor at the sub-run's paragraph offset. A glyph
                    sitting on the U+FFFC sentinel of an inline object
                    has its advance overridden to the object's reserved
                    width and its `inline_object_id` set so the renderer
                    paints the bitmap instead of the placeholder glyph. */
                    let abs_cluster = brun_abs_start + g.cluster;
                    let info = cfg
                        .inline_objects
                        .iter()
                        .find(|info| info.at == abs_cluster);
                    let (image_rel, footnote_marker) = match info.map(|i| &i.kind) {
                        Some(crate::paragraph::InlineObjectInfoKind::Image { rel_id }) => {
                            (Some(rel_id.clone()), None)
                        }
                        Some(crate::paragraph::InlineObjectInfoKind::FootnoteMarker { text }) => {
                            (None, Some(text.clone()))
                        }
                        None => (None, None),
                    };
                    PositionedGlyph {
                        id: g.glyph_id as u16,
                        cluster: g.cluster,
                        x_advance: info.map_or(g.x_advance, |i| i.width_px),
                        y_advance: g.y_advance,
                        x_offset: g.x_offset,
                        y_offset: g.y_offset,
                        synthetic: false,
                        inline_image_rel_id: image_rel,
                        inline_footnote_marker: footnote_marker,
                        inline_object_height: info.map_or(0.0, |i| i.height_px),
                    }
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
                    faux_bold: synth.faux_bold,
                    faux_italic: synth.faux_italic,
                    underline: span.underline,
                    strike: span.strike,
                    bg_color: span.bg_color,
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
        .find(|s| offset >= s.start && offset < s.end)
        .cloned()
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
        let mut cursor = abs_start + srange.start as u32;
        let seg_end = abs_start + srange.end as u32;
        while cursor < seg_end {
            let Some(span) = style_at(spans, cursor) else {
                break;
            };
            let piece_end = span.end.min(seg_end);
            /* Resolve per span: an explicit font family changes shaping (and
            width); faux bold/italic do not, so weight/slant stay `false`. */
            let Some((_, face, _)) =
                fonts.resolve(script, span.font_family.as_deref(), false, false)
            else {
                break;
            };
            let sub = &text[(cursor - abs_start) as usize..(piece_end - abs_start) as usize];
            total += shape_text(face, sub, direction, span.px_size).total_advance;
            cursor = piece_end;
        }
    }
    total
}

/// Horizontal offset of a line within its paragraph's content width.
/// `Center` centres the line; `End` flushes it to the writing-direction
/// trailing edge; `Start` / `Justify` flush to the leading edge — which is the
/// right edge for an RTL base, so a short RTL line still hugs the margin.
fn alignment_origin_x(
    line_width: f32,
    content_width: f32,
    alignment: Alignment,
    base: ShapingDirection,
) -> f32 {
    let rtl = matches!(base, ShapingDirection::Rtl);
    match alignment {
        Alignment::Center => (content_width - line_width) / 2.0,
        /* `End` — the trailing edge: visual-right for an LTR base, visual-left
        (offset 0) for RTL. */
        Alignment::End => {
            if rtl {
                0.0
            } else {
                content_width - line_width
            }
        }
        /* `Start` leads; `Justify` already stretched the line to the full
        width, so its leftover offset follows the same leading-edge rule. */
        Alignment::Start | Alignment::Justify => {
            if rtl && line_width < content_width - 0.5 {
                content_width - line_width
            } else {
                0.0
            }
        }
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
/// a weighted split for mixed — and stretch the line to `target_width`.
fn justify_line(line: &mut LineBox, target_width: f32, source_text: &str, fonts: &FontStack) {
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
        JustifyMode::Kashida => distribute_to_kashida_points(line, &slots, extra, fonts),
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
            /* Spaces first: `distribute_to_kashida_points` inserts Tatweel
            glyphs, which would shift the space slots' glyph indices. */
            distribute_to_spaces(line, &slots, extra - arabic_share);
            distribute_to_kashida_points(line, &slots, arabic_share, fonts);
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
fn distribute_to_kashida_points(
    line: &mut LineBox,
    slots: &[JustifySlot],
    extra: f32,
    fonts: &FontStack,
) {
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
    /* Resolve each kashida target to a (run, glyph) site, then inject the
    Tatweel ink highest glyph-index first per run — inserting glyphs shifts
    later indices, so earlier sites must be processed last. */
    let mut sites: Vec<(usize, usize)> = targets
        .iter()
        .map(|&i| (slots[i].run, slots[i].glyph))
        .collect();
    sites.sort_unstable();
    sites.dedup();
    for &(run_idx, glyph_idx) in sites.iter().rev() {
        inject_kashida(&mut line.runs[run_idx], glyph_idx, per, fonts);
    }
}

/// Fill a Kashida elongation of width `extra`, sitting after
/// `run.glyphs[glyph_idx]`, with real Tatweel (U+0640) ink instead of white
/// space (Backlog #2).
///
/// Tiles `n` natural-width Tatweel glyphs — the count nearest `extra / tatweel
/// advance` — and parks the sub-Tatweel remainder on the elongated glyph's
/// own advance so the total elongation width stays exact. Every synthetic
/// glyph copies the elongated glyph's `cluster`, so it maps to the same source
/// byte and the byte<->glyph map used by hit-testing is preserved. Falls back
/// to a plain advance bump when the font carries no Tatweel glyph.
fn inject_kashida(run: &mut VisualRun, glyph_idx: usize, extra: f32, fonts: &FontStack) {
    let tatweel = fonts.face(&run.font).and_then(|face| {
        let gid = face.glyph_id('\u{0640}')?;
        let adv = face
            .glyph_metrics('\u{0640}', run.attrs.px_size)
            .ok()?
            .advance_width;
        (adv > 0.0).then_some((gid, adv))
    });
    let Some((tw_gid, tw_adv)) = tatweel else {
        /* No Tatweel in the font — keep the Phase 3 white-gap behaviour. */
        run.glyphs[glyph_idx].x_advance += extra;
        return;
    };
    let n = ((extra / tw_adv).round() as i64).max(1) as usize;
    let remainder = extra - (n as f32) * tw_adv;
    run.glyphs[glyph_idx].x_advance += remainder;
    let tatweel_glyph = PositionedGlyph {
        id: tw_gid,
        cluster: run.glyphs[glyph_idx].cluster,
        x_advance: tw_adv,
        y_advance: 0.0,
        x_offset: 0.0,
        y_offset: 0.0,
        synthetic: true,
        inline_image_rel_id: None,
        inline_footnote_marker: None,
        inline_object_height: 0.0,
    };
    for _ in 0..n {
        run.glyphs.insert(glyph_idx + 1, tatweel_glyph.clone());
    }
}
