//! Issue #335 — the soft-hyphen break and the non-breaking hyphen through
//! the greedy composer.

use crate::boxes::{LineHyphen, ParagraphBox, StyleSpan};
use crate::paragraph::{ParagraphConfig, layout_paragraph};
use std::collections::HashMap;
use std::sync::Arc;
use text_pipeline::{Alignment, FontStack, LoadedFont, ShapingDirection, shape_text};

const SHY: char = '\u{00AD}';
const NBH: char = '\u{2011}';
const PX: f32 = 10.0;

fn stack() -> FontStack {
    let face = |id: &str, bytes: &[u8]| {
        Arc::new(LoadedFont::parse(id.into(), bytes.to_vec()).expect("font"))
    };
    let mut faces = HashMap::new();
    faces.insert(
        "latin".to_string(),
        face(
            "latin",
            include_bytes!("../../../ts/fonts/LiberationSans-Regular.ttf"),
        ),
    );
    faces.insert(
        "naskh".to_string(),
        face(
            "naskh",
            include_bytes!("../../../ts/fonts/Amiri-Regular.ttf"),
        ),
    );
    FontStack::from_faces(faces, "latin")
}

fn span(len: usize) -> StyleSpan {
    StyleSpan {
        start: 0,
        end: len as u32,
        px_size: PX,
        color: [0, 0, 0, 255],
        bold: false,
        italic: false,
        underline: engine::UnderlineStyle::None,
        strike: false,
        bg_color: None,
        font_family: None,
        caps_transform: false,
        baseline_shift_px: 0.0,
        cs: None,
    }
}

fn lay(fonts: &FontStack, text: &str, max_width: f32, dir: ShapingDirection) -> ParagraphBox {
    layout_paragraph(ParagraphConfig {
        text,
        fonts,
        inline_objects: &[],
        spans: &[span(text.len())],
        base_direction: dir,
        max_width,
        line_height: 14.0,
        line_height_exact: false,
        alignment: Alignment::Start,
        indent_start_px: 0.0,
        indent_end_px: 0.0,
        first_line_indent_px: 0.0,
        hanging_indent_px: 0.0,
        marker_text: None,
        px_size_for_marker: PX,
        tab_stops_px: &[],
    })
}

fn width(fonts: &FontStack, s: &str) -> f32 {
    shape_text(fonts.face("latin").unwrap(), s, ShapingDirection::Ltr, PX).total_advance
}

fn line_text<'a>(text: &'a str, para: &ParagraphBox, i: usize) -> &'a str {
    let l = &para.lines[i];
    let start = l.source_start as usize;
    let end = l
        .runs
        .iter()
        .map(|r| r.source_range.end as usize)
        .max()
        .unwrap_or(start);
    &text[start..end]
}

/// A line that breaks right after a U+00AD ends with ONE synthetic glyph —
/// the face's hyphen-minus — counted in the line width, and the line is
/// flagged `Soft`. The next line starts behind the soft hyphen.
#[test]
fn a_soft_hyphen_break_draws_a_synthetic_hyphen() {
    let fonts = stack();
    let text = format!("aaaa bbbbbb{SHY}cccccc dddd");
    let max = width(&fonts, "aaaa bbbbbb-") + 1.0;
    let para = lay(&fonts, &text, max, ShapingDirection::Ltr);
    assert!(para.lines.len() >= 2, "{:?}", para.lines.len());
    let l0 = &para.lines[0];
    assert_eq!(line_text(&text, &para, 0), format!("aaaa bbbbbb{SHY}"));
    assert_eq!(l0.hyphen, LineHyphen::Soft);
    let last = l0.runs.last().unwrap().glyphs.last().unwrap();
    assert!(last.synthetic, "the hyphen is synthetic");
    assert_eq!(Some(last.id), fonts.face("latin").unwrap().glyph_id('-'));
    let synth = l0
        .runs
        .iter()
        .flat_map(|r| &r.glyphs)
        .filter(|g| g.synthetic)
        .count();
    assert_eq!(synth, 1);
    assert!(l0.width <= max, "{} > {max}", l0.width);
    assert!((l0.width - width(&fonts, "aaaa bbbbbb-")).abs() < 0.01);
    assert_eq!(para.lines[1].hyphen, LineHyphen::None);
    assert_eq!(line_text(&text, &para, 1), "cccccc dddd");
}

/// An unbroken soft hyphen is invisible: zero width, no drawn hyphen.
#[test]
fn an_unbroken_soft_hyphen_is_invisible() {
    let fonts = stack();
    let text = format!("aaaa bbbbbb{SHY}cccccc dddd");
    let para = lay(&fonts, &text, 10_000.0, ShapingDirection::Ltr);
    assert_eq!(para.lines.len(), 1);
    let l0 = &para.lines[0];
    assert_eq!(l0.hyphen, LineHyphen::None);
    assert!(l0.runs.iter().flat_map(|r| &r.glyphs).all(|g| !g.synthetic));
    assert!((l0.width - width(&fonts, "aaaa bbbbbbcccccc dddd")).abs() < 0.01);
}

/// The break at a soft hyphen needs room for the hyphen it draws: a word
/// part that fits only without it moves to the next line whole.
#[test]
fn a_soft_hyphen_break_needs_room_for_the_hyphen() {
    let fonts = stack();
    let text = format!("aaaa bbbbbb{SHY}cccccc dddd");
    let max = width(&fonts, "aaaa bbbbbb") + 0.5;
    assert!(max < width(&fonts, "aaaa bbbbbb-"));
    let para = lay(&fonts, &text, max, ShapingDirection::Ltr);
    assert_eq!(line_text(&text, &para, 0), "aaaa ");
    assert_eq!(para.lines[0].hyphen, LineHyphen::None);
    for l in &para.lines {
        assert!(l.width <= max + 0.01 || l.runs.len() == 1, "no overflow");
    }
}

/// U+2011 renders as a hyphen (never `.notdef`, even in a face without a
/// U+2011 glyph — the shaper falls back to U+2010) and never breaks: the
/// whole word moves to the next line.
#[test]
fn a_non_breaking_hyphen_renders_and_never_breaks() {
    let fonts = stack();
    let text = format!("aaaaaaaa bbbb{NBH}cccc dd");
    let max = width(&fonts, "aaaaaaaa bbbb-c");
    let para = lay(&fonts, &text, max, ShapingDirection::Ltr);
    assert_eq!(line_text(&text, &para, 0), "aaaaaaaa ");
    assert_eq!(line_text(&text, &para, 1), format!("bbbb{NBH}cccc dd"));
    let nbh_at = text.find(NBH).unwrap() as u32;
    let g = para
        .lines
        .iter()
        .flat_map(|l| &l.runs)
        .flat_map(|r| {
            r.glyphs
                .iter()
                .map(move |g| (r.source_range.start + g.cluster, g))
        })
        .find(|(at, _)| *at == nbh_at)
        .map(|(_, g)| g)
        .expect("a glyph for U+2011");
    assert_ne!(g.id, 0, "never .notdef");
    assert!(g.x_advance > 0.0);
    assert!(para.lines.iter().all(|l| l.hyphen.is_none()));
}

/// In an RTL run the hyphen sits at the logical end — the visual LEFT of
/// the last character (Amiri carries a hyphen-minus).
#[test]
fn an_rtl_soft_hyphen_break_draws_the_hyphen_on_the_left() {
    let fonts = stack();
    let word = "\u{0628}\u{0628}\u{0628}\u{0628}";
    let text = format!("{word} {word}{SHY}{word} {word}");
    let arabic = fonts.face("naskh").unwrap();
    let probe = |s: &str| shape_text(arabic, s, ShapingDirection::Rtl, PX).total_advance;
    let max = probe(&format!("{word} {word}-")) + 1.0;
    let para = lay(&fonts, &text, max, ShapingDirection::Rtl);
    let l0 = &para.lines[0];
    assert_eq!(
        l0.hyphen,
        LineHyphen::Soft,
        "{:?}",
        line_text(&text, &para, 0)
    );
    let run = l0
        .runs
        .iter()
        .find(|r| r.glyphs.iter().any(|g| g.synthetic))
        .expect("a run with the hyphen");
    assert_eq!(run.direction, ShapingDirection::Rtl);
    let i = run.glyphs.iter().position(|g| g.synthetic).unwrap();
    let last_cluster = run
        .glyphs
        .iter()
        .filter(|g| !g.synthetic)
        .map(|g| g.cluster)
        .max();
    assert_eq!(
        run.glyphs.get(i + 1).map(|g| g.cluster),
        last_cluster,
        "hyphen left of the logically last glyph"
    );
    assert_eq!(Some(run.glyphs[i].id), arabic.glyph_id('-'));
}
