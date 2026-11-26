//! Issue #599 — U+00A0 NO-BREAK SPACE through the shaper and the greedy
//! composer. `&nbsp;` in pasted / opened HTML now decodes to it (it used to
//! become an ASCII space and collapse), so the layout side must treat it as
//! what it is: a space-wide glyph that no line break may split.
//!
//! The break rule is UAX #14 (`GL`, glue: no break before or after) through
//! `icu_segmenter`; the advance is whatever the face's `cmap` / HarfBuzz's
//! space fallback gives U+00A0 — pinned against U+0020 for every face the
//! app ships (`ts/public/fonts/`).

use crate::boxes::{ParagraphBox, StyleSpan};
use crate::paragraph::{ParagraphConfig, layout_paragraph};
use std::collections::HashMap;
use std::sync::Arc;
use text_pipeline::{Alignment, FontStack, LoadedFont, ShapingDirection, shape_text};

const NBSP: char = '\u{00A0}';
const PX: f32 = 10.0;

fn face(id: &str, bytes: &[u8]) -> Arc<LoadedFont> {
    Arc::new(LoadedFont::parse(id.into(), bytes.to_vec()).expect("font"))
}

fn stack() -> FontStack {
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

/// Every face the app ships draws U+00A0 as ONE real glyph (never `.notdef`)
/// exactly as wide as U+0020, so a run of no-break spaces keeps the
/// alignment gap a browser / Word shows.
#[test]
fn every_shipped_face_draws_a_no_break_space_as_wide_as_a_space() {
    let faces: [(&str, &[u8]); 11] = [
        (
            "Amiri",
            include_bytes!("../../../ts/public/fonts/Amiri-Regular.ttf"),
        ),
        (
            "Cairo",
            include_bytes!("../../../ts/public/fonts/Cairo-Regular.ttf"),
        ),
        (
            "Caladea",
            include_bytes!("../../../ts/public/fonts/Caladea-Regular.ttf"),
        ),
        (
            "Carlito",
            include_bytes!("../../../ts/public/fonts/Carlito-Regular.ttf"),
        ),
        (
            "Gelasio",
            include_bytes!("../../../ts/public/fonts/Gelasio-Regular.ttf"),
        ),
        (
            "LiberationMono",
            include_bytes!("../../../ts/public/fonts/LiberationMono-Regular.ttf"),
        ),
        (
            "LiberationSans",
            include_bytes!("../../../ts/public/fonts/LiberationSans-Regular.ttf"),
        ),
        (
            "LiberationSerif",
            include_bytes!("../../../ts/public/fonts/LiberationSerif-Regular.ttf"),
        ),
        (
            "NotoNaskhArabic",
            include_bytes!("../../../ts/public/fonts/NotoNaskhArabic-Regular.ttf"),
        ),
        (
            "ScheherazadeNew",
            include_bytes!("../../../ts/public/fonts/ScheherazadeNew-Regular.ttf"),
        ),
        (
            "Selawik",
            include_bytes!("../../../ts/public/fonts/Selawik-Regular.ttf"),
        ),
    ];
    for (name, bytes) in faces {
        let font = face(name, bytes);
        let plain = shape_text(&font, "a b", ShapingDirection::Ltr, 16.0);
        let nbsp = shape_text(&font, &format!("a{NBSP}b"), ShapingDirection::Ltr, 16.0);
        assert_eq!(nbsp.glyphs.len(), 3, "{name}: one glyph per character");
        assert_ne!(nbsp.glyphs[1].glyph_id, 0, "{name}: U+00A0 is .notdef");
        assert!(
            (nbsp.glyphs[1].x_advance - plain.glyphs[1].x_advance).abs() < 0.01,
            "{name}: U+00A0 advances {} but U+0020 {}",
            nbsp.glyphs[1].x_advance,
            plain.glyphs[1].x_advance
        );
        assert!(nbsp.glyphs[1].x_advance > 0.0, "{name}: a zero-width gap");
    }
}

/// UAX #14: no break opportunity before or after U+00A0 (`GL`), where an
/// ASCII space offers one after it.
#[test]
fn there_is_no_break_opportunity_at_a_no_break_space() {
    use text_pipeline::line_break::break_opportunities;
    let plain = "aa bb";
    assert_eq!(break_opportunities(plain), vec![0, 3, 5]);
    let glued = format!("aa{NBSP}bb");
    assert_eq!(break_opportunities(&glued), vec![0, glued.len()]);
    /* A run of them, and one beside an ASCII space (that one still breaks). */
    let run = format!("aa{NBSP}{NBSP}{NBSP}bb");
    assert_eq!(break_opportunities(&run), vec![0, run.len()]);
    let mixed = format!("aa {NBSP}bb");
    assert_eq!(break_opportunities(&mixed), vec![0, 3, mixed.len()]);
}

/// The composer never wraps at a no-break space: with room for "xx aaaa" but
/// not for "xx aaaa bbbb", an ASCII space breaks after "aaaa " while the
/// no-break space carries "aaaa" down with "bbbb". The glued word is exactly
/// as wide as the spaced one.
#[test]
fn a_line_is_never_broken_at_a_no_break_space() {
    let fonts = stack();
    let max = width(&fonts, "aaaa bbbb") + 1.0;
    assert!(width(&fonts, "xx aaaa") < max, "the fixture's premise");
    assert!(width(&fonts, "xx aaaa bbbb") > max, "the fixture's premise");

    let spaced = "xx aaaa bbbb cccc";
    let para = lay(&fonts, spaced, max, ShapingDirection::Ltr);
    assert_eq!(line_text(spaced, &para, 0), "xx aaaa ");

    let glued = format!("xx aaaa{NBSP}bbbb cccc");
    let para = lay(&fonts, &glued, max, ShapingDirection::Ltr);
    assert_eq!(line_text(&glued, &para, 0), "xx ");
    assert_eq!(
        line_text(&glued, &para, 1).trim_end_matches(' '),
        format!("aaaa{NBSP}bbbb")
    );
    assert!(
        (width(&fonts, &format!("aaaa{NBSP}bbbb")) - width(&fonts, "aaaa bbbb")).abs() < 0.01,
        "the no-break space is as wide as a space"
    );
    /* A run of them keeps every one's width on the line. */
    let run = format!("aaaa{NBSP}{NBSP}{NBSP}bbbb");
    let para = lay(&fonts, &run, 10_000.0, ShapingDirection::Ltr);
    assert_eq!(para.lines.len(), 1);
    assert!(
        (para.lines[0].width - width(&fonts, "aaaa   bbbb")).abs() < 0.01,
        "{} vs {}",
        para.lines[0].width,
        width(&fonts, "aaaa   bbbb")
    );
}

/// Between Arabic words (the other shipped script) it is still one line and
/// no glyph falls back to `.notdef`.
#[test]
fn a_no_break_space_between_arabic_words_lays_out_without_notdef() {
    let fonts = stack();
    let text = format!("\u{634}\u{631}\u{643}\u{629}{NBSP}\u{627}\u{644}\u{639}\u{631}\u{628}");
    let para = lay(&fonts, &text, 10_000.0, ShapingDirection::Rtl);
    assert_eq!(para.lines.len(), 1);
    for run in &para.lines[0].runs {
        for g in &run.glyphs {
            assert_ne!(g.id, 0, "a .notdef glyph in {text:?}");
        }
    }
}
