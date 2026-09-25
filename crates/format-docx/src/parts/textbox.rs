//! Issue #83 — text boxes: `<wps:wsp>` shapes carrying a
//! `<wps:txbx><w:txbxContent>` story, and their VML twin (`<v:shape>` +
//! `<v:textbox>`, either inside `<mc:Fallback>` or as a bare `<w:pict>`).
//!
//! **Reader side.** `parts::document` drives the state machine (it owns the
//! run / anchor accumulators); this module holds the pieces it calls:
//! story parsing (the `<w:txbxContent>` children re-rooted under the
//! part's namespace scope and parsed by the body parser itself — the same
//! trick `parts::table::parse_cell_paragraph` uses, so a story carries
//! spans, grab bags, pictures and fields exactly like the body), the
//! `<wps:bodyPr>` / `<wps:spPr>` readers, the element locator the writer's
//! splice ranges come from, and the VML text-box reader.
//!
//! **Writer side.** [`container_xml`] re-emits a text box: the verbatim
//! source container when the story is clean, the source container with
//! every `<w:txbxContent>` element regenerated when it is dirty (so the
//! DrawingML choice and its VML fallback stay in step), or a synthesized
//! `<w:drawing>` for an engine-authored box. [`splice_host`] swaps
//! regenerated containers into a CLEAN host paragraph's passthrough
//! bytes, so a story edit leaves the surrounding runs byte-identical.
//!
//! **Self-defense.** A story may itself contain a text box (Word never
//! writes one, but attacker-shaped input can nest them arbitrarily). The
//! story parse recurses through the body parser, so the nesting depth is
//! capped at [`MAX_TEXT_BOX_NESTING`]; a deeper box is not modeled (the
//! host paragraph's passthrough keeps its bytes).

use crate::schema::grab_bag::NamespaceScope;
use crate::schema::wp_anchor::emit_anchor_open;
use crate::style_resolver::StyleResolver;
use engine::{
    Block, FloatAlign, FloatAnchor, FloatOffset, HPosition, HRelativeFrom, InlineKind, Paragraph,
    ShapeOutline, TextBoxStory, TextBoxVAlign, VPosition, VRelativeFrom, WrapKind,
};
use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;
use std::cell::Cell;

/// Maximum text-box-in-text-box nesting the reader models.
pub(crate) const MAX_TEXT_BOX_NESTING: u32 = 2;

thread_local! {
    static STORY_DEPTH: Cell<u32> = const { Cell::new(0) };
}

/// `true` while the reader may model one more level of text box.
pub(crate) fn may_nest() -> bool {
    STORY_DEPTH.with(|d| d.get() < MAX_TEXT_BOX_NESTING)
}

/// Attribute lookup by exact qualified name.
fn attr(e: &BytesStart, key: &[u8]) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.as_ref() == key)
        .and_then(|a| a.unescape_value().ok().map(|v| v.into_owned()))
}

/// Parse one `<w:txbxContent>…</w:txbxContent>` element into story
/// blocks. `None` when the nesting cap is hit or the fragment is not a
/// story element. An empty story still yields one empty paragraph (the
/// caret needs a home, and Word writes `<w:p/>` for an empty box).
pub(crate) fn parse_story(
    fragment: &[u8],
    resolver: &StyleResolver<'_>,
    ns: &NamespaceScope,
) -> Option<Vec<Block>> {
    if !may_nest() {
        return None;
    }
    let inner = element_inner(fragment)?;
    let mut wrapped: Vec<u8> = Vec::with_capacity(inner.len() + 256);
    wrapped.extend_from_slice(b"<w:document");
    if ns.uri("w").is_none() {
        wrapped.extend_from_slice(b" xmlns:w=\"");
        wrapped.extend_from_slice(crate::schema::NS_W.as_bytes());
        wrapped.push(b'"');
    }
    for (prefix, uri) in ns.declarations() {
        wrapped.extend_from_slice(b" xmlns:");
        wrapped.extend_from_slice(prefix.as_bytes());
        wrapped.extend_from_slice(b"=\"");
        wrapped.extend_from_slice(uri.as_bytes());
        wrapped.push(b'"');
    }
    wrapped.extend_from_slice(b"><w:body>");
    wrapped.extend_from_slice(inner);
    wrapped.extend_from_slice(b"</w:body></w:document>");

    STORY_DEPTH.with(|d| d.set(d.get() + 1));
    let parsed = crate::parts::document::parse_document_xml(&wrapped, resolver);
    STORY_DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
    let tree = parsed.ok()?;
    let mut blocks: Vec<Block> = tree
        .blocks
        .into_iter()
        .map(|b| match b {
            Block::Paragraph(mut p) => {
                p.section_end = None;
                Block::Paragraph(p)
            }
            t => t,
        })
        .collect();
    if blocks.is_empty() {
        blocks.push(Block::Paragraph(Paragraph::default()));
    }
    Some(blocks)
}

/// The bytes between an element's start tag and its end tag (`None` for
/// a self-closing element — the caller treats it as an empty story).
fn element_inner(fragment: &[u8]) -> Option<&[u8]> {
    let open_end = fragment.iter().position(|&b| b == b'>')?;
    if open_end > 0 && fragment[open_end - 1] == b'/' {
        return Some(&[]);
    }
    let close = fragment.iter().rposition(|&b| b == b'<')?;
    (close > open_end).then(|| &fragment[open_end + 1..close])
}

/// `<wps:bodyPr lIns tIns rIns bIns anchor>` → insets + vertical anchor.
/// Absent insets keep Word's defaults (already on the story).
pub(crate) fn apply_body_pr(e: &BytesStart, story: &mut TextBoxStory) {
    let num = |k: &[u8]| attr(e, k).and_then(|v| v.trim().parse::<i64>().ok());
    if let Some(v) = num(b"lIns") {
        story.inset_left_emu = v.max(0);
    }
    if let Some(v) = num(b"tIns") {
        story.inset_top_emu = v.max(0);
    }
    if let Some(v) = num(b"rIns") {
        story.inset_right_emu = v.max(0);
    }
    if let Some(v) = num(b"bIns") {
        story.inset_bottom_emu = v.max(0);
    }
    story.v_align = match attr(e, b"anchor").as_deref() {
        Some("ctr") => TextBoxVAlign::Center,
        Some("b") => TextBoxVAlign::Bottom,
        _ => TextBoxVAlign::Top,
    };
}

/// A DrawingML colour element (`srgbClr`, `schemeClr`, `prstClr`,
/// `sysClr`) → RGBA. Theme colours cannot be resolved without the theme
/// part; the two neutral slots map to white / black and the accents to
/// a mid grey so the shape stays visible.
fn drawing_color(e: &BytesStart) -> Option<[u8; 4]> {
    let name = e.name();
    let val = attr(e, b"val").unwrap_or_default();
    match name.as_ref() {
        b"a:srgbClr" => hex_rgb(&val),
        b"a:sysClr" => attr(e, b"lastClr")
            .and_then(|v| hex_rgb(&v))
            .or(Some([0, 0, 0, 255])),
        b"a:schemeClr" => Some(match val.as_str() {
            "lt1" | "bg1" | "lt2" | "bg2" => [255, 255, 255, 255],
            "dk1" | "tx1" | "dk2" | "tx2" => [0, 0, 0, 255],
            _ => [0x80, 0x80, 0x80, 255],
        }),
        b"a:prstClr" => Some(match val.as_str() {
            "white" => [255, 255, 255, 255],
            _ => [0, 0, 0, 255],
        }),
        _ => None,
    }
}

/// `"RRGGBB"` / `"#RRGGBB"` / `"#RGB"` → opaque RGBA.
fn hex_rgb(v: &str) -> Option<[u8; 4]> {
    let h = v.trim().trim_start_matches('#');
    let byte = |s: &str| u8::from_str_radix(s, 16).ok();
    match h.len() {
        6 => Some([byte(&h[0..2])?, byte(&h[2..4])?, byte(&h[4..6])?, 255]),
        3 => {
            let d = |i: usize| byte(&h[i..=i]).map(|x| x * 17);
            Some([d(0)?, d(1)?, d(2)?, 255])
        }
        _ => None,
    }
}

/// Read fill + outline from a captured `<wps:spPr>…</wps:spPr>` fragment.
/// Only the shape's own `solidFill` / `noFill` (direct children of
/// `spPr`) set the fill; the ones under `<a:ln>` set the outline.
pub(crate) fn apply_sp_pr(fragment: &[u8], story: &mut TextBoxStory) {
    let mut reader = Reader::from_reader(fragment);
    let mut buf = Vec::new();
    let mut stack: Vec<Vec<u8>> = Vec::new();
    let mut ln_width: Option<i64> = None;
    let mut ln_color: Option<[u8; 4]> = None;
    let mut ln_none = false;
    let mut saw_ln = false;
    loop {
        let (e, is_start) = match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => (e.into_owned(), true),
            Ok(Event::Empty(e)) => (e.into_owned(), false),
            Ok(Event::End(_)) => {
                stack.pop();
                buf.clear();
                continue;
            }
            Ok(Event::Eof) | Err(_) => break,
            Ok(_) => {
                buf.clear();
                continue;
            }
        };
        let name = e.name().as_ref().to_vec();
        let parent = stack.last().map(|n| n.as_slice()).unwrap_or(b"");
        let grand = if stack.len() >= 2 {
            stack[stack.len() - 2].as_slice()
        } else {
            b""
        };
        match name.as_slice() {
            b"a:ln" => {
                saw_ln = true;
                ln_width = attr(&e, b"w").and_then(|v| v.parse().ok());
            }
            b"a:noFill" if parent == b"wps:spPr" => story.fill = None,
            b"a:noFill" if parent == b"a:ln" => ln_none = true,
            _ if parent == b"a:solidFill" && grand == b"wps:spPr" => {
                if let Some(c) = drawing_color(&e) {
                    story.fill = Some(c);
                }
            }
            _ if parent == b"a:solidFill" && grand == b"a:ln" => {
                if let Some(c) = drawing_color(&e) {
                    ln_color = Some(c);
                }
            }
            _ => {}
        }
        if is_start {
            stack.push(name);
        }
        buf.clear();
    }
    story.outline = (saw_ln && !ln_none).then(|| ShapeOutline {
        color: ln_color.unwrap_or([0, 0, 0, 255]),
        width_emu: ln_width.unwrap_or(9_525),
    });
}

/// Byte ranges `[start, end)` of every OUTERMOST `qname` element inside
/// `fragment` (self-closing ones included), in document order.
pub(crate) fn element_ranges(fragment: &[u8], qname: &[u8]) -> Vec<(usize, usize)> {
    let mut reader = Reader::from_reader(fragment);
    reader.config_mut().trim_text(false);
    let mut out = Vec::new();
    let mut buf = Vec::new();
    let mut prev = 0usize;
    while let Ok(ev) = reader.read_event_into(&mut buf) {
        match ev {
            Event::Start(e) if e.name().as_ref() == qname => {
                let end_tag = e.to_end().into_owned();
                let mut skip = Vec::new();
                if reader.read_to_end_into(end_tag.name(), &mut skip).is_err() {
                    break;
                }
                out.push((prev, reader.buffer_position() as usize));
            }
            Event::Empty(e) if e.name().as_ref() == qname => {
                out.push((prev, reader.buffer_position() as usize));
            }
            Event::Eof => break,
            _ => {}
        }
        prev = reader.buffer_position() as usize;
        buf.clear();
    }
    out
}

/* ------------------------------------------------------------------ */
/* VML                                                                 */
/* ------------------------------------------------------------------ */

/// A text box read from a VML `<w:pict>` / `<mc:Fallback>` subtree.
pub(crate) struct VmlTextBox {
    pub width_emu: i64,
    pub height_emu: i64,
    pub anchor: Option<Box<FloatAnchor>>,
    pub story: TextBoxStory,
}

/// A CSS-ish VML length (`72pt`, `1in`, `2.54cm`, `10mm`, `96px`, bare
/// numbers as px) → EMU.
fn vml_len_emu(v: &str) -> Option<i64> {
    let v = v.trim();
    let (num, unit) = match v.find(|c: char| c.is_ascii_alphabetic() || c == '%') {
        Some(i) => (&v[..i], &v[i..]),
        None => (v, "px"),
    };
    let n: f64 = num.trim().parse().ok()?;
    let per = match unit {
        "pt" => 12_700.0,
        "in" => 914_400.0,
        "cm" => 360_000.0,
        "mm" => 36_000.0,
        "px" => 9_525.0,
        "emu" => 1.0,
        _ => return None,
    };
    Some((n * per).round() as i64)
}

/// VML colour (`#rrggbb`, `#rgb`, `black`, `white`, `red [10]`, …).
fn vml_color(v: &str) -> Option<[u8; 4]> {
    let v = v.trim();
    let v = v.split_whitespace().next().unwrap_or(v);
    if v.starts_with('#') {
        return hex_rgb(v);
    }
    match v {
        "black" => Some([0, 0, 0, 255]),
        "white" => Some([255, 255, 255, 255]),
        "red" => Some([255, 0, 0, 255]),
        "green" => Some([0, 128, 0, 255]),
        "blue" => Some([0, 0, 255, 255]),
        "yellow" => Some([255, 255, 0, 255]),
        "gray" | "grey" => Some([128, 128, 128, 255]),
        _ => None,
    }
}

fn vml_on(v: Option<String>, default: bool) -> bool {
    match v.as_deref().map(str::trim) {
        Some("f") | Some("false") | Some("0") => false,
        Some("t") | Some("true") | Some("1") => true,
        _ => default,
    }
}

/// Parse a VML text box out of `fragment` (a `<w:pict>` or
/// `<mc:Fallback>` element). `None` when the subtree holds no
/// `<v:textbox><w:txbxContent>` (a legacy picture, a line, …).
pub(crate) fn parse_vml(
    fragment: &[u8],
    resolver: &StyleResolver<'_>,
    ns: &NamespaceScope,
) -> Option<VmlTextBox> {
    let mut reader = Reader::from_reader(fragment);
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut prev = 0usize;
    let mut shape_style: Option<String> = None;
    let mut filled = true;
    let mut fill: Option<[u8; 4]> = Some([255, 255, 255, 255]);
    let mut stroked = true;
    let mut stroke_color = [0u8, 0, 0, 255];
    let mut stroke_w: i64 = 9_525;
    let mut inset: Option<String> = None;
    let mut wrap: Option<String> = None;
    let mut story_blocks: Option<Vec<Block>> = None;
    loop {
        let ev = match reader.read_event_into(&mut buf) {
            Ok(ev) => ev,
            Err(_) => return None,
        };
        let (e, is_start) = match ev {
            Event::Start(e) => (e.into_owned(), true),
            Event::Empty(e) => (e.into_owned(), false),
            Event::Eof => break,
            _ => {
                prev = reader.buffer_position() as usize;
                buf.clear();
                continue;
            }
        };
        match e.name().as_ref() {
            b"v:shape" | b"v:rect" | b"v:roundrect" if shape_style.is_none() => {
                shape_style = Some(attr(&e, b"style").unwrap_or_default());
                filled = vml_on(attr(&e, b"filled"), true);
                if let Some(c) = attr(&e, b"fillcolor") {
                    fill = vml_color(&c).or(fill);
                }
                stroked = vml_on(attr(&e, b"stroked"), true);
                if let Some(c) = attr(&e, b"strokecolor").and_then(|c| vml_color(&c)) {
                    stroke_color = c;
                }
                if let Some(w) = attr(&e, b"strokeweight").and_then(|w| vml_len_emu(&w)) {
                    stroke_w = w;
                }
            }
            b"v:textbox" => inset = attr(&e, b"inset"),
            b"w10:wrap" => wrap = attr(&e, b"type"),
            b"w:txbxContent" if story_blocks.is_none() => {
                let start = prev;
                if is_start {
                    let end_tag = e.to_end().into_owned();
                    let mut skip = Vec::new();
                    reader.read_to_end_into(end_tag.name(), &mut skip).ok()?;
                }
                let end = reader.buffer_position() as usize;
                story_blocks = Some(parse_story(fragment.get(start..end)?, resolver, ns)?);
            }
            _ => {}
        }
        prev = reader.buffer_position() as usize;
        buf.clear();
    }
    let body = story_blocks?;
    let style = shape_style?;
    let props: Vec<(String, String)> = style
        .split(';')
        .filter_map(|kv| {
            let (k, v) = kv.split_once(':')?;
            Some((k.trim().to_ascii_lowercase(), v.trim().to_string()))
        })
        .collect();
    let get = |k: &str| props.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
    let width_emu = get("width").and_then(vml_len_emu).unwrap_or(914_400).max(1);
    let height_emu = get("height")
        .and_then(vml_len_emu)
        .unwrap_or(457_200)
        .max(1);

    let mut story = TextBoxStory {
        body,
        fill: if filled { fill } else { None },
        outline: stroked.then_some(ShapeOutline {
            color: stroke_color,
            width_emu: stroke_w,
        }),
        v_align: match get("v-text-anchor") {
            Some("middle") | Some("middle-center") => TextBoxVAlign::Center,
            Some("bottom") => TextBoxVAlign::Bottom,
            _ => TextBoxVAlign::Top,
        },
        ..TextBoxStory::default()
    };
    if let Some(ins) = inset {
        let parts: Vec<Option<i64>> = ins.split(',').map(vml_len_emu).collect();
        let pick = |i: usize, d: i64| parts.get(i).copied().flatten().unwrap_or(d);
        story.inset_left_emu = pick(0, story.inset_left_emu);
        story.inset_top_emu = pick(1, story.inset_top_emu);
        story.inset_right_emu = pick(2, story.inset_right_emu);
        story.inset_bottom_emu = pick(3, story.inset_bottom_emu);
    }

    let anchor = (get("position") == Some("absolute")).then(|| {
        let h_align = match get("mso-position-horizontal") {
            Some("left") => Some(FloatAlign::Left),
            Some("center") => Some(FloatAlign::Center),
            Some("right") => Some(FloatAlign::Right),
            Some("inside") => Some(FloatAlign::Inside),
            Some("outside") => Some(FloatAlign::Outside),
            _ => None,
        };
        let v_align = match get("mso-position-vertical") {
            Some("top") => Some(FloatAlign::Top),
            Some("center") => Some(FloatAlign::Center),
            Some("bottom") => Some(FloatAlign::Bottom),
            Some("inside") => Some(FloatAlign::Inside),
            Some("outside") => Some(FloatAlign::Outside),
            _ => None,
        };
        let h_rel = match get("mso-position-horizontal-relative") {
            Some("page") => HRelativeFrom::Page,
            Some("margin") => HRelativeFrom::Margin,
            Some("left-margin-area") => HRelativeFrom::LeftMargin,
            Some("right-margin-area") => HRelativeFrom::RightMargin,
            Some("inner-margin-area") => HRelativeFrom::InsideMargin,
            Some("outer-margin-area") => HRelativeFrom::OutsideMargin,
            Some("char") => HRelativeFrom::Character,
            _ => HRelativeFrom::Column,
        };
        let v_rel = match get("mso-position-vertical-relative") {
            Some("page") => VRelativeFrom::Page,
            Some("margin") => VRelativeFrom::Margin,
            Some("top-margin-area") => VRelativeFrom::TopMargin,
            Some("bottom-margin-area") => VRelativeFrom::BottomMargin,
            Some("inner-margin-area") => VRelativeFrom::InsideMargin,
            Some("outer-margin-area") => VRelativeFrom::OutsideMargin,
            Some("line") => VRelativeFrom::Line,
            _ => VRelativeFrom::Paragraph,
        };
        let off = |k: &str| get(k).and_then(vml_len_emu).unwrap_or(0);
        let z: i64 = get("z-index").and_then(|z| z.parse().ok()).unwrap_or(0);
        Box::new(FloatAnchor {
            position_h: HPosition {
                relative_from: h_rel,
                offset: h_align.map_or(FloatOffset::Emu(off("margin-left")), FloatOffset::Align),
            },
            position_v: VPosition {
                relative_from: v_rel,
                offset: v_align.map_or(FloatOffset::Emu(off("margin-top")), FloatOffset::Align),
            },
            relative_height: z.unsigned_abs().min(u32::MAX as u64) as u32,
            behind_doc: z < 0,
            wrap: match wrap.as_deref() {
                Some("square") => WrapKind::Square,
                Some("tight") => WrapKind::Tight,
                Some("through") => WrapKind::Through,
                Some("topAndBottom") => WrapKind::TopAndBottom,
                _ => WrapKind::None,
            },
            ..FloatAnchor::default()
        })
    });
    Some(VmlTextBox {
        width_emu,
        height_emu,
        anchor,
        story,
    })
}

/* ------------------------------------------------------------------ */
/* Writer                                                              */
/* ------------------------------------------------------------------ */

/// Story regeneration callback: serialize `blocks` (the body's own block
/// emitter, passthrough included) into `out`.
pub(crate) type EmitBlocks<'a> = &'a dyn Fn(&[Block], &mut String);

/// A regenerated `<w:txbxContent>` element.
fn story_element(blocks: &[Block], emit: EmitBlocks<'_>) -> String {
    let mut s = String::from("<w:txbxContent>");
    if blocks.is_empty() {
        s.push_str("<w:p/>");
    } else {
        emit(blocks, &mut s);
    }
    s.push_str("</w:txbxContent>");
    s
}

/// The text box's container XML (no enclosing `<w:r>`): verbatim when
/// clean, story-spliced when dirty, synthesized when engine-authored (or
/// when a dirty box lost its splice ranges).
pub(crate) fn container_xml(
    width_emu: i64,
    height_emu: i64,
    anchor: Option<&FloatAnchor>,
    story: &TextBoxStory,
    emit: EmitBlocks<'_>,
) -> String {
    if let Some(src) = &story.source_xml {
        if !story.dirty {
            return src.clone();
        }
        if let Some(spliced) = splice_story(src, &story.story_ranges, &story.body, emit) {
            return spliced;
        }
    }
    synthesize(width_emu, height_emu, anchor, story, emit)
}

/// Replace every `<w:txbxContent>` range of `src` with the regenerated
/// story. `None` when a range is out of bounds, overlapping, not on a
/// char boundary or does not start with the story element (regenerate
/// beats splicing a misaligned range).
fn splice_story(
    src: &str,
    ranges: &[(u32, u32)],
    body: &[Block],
    emit: EmitBlocks<'_>,
) -> Option<String> {
    if ranges.is_empty() {
        return None;
    }
    let story = story_element(body, emit);
    let mut out = String::with_capacity(src.len() + story.len());
    let mut cursor = 0usize;
    for &(s, e) in ranges {
        let (s, e) = (s as usize, e as usize);
        if s < cursor || e < s || e > src.len() || !src.is_char_boundary(s) {
            return None;
        }
        if !src.is_char_boundary(e) || !src[s..].starts_with("<w:txbxContent") {
            return None;
        }
        out.push_str(&src[cursor..s]);
        out.push_str(&story);
        cursor = e;
    }
    out.push_str(&src[cursor..]);
    Some(out)
}

/// `true` when some text box of `para` needs its container rewritten.
pub(crate) fn has_dirty_text_box(para: &Paragraph) -> bool {
    para.inline_objects
        .iter()
        .any(|io| matches!(&io.kind, InlineKind::TextBox { story, .. } if story.dirty))
}

/// Splice every dirty text box's regenerated container into the CLEAN
/// host paragraph's passthrough bytes `raw` at its `host_range`. `None`
/// (the caller regenerates the paragraph) when a dirty box has no host
/// range, or a range does not hold exactly the box's source container.
pub(crate) fn splice_host(raw: &str, para: &Paragraph, emit: EmitBlocks<'_>) -> Option<String> {
    let mut edits: Vec<(usize, usize, String)> = Vec::new();
    for io in &para.inline_objects {
        let InlineKind::TextBox {
            width_emu,
            height_emu,
            story,
        } = &io.kind
        else {
            continue;
        };
        if !story.dirty {
            continue;
        }
        let (s, e) = story.host_range?;
        let (s, e) = (s as usize, e as usize);
        let src = story.source_xml.as_deref()?;
        if e > raw.len() || !raw.is_char_boundary(s) || !raw.is_char_boundary(e) {
            return None;
        }
        if raw.get(s..e)? != src {
            return None;
        }
        edits.push((
            s,
            e,
            container_xml(*width_emu, *height_emu, io.anchor.as_deref(), story, emit),
        ));
    }
    edits.sort_by_key(|(s, _, _)| *s);
    let mut out = String::with_capacity(raw.len() + 256);
    let mut cursor = 0usize;
    for (s, e, x) in edits {
        if s < cursor {
            return None;
        }
        out.push_str(&raw[cursor..s]);
        out.push_str(&x);
        cursor = e;
    }
    out.push_str(&raw[cursor..]);
    Some(out)
}

/// `RRGGBB` for a DrawingML `srgbClr`.
fn rgb_hex(c: [u8; 4]) -> String {
    format!("{:02X}{:02X}{:02X}", c[0], c[1], c[2])
}

/// Synthesize a `<w:drawing>` text box (`<wps:wsp>` inside a
/// `<wp:anchor>` or `<wp:inline>`) from the typed model. The document
/// root must bind `wps` (`writer::build_document_xml_with_root`).
fn synthesize(
    width_emu: i64,
    height_emu: i64,
    anchor: Option<&FloatAnchor>,
    story: &TextBoxStory,
    emit: EmitBlocks<'_>,
) -> String {
    let cx = width_emu.max(1);
    let cy = height_emu.max(1);
    let mut out = String::from("<w:drawing>");
    match anchor {
        Some(a) => emit_anchor_open(a, cx, cy, &mut out),
        None => out.push_str(&format!(
            "<wp:inline distT=\"0\" distB=\"0\" distL=\"0\" distR=\"0\">\
             <wp:extent cx=\"{cx}\" cy=\"{cy}\"/>\
             <wp:effectExtent l=\"0\" t=\"0\" r=\"0\" b=\"0\"/>\
             <wp:docPr id=\"1\" name=\"Text Box\"/>"
        )),
    }
    out.push_str(
        "<wp:cNvGraphicFramePr/><a:graphic>\
         <a:graphicData uri=\"http://schemas.microsoft.com/office/word/2010/wordprocessingShape\">\
         <wps:wsp><wps:cNvSpPr txBox=\"1\"/><wps:spPr>",
    );
    out.push_str(&format!(
        "<a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"{cx}\" cy=\"{cy}\"/></a:xfrm>\
         <a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom>"
    ));
    match story.fill {
        Some(c) => out.push_str(&format!(
            "<a:solidFill><a:srgbClr val=\"{}\"/></a:solidFill>",
            rgb_hex(c)
        )),
        None => out.push_str("<a:noFill/>"),
    }
    match story.outline {
        Some(o) => out.push_str(&format!(
            "<a:ln w=\"{}\"><a:solidFill><a:srgbClr val=\"{}\"/></a:solidFill></a:ln>",
            o.width_emu.max(0),
            rgb_hex(o.color)
        )),
        None => out.push_str("<a:ln><a:noFill/></a:ln>"),
    }
    out.push_str("</wps:spPr><wps:txbx>");
    out.push_str(&story_element(&story.body, emit));
    let anchor_attr = match story.v_align {
        TextBoxVAlign::Top => "t",
        TextBoxVAlign::Center => "ctr",
        TextBoxVAlign::Bottom => "b",
    };
    out.push_str(&format!(
        "</wps:txbx><wps:bodyPr rot=\"0\" vert=\"horz\" wrap=\"square\" \
         lIns=\"{}\" tIns=\"{}\" rIns=\"{}\" bIns=\"{}\" anchor=\"{anchor_attr}\" anchorCtr=\"0\">{}</wps:bodyPr>",
        story.inset_left_emu,
        story.inset_top_emu,
        story.inset_right_emu,
        story.inset_bottom_emu,
        if story.auto_fit {
            "<a:spAutoFit/>"
        } else {
            "<a:noAutofit/>"
        }
    ));
    out.push_str("</wps:wsp></a:graphicData></a:graphic>");
    out.push_str(if anchor.is_some() {
        "</wp:anchor></w:drawing>"
    } else {
        "</wp:inline></w:drawing>"
    });
    out
}

/// `xmlns:wps` — bound on the synthesized document root whenever an
/// engine-authored text box is written.
pub(crate) const NS_WPS: &str = "http://schemas.microsoft.com/office/word/2010/wordprocessingShape";

/// `true` when any paragraph in `blocks` (cells and nested stories
/// included) carries an engine-authored text box — the writer must then
/// bind `wps` (and the DrawingML prefixes) on the part root.
pub(crate) fn blocks_need_wps(blocks: &[Block]) -> bool {
    blocks.iter().any(|b| match b {
        Block::Paragraph(p) => p.inline_objects.iter().any(|io| match &io.kind {
            InlineKind::TextBox { story, .. } => {
                story.source_xml.is_none()
                    || (story.dirty && story.story_ranges.is_empty())
                    || blocks_need_wps(&story.body)
            }
            _ => false,
        }),
        Block::Table(t) => t
            .rows
            .iter()
            .any(|r| r.cells.iter().any(|c| blocks_need_wps(&c.blocks))),
    })
}

/// `true` when any paragraph in `blocks` carries a text box at all.
pub(crate) fn blocks_have_text_box(blocks: &[Block]) -> bool {
    blocks.iter().any(|b| match b {
        Block::Paragraph(p) => p
            .inline_objects
            .iter()
            .any(|io| matches!(io.kind, InlineKind::TextBox { .. })),
        Block::Table(t) => t
            .rows
            .iter()
            .any(|r| r.cells.iter().any(|c| blocks_have_text_box(&c.blocks))),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vml_lengths_convert_to_emu() {
        assert_eq!(vml_len_emu("72pt"), Some(914_400));
        assert_eq!(vml_len_emu("1in"), Some(914_400));
        assert_eq!(vml_len_emu("2.54cm"), Some(914_400));
        assert_eq!(vml_len_emu("96"), Some(914_400));
        assert_eq!(vml_len_emu("x"), None);
    }

    #[test]
    fn element_ranges_find_outermost_story_elements() {
        let xml = b"<a><w:txbxContent><w:p/></w:txbxContent><b><w:txbxContent/></b></a>";
        let r = element_ranges(xml, b"w:txbxContent");
        assert_eq!(r.len(), 2);
        assert_eq!(
            &xml[r[0].0..r[0].1],
            b"<w:txbxContent><w:p/></w:txbxContent>"
        );
        assert_eq!(&xml[r[1].0..r[1].1], b"<w:txbxContent/>");
    }

    #[test]
    fn splice_refuses_misaligned_ranges() {
        let emit: EmitBlocks<'_> = &|_, out: &mut String| out.push_str("<w:p/>");
        assert!(splice_story("<x/>", &[(0, 4)], &[], emit).is_none());
        let src = "<a><w:txbxContent>old</w:txbxContent></a>";
        let got = splice_story(src, &[(3, 37)], &[], emit).expect("aligned");
        assert_eq!(got, "<a><w:txbxContent><w:p/></w:txbxContent></a>");
    }
}
