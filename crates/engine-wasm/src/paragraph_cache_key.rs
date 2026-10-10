//! Issue #456 — the paragraph layout cache key, exhaustive by
//! construction (the pattern of `table_cache.rs`, issue #379).
//!
//! The key used to be a hand-kept field list: a new `Paragraph` /
//! `SpanStyle` field silently stayed out of it, and the table cache —
//! which embeds every cell paragraph through this same key — inherited
//! each omission. Now every struct the layout can read is destructured
//! WITHOUT `..`: adding a field is a compile error here until it is
//! either hashed or listed as layout-irrelevant with a reason. The
//! runtime backstop stays [`cached_paragraph_is_consistent`] (issue
//! #87): a key is a prediction, a hit is verified.
//!
//! What "layout" means here is [`layout_paragraph_wrapped_uncached`]
//! and its helpers (`paragraph_layout_spans`, `build_inline_object_infos`,
//! `auto_hyphenation_ranges`). Things the callers stamp onto the box AFTER
//! the cache lookup (`fields`, `borders`, `shading`, the review-mark
//! colour) or stack AROUND it (`spacing`) are not box inputs.

use super::*;
use std::hash::{Hash, Hasher};

/// A fieldless enum has no `Hash` derive in the model: its discriminant
/// is the whole value.
fn hash_disc<T, H: Hasher>(value: &T, h: &mut H) {
    std::mem::discriminant(value).hash(h);
}

/// Every layout-read field of a run style. Used for the cascade base
/// (`sctx.run_base`), each span's direct formatting, and the paragraph
/// mark's style, so a field is classified once for all three.
fn hash_span_style<H: Hasher>(style: &engine::SpanStyle, h: &mut H) {
    let engine::SpanStyle {
        font_size,
        font_size_cs,
        color,
        bold,
        bold_cs,
        italic,
        italic_cs,
        underline,
        strike,
        bg_color,
        font_family,
        font_family_cs,
        // Never read by layout: only `merged_with` copies the name.
        char_style: _,
        caps,
        small_caps,
        vert_align,
        raw_font_family,
        font_theme,
        // The only layout read of the grab bag is `forces_complex_script()`
        // (the `<w:rtl/>` / `<w:cs/>` routing flag), hashed below.
        grab_bag: _,
        font_bindings,
        color_theme,
        // Only decides hyphenation, which the key mixes in through the
        // resolved hyphenatable ranges (and the hyphenator identity).
        lang: _,
    } = style;
    font_size.map(f32::to_bits).hash(h);
    font_size_cs.map(f32::to_bits).hash(h);
    color.hash(h);
    bold.hash(h);
    bold_cs.hash(h);
    italic.hash(h);
    italic_cs.hash(h);
    underline.hash(h);
    strike.hash(h);
    bg_color.hash(h);
    font_family.hash(h);
    font_family_cs.hash(h);
    caps.hash(h);
    small_caps.hash(h);
    vert_align.as_ref().map(std::mem::discriminant).hash(h);
    raw_font_family.hash(h);
    font_theme.hash(h);
    font_bindings.hash(h);
    color_theme.hash(h);
    style.forces_complex_script().hash(h);
}

fn hash_indent<H: Hasher>(indent: &engine::Indent, h: &mut H) {
    let engine::Indent {
        start_twips,
        end_twips,
        first_line_twips,
        hanging_twips,
    } = indent;
    start_twips.hash(h);
    end_twips.hash(h);
    first_line_twips.hash(h);
    hanging_twips.hash(h);
}

fn hash_float_anchor<H: Hasher>(anchor: &engine::FloatAnchor, h: &mut H) {
    let engine::FloatAnchor {
        position_h,
        position_v,
        simple_pos,
        simple_pos_x_emu,
        simple_pos_y_emu,
        relative_height,
        behind_doc,
        // Editing locks / table-cell anchoring / overlap policy: pagination
        // and the editor read them, `float_spec_from_anchor` /
        // `float_wrap_from_anchor` (the layout lowering) do not.
        locked: _,
        layout_in_cell: _,
        allow_overlap: _,
        hidden,
        dist_top_emu,
        dist_bottom_emu,
        dist_left_emu,
        dist_right_emu,
        wrap,
        wrap_text,
        wrap_polygon,
        // Verbatim source markup for the writer.
        wrap_xml: _,
        doc_pr_xml: _,
    } = anchor;
    position_h.hash(h);
    position_v.hash(h);
    simple_pos.hash(h);
    simple_pos_x_emu.hash(h);
    simple_pos_y_emu.hash(h);
    relative_height.hash(h);
    behind_doc.hash(h);
    hidden.hash(h);
    /* The wrap contract rides the object to pagination for every kind
    (`float_wrap_from_anchor`), not only text boxes. */
    dist_top_emu.hash(h);
    dist_bottom_emu.hash(h);
    dist_left_emu.hash(h);
    dist_right_emu.hash(h);
    wrap.hash(h);
    wrap_text.hash(h);
    wrap_polygon.hash(h);
}

fn hash_inline_object<H: Hasher>(io: &engine::InlineObject, h: &mut H) {
    let engine::InlineObject {
        at,
        kind,
        anchor,
        // Verbatim source markup for the writer.
        source_xml: _,
    } = io;
    at.hash(h);
    match kind {
        engine::InlineKind::Image {
            rel_id,
            width_emu,
            height_emu,
            media_key,
        } => {
            1u8.hash(h);
            rel_id.hash(h);
            /* Issue #188 — the laid-out glyph carries the media key. */
            media_key.hash(h);
            width_emu.hash(h);
            height_emu.hash(h);
        }
        engine::InlineKind::FootnoteRef {
            id,
            custom_mark_follows,
        } => {
            2u8.hash(h);
            id.hash(h);
            custom_mark_follows.hash(h);
        }
        engine::InlineKind::EndnoteRef {
            id,
            custom_mark_follows,
        } => {
            3u8.hash(h);
            id.hash(h);
            custom_mark_follows.hash(h);
        }
        /* Issue #80 — the self-mark's resolved text is mixed in below. */
        engine::InlineKind::NoteSelfRef { kind } => {
            4u8.hash(h);
            matches!(kind, engine::NoteKind::Endnote).hash(h);
        }
        /* Issue #83 — the story rides the sentinel glyph, so its content
        is a layout input of the HOST paragraph. */
        engine::InlineKind::TextBox {
            width_emu,
            height_emu,
            story,
        } => {
            5u8.hash(h);
            text_box_key(story, *width_emu, *height_emu).hash(h);
        }
        /* Issue #357. */
        engine::InlineKind::Symbol { font, char } => {
            6u8.hash(h);
            font.hash(h);
            char.hash(h);
        }
        engine::InlineKind::PositionalTab {
            alignment,
            relative_to,
            leader,
        } => {
            7u8.hash(h);
            alignment.hash(h);
            relative_to.hash(h);
            leader.hash(h);
        }
    }
    match anchor.as_deref() {
        None => 0u8.hash(h),
        Some(a) => {
            1u8.hash(h);
            hash_float_anchor(a, h);
        }
    }
}

/// Content + render-config hash that keys the paragraph layout cache
/// (Backlog #13). Two paragraphs hash equal only when `layout_paragraph`
/// would produce identical boxes. `scale` is the value passed to
/// `build_page` — PDF export lays out at `1.0` regardless of the cached
/// device scale — so it is hashed explicitly.
pub(crate) fn paragraph_layout_key(
    para: &engine::Paragraph,
    cfg: &RenderConfig,
    scale: f32,
    max_width_px: f32,
    sctx: StyleContext,
) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();

    /* ---- the paragraph: no `..`, every field classified ---- */
    let engine::Paragraph {
        text,
        spans,
        props,
        // The numbering resolver turns it into `resolved_marker` /
        // `resolved_list_indent` (hashed); layout reads only those.
        list_item: _,
        resolved_marker,
        resolved_list_indent,
        // Writer passthrough flags / source bytes.
        dirty: _,
        source_xml: _,
        inline_objects,
        hyperlinks,
        revisions,
        // Evaluated into the text (`splice_resolved_fields_into`) before
        // layout; the box only gets the ranges stamped on afterwards.
        fields: _,
        style_id,
        // Already folded into the resolved `props` (cascade ∪ overrides).
        direct_overrides: _,
        // Pagination (section break), not a box input.
        section_end: _,
        // Bookmark anchors paint nothing and move no glyph.
        bookmarks: _,
        body_xml: _,
        source_markup: _,
        // Review-mark pilcrow tint: stamped onto the box after the cache
        // lookup (`review_mark_color`), paint-only.
        mark_revisions: _,
        mark_style,
    } = para;

    text.hash(&mut h);

    /* ---- paragraph properties ---- */
    let engine::ParaProperties {
        alignment,
        indent,
        // Stacked around the box by `build_pages` / the cell stacker.
        spacing: _,
        direction,
        line_height,
        // Pagination (keep chains, page breaks, widow control).
        keep_next: _,
        keep_lines: _,
        page_break_before: _,
        // Border / shading rules are stamped onto the box after the lookup.
        borders: _,
        border_spelling: _,
        tab_stops,
        // See `Paragraph::list_item` above.
        list_item: _,
        shading: _,
        shading_pattern: _,
        grab_bag: _,
        // Navigation / TOC metadata.
        outline_level: _,
        widow_control: _,
        suppress_auto_hyphens,
    } = props;
    engine_align_disc(*alignment).hash(&mut h);
    /* Phase 9c — paragraph base direction is part of the layout contract
    (`resolve_base_direction` prefers the explicit override). */
    match direction {
        None => 0u8.hash(&mut h),
        Some(engine::TextDirection::Ltr) => 1u8.hash(&mut h),
        Some(engine::TextDirection::Rtl) => 2u8.hash(&mut h),
    }
    hash_indent(indent, &mut h);
    match line_height {
        None => 0u8.hash(&mut h),
        Some(engine::LineHeight::Auto { twips }) => {
            1u8.hash(&mut h);
            twips.hash(&mut h);
        }
        Some(engine::LineHeight::Exact { twips }) => {
            2u8.hash(&mut h);
            twips.hash(&mut h);
        }
        Some(engine::LineHeight::AtLeast { twips }) => {
            3u8.hash(&mut h);
            twips.hash(&mut h);
        }
    }
    suppress_auto_hyphens.hash(&mut h);
    /* Audit gap A.M3 / issue #346 — tab stops steer glyph advances in the
    line builder's post-pass: position AND kind AND leader (a left → right
    change at the same position must not collide). */
    (tab_stops.len() as u64).hash(&mut h);
    for stop in tab_stops {
        let engine::TabStop {
            position_pt,
            kind,
            leader,
        } = stop;
        position_pt.to_bits().hash(&mut h);
        hash_disc(kind, &mut h);
        leader.hash(&mut h);
    }

    /* Issue #50 — the marker text and the numbering level's indent are
    layout inputs (`marker_text` + `effective_layout_indents`). */
    resolved_marker.hash(&mut h);
    match resolved_list_indent {
        None => 0u8.hash(&mut h),
        Some(li) => {
            1u8.hash(&mut h);
            hash_indent(li, &mut h);
        }
    }

    /* ---- run styles ---- */
    /* Issue #29/#21 — the resolved run-cascade base feeds span
    materialization, so two style-table states must never collide on a
    key. `style_id` itself is covered by what it resolves to. */
    hash_span_style(&sctx.run_base(style_id.as_deref()), &mut h);
    /* Issue #355 — the theme decides what a font binding names. */
    let StyleContext {
        // Reached through `run_base` / the marker texts hashed below.
        styles: _,
        run_defaults: _,
        note_markers: _,
        theme: _,
        theme_key,
        note_self_mark,
        settings: _,
        word_line_metrics,
    } = sctx;
    theme_key.hash(&mut h);
    /* Issue #370 — an EMPTY paragraph's line is sized by its mark (the
    pilcrow's size / face). Mixed in only for empty paragraphs, so every
    paragraph with text keeps its key. */
    if text.is_empty()
        && let Some(mark) = mark_style.as_deref()
    {
        0x70_u8.hash(&mut h);
        hash_span_style(mark, &mut h);
    }
    /* Issue #329 — the line-pitch model (Word's font-derived pitch for a
    document read from a Word package), and under it the paragraph mark's
    face, which sizes any line with no text run (a doubled soft break, not
    only an empty paragraph). */
    word_line_metrics.hash(&mut h);
    if word_line_metrics && let Some(mark) = mark_style.as_deref() {
        0x329_u16.hash(&mut h);
        hash_span_style(mark, &mut h);
    }
    /* Audit gap A.H2 — the laid-out max width: the same paragraph at
    page-wide vs column-narrow widths breaks differently. */
    max_width_px.to_bits().hash(&mut h);
    (spans.len() as u64).hash(&mut h);
    for run in spans {
        let engine::StyleRun { start, end, style } = run;
        start.hash(&mut h);
        end.hash(&mut h);
        hash_span_style(style, &mut h);
    }

    /* Issue #69 (and #44) — inline objects are layout inputs. Undo / redo
    / recover swap whole trees WITHOUT the explicit `layout_cache.clear()`
    the resize / move commands do, so the key itself must see every one of
    them. */
    (inline_objects.len() as u64).hash(&mut h);
    for io in inline_objects {
        hash_inline_object(io, &mut h);
    }

    /* Phase 7 — hyperlinks change the span overlay; an internal
    (`#anchor`) link renders in the run's own look, so the target's
    internal-ness is an input, not only the range. */
    (hyperlinks.len() as u64).hash(&mut h);
    for hl in hyperlinks {
        let engine::Hyperlink {
            start,
            end,
            target,
            // Verbatim `<w:hyperlink>` attributes for the writer.
            attrs: _,
        } = hl;
        start.hash(&mut h);
        end.hash(&mut h);
        target.starts_with('#').hash(&mut h);
    }
    /* Phase 8b / issues #247, #307 — revisions overlay the spans, and each
    kind paints differently (Insert / Delete / Move* / FormatChange): the
    full discriminant, not "insert or not". */
    (revisions.len() as u64).hash(&mut h);
    for r in revisions {
        let engine::Revision {
            start,
            end,
            kind,
            // Review-pane metadata; the overlay paints one tint per kind.
            author: _,
            date: _,
            id: _,
            prev_attrs: _,
            move_name: _,
        } = r;
        start.hash(&mut h);
        end.hash(&mut h);
        hash_disc(kind, &mut h);
    }

    /* Issue #80 — note markers are shaped into the line, so the resolved
    display text of every reference (and the self-mark of a note body) is
    a layout input: inserting a note renumbers every later reference
    without touching its paragraph's text. */
    for obj in inline_objects {
        match &obj.kind {
            engine::InlineKind::FootnoteRef { id, .. } => {
                1u8.hash(&mut h);
                sctx.note_marker_text(engine::NoteAnchor {
                    kind: engine::NoteKind::Footnote,
                    id: *id,
                })
                .hash(&mut h);
            }
            engine::InlineKind::EndnoteRef { id, .. } => {
                2u8.hash(&mut h);
                sctx.note_marker_text(engine::NoteAnchor {
                    kind: engine::NoteKind::Endnote,
                    id: *id,
                })
                .hash(&mut h);
            }
            engine::InlineKind::NoteSelfRef { .. } => {
                3u8.hash(&mut h);
                note_self_mark.hash(&mut h);
            }
            /* Issue #278 — a box's story references ride its sentinel
            glyph with their markers. Nothing is mixed in for a story
            without one. */
            engine::InlineKind::TextBox { story, .. } => {
                for (anchor, mark) in text_box_note_anchors(story, sctx) {
                    4u8.hash(&mut h);
                    anchor.hash(&mut h);
                    mark.hash(&mut h);
                }
            }
            engine::InlineKind::Image { .. }
            | engine::InlineKind::Symbol { .. }
            | engine::InlineKind::PositionalTab { .. } => {}
        }
    }

    /* ---- render config: no `..` ---- */
    let RenderConfig {
        font_id,
        base_direction,
        px_size,
        line_height: cfg_line_height,
        alignment: cfg_alignment,
        // Paint-side scale / zoom are not layout inputs: the layout scale
        // is the explicit `scale` argument, hashed below.
        scale: _,
        base_scale: _,
        zoom: _,
    } = cfg;
    font_id.hash(&mut h);
    matches!(base_direction, ShapingDirection::Rtl).hash(&mut h);
    px_size.to_bits().hash(&mut h);
    cfg_line_height.to_bits().hash(&mut h);
    tp_align_disc(*cfg_alignment).hash(&mut h);
    scale.to_bits().hash(&mut h);

    /* Issue #326 — automatic hyphenation is a layout input: the settings
    and the hyphenatable ranges (the resolved run languages). Mixed in
    only when it applies, so every other key is unchanged. */
    let hy_ranges = auto_hyphenation_ranges(para, sctx);
    if let Some(s) = sctx.settings.filter(|_| !hy_ranges.is_empty()) {
        0x326_u16.hash(&mut h);
        s.hyphenation_zone_twips().hash(&mut h);
        s.consecutive_hyphen_limit.hash(&mut h);
        s.do_not_hyphenate_caps.hash(&mut h);
        for (r, hy) in &hy_ranges {
            r.start.hash(&mut h);
            r.end.hash(&mut h);
            (*hy as *const text_pipeline::Hyphenator as usize).hash(&mut h);
        }
    }
    h.finish()
}
