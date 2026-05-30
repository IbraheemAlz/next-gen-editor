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
use std::borrow::Cow;
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
    /// Audit gap A.M3 — `<w:pPr><w:tabs>` custom stops in
    /// paragraph-content-relative layout px, paired with the stop's
    /// geometric kind. Empty ⇒ the line builder falls back to the
    /// default 0.5-inch grid (`DEFAULT_TAB_GRID_PT`). `Clear` entries
    /// are filtered out by the engine-wasm adapter before reaching us.
    /// L2.1 (#6) — non-`Left` kinds (Center/Right/Decimal) trigger the
    /// shape-then-place pass in `apply_tab_advances`.
    pub tab_stops_px: &'a [(f32, TabKind)],
}

/// L2.1 (#6) — geometric kind of a single custom tab stop. Mirrors
/// `engine::TabKind` minus `Clear` (which the engine-wasm adapter
/// strips before reaching the layout pass). Kept local to the layout
/// crate so `engine` does not leak into `ParagraphConfig`'s public API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TabKind {
    /// Tab cursor jumps to the stop; the following text lands flush
    /// against its left edge. Word's default and the only kind honoured
    /// before L2.1.
    #[default]
    Left,
    /// Following text's midpoint lands at the stop.
    Center,
    /// Following text's right edge lands at the stop.
    Right,
    /// Following text's decimal separator (`.` or `,`) lands at the
    /// stop. Falls back to `Right` semantics when the segment contains
    /// no separator.
    Decimal,
}

/// Audit gap A.M3 — default tab grid in layout pt at scale=1. Word's
/// "default tab stops" knob (`<w:settings><w:defaultTabStop>`) defaults
/// to 720 twips = 36 pt = ½ inch; the line builder steps in 36-pt
/// increments whenever no custom stop in the paragraph's tab list
/// fits. Grid stops are always `TabKind::Left` — Word does not allow
/// kind overrides on default-grid stops.
pub const DEFAULT_TAB_GRID_PT: f32 = 36.0;

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

    /* Audit gap A.M3 / L2.1 (#6) — geometric tab stops. Pass runs
    BEFORE justify so post-fix advances are baked into `line.width`
    and justify sees a clean total. Each tab character (U+0009) in a
    glyph's source cluster has its advance overridden so the next
    glyph lands at the requested stop. Custom stops carry a
    `TabKind` (`Left`/`Center`/`Right`/`Decimal`); the helper does
    shape-then-place for the non-Left kinds. Falls back to the
    default half-inch grid (`DEFAULT_TAB_GRID_PT`, always `Left`).
    The pen starts at the line's leading-edge indent so it tracks
    paragraph-content-relative coordinates — tab stops (also
    paragraph-content-relative) compare directly without a transform. */
    /* Tab stops resolve in the LOGICAL frame: the pen measures reading-order
    distance from the content's LEADING edge, and `tab_stops_px` are stored
    content-edge-relative (0 = the margin, before any indent — Word's ruler
    convention). The first glyph sits at the `<w:start>` (leading) indent, so
    the pen base is `indent_start_px` in BOTH directions.

    This pairs with the per-direction `leading_off` chosen below
    (`indent_start` for LTR, `indent_end` for RTL) by a cancellation that is
    load-bearing — DO NOT "simplify" the pen base back to `indent_end` for
    RTL. For RTL Start alignment a logical position `q` maps to
    `box_x = leading_off + content_width + pen_base − q`, and
    `indent_end + (max_width − indent_start − indent_end) + indent_start
    == max_width`, so the stop at content-relative `p` lands at
    `box_x = max_width − p` regardless of how the start/end indents split.
    The pre-fix code used `indent_end_px` as the RTL base, which only agreed
    with `leading_off` when the indents were symmetric. */
    let pre_alignment_leading_off = cfg.indent_start_px;
    for (line, _) in composed.iter_mut() {
        apply_tab_advances(
            line,
            cfg.text,
            cfg.tab_stops_px,
            pre_alignment_leading_off,
            cfg.base_direction,
        );
    }

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
        Both already in layout px. Subsequent lines — soft-wrapped OR forced
        by a soft break (Shift+Enter) — hug `leading_off` (the Golden Rule;
        see `first_line_offset_px`). */
        let first_line_extra =
            first_line_offset_px(i, cfg.first_line_indent_px, cfg.hanging_indent_px);
        let inner_origin = alignment_origin_x(
            line.width,
            (content_width - first_line_extra).max(0.0),
            line.alignment,
            cfg.base_direction,
        );
        /* The first-line indent insets the LEADING edge. For LTR that edge is
        the physical LEFT, so the line origin shifts right by `first_line_extra`.
        For RTL the leading edge is the physical RIGHT: the shrunken width above
        + RTL right-alignment already pull that right edge inward, so adding
        `first_line_extra` to the (physical-left) origin as well DOUBLE-counts —
        and under Start alignment it cancels algebraically, zeroing the indent
        (the dead `▽` marker). RTL therefore contributes nothing to the origin
        here; the inset lives entirely in the reduced width. Hanging
        (`first_line_extra < 0`) rides the same path — it grows the width,
        pushing the first line's leading edge OUT past the body indent. */
        let first_line_origin_shift = if rtl { 0.0 } else { first_line_extra };
        line.origin = Point {
            x: leading_off + first_line_origin_shift + inner_origin,
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
            /* Empty paragraph → caret at offset 0. */
            source_start: 0,
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
        fields: Vec::new(),
        page_break_after_line: Vec::new(),
        borders: None,
        shading: None,
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
                underline: engine::UnderlineStyle::None,
                strike: false,
                faux_bold: false,
                faux_italic: false,
                baseline_shift_px: 0.0,
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
        tab_stops_px: cfg.tab_stops_px,
    };
    compose_lines(&scoped)
}

/// L2.1 (#6) — minimum advance assigned to a tab glyph after a
/// non-Left shape-then-place computation lands beyond the next stop.
///
/// Prevents the tab from collapsing to 0 (which would visually overlap
/// the preceding content); the segment then naturally overflows the
/// stop instead. Chosen at 4 px to match Word's observable minimum tab
/// width on a 12-pt body.
const MIN_TAB_FILL_PX: f32 = 4.0;

// Audit gap A.M3 / L2.1 (#6) / RTL fix (issue #20, Left-tab) — geometric
// tab-stop advance.
//
// Tab stops are resolved in LOGICAL (reading) order, which is the only frame
// in which a stop position is meaningful: a stop at `p` means "the content
// that logically FOLLOWS the tab begins at distance `p` from the paragraph's
// leading edge." For an LTR paragraph logical order equals visual
// (physical-left-to-right) order, so a naive visual-order pen walk happened
// to be correct. For an RTL paragraph the per-line BiDi reorder makes the
// content sitting visually-BEFORE a tab the content that logically FOLLOWS
// it; a visual-order pen therefore measured the wrong baseline and the Left
// advance ballooned past the content width, ejecting the leading run off the
// physical-left page edge (the "RTL tab explosion").
//
// This pass builds a logical-order index of every glyph (sorted by absolute
// source cluster — clusters are paragraph-absolute and monotonic with
// reading position, so the sort recovers logical order across BiDi runs) and
// accumulates the pen in that order, starting at `leading_off_px` (the
// leading `<w:start>` indent). When a glyph's source cluster points at a
// U+0009 TAB the advance is rewritten per the matching stop's `TabKind`:
//   * `Left`   — content after the tab starts at the stop.
//   * `Center` — the following segment's midpoint lands at the stop.
//   * `Right`  — the following segment's trailing edge lands at the stop.
//   * `Decimal`— the following segment's first `.`/`,` lands at the stop
//                (falls back to `Right` when the segment has no separator).
// The "following segment" is measured by walking forward in LOGICAL order to
// the next tab (or end of line). The renderer then lays the glyphs out in
// visual order and the alignment offset is applied; because the advance was
// computed against the logical pen, the logically-following run's leading
// edge lands at the stop in BOTH directions (proof in the pen-base comment
// of `layout_paragraph`).
//
// `line.width` is the post-adjustment extent (sum of every glyph advance,
// which equals `pen − leading_off_px`) so downstream justify / alignment
// math operates on visual width.
//
// Interior-anchor directional mirror (issue #20): the alignment anchor of a
// Center / Right / Decimal tab is measured along the segment's READING axis
// from its leading edge, then placed via the same telescoping identity the
// Left tab uses. For a Decimal tab the separator is an INTERIOR point of the
// following segment, so its offset is measured in VISUAL order and mirrored
// for RTL (`compute_tab_advance`) — without that the `.` of an LTR number
// embedded in RTL text landed ~1 glyph-width short of the stop. `base_dir`
// is the paragraph base direction (threaded from `cfg.base_direction`).
fn apply_tab_advances(
    line: &mut LineBox,
    para_text: &str,
    tab_stops_px: &[(f32, TabKind)],
    leading_off_px: f32,
    base_dir: ShapingDirection,
) {
    if !para_text.contains('\u{0009}') {
        line.width = line_advance(&line.runs);
        return;
    }
    let bytes = para_text.as_bytes();
    let is_tab = |abs_cluster: usize| bytes.get(abs_cluster).copied() == Some(b'\t');

    /* Logical-order index of every glyph: `(run, glyph, absolute source
    cluster byte)`, sorted ascending by the absolute cluster. The sort is
    stable, so ligature glyphs that share a cluster keep their intra-run
    (visual) order — only the cross-direction reordering is undone. */
    let mut order: Vec<(usize, usize, usize)> = line
        .runs
        .iter()
        .enumerate()
        .flat_map(|(ri, run)| {
            let base = run.source_range.start as usize;
            run.glyphs
                .iter()
                .enumerate()
                .map(move |(gi, g)| (ri, gi, base + g.cluster as usize))
        })
        .collect();
    order.sort_by_key(|&(_, _, c)| c);

    let ctx = TabCtx {
        order: &order,
        bytes,
        base_dir,
    };
    let mut pen = leading_off_px;
    for (k, &(ri, gi, abs)) in order.iter().enumerate() {
        if is_tab(abs) {
            let (stop_pos, stop_kind) = next_tab_stop_after(pen, tab_stops_px);
            let advance = compute_tab_advance(pen, stop_pos, stop_kind, line, k, &ctx);
            line.runs[ri].glyphs[gi].x_advance = advance;
            pen += advance;
        } else {
            pen += line.runs[ri].glyphs[gi].x_advance;
        }
    }
    line.width = (pen - leading_off_px).max(0.0);
}

/// Read-only context threaded into [`compute_tab_advance`]: the logical-order
/// glyph index, the paragraph byte buffer, and the base direction. Bundled so
/// the helper stays within the argument-count lint while the (mutable) line
/// is borrowed only per-call.
struct TabCtx<'a> {
    order: &'a [(usize, usize, usize)],
    bytes: &'a [u8],
    base_dir: ShapingDirection,
}

/// L2.1 (#6) / issue #20 — compute the `x_advance` for the tab glyph at
/// logical index `tab_k` (into `ctx.order`) sitting at logical pen `pen`,
/// targeting `stop_pos` with geometric kind `stop_kind`.
///
/// The non-`Left` kinds align an *anchor* of the logically-following segment
/// (the content from `tab_k + 1` to the next tab, or end of line) to the
/// stop:
///
/// * `Right` — the segment's reading-trailing edge.
/// * `Center` — the segment's midpoint.
/// * `Decimal` — the segment's first `.`/`,` separator (falls back to
///   `Right` when absent).
///
/// **Directional mirror.** The segment's reading-LEADING edge (adjacent to
/// the tab) lands at `box_x = pen + A` (LTR, physical-left) /
/// `max_width − (pen + A)` (RTL, physical-right) — the telescoping identity
/// the Left tab uses. So the advance only needs the anchor's offset *along
/// the reading axis* from that leading edge: `A = stop − pen − anchor_off`.
///
/// `Right` (`segment_advance`) and `Center` (`segment_advance / 2`) offsets
/// are direction-symmetric. The `Decimal` separator is an INTERIOR point, so
/// its offset is measured in VISUAL order (`visual_pre` = advances of segment
/// glyphs visually-left of the `.`) and mirrored for RTL: the reading-leading
/// edge is the physical-RIGHT, so the offset is the complement
/// `segment_advance − visual_pre`. The pre-fix code used the LOGICAL pre-`.`
/// advance, which equals `visual_pre` only when the number runs in the base
/// direction — an LTR number embedded in RTL drifted by the post-`.` span.
///
/// `ctx.order` is the `(run, glyph, absolute cluster)` reading-order index
/// built by [`apply_tab_advances`]; `line.runs` is in VISUAL order, so a
/// glyph's `(run, glyph)` locator compares lexicographically as its visual
/// position.
fn compute_tab_advance(
    pen: f32,
    stop_pos: f32,
    stop_kind: TabKind,
    line: &LineBox,
    tab_k: usize,
    ctx: &TabCtx<'_>,
) -> f32 {
    if matches!(stop_kind, TabKind::Left) {
        return (stop_pos - pen).max(MIN_TAB_FILL_PX);
    }

    let want_decimal = matches!(stop_kind, TabKind::Decimal);
    let mut segment_advance = 0.0_f32;
    /* Visual locators + advances of the following segment's glyphs (only
    collected when needed — Decimal — to find the `.`'s visual offset). */
    let mut seg: Vec<(usize, usize, f32)> = Vec::new();
    let mut dot: Option<(usize, usize)> = None;
    /* Walk forward in LOGICAL order from the glyph after this tab, until the
    next tab (exclusive) or end of line. Using `order` (not the raw run/glyph
    nesting) is what makes multi-tab RTL / mixed-bidi lines measure the
    correct logical segment — the visually-next tab is the logically-PREVIOUS
    one in RTL. */
    for &(ri, gi, abs) in &ctx.order[tab_k + 1..] {
        if ctx.bytes.get(abs).copied() == Some(b'\t') {
            break;
        }
        let adv = line.runs[ri].glyphs[gi].x_advance;
        if want_decimal {
            if dot.is_none() {
                let ch = ctx.bytes.get(abs).copied();
                if ch == Some(b'.') || ch == Some(b',') {
                    dot = Some((ri, gi));
                }
            }
            seg.push((ri, gi, adv));
        }
        segment_advance += adv;
    }

    let rtl = matches!(ctx.base_dir, ShapingDirection::Rtl);
    /* Offset of the alignment anchor from the segment's reading-leading edge. */
    let anchor_off = match stop_kind {
        TabKind::Left => 0.0, // unreachable — early return above.
        TabKind::Right => segment_advance,
        TabKind::Center => segment_advance / 2.0,
        TabKind::Decimal => match dot {
            Some(dloc) => {
                let visual_pre: f32 = seg
                    .iter()
                    .filter(|&&(ri, gi, _)| (ri, gi) < dloc)
                    .map(|&(_, _, adv)| adv)
                    .sum();
                if rtl {
                    segment_advance - visual_pre
                } else {
                    visual_pre
                }
            }
            /* No separator → Word's documented fallback to Right semantics. */
            None => segment_advance,
        },
    };

    (stop_pos - pen - anchor_off).max(MIN_TAB_FILL_PX)
}

/// Audit gap A.M3 / L2.1 (#6) — choose the next tab stop strictly past
/// `pen_x`. Custom stops win when one fits; otherwise the default
/// half-inch grid (`DEFAULT_TAB_GRID_PT`) ceiling-rounded. Grid stops
/// are always `Left` — Word does not allow kind overrides on default-
/// grid stops.
fn next_tab_stop_after(pen_x: f32, custom: &[(f32, TabKind)]) -> (f32, TabKind) {
    let custom_next = custom
        .iter()
        .copied()
        .filter(|&(p, _)| p > pen_x + 0.001)
        .min_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    if let Some(stop) = custom_next {
        return stop;
    }
    /* Grid fallback. `(pen / grid).floor() + 1` jumps to the next
    multiple of the grid step regardless of how close pen is. */
    let grid = DEFAULT_TAB_GRID_PT;
    (((pen_x / grid).floor() + 1.0) * grid, TabKind::Left)
}

/// Compute one line's `(ascent, descent)` — the line box's height split at
/// the baseline. The configured `line_height` is the author's target
/// (the `<w:spacing w:line>`-equivalent at the engine boundary, matching
/// Word's `lineRule="auto"`): the line box matches it, with the baseline
/// placed so font ascender / descender fit inside.
///
/// Naskh fonts (Amiri, Noto Naskh) ship intrinsic ascent + descent of
/// 2-3× the em size to leave room for tashkeel + verse marks. Before the
/// original fix the layer used the raw font metrics directly — a 24 pt
/// Amiri paragraph rendered at ~70 CSS px per line instead of the
/// configured 36 pt, blowing the page out vertically.
///
/// Policy (mirrors Word's default Auto rule, with a floor for huge runs):
/// - The line box is **at least** `line_height` tall, with baseline at
///   `0.82 × line_height` from the top when font metrics fit inside.
/// - When a run's resolved font envelope (`max ascent + max descent`)
///   exceeds `line_height` — e.g. a 300 pt font in a 36 pt paragraph
///   — the line **expands to fit the envelope** instead of clipping
///   into the top margin or overlapping the next line. The baseline
///   sits at the run's max ascent so the glyph never crosses the line's
///   top edge.
/// - If `line_height` is zero (defensive), fall back to font metrics.
/// - Phase 7 inline images grow the line above `line_height` when an
///   image's height exceeds the box.
///
/// Closes Issue #22 symptoms (1) and (2): top-padding clip + overlapping
/// line wrap on massive `font_size`.
fn line_extents(line: &LineBox, fonts: &FontStack, line_height: f32) -> (f32, f32) {
    let mut font_ascent = 0.0_f32;
    let mut font_descent = 0.0_f32;
    let mut inline_image_h = 0.0_f32;
    for run in &line.runs {
        if let Some(face) = fonts.face(&run.font) {
            let m = face.metrics(run.attrs.px_size);
            font_ascent = font_ascent.max(m.ascent);
            font_descent = font_descent.max(m.descent.abs());
        }
        /* Phase 7 — inline images can grow the line beyond `line_height`. */
        for g in &run.glyphs {
            if g.inline_image_rel_id.is_some() {
                inline_image_h = inline_image_h.max(g.inline_object_height);
            }
        }
    }

    /* Defensive fallback: callers pass `cfg.line_height * scale` which is
    always > 0 in production, but if it slipped to 0, use font metrics. */
    if line_height <= 0.0 {
        if font_ascent + font_descent <= 0.0 {
            return (0.0, 0.0);
        }
        return (font_ascent.max(inline_image_h), font_descent);
    }

    /* Issue #22 — the line must accommodate the tallest glyph in any
    of its runs. When `font_ascent + font_descent > line_height` the
    `line_height` target alone clips the glyph into the previous line
    or the top margin. Take the max so the line box grows under massive
    font sizes; small sizes stay on the original Auto path. */
    let glyph_envelope = font_ascent + font_descent;
    let inline_floor = inline_image_h + line_height * 0.18;
    let total = line_height.max(glyph_envelope).max(inline_floor);

    if glyph_envelope > line_height {
        /* Big glyph — baseline at the run's actual ascent so the top of
        the glyph aligns with the line box top. Descender fills the
        remainder; if the total grew further (inline image floor),
        the slack goes to the descent band. */
        let ascent = font_ascent.max(inline_image_h);
        let descent = (total - ascent).max(font_descent);
        return (ascent, descent);
    }

    /* Small glyph (the original Word Auto path): baseline at 82% of the
    line box height. */
    let ascent = (total * 0.82).max(inline_image_h);
    let descent = total - ascent;
    (ascent, descent)
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
/// Leading-edge offset applied to one line for `<w:ind w:firstLine>` /
/// `<w:hanging>`.
///
/// **The Golden Rule.** The first-line indent (and the hanging pull-back)
/// apply to the paragraph's absolute first line ONLY — visual index 0.
/// Every continuation line — whether produced by soft wrapping or by an
/// explicit soft break (Shift+Enter, U+2028) — hugs `indent_start` and
/// receives zero first-line offset, mirroring Word / Google Docs. This is
/// a pure function of the visual line index so the rule is unit-testable
/// without shaping a single glyph.
fn first_line_offset_px(line_index: usize, first_line_px: f32, hanging_px: f32) -> f32 {
    if line_index == 0 {
        first_line_px - hanging_px
    } else {
        0.0
    }
}

fn compose_lines(cfg: &ParagraphConfig<'_>) -> Vec<(LineBox, bool)> {
    if cfg.text.is_empty() {
        return vec![];
    }
    let breaks = break_opportunities(cfg.text);
    let segments = soft_break_segments(cfg.text);

    /* Fast path — no soft break (the overwhelmingly common case). One hard
    segment spanning the whole text; delegate straight to the width-breaker
    so output stays byte-identical to the pre-soft-break composer and the
    visual-diff goldens hold at 0.000 %. */
    if segments.len() == 1 {
        return compose_width_lines(cfg, &breaks, 0, cfg.text.len());
    }

    /* Each U+2028 LINE SEPARATOR (inserted by Shift+Enter) forces a line
    break WITHOUT splitting the paragraph block. Width-break every hard
    segment independently and concatenate. The separator is excluded from
    every segment's byte range (see `soft_break_segments`), so it never
    shapes to a glyph. The line that ends a non-final segment ended at an
    explicit break, not a width fill — flag it `broke == false` so justify
    leaves it ragged (same treatment as a paragraph's final line). The key
    consequence for indentation: every line a soft break produces lands at
    visual index `i > 0` in `layout_paragraph`, so `first_line_indent`
    applies to the paragraph's absolute first line only. */
    let mut lines: Vec<(LineBox, bool)> = vec![];
    let last_seg = segments.len() - 1;
    for (si, &(lo, hi)) in segments.iter().enumerate() {
        let mut seg_lines = compose_width_lines(cfg, &breaks, lo, hi);
        if seg_lines.is_empty() {
            /* Empty hard segment (doubled separators, or a soft break at the
            very start / end) — emit a zero-width placeholder line so the
            caret can rest on the blank line. */
            seg_lines.push((build_line(cfg, lo, hi), false));
        }
        if si != last_seg {
            if let Some(last_line) = seg_lines.last_mut() {
                last_line.1 = false;
            }
        }
        lines.extend(seg_lines);
    }
    lines
}

/// Rendered byte-ranges of `text`, split at each U+2028 LINE SEPARATOR
/// (the soft-break character a Shift+Enter inserts). The separator itself
/// is excluded from every range so [`build_line`] never shapes it to a
/// glyph. Always returns at least one range; a trailing or doubled
/// separator yields an empty range so the caret can rest on the blank
/// line it produces. With no separator present the result is the single
/// range `[(0, text.len())]` — the composer's fast path.
fn soft_break_segments(text: &str) -> Vec<(usize, usize)> {
    const LINE_SEP: char = '\u{2028}';
    let sep_len = LINE_SEP.len_utf8();
    let mut out: Vec<(usize, usize)> = Vec::new();
    let mut seg_start = 0_usize;
    for (pos, _) in text.match_indices(LINE_SEP) {
        out.push((seg_start, pos));
        seg_start = pos + sep_len;
    }
    out.push((seg_start, text.len()));
    out
}

/// Greedy width-based line breaking over the byte range `[lo, hi)` of
/// `cfg.text`, using the precomputed `breaks` (UAX #14 opportunities over
/// the whole text, filtered to this range). Returns each line paired with
/// whether it ended at a break opportunity (`true`) rather than the end of
/// the range (`false`) — only opportunity-broken non-final lines are
/// justified.
///
/// Each inter-break segment is measured exactly once and accumulated into a
/// running line total — so the greedy walk is O(breaks) per range, not the
/// O(breaks²) of re-measuring every growing prefix. Segment widths sum
/// cleanly because break opportunities fall on spaces, where no ligature,
/// kerning or cursive join crosses the boundary.
fn compose_width_lines(
    cfg: &ParagraphConfig<'_>,
    breaks: &[usize],
    lo: usize,
    hi: usize,
) -> Vec<(LineBox, bool)> {
    if lo >= hi {
        return vec![];
    }

    let mut lines: Vec<(LineBox, bool)> = vec![];
    let mut start = lo;
    let mut last_fit_end = start;
    /* `line_width` is the accumulated width of `[start..seg_from]` — the run
    of segments already accepted onto the current line. */
    let mut seg_from = start;
    let mut line_width = 0.0_f32;

    for &b in breaks.iter() {
        /* Bound the opportunities to this hard segment: anything at or
        before the line start is behind us; anything past `hi` belongs to
        a later segment (a soft break splits them). */
        if b <= start || b > hi {
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

    if start < hi {
        /* The final tail may itself overflow (e.g. a giant trailing
        URL) — keep force-breaking until it fits. */
        while start < hi {
            let end = hi;
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
            let sub_text_raw = &brun_text[rel_start..rel_end];
            /* L1.3 (#4) — two shape-time substitutions:
            * `caps_transform` upper-cases the slice. Byte-length
              changes (German `ß → SS`, ligature `ﬁ → FI`, etc.) are
              handled via `transform_for_shape`'s per-byte
              transformed→source map, so glyph clusters land on
              source char boundaries downstream.
            * U+0009 TAB → U+0020 SPACE keeps the tab anchor in
              `para.text` for the writer while giving the shaper a
              non-`.notdef` glyph. */
            let needs_tab_sub = sub_text_raw.contains('\u{0009}');
            let (sub_text_cow, cluster_map) =
                transform_for_shape(sub_text_raw, span.caps_transform, needs_tab_sub);
            let sub_text: &str = &sub_text_cow;
            let shaped = shape_text(face, sub_text, brun.direction, span.px_size);
            let brun_abs_start = brun_abs + rel_start as u32;
            let glyphs: Vec<PositionedGlyph> = shaped
                .glyphs
                .iter()
                .map(|g| {
                    /* Phase 7 — paragraph-absolute byte offset of this
                    glyph's cluster. The shaper reports cluster relative
                    to the input string we passed (`sub_text`); for
                    caps/tab-substituted slices `cluster_map` remaps
                    transformed-byte → source-byte so the cluster always
                    lands on a UTF-8 char boundary in the original text.
                    A glyph sitting on the U+FFFC sentinel of an inline
                    object has its advance overridden to the object's
                    reserved width and its `inline_object_id` set so the
                    renderer paints the bitmap instead of the placeholder
                    glyph. */
                    let cluster_src = match cluster_map.as_deref() {
                        Some(map) => map
                            .get(g.cluster as usize)
                            .copied()
                            .unwrap_or(sub_text_raw.len() as u32),
                        None => g.cluster,
                    };
                    let abs_cluster = brun_abs_start + cluster_src;
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
                        cluster: cluster_src,
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
                    baseline_shift_px: span.baseline_shift_px,
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
        /* The line's source offset — load-bearing when the range is empty
        (a soft-break placeholder line) and `runs` carries no byte info. */
        source_start: start as u32,
    }
}

/// Sum of every glyph advance across a line's runs.
fn line_advance(runs: &[VisualRun]) -> f32 {
    runs.iter()
        .flat_map(|r| &r.glyphs)
        .map(|g| g.x_advance)
        .sum()
}

/// L1.3 (#4) — apply caps and/or tab-to-space substitution to `src`,
/// returning the transformed text plus an optional per-byte
/// transformed→source map.
///
/// The shaper reports cluster offsets relative to the slice it was
/// handed; when caps expands a character to a multi-byte run (German
/// `ß → SS`, Latin ligatures `ﬁ → FI`, etc.) every byte of the
/// expansion maps back to the SOURCE byte offset of the originating
/// char. Downstream consumers that index `source_text` by cluster
/// (caret rendering, hit-testing, BiDi reordering, revision overlays)
/// therefore always land on a UTF-8 char boundary.
///
/// Tab substitution preserves byte alignment (`\t` and ` ` are both
/// 1-byte ASCII), so when caps is off the map is unnecessary.
///
/// Returns `(Cow::Borrowed(src), None)` for the no-op case to avoid
/// allocation on the 99% path.
fn transform_for_shape(
    src: &str,
    caps: bool,
    tabs_to_space: bool,
) -> (Cow<'_, str>, Option<Vec<u32>>) {
    if !caps && !tabs_to_space {
        return (Cow::Borrowed(src), None);
    }
    if !caps {
        return (Cow::Owned(src.replace('\t', " ")), None);
    }
    /* Caps path — must construct the transformed→source byte map. */
    let mut upper = String::with_capacity(src.len());
    let mut map: Vec<u32> = Vec::with_capacity(src.len());
    for (src_byte_idx, ch) in src.char_indices() {
        let normalized = if tabs_to_space && ch == '\t' { ' ' } else { ch };
        for upper_ch in normalized.to_uppercase() {
            let before = upper.len();
            upper.push(upper_ch);
            for _ in before..upper.len() {
                map.push(src_byte_idx as u32);
            }
        }
    }
    (Cow::Owned(upper), Some(map))
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
            let sub_raw = &text[(cursor - abs_start) as usize..(piece_end - abs_start) as usize];
            /* L1.3 (#4) — keep the greedy width probe in sync with
            `build_line`'s shape input via the shared
            `transform_for_shape` helper. Width probe ignores the
            cluster map (only the total advance matters here). */
            let needs_tab_sub = sub_raw.contains('\u{0009}');
            let (sub_cow, _) = transform_for_shape(sub_raw, span.caps_transform, needs_tab_sub);
            total += shape_text(face, &sub_cow, direction, span.px_size).total_advance;
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

#[cfg(test)]
mod tests {
    use super::*;

    const LINE_SEP: &str = "\u{2028}";

    #[test]
    fn soft_break_segments_no_separator_is_single_full_range() {
        /* The fast path: text without a soft break yields exactly one
        range spanning the whole string, so `compose_lines` delegates
        straight to the width-breaker — byte-identical to the old path. */
        assert_eq!(soft_break_segments("hello world"), vec![(0, 11)]);
        assert_eq!(soft_break_segments(""), vec![(0, 0)]);
    }

    #[test]
    fn soft_break_segments_splits_and_excludes_separator() {
        /* "a\u{2028}b": separator is 3 bytes at offset 1. Rendered ranges
        are [0,1) = "a" and [4,5) = "b" — the separator bytes [1,4) are
        excluded from BOTH, so it never shapes to a glyph. */
        let s = format!("a{LINE_SEP}b");
        assert_eq!(soft_break_segments(&s), vec![(0, 1), (4, 5)]);
    }

    #[test]
    fn soft_break_segments_trailing_separator_adds_empty_range() {
        /* "a\u{2028}" → "a" then an EMPTY range so a blank line exists for
        the caret to rest on after the break. */
        let s = format!("a{LINE_SEP}");
        assert_eq!(soft_break_segments(&s), vec![(0, 1), (4, 4)]);
    }

    #[test]
    fn soft_break_segments_leading_and_doubled_separators_are_empty_ranges() {
        /* Leading break → empty first range. */
        let lead = format!("{LINE_SEP}b");
        assert_eq!(soft_break_segments(&lead), vec![(0, 0), (3, 4)]);
        /* Doubled break → empty middle range (a blank line between). */
        let doubled = format!("a{LINE_SEP}{LINE_SEP}b");
        assert_eq!(soft_break_segments(&doubled), vec![(0, 1), (4, 4), (7, 8)]);
    }

    #[test]
    fn first_line_offset_applies_to_absolute_first_line_only() {
        /* THE GOLDEN RULE. Line 0 gets the full first-line indent (minus
        any hanging); every later line — soft-wrapped OR forced by a soft
        break — gets exactly zero, so it hugs `indent_start`. */
        assert_eq!(first_line_offset_px(0, 20.0, 0.0), 20.0);
        assert_eq!(first_line_offset_px(1, 20.0, 0.0), 0.0);
        assert_eq!(first_line_offset_px(2, 20.0, 0.0), 0.0);
        /* Hanging indent: line 0 pulls BACK by the hanging amount; later
        lines still get zero offset (they sit at `indent_start`). */
        assert_eq!(first_line_offset_px(0, 0.0, 18.0), -18.0);
        assert_eq!(first_line_offset_px(1, 0.0, 18.0), 0.0);
    }

    #[test]
    fn transform_for_shape_noop_borrows() {
        let (text, map) = transform_for_shape("hello", false, false);
        assert!(matches!(text, Cow::Borrowed(_)));
        assert_eq!(text.as_ref(), "hello");
        assert!(map.is_none());
    }

    #[test]
    fn transform_for_shape_tabs_only_skips_map() {
        let (text, map) = transform_for_shape("a\tb", false, true);
        assert_eq!(text.as_ref(), "a b");
        assert!(
            map.is_none(),
            "tab subs are byte-aligned, no cluster remap needed"
        );
    }

    #[test]
    fn transform_for_shape_maps_german_sharp_s() {
        /* `Straße` = S t r a ß e at byte indices 0,1,2,3,4,6
        (ß is 2 UTF-8 bytes at byte index 4). Uppercase expands
        ß → "SS" so the transformed text is 7 ASCII bytes "STRASSE";
        the two "SS" bytes both map back to source byte index 4. */
        let (text, map) = transform_for_shape("Straße", true, false);
        assert_eq!(text.as_ref(), "STRASSE");
        let map = map.expect("caps path returns map");
        assert_eq!(map, vec![0, 1, 2, 3, 4, 4, 6]);
    }

    #[test]
    fn transform_for_shape_maps_ligature_expansion() {
        /* `ﬁle`: ﬁ (U+FB01) is 3 UTF-8 bytes at index 0, then `l`
        at index 3, `e` at index 4. ﬁ uppercases to "FI" (2 ASCII
        bytes), so the transformed string is 4 bytes "FILE" and the
        first two bytes both map back to source byte 0. */
        let (text, map) = transform_for_shape("ﬁle", true, false);
        assert_eq!(text.as_ref(), "FILE");
        let map = map.expect("caps path returns map");
        assert_eq!(map, vec![0, 0, 3, 4]);
    }

    #[test]
    fn transform_for_shape_greek_final_sigma() {
        /* `ως` (omega + final sigma): ω at byte 0 (2 bytes),
        ς at byte 2 (2 bytes). Uppercase → ΩΣ, same byte width.
        Map is identity in source-space. */
        let (text, map) = transform_for_shape("ως", true, false);
        assert_eq!(text.as_ref(), "ΩΣ");
        let map = map.expect("caps path returns map");
        assert_eq!(map, vec![0, 0, 2, 2]);
    }

    #[test]
    fn transform_for_shape_caps_with_tabs() {
        /* `a\tß` — tab at byte 1, ß at byte 2 (2 bytes). With caps
        + tab subs: tab → ' ', ß → "SS". Transformed = "A SS",
        map = [0,1,2,2]. */
        let (text, map) = transform_for_shape("a\tß", true, true);
        assert_eq!(text.as_ref(), "A SS");
        let map = map.expect("caps path returns map");
        assert_eq!(map, vec![0, 1, 2, 2]);
    }

    /* ---------- L2.1 (#6) tab stops ---------- */

    fn test_glyph(cluster: u32, x_advance: f32) -> PositionedGlyph {
        PositionedGlyph {
            id: 0,
            cluster,
            x_advance,
            y_advance: 0.0,
            x_offset: 0.0,
            y_offset: 0.0,
            synthetic: false,
            inline_image_rel_id: None,
            inline_footnote_marker: None,
            inline_object_height: 0.0,
        }
    }

    fn test_attrs() -> TextAttrs {
        TextAttrs {
            px_size: 12.0,
            color: [0, 0, 0, 255],
            faux_bold: false,
            faux_italic: false,
            underline: engine::UnderlineStyle::None,
            strike: false,
            bg_color: None,
            baseline_shift_px: 0.0,
        }
    }

    /// Build a one-run LineBox where each `(cluster_byte, x_advance)`
    /// pair describes one glyph in visual order. `source_range` spans
    /// the entire input text so the cluster lookup in
    /// `apply_tab_advances` resolves against `text`.
    fn line_with_glyphs(text: &str, glyphs: &[(u32, f32)]) -> LineBox {
        let runs = vec![VisualRun {
            glyphs: glyphs
                .iter()
                .copied()
                .map(|(c, a)| test_glyph(c, a))
                .collect(),
            font: "test".to_string(),
            direction: ShapingDirection::Ltr,
            source_range: 0..text.len() as u32,
            attrs: test_attrs(),
        }];
        LineBox {
            origin: Point::default(),
            baseline: 0.0,
            height: 0.0,
            width: 0.0,
            runs,
            alignment: Alignment::Start,
            source_start: 0,
        }
    }

    fn assert_close(actual: f32, expected: f32, label: &str) {
        let delta = (actual - expected).abs();
        assert!(
            delta < 0.001,
            "{label}: expected {expected}, got {actual} (Δ {delta})"
        );
    }

    #[test]
    fn tab_pen_starts_at_indent_for_indented_ltr() {
        /* Pen starts at `leading_off_px = 20`. Tab at line-local 0
        becomes paragraph-relative 20; next stop after 20 is 100;
        tab.x_advance = 80 so the next glyph lands at paragraph
        position 100. */
        let mut line = line_with_glyphs("\tA", &[(0, 0.0), (1, 10.0)]);
        let stops = [(100.0_f32, TabKind::Left)];
        apply_tab_advances(&mut line, "\tA", &stops, 20.0, ShapingDirection::Ltr);

        let tab_adv = line.runs[0].glyphs[0].x_advance;
        assert_close(tab_adv, 80.0, "tab fills from 20 to 100");
        /* Line.width = paragraph-relative pen (110) - leading_off (20) = 90. */
        assert_close(line.width, 90.0, "line width is line-local extent");
    }

    #[test]
    fn tab_kind_center_lands_segment_midpoint_at_stop() {
        /* text = "X\tWORD"; X has advance 10. Tab kind = Center.
        Stop = 100. Segment "WORD" advance = 40 (4 glyphs × 10).
        Expected: tab.x_advance = 100 - 10 - 40/2 = 70.
        Segment midpoint then lands at pen=80 + 20 = 100. */
        let mut line = line_with_glyphs(
            "X\tWORD",
            &[
                (0, 10.0),
                (1, 0.0),
                (2, 10.0),
                (3, 10.0),
                (4, 10.0),
                (5, 10.0),
            ],
        );
        let stops = [(100.0_f32, TabKind::Center)];
        apply_tab_advances(&mut line, "X\tWORD", &stops, 0.0, ShapingDirection::Ltr);

        let tab_adv = line.runs[0].glyphs[1].x_advance;
        assert_close(tab_adv, 70.0, "Center: tab fills to stop - w/2");
        /* Recompute pen at midpoint of segment for sanity. */
        let pen_after_tab = 10.0 + tab_adv;
        let segment_midpoint = pen_after_tab + 40.0 / 2.0;
        assert_close(segment_midpoint, 100.0, "segment midpoint at stop");
    }

    #[test]
    fn tab_kind_right_lands_segment_right_at_stop() {
        /* Same shape as the Center test but kind = Right. tab.x_advance
        = 100 - 10 - 40 = 50. Segment right edge at 60 + 40 = 100. */
        let mut line = line_with_glyphs(
            "X\tWORD",
            &[
                (0, 10.0),
                (1, 0.0),
                (2, 10.0),
                (3, 10.0),
                (4, 10.0),
                (5, 10.0),
            ],
        );
        let stops = [(100.0_f32, TabKind::Right)];
        apply_tab_advances(&mut line, "X\tWORD", &stops, 0.0, ShapingDirection::Ltr);

        let tab_adv = line.runs[0].glyphs[1].x_advance;
        assert_close(tab_adv, 50.0, "Right: tab fills to stop - w");
        let pen_after_tab = 10.0 + tab_adv;
        let segment_right = pen_after_tab + 40.0;
        assert_close(segment_right, 100.0, "segment right edge at stop");
    }

    #[test]
    fn tab_kind_decimal_lands_separator_at_stop_dot() {
        /* text = "X\t12.5"; "1" + "2" advance to the '.' at cluster 4.
        decimal_offset = 20 (two digits × 10 each). tab.x_advance =
        100 - 10 - 20 = 70. Decimal sits at pen 80 + 20 = 100. */
        let mut line = line_with_glyphs(
            "X\t12.5",
            &[
                (0, 10.0),
                (1, 0.0),
                (2, 10.0),
                (3, 10.0),
                (4, 10.0),
                (5, 10.0),
            ],
        );
        let stops = [(100.0_f32, TabKind::Decimal)];
        apply_tab_advances(&mut line, "X\t12.5", &stops, 0.0, ShapingDirection::Ltr);

        let tab_adv = line.runs[0].glyphs[1].x_advance;
        assert_close(tab_adv, 70.0, "Decimal: tab fills to stop - decimal_offset");
        let pen_after_tab = 10.0 + tab_adv;
        let decimal_pos = pen_after_tab + 20.0;
        assert_close(decimal_pos, 100.0, "decimal separator at stop");
    }

    #[test]
    fn tab_kind_decimal_falls_back_to_right_when_no_separator() {
        /* text = "X\thello"; no '.' / ',' so Decimal falls back to
        Right. tab.x_advance = 100 - 10 - 50 = 40. */
        let mut line = line_with_glyphs(
            "X\thello",
            &[
                (0, 10.0),
                (1, 0.0),
                (2, 10.0),
                (3, 10.0),
                (4, 10.0),
                (5, 10.0),
                (6, 10.0),
            ],
        );
        let stops = [(100.0_f32, TabKind::Decimal)];
        apply_tab_advances(&mut line, "X\thello", &stops, 0.0, ShapingDirection::Ltr);

        let tab_adv = line.runs[0].glyphs[1].x_advance;
        assert_close(
            tab_adv,
            40.0,
            "Decimal without separator falls back to Right",
        );
    }

    #[test]
    fn tab_clamp_when_segment_overflows_stop() {
        /* text = "X\tWORD"; stop = 15 (too close to fit). Center math
        gives 15 - 10 - 20 = -15. Clamp to MIN_TAB_FILL_PX (4.0). */
        let mut line = line_with_glyphs(
            "X\tWORD",
            &[
                (0, 10.0),
                (1, 0.0),
                (2, 10.0),
                (3, 10.0),
                (4, 10.0),
                (5, 10.0),
            ],
        );
        let stops = [(15.0_f32, TabKind::Center)];
        apply_tab_advances(&mut line, "X\tWORD", &stops, 0.0, ShapingDirection::Ltr);

        let tab_adv = line.runs[0].glyphs[1].x_advance;
        assert_close(tab_adv, MIN_TAB_FILL_PX, "overflow clamps to min fill");
    }

    #[test]
    fn tab_left_resolves_in_logical_order_for_rtl_visual_runs() {
        /* RTL line "A\tB": logical order is A(0), tab(1), B(2). After the
        per-line BiDi reorder the glyphs are STORED in visual order
        [B, tab, A] (clusters 2, 1, 0). The tab's fill must be computed
        from the content LOGICALLY before it (A, width 10), NOT the content
        visually before it (B, width 40). Pre-fix the visual-order pen used
        B's width and the Left advance ballooned — the RTL tab explosion. */
        let mut line = line_with_glyphs("A\tB", &[(2, 40.0), (1, 0.0), (0, 10.0)]);
        let stops = [(100.0_f32, TabKind::Left)];
        apply_tab_advances(&mut line, "A\tB", &stops, 0.0, ShapingDirection::Rtl);

        /* Tab glyph is at visual index 1. Logical pen at the tab = width of
        A (10), so fill = 100 - 10 = 90 — NOT 100 - 40 = 60 (the old bug). */
        let tab_adv = line.runs[0].glyphs[1].x_advance;
        assert_close(
            tab_adv,
            90.0,
            "RTL tab fills from the logical pen (A), not visual (B)",
        );
        /* line.width is the sum of advances (order-independent): 40+90+10. */
        assert_close(line.width, 140.0, "line width is the post-fill extent");
    }

    #[test]
    fn tab_right_measures_logical_following_segment_for_rtl() {
        /* RTL line "A\tBC": logical A(0), tab(1), B(2), C(3); stored in
        visual order [C, B, tab, A] (clusters 3, 2, 1, 0). A Right tab must
        measure the LOGICALLY-following segment "BC" (20+20=40), not the
        visually-following glyph A. fill = stop(100) - pen(10) - 40 = 50. */
        let mut line = line_with_glyphs("A\tBC", &[(3, 20.0), (2, 20.0), (1, 0.0), (0, 10.0)]);
        let stops = [(100.0_f32, TabKind::Right)];
        apply_tab_advances(&mut line, "A\tBC", &stops, 0.0, ShapingDirection::Rtl);

        /* Tab glyph is at visual index 2. */
        let tab_adv = line.runs[0].glyphs[2].x_advance;
        assert_close(
            tab_adv,
            50.0,
            "Right tab measures the logical-following segment BC",
        );
    }

    /* Physical-left box-x of the glyph at visual index `idx` under RTL Start
    alignment: the line right-flushes, so line-local 0 sits at
    `content_width − line.width`. Single-run test lines only. */
    fn rtl_start_glyph_left(line: &LineBox, idx: usize, content_width: f32) -> f32 {
        let line_width: f32 = line.runs[0].glyphs.iter().map(|g| g.x_advance).sum();
        let before: f32 = line.runs[0].glyphs[..idx].iter().map(|g| g.x_advance).sum();
        (content_width - line_width) + before
    }

    #[test]
    fn tab_decimal_mirrors_interior_anchor_for_rtl_embedded_ltr_number() {
        /* Issue #20 — RTL Arabic "س" + tab + embedded LTR number "3.5",
        Decimal tab @ 100 in a 200-wide content box. Post-BiDi visual order
        is [3, ., 5, tab, س]; "3.5" is an LTR-internal run, so the decimal
        separator is an INTERIOR anchor of an embedded run. Advances:
        3=10, .=5, 5=10, س=10. Pre-fix used the LOGICAL pre-'.' advance (10)
        → tab fill 80 → '.' landed at box-x 95, one glyph-width (5) short.
        The fix measures the anchor in VISUAL order (visual_pre = 10) and
        mirrors for RTL (anchor_off = 25 − 10 = 15) → fill = 100 − 10 − 15
        = 75. */
        let cw = 200.0_f32;
        let stop = 100.0_f32;
        let mut line = line_with_glyphs(
            "س\t3.5",
            &[(3, 10.0), (4, 5.0), (5, 10.0), (2, 0.0), (0, 10.0)],
        );
        apply_tab_advances(
            &mut line,
            "س\t3.5",
            &[(stop, TabKind::Decimal)],
            0.0,
            ShapingDirection::Rtl,
        );

        /* Tab glyph is at visual index 3. */
        assert_close(
            line.runs[0].glyphs[3].x_advance,
            75.0,
            "RTL decimal tab fill",
        );
        /* Mathematical proof: the '.' (visual index 1) physical-left edge
        must equal the stop column box-x = content_width − stop = 100. */
        let dot_left = rtl_start_glyph_left(&line, 1, cw);
        assert_close(
            dot_left,
            cw - stop,
            "decimal separator lands at the stop column",
        );
    }

    #[test]
    fn tab_center_lands_segment_midpoint_for_rtl_embedded_ltr() {
        /* Same RTL line, Center @ 100. The midpoint is direction-symmetric
        (segment_advance / 2 from either edge), so it was already correct —
        this locks that in. segment "3.5" advance = 25, anchor_off = 12.5,
        fill = 100 − 10 − 12.5 = 77.5; the block's centre lands on the stop. */
        let cw = 200.0_f32;
        let stop = 100.0_f32;
        let mut line = line_with_glyphs(
            "س\t3.5",
            &[(3, 10.0), (4, 5.0), (5, 10.0), (2, 0.0), (0, 10.0)],
        );
        apply_tab_advances(
            &mut line,
            "س\t3.5",
            &[(stop, TabKind::Center)],
            0.0,
            ShapingDirection::Rtl,
        );

        assert_close(
            line.runs[0].glyphs[3].x_advance,
            77.5,
            "RTL center tab fill",
        );
        /* Segment "3.5" is visual glyphs 0..3 (advance 25); its centre is at
        its left edge + 12.5 and must sit on the stop column box-x = 100. */
        let seg_center = rtl_start_glyph_left(&line, 0, cw) + 25.0 / 2.0;
        assert_close(
            seg_center,
            cw - stop,
            "segment midpoint lands at the stop column",
        );
    }

    #[test]
    fn transform_for_shape_cluster_stays_on_char_boundary() {
        /* Every cluster index in the transformed string, when
        remapped, must land on a UTF-8 char boundary in the source.
        Run a small alphabet of expanding cases and verify. */
        for src in &["Straße", "ﬁle", "ﬂip", "ﬀ", "a\tß", "ως"] {
            let (text, map) = transform_for_shape(src, true, true);
            let map = map.expect("caps path returns map");
            for &source_byte in &map {
                assert!(
                    src.is_char_boundary(source_byte as usize),
                    "source byte {source_byte} not a char boundary in {src:?}"
                );
            }
            assert_eq!(map.len(), text.len(), "map covers every transformed byte");
        }
    }
}
