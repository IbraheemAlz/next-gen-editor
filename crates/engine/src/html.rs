//! Lightweight HTML (de)serialization for styled paragraphs (Backlog #12).
//!
//! The rich clipboard copies a selection out as semantic HTML and pastes
//! HTML back in. [`to_html`] renders `&[Paragraph]`; [`from_html`] parses an
//! HTML string into `Vec<Paragraph>`.
//!
//! [`from_html`] is a hand-rolled tag tokenizer driving a **style stack** —
//! no `html5ever` / `scraper` dependency, so the engine crate stays
//! dependency-free and the WASM artifact small. It is deliberately lenient:
//! unknown tags are ignored, unclosed tags are tolerated (a close pops down
//! to the nearest matching open, or nothing), and whitespace is collapsed the
//! way a browser would. That is enough for our own `to_html` output and for
//! the well-formed HTML Google Docs / Word place on the clipboard.

use crate::{
    Block, BorderStroke, BorderStyle, CellBorders, CellProperties, FontFamily, InlineKind,
    InlineObject, ParaProperties, Paragraph, RowProperties, SpanStyle, StyleRun, Table, TableCell,
    TableProperties, TableRow,
};

/// `<w:sz>` is in eighths of a point. CSS borders are typed in CSS px;
/// at 96 DPI, 1 pt = 4/3 CSS px → 1 eighth-pt = 1/6 CSS px. Round-trip
/// boundary: an OOXML stroke of size 4 eighth-pt (= 0.5 pt) emits as
/// `0.67px` and re-parses to ~4 eighth-pt (rounded to nearest integer).
fn eighth_pt_to_css_px(eighth_pt: u16) -> f32 {
    (eighth_pt as f32) / 6.0
}

fn css_px_to_eighth_pt(px: f32) -> u16 {
    /* `0` would round-trip as "no border"; clamp upward. */
    (px * 6.0).round().max(1.0) as u16
}

/// CSS `border-style` token for an OOXML border style.
fn border_css_style(style: &BorderStyle) -> &'static str {
    match style {
        BorderStyle::Single => "solid",
        BorderStyle::Double => "double",
        BorderStyle::Dotted => "dotted",
        BorderStyle::Dashed => "dashed",
        BorderStyle::None => "none",
        BorderStyle::Other(_) => "solid",
    }
}

fn border_style_from_css(token: &str) -> BorderStyle {
    match token.trim().to_ascii_lowercase().as_str() {
        "solid" => BorderStyle::Single,
        "double" => BorderStyle::Double,
        "dotted" => BorderStyle::Dotted,
        "dashed" => BorderStyle::Dashed,
        "none" | "hidden" => BorderStyle::None,
        other => BorderStyle::Other(other.to_string()),
    }
}

/* ---- font family <-> CSS name ------------------------------------------ */

fn family_name(f: FontFamily) -> &'static str {
    match f {
        FontFamily::Amiri => "Amiri",
        FontFamily::LiberationSans => "Liberation Sans",
        FontFamily::NotoNaskhArabic => "Noto Naskh Arabic",
    }
}

fn family_from_name(s: &str) -> Option<FontFamily> {
    /* Accept the CSS name and the engine's loaded-font id alike. */
    match s
        .trim()
        .trim_matches(['"', '\''])
        .to_ascii_lowercase()
        .as_str()
    {
        "amiri" => Some(FontFamily::Amiri),
        "liberation sans" | "liberation" => Some(FontFamily::LiberationSans),
        "noto naskh arabic" | "noto-naskh" => Some(FontFamily::NotoNaskhArabic),
        _ => None,
    }
}

/* ---- colour helpers ---------------------------------------------------- */

fn color_to_hex(c: [u8; 4]) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

/// Parse a CSS colour: `#rgb`, `#rrggbb`, or `rgb()/rgba()`. Alpha is forced
/// opaque — the document model paints solid colours.
fn parse_css_color(v: &str) -> Option<[u8; 4]> {
    let v = v.trim();
    if let Some(hex) = v.strip_prefix('#') {
        let h = hex.trim();
        if h.len() == 3 {
            let d = |i: usize| u8::from_str_radix(&h[i..i + 1], 16).ok();
            let (r, g, b) = (d(0)?, d(1)?, d(2)?);
            return Some([r * 17, g * 17, b * 17, 255]);
        }
        if h.len() == 6 {
            let d = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
            return Some([d(0)?, d(2)?, d(4)?, 255]);
        }
        return None;
    }
    if let Some(inner) = v.strip_prefix("rgb").and_then(|s| {
        s.trim_start_matches('a')
            .trim()
            .strip_prefix('(')?
            .strip_suffix(')')
    }) {
        let mut it = inner.split(',').map(|n| n.trim().parse::<f32>().ok());
        let r = it.next()??;
        let g = it.next()??;
        let b = it.next()??;
        let q = |f: f32| f.clamp(0.0, 255.0).round() as u8;
        return Some([q(r), q(g), q(b), 255]);
    }
    None
}

/* ======================================================================= */
/* Serialization — Paragraph -> HTML                                       */
/* ======================================================================= */

fn escape_into(text: &str, out: &mut String) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
}

/// `style="..."` declarations for the non-flag attributes, or `None` when the
/// run carries colour/background/family/size that all sit at their defaults.
fn style_attr(s: &SpanStyle) -> Option<String> {
    let mut css = String::new();
    if let Some(c) = s.color {
        css.push_str(&format!("color:{};", color_to_hex(c)));
    }
    if let Some(c) = s.bg_color {
        css.push_str(&format!("background-color:{};", color_to_hex(c)));
    }
    if let Some(f) = s.font_family {
        css.push_str(&format!("font-family:{};", family_name(f)));
    }
    if let Some(px) = s.font_size {
        css.push_str(&format!("font-size:{px}px;"));
    }
    if css.is_empty() { None } else { Some(css) }
}

/// Emit one styled run: a `<span style>` wrapper for colour/family/size, then
/// nested `<b>/<i>/<u>/<s>` for the flags.
fn emit_run(text: &str, s: &SpanStyle, out: &mut String) {
    let span = style_attr(s);
    if let Some(css) = &span {
        out.push_str("<span style=\"");
        out.push_str(css);
        out.push_str("\">");
    }
    let flags: [(bool, &str); 4] = [
        (s.bold == Some(true), "b"),
        (s.italic == Some(true), "i"),
        (
            s.underline.is_some_and(crate::UnderlineStyle::is_visible),
            "u",
        ),
        (s.strike == Some(true), "s"),
    ];
    for (on, tag) in flags {
        if on {
            out.push('<');
            out.push_str(tag);
            out.push('>');
        }
    }
    escape_into(text, out);
    for (on, tag) in flags.iter().rev() {
        if *on {
            out.push_str("</");
            out.push_str(tag);
            out.push('>');
        }
    }
    if span.is_some() {
        out.push_str("</span>");
    }
}

/// EMU → CSS px (1 EMU = 1/914400 in; CSS px = 1/96 in → 1 EMU =
/// 96/914400 CSS px = 1/9525 CSS px). Round to nearest integer for
/// readable HTML; `data-*-emu` attributes carry the exact value for
/// lossless engine round-trip.
fn emu_to_css_px(emu: i64) -> i64 {
    (emu + 4762) / 9525
}

fn css_px_to_emu(px: i64) -> i64 {
    px.saturating_mul(9525)
}

/// Emit a `<p>` paragraph with inline-image anchor support. Inline
/// objects at byte offset `at` interrupt the text walk: the segment up
/// to `at` emits with its current style; the object emits as an
/// `<img>` (or footnote ref marker); the walk resumes from the
/// OBJECT REPLACEMENT CHARACTER's end.
fn emit_paragraph(p: &Paragraph, out: &mut String) {
    out.push_str("<p>");
    let len = p.text.len();
    let mut cursor = 0usize;
    /* Index inline_objects by byte offset for the walk. Sorted by `at`
    per the engine invariant. */
    let mut next_obj_idx = 0;
    for run in &p.spans {
        let (rs, re) = (run.start as usize, run.end as usize);
        if rs > cursor {
            emit_styled_text_with_objects(
                p,
                &mut next_obj_idx,
                cursor,
                rs,
                &SpanStyle::default(),
                out,
            );
        }
        if re > rs {
            emit_styled_text_with_objects(p, &mut next_obj_idx, rs, re, &run.style, out);
        }
        cursor = re;
    }
    if cursor < len {
        emit_styled_text_with_objects(
            p,
            &mut next_obj_idx,
            cursor,
            len,
            &SpanStyle::default(),
            out,
        );
    }
    out.push_str("</p>");
}

/// Walk `text[lo..hi]` segment by inline-object anchor. Each anchor
/// found in the range splits the text emit at `obj.at` and emits the
/// inline object inline (currently only `<img>` for `InlineKind::Image`).
fn emit_styled_text_with_objects(
    p: &Paragraph,
    next_obj_idx: &mut usize,
    lo: usize,
    hi: usize,
    style: &SpanStyle,
    out: &mut String,
) {
    let mut start = lo;
    while *next_obj_idx < p.inline_objects.len() {
        let obj = &p.inline_objects[*next_obj_idx];
        let at = obj.at as usize;
        if at < start {
            *next_obj_idx += 1;
            continue;
        }
        if at >= hi {
            break;
        }
        if at > start {
            emit_run(&p.text[start..at], style, out);
        }
        emit_inline_object(obj, out);
        /* Skip the OBJECT REPLACEMENT CHARACTER (U+FFFC, 3 UTF-8 bytes). */
        let after = at.saturating_add(3).min(hi);
        start = after;
        *next_obj_idx += 1;
    }
    if start < hi {
        emit_run(&p.text[start..hi], style, out);
    }
}

fn emit_inline_object(obj: &InlineObject, out: &mut String) {
    match &obj.kind {
        InlineKind::Image {
            rel_id,
            width_emu,
            height_emu,
        } => {
            let w_px = emu_to_css_px(*width_emu);
            let h_px = emu_to_css_px(*height_emu);
            // `data-rel-id` lets the engine resolve the image back to
            // its word/media archive blob on same-document paste — the
            // rich HTML keeps the document-local pointer. The data-emu
            // attributes carry the exact OOXML extents so the engine
            // round-trips lossless without depending on the px↔EMU
            // rounding. `src` is empty for now (data URI emission ships
            // in a follow-up); the attributes are enough for engine
            // round-trip.
            out.push_str("<img src=\"\" data-rel-id=\"");
            escape_into(rel_id, out);
            out.push_str(&format!(
                "\" data-width-emu=\"{width_emu}\" data-height-emu=\"{height_emu}\" width=\"{w_px}\" height=\"{h_px}\"/>"
            ));
        }
        InlineKind::FootnoteRef {
            id: _,
            display_number,
        } => {
            /* Footnote references render as a superscript marker in the
            HTML payload. Not yet a paste target — Phase 8a's footnote
            insertion uses a separate command path. */
            out.push_str(&format!("<sup>{display_number}</sup>"));
        }
    }
}

/// Render styled paragraphs as semantic HTML — one `<p>` per paragraph.
pub fn to_html(paras: &[Paragraph]) -> String {
    let mut out = String::new();
    for p in paras {
        emit_paragraph(p, &mut out);
    }
    out
}

/// CSS declarations for a cell's shading + per-edge borders. Returns
/// `None` when the cell carries no styling (avoid an empty `style=""`).
fn cell_style_attr(props: &CellProperties) -> Option<String> {
    let mut css = String::new();
    if let Some(shade) = props.shading {
        /* OOXML alpha is opaque-ish; CSS treats `background-color`
        without alpha as opaque. Hex form is the most readable. */
        css.push_str(&format!("background-color:{};", color_to_hex(shade)));
    }
    if let Some(borders) = &props.borders {
        for (edge, side) in [
            (&borders.top, "top"),
            (&borders.left, "left"),
            (&borders.bottom, "bottom"),
            (&borders.right, "right"),
        ] {
            if let Some(stroke) = edge {
                let style = border_css_style(&stroke.style);
                if matches!(style, "none") {
                    /* `border-X: none` is the engine's source of truth
                    for "this edge is off"; emit it. */
                    css.push_str(&format!("border-{side}:none;"));
                    continue;
                }
                let px = eighth_pt_to_css_px(stroke.size_eighth_pt);
                let color = stroke
                    .color
                    .map(color_to_hex)
                    .unwrap_or_else(|| "#000000".to_string());
                css.push_str(&format!("border-{side}:{px}px {style} {color};"));
            }
        }
    }
    if css.is_empty() { None } else { Some(css) }
}

fn emit_table(t: &Table, out: &mut String) {
    /* Outer style: collapse borders so cell borders abut without
    doubling. Word's default. */
    out.push_str("<table style=\"border-collapse:collapse;\">");
    for row in &t.rows {
        out.push_str("<tr>");
        for cell in &row.cells {
            if cell.props.v_merge == crate::VMergeRole::Continue {
                /* Continue cells are not emitted; the visual span is
                owned by the Restart cell. Skipping them keeps the HTML
                table rectangular without a `rowspan` (Phase 5 PR 3
                tracks merges via VMerge, not via OOXML rowSpan). */
                continue;
            }
            out.push_str("<td");
            if let Some(css) = cell_style_attr(&cell.props) {
                out.push_str(" style=\"");
                out.push_str(&css);
                out.push('"');
            }
            out.push('>');
            for inner in &cell.blocks {
                match inner {
                    Block::Paragraph(p) => emit_paragraph(p, out),
                    Block::Table(nested) => emit_table(nested, out),
                }
            }
            out.push_str("</td>");
        }
        out.push_str("</tr>");
    }
    out.push_str("</table>");
}

/// Render a block sequence as semantic HTML — paragraphs become `<p>`,
/// tables become `<table>` with inline CSS for shading + borders so
/// the engine round-trips cell styling losslessly through the
/// clipboard. Inline images emit as `<img data-rel-id=... width=H
/// height=W>` for same-document round-trip; `data-width-emu` /
/// `data-height-emu` carry the exact OOXML extents.
pub fn to_html_blocks(blocks: &[Block]) -> String {
    let mut out = String::new();
    for b in blocks {
        match b {
            Block::Paragraph(p) => emit_paragraph(p, &mut out),
            Block::Table(t) => emit_table(t, &mut out),
        }
    }
    out
}

/* ======================================================================= */
/* Parsing — HTML -> Paragraph                                             */
/* ======================================================================= */

fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let Some(semi) = rest[..rest.len().min(12)].find(';') else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let ent = &rest[1..semi];
        let decoded = match ent {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" | "#39" => Some('\''),
            "nbsp" => Some(' '),
            _ => ent
                .strip_prefix("#x")
                .or_else(|| ent.strip_prefix("#X"))
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| ent.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &rest[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Collapse every run of ASCII whitespace to a single space — the browser
/// rule for non-`<pre>` content; keeps pasted HTML indentation out of the text.
fn collapse_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_ws = false;
    for c in s.chars() {
        if c.is_ascii_whitespace() {
            if !in_ws {
                out.push(' ');
            }
            in_ws = true;
        } else {
            out.push(c);
            in_ws = false;
        }
    }
    out
}

/// Value of attribute `name` in a start-tag body (`span style="..."`),
/// quoted with `"` or `'`.
fn extract_attr(body: &str, name: &str) -> Option<String> {
    let lower = body.to_ascii_lowercase();
    let mut from = 0;
    while let Some(rel) = lower[from..].find(name) {
        let at = from + rel;
        let after = at + name.len();
        let pre_ok = at == 0 || body.as_bytes()[at - 1].is_ascii_whitespace();
        let eq = body[after..].trim_start();
        if pre_ok && eq.starts_with('=') {
            let v = eq[1..].trim_start();
            let quote = v.chars().next()?;
            if quote == '"' || quote == '\'' {
                let end = v[1..].find(quote)?;
                return Some(v[1..1 + end].to_string());
            }
        }
        from = after;
    }
    None
}

/// Parse a CSS `style` attribute into the style fields it sets.
fn parse_inline_style(css: &str) -> SpanStyle {
    let mut s = SpanStyle::default();
    for decl in css.split(';') {
        let Some((k, v)) = decl.split_once(':') else {
            continue;
        };
        let (k, v) = (k.trim().to_ascii_lowercase(), v.trim());
        match k.as_str() {
            "color" => s.color = parse_css_color(v),
            "background-color" | "background" => s.bg_color = parse_css_color(v),
            "font-family" => {
                s.font_family = v.split(',').find_map(family_from_name);
            }
            "font-weight" => {
                let bold = v.eq_ignore_ascii_case("bold")
                    || v.parse::<u32>().map(|n| n >= 600).unwrap_or(false);
                if bold {
                    s.bold = Some(true);
                }
            }
            "font-style"
                if v.eq_ignore_ascii_case("italic") || v.eq_ignore_ascii_case("oblique") =>
            {
                s.italic = Some(true);
            }
            "text-decoration" | "text-decoration-line" => {
                if v.contains("underline") {
                    s.underline = Some(crate::UnderlineStyle::Single);
                }
                if v.contains("line-through") {
                    s.strike = Some(true);
                }
            }
            "font-size" => {
                if let Some(px) = v.strip_suffix("px").and_then(|n| n.trim().parse().ok()) {
                    s.font_size = Some(px);
                }
            }
            _ => {}
        }
    }
    s
}

/// The style a styling tag contributes, or `None` when the tag carries no
/// style (so the caller knows not to stack it).
fn tag_style(name: &str, body: &str) -> Option<SpanStyle> {
    let mut s = SpanStyle::default();
    match name {
        "b" | "strong" => s.bold = Some(true),
        "i" | "em" => s.italic = Some(true),
        "u" | "ins" => s.underline = Some(crate::UnderlineStyle::Single),
        "s" | "strike" | "del" => s.strike = Some(true),
        "span" | "font" => {
            if let Some(css) = extract_attr(body, "style") {
                s = parse_inline_style(&css);
            }
            if let Some(c) = extract_attr(body, "color").and_then(|c| parse_css_color(&c)) {
                s.color = Some(c);
            }
        }
        _ => return None,
    }
    Some(s)
}

const VOID_TAGS: [&str; 6] = ["br", "hr", "img", "meta", "link", "input"];

fn is_block(name: &str) -> bool {
    matches!(
        name,
        "p" | "div" | "br" | "li" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "blockquote" | "tr"
    )
}

/// Object replacement character used as the anchor byte for an inline
/// object inside a paragraph's `text`. Mirrors `engine::OBJECT_REPLACE`.
const OBJECT_REPLACE: char = '\u{FFFC}';

/// Parse an `<img>` body's attributes into an `InlineObject` carrying
/// the image's archive rel-id (via `data-rel-id`) and EMU extents (via
/// `data-width-emu` / `data-height-emu`, else inferred from `width` /
/// `height` CSS px). Returns `None` when no rel-id is present — an
/// inline image without an archive target can't round-trip through the
/// paragraph's `inline_objects` list.
fn parse_img(body: &str) -> Option<InlineObject> {
    let rel_id = extract_attr(body, "data-rel-id")?;
    /* EMU is the engine's canonical extent unit; the px-derived
    fallback covers HTML pasted from other editors that emit only
    `width` / `height`. */
    let width_emu = extract_attr(body, "data-width-emu")
        .and_then(|s| s.trim().parse::<i64>().ok())
        .or_else(|| {
            extract_attr(body, "width")
                .and_then(|s| s.trim().trim_end_matches("px").parse::<i64>().ok())
                .map(css_px_to_emu)
        })
        .unwrap_or(0);
    let height_emu = extract_attr(body, "data-height-emu")
        .and_then(|s| s.trim().parse::<i64>().ok())
        .or_else(|| {
            extract_attr(body, "height")
                .and_then(|s| s.trim().trim_end_matches("px").parse::<i64>().ok())
                .map(css_px_to_emu)
        })
        .unwrap_or(0);
    Some(InlineObject {
        /* `at` is filled in by the caller once the paragraph's running
        text length is known. */
        at: 0,
        kind: InlineKind::Image {
            rel_id,
            width_emu,
            height_emu,
        },
    })
}

/// Parse one CSS `border-X: Wpx STYLE COLOR;` shorthand into a
/// `BorderStroke`. Lenient: missing tokens default; `none` style returns
/// `None` (caller represents "no border" by absence on `CellBorders`).
fn parse_border_shorthand(v: &str) -> Option<BorderStroke> {
    let parts: Vec<&str> = v.split_ascii_whitespace().collect();
    if parts.is_empty() {
        return None;
    }
    let mut size_eighth_pt: Option<u16> = None;
    let mut style: Option<BorderStyle> = None;
    let mut color: Option<[u8; 4]> = None;
    for tok in parts {
        if let Some(num) = tok.strip_suffix("px").and_then(|s| s.parse::<f32>().ok()) {
            size_eighth_pt = Some(css_px_to_eighth_pt(num));
        } else if let Some(c) = parse_css_color(tok) {
            color = Some(c);
        } else {
            style = Some(border_style_from_css(tok));
        }
    }
    let style = style.unwrap_or(BorderStyle::Single);
    if matches!(style, BorderStyle::None) {
        return None;
    }
    Some(BorderStroke {
        style,
        size_eighth_pt: size_eighth_pt.unwrap_or(4),
        color,
    })
}

/// Parse a `<td>`'s `style` attribute into `CellProperties` (shading +
/// per-edge borders). The pure-text fields are left at their defaults;
/// `grid_span`, `v_merge`, `v_align`, `width` aren't expressible in
/// inline CSS without bespoke `data-*` extensions.
fn parse_cell_style(css: &str) -> CellProperties {
    let mut props = CellProperties::default();
    let mut borders = CellBorders::default();
    let mut any_border = false;
    for decl in css.split(';') {
        let Some((k, v)) = decl.split_once(':') else {
            continue;
        };
        let (k, v) = (k.trim().to_ascii_lowercase(), v.trim());
        match k.as_str() {
            "background-color" | "background" => {
                if let Some(c) = parse_css_color(v) {
                    props.shading = Some(c);
                }
            }
            "border-top" => {
                borders.top = parse_border_shorthand(v);
                any_border = true;
            }
            "border-left" => {
                borders.left = parse_border_shorthand(v);
                any_border = true;
            }
            "border-bottom" => {
                borders.bottom = parse_border_shorthand(v);
                any_border = true;
            }
            "border-right" => {
                borders.right = parse_border_shorthand(v);
                any_border = true;
            }
            "border" => {
                /* CSS `border` shorthand applies to all four edges. */
                let stroke = parse_border_shorthand(v);
                borders.top = stroke.clone();
                borders.left = stroke.clone();
                borders.bottom = stroke.clone();
                borders.right = stroke;
                any_border = true;
            }
            _ => {}
        }
    }
    if any_border {
        props.borders = Some(borders);
    }
    props
}

/// Find the first `<table` opening tag in `s` (case-insensitive),
/// returning its start byte index or `None`.
fn find_open_tag(s: &str, name: &str) -> Option<usize> {
    let lower = s.to_ascii_lowercase();
    let needle_full = format!("<{name}");
    let mut from = 0;
    while let Some(rel) = lower[from..].find(&needle_full) {
        let at = from + rel;
        let after = at + needle_full.len();
        /* Reject tag-name prefix collisions: `<table` must end at `>`,
        whitespace, or `/`. */
        match lower.as_bytes().get(after) {
            None => return Some(at),
            Some(&b) if b == b'>' || b == b'/' || b.is_ascii_whitespace() => return Some(at),
            _ => from = after,
        }
    }
    None
}

/// Locate the matching `</table>` for a `<table>` opening at `from`.
/// Returns `(close_start, after_close)` byte offsets. Tolerates nested
/// tables by counting depth.
fn find_close_table(s: &str, from: usize) -> Option<(usize, usize)> {
    let lower = s.to_ascii_lowercase();
    let mut depth: u32 = 1;
    let mut i = from + "<table".len();
    while i < s.len() {
        if let Some(open_rel) = find_open_tag(&s[i..], "table") {
            let abs_open = i + open_rel;
            if let Some(close_rel) = lower[i..].find("</table>") {
                let abs_close = i + close_rel;
                if abs_open < abs_close {
                    depth += 1;
                    i = abs_open + "<table".len();
                    continue;
                }
                depth -= 1;
                if depth == 0 {
                    return Some((abs_close, abs_close + "</table>".len()));
                }
                i = abs_close + "</table>".len();
                continue;
            }
            return None;
        }
        if let Some(close_rel) = lower[i..].find("</table>") {
            let abs_close = i + close_rel;
            depth -= 1;
            if depth == 0 {
                return Some((abs_close, abs_close + "</table>".len()));
            }
            i = abs_close + "</table>".len();
            continue;
        }
        return None;
    }
    None
}

/// Carve `<tr>...</tr>` substrings out of a `<table>` body. Tolerant
/// of malformed input: unclosed rows are treated as ending at the
/// table's end.
fn split_rows(table_body: &str) -> Vec<&str> {
    let mut rows: Vec<&str> = Vec::new();
    let lower = table_body.to_ascii_lowercase();
    let mut from = 0;
    while let Some(open_rel) = lower[from..].find("<tr") {
        let open_at = from + open_rel;
        /* Skip to the end of the `<tr ...>` opening tag. */
        let body_start = match table_body[open_at..].find('>') {
            Some(rel) => open_at + rel + 1,
            None => break,
        };
        let close_at = lower[body_start..]
            .find("</tr>")
            .map(|rel| body_start + rel)
            .unwrap_or(table_body.len());
        rows.push(&table_body[body_start..close_at]);
        from = close_at + "</tr>".len().min(table_body.len() - close_at);
    }
    rows
}

/// Split a `<tr>` body into its `<td>` cells. Returns each cell as
/// `(style_attr_value, inner_html)` so the caller can parse styles
/// separately. Tolerant of unclosed cells.
fn split_cells(row_body: &str) -> Vec<(Option<String>, &str)> {
    let mut cells: Vec<(Option<String>, &str)> = Vec::new();
    let lower = row_body.to_ascii_lowercase();
    let mut from = 0;
    while let Some(open_rel) = lower[from..].find("<td") {
        let open_at = from + open_rel;
        let tag_end = match row_body[open_at..].find('>') {
            Some(rel) => open_at + rel,
            None => break,
        };
        let tag_body = &row_body[open_at + 3..tag_end];
        let style = extract_attr(tag_body, "style");
        let body_start = tag_end + 1;
        let close_at = lower[body_start..]
            .find("</td>")
            .map(|rel| body_start + rel)
            .unwrap_or(row_body.len());
        cells.push((style, &row_body[body_start..close_at]));
        from = close_at + "</td>".len().min(row_body.len() - close_at);
    }
    cells
}

/// Parse a `<table>...</table>` substring (excluding the outer tags
/// for inner scan, but accepting the full thing) into a `Table`.
fn parse_table_html(table_str: &str) -> Option<Table> {
    /* Skip past the opening `<table ...>`. */
    let open_end = table_str.find('>')?;
    let body = &table_str[open_end + 1..];
    /* Trim a trailing `</table>` if present. */
    let lower = body.to_ascii_lowercase();
    let inner = match lower.rfind("</table>") {
        Some(at) => &body[..at],
        None => body,
    };
    let mut rows: Vec<TableRow> = Vec::new();
    let mut max_cells: usize = 0;
    for row_inner in split_rows(inner) {
        let mut cells_out: Vec<TableCell> = Vec::new();
        for (style, cell_inner) in split_cells(row_inner) {
            let props = style.as_deref().map(parse_cell_style).unwrap_or_default();
            /* Cell contents — recursive descent: parse nested
            paragraphs (and tables) into Block via `from_html_blocks`
            for full nesting support. */
            let blocks = from_html_blocks(cell_inner);
            let final_blocks = if blocks.is_empty() {
                vec![Block::Paragraph(Paragraph::default())]
            } else {
                blocks
            };
            cells_out.push(TableCell {
                props,
                blocks: final_blocks,
            });
        }
        if cells_out.is_empty() {
            continue;
        }
        max_cells = max_cells.max(cells_out.len());
        rows.push(TableRow {
            props: RowProperties::default(),
            cells: cells_out,
        });
    }
    if rows.is_empty() {
        return None;
    }
    /* Even column grid — caller can adjust if widths are supplied via
    `data-grid` or `<col>` extensions. Twips per column = A4 content
    width / max_cells (matches `insert_table`'s default). */
    let per_col = (crate::DEFAULT_A4_CONTENT_TWIPS / max_cells.max(1) as i32).max(720);
    let grid = vec![per_col; max_cells];
    Some(Table {
        grid,
        props: TableProperties::default(),
        rows,
        /* Pasted tables are engine-synthesised — no source bytes. */
        dirty: true,
        source_xml: None,
    })
}

/// Parse an HTML string into a `Vec<Block>` — paragraphs and tables
/// preserved as the engine block model. Tables surface their cell
/// shading + borders from inline CSS; nested tables work via recursive
/// descent. Inline images inside paragraphs are not extracted by this
/// path — they are picked up by the inner `from_html`'s `<img>`
/// handling (which fills in `inline_objects` on each Paragraph).
pub fn from_html_blocks(html: &str) -> Vec<Block> {
    let mut out: Vec<Block> = Vec::new();
    let mut rest = html;
    while !rest.is_empty() {
        match find_open_tag(rest, "table") {
            Some(idx) => {
                if idx > 0 {
                    for p in from_html(&rest[..idx]) {
                        out.push(Block::Paragraph(p));
                    }
                }
                match find_close_table(rest, idx) {
                    Some((_close_start, after_close)) => {
                        let table_str = &rest[idx..after_close];
                        if let Some(t) = parse_table_html(table_str) {
                            out.push(Block::Table(t));
                        }
                        rest = &rest[after_close..];
                    }
                    None => {
                        /* Unclosed table — fall back to paragraph
                        parsing on the remainder so we don't lose data. */
                        for p in from_html(&rest[idx..]) {
                            out.push(Block::Paragraph(p));
                        }
                        break;
                    }
                }
            }
            None => {
                for p in from_html(rest) {
                    out.push(Block::Paragraph(p));
                }
                break;
            }
        }
    }
    out
}

/// One open styling tag on the parser stack.
struct Frame {
    tag: String,
    style: SpanStyle,
}

/// One emit-event the parser feeds to the paragraph builder. `Text` is
/// styled UTF-8; `Object` is an inline anchor (image, footnote ref).
enum BuilderEvent {
    Text(String, SpanStyle),
    Object(InlineObject),
}

/// Accumulates the events of one paragraph, then bakes them into a
/// `Paragraph` with `inline_objects` set in document order.
#[derive(Default)]
struct ParaBuilder {
    events: Vec<BuilderEvent>,
}

impl ParaBuilder {
    fn push_text(&mut self, text: &str, style: SpanStyle) {
        if !text.is_empty() {
            self.events
                .push(BuilderEvent::Text(text.to_string(), style));
        }
    }

    fn push_object(&mut self, obj: InlineObject) {
        self.events.push(BuilderEvent::Object(obj));
    }

    fn is_empty(&self) -> bool {
        self.events.iter().all(|e| match e {
            BuilderEvent::Text(t, _) => t.trim().is_empty(),
            BuilderEvent::Object(_) => false,
        })
    }

    /// Bake into a `Paragraph`, or `None` when the paragraph holds no
    /// text or objects (inter-tag whitespace between block elements).
    fn finish(&mut self) -> Option<Paragraph> {
        if self.events.is_empty() || self.is_empty() {
            self.events.clear();
            return None;
        }
        let mut text = String::new();
        let mut spans: Vec<StyleRun> = Vec::new();
        let mut inline_objects: Vec<InlineObject> = Vec::new();
        for ev in self.events.drain(..) {
            match ev {
                BuilderEvent::Text(chunk, style) => {
                    let start = text.len() as u32;
                    text.push_str(&chunk);
                    let end = text.len() as u32;
                    if style == SpanStyle::default() || start == end {
                        continue;
                    }
                    match spans.last_mut() {
                        Some(last) if last.end == start && last.style == style => last.end = end,
                        _ => spans.push(StyleRun { start, end, style }),
                    }
                }
                BuilderEvent::Object(mut obj) => {
                    /* The anchor byte is the OBJECT REPLACEMENT
                    CHARACTER's start in the running text. */
                    obj.at = text.len() as u32;
                    text.push(OBJECT_REPLACE);
                    inline_objects.push(obj);
                }
            }
        }
        Some(Paragraph {
            text,
            spans,
            props: ParaProperties::default(),
            list_item: None,
            resolved_marker: None,
            /* HTML paste creates fresh paragraphs; never inherits source. */
            dirty: false,
            source_xml: None,
            inline_objects,
            hyperlinks: Vec::new(),
            revisions: Vec::new(),
            fields: Vec::new(),
        })
    }
}

/// Parse an HTML string into styled paragraphs. Tolerant of unknown and
/// unclosed tags; returns an empty vec when there is no text content.
pub fn from_html(html: &str) -> Vec<Paragraph> {
    let mut paras: Vec<Paragraph> = Vec::new();
    let mut cur = ParaBuilder::default();
    let mut stack: Vec<Frame> = Vec::new();
    let bytes = html.as_bytes();
    let mut i = 0;

    /* Resolved style = every stacked frame's style merged bottom-to-top. */
    let resolved = |stack: &[Frame]| -> SpanStyle {
        stack
            .iter()
            .fold(SpanStyle::default(), |acc, f| acc.merged_with(f.style))
    };

    while i < bytes.len() {
        if bytes[i] == b'<' {
            let Some(close_rel) = html[i..].find('>') else {
                break;
            };
            let raw = &html[i + 1..i + close_rel];
            i += close_rel + 1;

            /* Skip comments / CDATA / doctype / processing instructions. */
            if raw.starts_with('!') || raw.starts_with('?') {
                continue;
            }
            let is_end = raw.starts_with('/');
            let tag_body = raw.trim_start_matches('/').trim().trim_end_matches('/');
            let name = tag_body
                .split([' ', '\t', '\n', '\r'])
                .next()
                .unwrap_or("")
                .to_ascii_lowercase();
            if name.is_empty() {
                continue;
            }

            if is_end {
                if is_block(&name) {
                    if let Some(p) = cur.finish() {
                        paras.push(p);
                    }
                }
                /* Pop down to the nearest matching open frame, if any. */
                if let Some(pos) = stack.iter().rposition(|f| f.tag == name) {
                    stack.truncate(pos);
                }
            } else {
                if is_block(&name) {
                    if let Some(p) = cur.finish() {
                        paras.push(p);
                    }
                }
                if name == "img" {
                    if let Some(obj) = parse_img(tag_body) {
                        cur.push_object(obj);
                    }
                    continue;
                }
                if VOID_TAGS.contains(&name.as_str()) {
                    continue;
                }
                if let Some(style) = tag_style(&name, tag_body) {
                    stack.push(Frame { tag: name, style });
                }
            }
        } else {
            let Some(next_rel) = html[i..].find('<') else {
                let chunk = collapse_ws(&decode_entities(&html[i..]));
                cur.push_text(&chunk, resolved(&stack));
                break;
            };
            let chunk = collapse_ws(&decode_entities(&html[i..i + next_rel]));
            cur.push_text(&chunk, resolved(&stack));
            i += next_rel;
        }
    }
    if let Some(p) = cur.finish() {
        paras.push(p);
    }
    paras
}

#[cfg(test)]
mod tests {
    use super::*;

    fn red() -> [u8; 4] {
        [255, 0, 0, 255]
    }

    #[test]
    fn to_html_plain_paragraph() {
        let p = Paragraph {
            text: "hello".into(),
            spans: vec![],
            props: ParaProperties::default(),
            list_item: None,
            resolved_marker: None,
            dirty: false,
            source_xml: None,
            inline_objects: Vec::new(),
            hyperlinks: Vec::new(),
            revisions: Vec::new(),
            fields: Vec::new(),
        };
        assert_eq!(to_html(&[p]), "<p>hello</p>");
    }

    #[test]
    fn to_html_escapes_text() {
        let p = Paragraph {
            text: "a < b & c".into(),
            spans: vec![],
            props: ParaProperties::default(),
            list_item: None,
            resolved_marker: None,
            dirty: false,
            source_xml: None,
            inline_objects: Vec::new(),
            hyperlinks: Vec::new(),
            revisions: Vec::new(),
            fields: Vec::new(),
        };
        assert_eq!(to_html(&[p]), "<p>a &lt; b &amp; c</p>");
    }

    #[test]
    fn to_html_emits_bold_and_colour() {
        let p = Paragraph {
            text: "xy".into(),
            spans: vec![StyleRun {
                start: 0,
                end: 2,
                style: SpanStyle {
                    bold: Some(true),
                    color: Some(red()),
                    ..Default::default()
                },
            }],
            props: ParaProperties::default(),
            list_item: None,
            resolved_marker: None,
            dirty: false,
            source_xml: None,
            inline_objects: Vec::new(),
            hyperlinks: Vec::new(),
            revisions: Vec::new(),
            fields: Vec::new(),
        };
        assert_eq!(
            to_html(&[p]),
            "<p><span style=\"color:#ff0000;\"><b>xy</b></span></p>"
        );
    }

    #[test]
    fn from_html_round_trips_our_own_output() {
        let original = vec![Paragraph {
            text: "bold red".into(),
            spans: vec![StyleRun {
                start: 0,
                end: 4,
                style: SpanStyle {
                    bold: Some(true),
                    color: Some(red()),
                    ..Default::default()
                },
            }],
            props: ParaProperties::default(),
            list_item: None,
            resolved_marker: None,
            dirty: false,
            source_xml: None,
            inline_objects: Vec::new(),
            hyperlinks: Vec::new(),
            revisions: Vec::new(),
            fields: Vec::new(),
        }];
        let parsed = from_html(&to_html(&original));
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].text, "bold red");
        assert_eq!(parsed[0].spans.len(), 1);
        assert_eq!(parsed[0].spans[0].start, 0);
        assert_eq!(parsed[0].spans[0].end, 4);
        assert_eq!(parsed[0].spans[0].style.bold, Some(true));
        assert_eq!(parsed[0].spans[0].style.color, Some(red()));
    }

    #[test]
    fn from_html_two_paragraphs() {
        let parsed = from_html("<p>one</p>\n  <p>two</p>");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].text, "one");
        assert_eq!(parsed[1].text, "two");
    }

    #[test]
    fn from_html_nested_and_italic_em() {
        let parsed = from_html("<p><b>a<em>b</em></b>c</p>");
        assert_eq!(parsed[0].text, "abc");
        assert_eq!(parsed[0].style_at(0).bold, Some(true));
        assert_eq!(parsed[0].style_at(1).bold, Some(true));
        assert_eq!(parsed[0].style_at(1).italic, Some(true));
        assert_eq!(parsed[0].style_at(2), SpanStyle::default());
    }

    #[test]
    fn from_html_tolerates_unknown_and_wrapper_tags() {
        let parsed = from_html("<html><body><div><p>hi <xyz>there</xyz></p></div></body></html>");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].text, "hi there");
    }

    #[test]
    fn from_html_decodes_entities() {
        let parsed = from_html("<p>a &amp; b &lt;c&gt; &#65;</p>");
        assert_eq!(parsed[0].text, "a & b <c> A");
    }

    #[test]
    fn from_html_span_style_background_and_family() {
        let parsed = from_html(
            "<p><span style=\"background-color:#00ff00;font-family:Liberation Sans\">x</span></p>",
        );
        let st = parsed[0].style_at(0);
        assert_eq!(st.bg_color, Some([0, 255, 0, 255]));
        assert_eq!(st.font_family, Some(FontFamily::LiberationSans));
    }

    #[test]
    fn from_html_empty_is_empty_vec() {
        assert!(from_html("").is_empty());
        assert!(from_html("   \n  ").is_empty());
    }

    #[test]
    fn from_html_unclosed_tag_does_not_panic() {
        let parsed = from_html("<p><b>stuck");
        assert_eq!(parsed[0].text, "stuck");
        assert_eq!(parsed[0].style_at(0).bold, Some(true));
    }

    /* ---- Phase 3: high-fidelity table + image round-trip ---------- */

    fn make_styled_table() -> Table {
        let yellow = [0xff, 0xff, 0x99, 0xff];
        let red = [0xff, 0, 0, 0xff];
        let mut cell = TableCell::default();
        cell.blocks.push(Block::Paragraph(Paragraph {
            text: "cell text".into(),
            ..Default::default()
        }));
        cell.props.shading = Some(yellow);
        cell.props.borders = Some(CellBorders {
            top: Some(BorderStroke {
                style: BorderStyle::Single,
                size_eighth_pt: 8, // 1pt
                color: Some(red),
            }),
            ..Default::default()
        });
        let row = TableRow {
            props: RowProperties::default(),
            cells: vec![cell],
        };
        Table {
            grid: vec![6765],
            props: TableProperties::default(),
            rows: vec![row],
            dirty: true,
            source_xml: None,
        }
    }

    /// Round-trip the cell shading + top-border through HTML and back.
    /// Shading and the OOXML border eighth-point stroke survive within
    /// ±1 eighth-pt of rounding (CSS px → eighth-pt has 6:1 ratio).
    #[test]
    fn html_blocks_round_trip_cell_shading_and_borders() {
        let original = vec![Block::Table(make_styled_table())];
        let html = to_html_blocks(&original);
        let parsed = from_html_blocks(&html);
        assert_eq!(parsed.len(), 1);
        let table = parsed[0].as_table().expect("parsed back into Table");
        assert_eq!(table.rows.len(), 1);
        let cell = &table.rows[0].cells[0];
        assert_eq!(
            cell.props.shading,
            Some([0xff, 0xff, 0x99, 0xff]),
            "shading round-trips byte-exact"
        );
        let borders = cell.props.borders.as_ref().expect("borders");
        let top = borders.top.as_ref().expect("top border");
        assert!(matches!(top.style, BorderStyle::Single));
        assert_eq!(top.color, Some([0xff, 0, 0, 0xff]));
        /* 8 eighth-pt = 1 pt = 1.33 CSS px → re-parsed at 1.33 × 6 = 8. */
        assert!(
            top.size_eighth_pt.abs_diff(8) <= 1,
            "border weight round-trips within ±1 eighth-pt: got {}",
            top.size_eighth_pt
        );
    }

    /// Inline image emits as `<img data-rel-id=... data-width-emu=...>`
    /// and parses back into the paragraph's `inline_objects` with the
    /// exact OOXML EMU extents preserved.
    #[test]
    fn html_inline_image_round_trips_emu_extents() {
        let p = Paragraph {
            text: "\u{FFFC}".into(),
            spans: Vec::new(),
            props: ParaProperties::default(),
            list_item: None,
            resolved_marker: None,
            dirty: false,
            source_xml: None,
            inline_objects: vec![InlineObject {
                at: 0,
                kind: InlineKind::Image {
                    rel_id: "rId7".into(),
                    width_emu: 1_905_000,  // 200 px @ 96 DPI
                    height_emu: 1_524_000, // 160 px @ 96 DPI
                },
            }],
            hyperlinks: Vec::new(),
            revisions: Vec::new(),
            fields: Vec::new(),
        };
        let html = to_html(&[p]);
        assert!(html.contains("data-rel-id=\"rId7\""));
        assert!(html.contains("data-width-emu=\"1905000\""));
        let parsed = from_html(&html);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].inline_objects.len(), 1);
        let kind = &parsed[0].inline_objects[0].kind;
        match kind {
            InlineKind::Image {
                rel_id,
                width_emu,
                height_emu,
            } => {
                assert_eq!(rel_id, "rId7");
                assert_eq!(*width_emu, 1_905_000, "EMU is lossless via data-*-emu");
                assert_eq!(*height_emu, 1_524_000);
            }
            _ => panic!("expected InlineKind::Image, got {kind:?}"),
        }
    }
}
