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

use crate::{FontFamily, ParaProperties, Paragraph, SpanStyle, StyleRun};

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
        (s.underline == Some(true), "u"),
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

/// Render styled paragraphs as semantic HTML — one `<p>` per paragraph.
pub fn to_html(paras: &[Paragraph]) -> String {
    let mut out = String::new();
    for p in paras {
        out.push_str("<p>");
        /* Walk the paragraph as maximal (range, style) segments: the explicit
        spans, plus default-styled gaps between and around them. */
        let mut cursor = 0usize;
        let len = p.text.len();
        for run in &p.spans {
            let (rs, re) = (run.start as usize, run.end as usize);
            if rs > cursor {
                emit_run(&p.text[cursor..rs], &SpanStyle::default(), &mut out);
            }
            if re > rs {
                emit_run(&p.text[rs..re], &run.style, &mut out);
            }
            cursor = re;
        }
        if cursor < len {
            emit_run(&p.text[cursor..len], &SpanStyle::default(), &mut out);
        }
        out.push_str("</p>");
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
                    s.underline = Some(true);
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
        "u" | "ins" => s.underline = Some(true),
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

/// One open styling tag on the parser stack.
struct Frame {
    tag: String,
    style: SpanStyle,
}

/// Accumulates the runs of one paragraph, then bakes them into a `Paragraph`.
#[derive(Default)]
struct ParaBuilder {
    runs: Vec<(String, SpanStyle)>,
}

impl ParaBuilder {
    fn push_text(&mut self, text: &str, style: SpanStyle) {
        if !text.is_empty() {
            self.runs.push((text.to_string(), style));
        }
    }

    fn joined_is_blank(&self) -> bool {
        self.runs.iter().all(|(t, _)| t.trim().is_empty())
    }

    /// Bake into a `Paragraph`, or `None` when the paragraph holds no text
    /// (inter-tag whitespace between block elements).
    fn finish(&mut self) -> Option<Paragraph> {
        if self.runs.is_empty() || self.joined_is_blank() {
            self.runs.clear();
            return None;
        }
        let mut text = String::new();
        let mut spans: Vec<StyleRun> = Vec::new();
        for (chunk, style) in self.runs.drain(..) {
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
        Some(Paragraph {
            text,
            spans,
            props: ParaProperties::default(),
            list_item: None,
            resolved_marker: None,
            /* HTML paste creates fresh paragraphs; never inherits source. */
            dirty: false,
            source_xml: None,
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
}
