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
        hyphenation: None,
        font_line: None,
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

/* ---- issue #326: automatic hyphenation ---------------------------- */

use crate::hyphen::AutoHyphenation;
use text_pipeline::Hyphenator;

const PROSE: &str = "The international organization recommends comprehensive documentation of \
representational characteristics in particular circumstances whenever administrative \
responsibilities are distributed.";

fn lay_auto(
    fonts: &FontStack,
    text: &str,
    max_width: f32,
    hy: Option<&AutoHyphenation<'_>>,
) -> ParagraphBox {
    layout_paragraph(ParagraphConfig {
        text,
        fonts,
        inline_objects: &[],
        spans: &[span(text.len())],
        base_direction: ShapingDirection::Ltr,
        max_width,
        line_height: 14.0,
        line_height_exact: false,
        alignment: Alignment::Justify,
        indent_start_px: 0.0,
        indent_end_px: 0.0,
        first_line_indent_px: 0.0,
        hanging_indent_px: 0.0,
        marker_text: None,
        px_size_for_marker: PX,
        tab_stops_px: &[],
        hyphenation: hy,
        font_line: None,
    })
}

/// The whole of `text` in American English.
fn auto(text: &str) -> Vec<(std::ops::Range<u32>, &'static Hyphenator)> {
    vec![(0..text.len() as u32, Hyphenator::en_us())]
}

fn line_end(para: &ParagraphBox, i: usize) -> usize {
    para.lines[i]
        .runs
        .iter()
        .map(|r| r.source_range.end as usize)
        .max()
        .unwrap_or(para.lines[i].source_start as usize)
}

/// With `autoHyphenation` the overflowing word breaks at a pattern point
/// inside it: the line ends between two letters with ONE synthetic hyphen
/// (flagged `Auto`, counted in the width, within the measure) and the next
/// line starts with the rest of the word. Off, the same text never
/// hyphenates.
#[test]
fn an_overflowing_word_hyphenates_at_a_pattern_point() {
    let fonts = stack();
    let max = 150.0;
    let ranges = auto(PROSE);
    let hy = AutoHyphenation {
        ranges: &ranges,
        zone_px: 5.0,
        consecutive_limit: 0,
        no_caps: false,
    };
    let para = lay_auto(&fonts, PROSE, max, Some(&hy));
    let auto_lines: Vec<usize> = (0..para.lines.len())
        .filter(|&i| para.lines[i].hyphen == LineHyphen::Auto)
        .collect();
    assert!(!auto_lines.is_empty(), "some line hyphenates");
    for &i in &auto_lines {
        let end = line_end(&para, i);
        let before = PROSE[..end].chars().last().unwrap();
        let after = PROSE[end..].chars().next().unwrap();
        assert!(
            before.is_alphabetic() && after.is_alphabetic(),
            "line {i} ends inside a word"
        );
        assert_eq!(para.lines[i + 1].source_start as usize, end);
        let synth = para.lines[i]
            .runs
            .iter()
            .flat_map(|r| &r.glyphs)
            .filter(|g| g.synthetic)
            .count();
        assert_eq!(synth, 1);
        assert!(para.lines[i].width <= max + 0.01);
    }
    /* Off: no line ends inside a word with a hyphen. */
    let off = lay_auto(&fonts, PROSE, max, None);
    assert!(off.lines.iter().all(|l| l.hyphen.is_none()));
    assert!(off.lines.len() >= para.lines.len());
}

/// The hyphenation zone: a gap narrower than the zone is left ragged
/// rather than hyphenating into it.
#[test]
fn a_wide_zone_keeps_words_whole() {
    let fonts = stack();
    let ranges = auto(PROSE);
    let hy = AutoHyphenation {
        ranges: &ranges,
        zone_px: 1000.0,
        consecutive_limit: 0,
        no_caps: false,
    };
    let para = lay_auto(&fonts, PROSE, 150.0, Some(&hy));
    assert!(para.lines.iter().all(|l| l.hyphen.is_none()));
}

/// `consecutiveHyphenLimit = 1`: never two hyphenated lines in a row.
#[test]
fn the_consecutive_limit_holds() {
    let fonts = stack();
    let text = "Internationalization institutionalization characterization \
                representationalism incomprehensibilities counterrevolutionaries \
                electroencephalography internationalization end.";
    let ranges = auto(text);
    for limit in [1u32, 2] {
        let hy = AutoHyphenation {
            ranges: &ranges,
            zone_px: 0.0,
            consecutive_limit: limit,
            no_caps: false,
        };
        let para = lay_auto(&fonts, text, 90.0, Some(&hy));
        let mut run = 0;
        for l in &para.lines {
            run = if l.hyphen.is_none() { 0 } else { run + 1 };
            assert!(run <= limit as usize, "limit {limit} exceeded");
        }
    }
}

/// Never the paragraph's last word; never a word in capitals under
/// `doNotHyphenateCaps`; never Arabic.
#[test]
fn last_words_capitals_and_arabic_stay_whole() {
    let fonts = stack();
    let w = width(&fonts, "Short words then ");
    let last = "Short words then incomprehensibilities.";
    let ranges = auto(last);
    let hy = AutoHyphenation {
        ranges: &ranges,
        zone_px: 0.0,
        consecutive_limit: 0,
        no_caps: false,
    };
    let para = lay_auto(&fonts, last, w + 30.0, Some(&hy));
    assert!(para.lines.iter().all(|l| l.hyphen.is_none()), "last word");

    let caps = "Short words then INCOMPREHENSIBILITIES and more.";
    let ranges = auto(caps);
    let mut hy = AutoHyphenation {
        ranges: &ranges,
        zone_px: 0.0,
        consecutive_limit: 0,
        no_caps: true,
    };
    let para = lay_auto(&fonts, caps, w + 30.0, Some(&hy));
    assert!(para.lines.iter().all(|l| l.hyphen.is_none()), "caps");
    hy.no_caps = false;
    let para = lay_auto(&fonts, caps, w + 30.0, Some(&hy));
    assert!(
        para.lines.iter().any(|l| l.hyphen == LineHyphen::Auto),
        "caps allowed"
    );

    let arabic = "\u{0627}\u{0644}\u{0643}\u{062A}\u{0627}\u{0628} \u{0627}\u{0644}\u{0645}\u{0633}\u{062A}\u{0634}\u{0641}\u{064A}\u{0627}\u{062A} \u{0648}\u{0627}\u{0644}\u{0645}\u{062F}\u{0627}\u{0631}\u{0633} \u{0647}\u{0646}\u{0627}";
    let ranges = auto(arabic);
    let hy = AutoHyphenation {
        ranges: &ranges,
        zone_px: 0.0,
        consecutive_limit: 0,
        no_caps: false,
    };
    let para = layout_paragraph(ParagraphConfig {
        text: arabic,
        fonts: &fonts,
        inline_objects: &[],
        spans: &[span(arabic.len())],
        base_direction: ShapingDirection::Rtl,
        max_width: 40.0,
        line_height: 14.0,
        line_height_exact: false,
        alignment: Alignment::Justify,
        indent_start_px: 0.0,
        indent_end_px: 0.0,
        first_line_indent_px: 0.0,
        hanging_indent_px: 0.0,
        marker_text: None,
        px_size_for_marker: PX,
        tab_stops_px: &[],
        hyphenation: Some(&hy),
        font_line: None,
    });
    assert!(para.lines.iter().all(|l| l.hyphen.is_none()), "Arabic");
}
