//! Issue #357 — run-content elements in the line composer: `<w:cr/>`
//! (U+000D) breaks the line, `<w:ptab>` aligns against the margins, a
//! `<w:sym>` draws its Unicode equivalent as one caret stop, and a
//! `<w:bdo>` / `<w:dir>` control applies on every line it spans.

use super::*;
use crate::boxes::StyleSpan;
use engine::run_content::{
    CARRIAGE_RETURN, POP_DIRECTIONAL, PTabAlignment, PTabLeader, PTabRelativeTo, RLE, RLO,
};
use std::collections::HashMap;
use std::sync::Arc;

const PX: f32 = 10.0;

fn stack() -> FontStack {
    let face = |id: &str, bytes: &[u8]| {
        Arc::new(text_pipeline::LoadedFont::parse(id.into(), bytes.to_vec()).expect("font"))
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

fn lay(
    fonts: &FontStack,
    text: &str,
    objects: &[InlineObjectInfo],
    max_width: f32,
    dir: ShapingDirection,
) -> ParagraphBox {
    layout_paragraph(ParagraphConfig {
        text,
        fonts,
        inline_objects: objects,
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

/// `<w:cr/>` is a line break exactly like `<w:br/>`: the text after it
/// starts a new line and the U+000D itself never shapes.
#[test]
fn a_carriage_return_breaks_the_line() {
    let fonts = stack();
    let text = format!("one{CARRIAGE_RETURN}two");
    assert_eq!(soft_break_segments(&text), vec![(0, 3), (4, 7)]);
    let para = lay(&fonts, &text, &[], 1000.0, ShapingDirection::Ltr);
    assert_eq!(para.lines.len(), 2);
    assert_eq!(para.lines[1].source_start, 4);
}

fn ptab(at: usize, alignment: PTabAlignment, leader: PTabLeader) -> InlineObjectInfo {
    InlineObjectInfo {
        at: at as u32,
        width_px: 0.0,
        height_px: 0.0,
        kind: InlineObjectInfoKind::PositionalTab {
            alignment,
            relative_to: PTabRelativeTo::Margin,
            leader,
        },
    }
}

/// Absolute x of the glyph whose source byte is `at` (LTR lines).
fn glyph_x(line: &LineBox, at: usize) -> f32 {
    let mut pen = line.origin.x;
    for r in &line.runs {
        for g in &r.glyphs {
            if (r.source_range.start + g.cluster) as usize == at && !g.synthetic {
                return pen;
            }
            pen += g.x_advance;
        }
    }
    panic!("no glyph at {at}");
}

/// A header line "Left · Middle · Right": the centre tab centres the
/// middle text between the margins, the right tab puts the last text flush
/// against the right margin (with its dot leader on the tab glyph), and
/// the tab glyphs draw nothing themselves.
#[test]
fn positional_tabs_align_against_the_margins() {
    let fonts = stack();
    let text = "Left\u{FFFC}Middle\u{FFFC}Right";
    let mid = text.find('\u{FFFC}').unwrap();
    let right = text.rfind('\u{FFFC}').unwrap();
    let objects = [
        ptab(mid, PTabAlignment::Center, PTabLeader::None),
        ptab(right, PTabAlignment::Right, PTabLeader::Dot),
    ];
    let max = 400.0;
    let para = lay(&fonts, text, &objects, max, ShapingDirection::Ltr);
    assert_eq!(para.lines.len(), 1);
    let line = &para.lines[0];
    assert!(
        (line.width - max).abs() < 0.5,
        "full measure: {}",
        line.width
    );
    let w = |s: &str| {
        shape_text(fonts.face("latin").unwrap(), s, ShapingDirection::Ltr, PX).total_advance
    };
    let middle_x = glyph_x(line, mid + 3);
    assert!(
        (middle_x + w("Middle") / 2.0 - max / 2.0).abs() < 0.5,
        "Middle centred: starts at {middle_x}"
    );
    let right_x = glyph_x(line, right + 3);
    assert!(
        (right_x + w("Right") - max).abs() < 0.5,
        "Right flush: starts at {right_x}"
    );
    let tabs: Vec<&PositionedGlyph> = line
        .runs
        .iter()
        .filter(|r| r.source_range.start as usize == mid || r.source_range.start as usize == right)
        .flat_map(|r| &r.glyphs)
        .collect();
    assert_eq!(tabs.len(), 2);
    assert!(tabs.iter().all(|g| g.id == 0 && g.x_advance > 0.0));
    assert_eq!(tabs[1].leader, Some(TabLeaderKind::Dot));
}

/// A symbol draws its Unicode equivalent — in the run's face when it has
/// the glyph, else in a face that does — as ONE caret stop.
#[test]
fn a_symbol_draws_its_unicode_equivalent() {
    let fonts = stack();
    let text = "a\u{FFFC}b";
    let objects = [InlineObjectInfo {
        at: 1,
        width_px: 0.0,
        height_px: 0.0,
        kind: InlineObjectInfoKind::Symbol {
            text: "\u{03B1}".into(),
        },
    }];
    let para = lay(&fonts, text, &objects, 1000.0, ShapingDirection::Ltr);
    let run = para.lines[0]
        .runs
        .iter()
        .find(|r| r.source_range == (1..4))
        .expect("the symbol's own run");
    let latin = fonts.face("latin").unwrap();
    assert_eq!(run.glyphs[0].id, latin.glyph_id('\u{03B1}').unwrap());
    assert!(run.glyphs[0].x_advance > 0.0);
    assert!(run.glyphs.iter().skip(1).all(|g| g.synthetic));
}

/// An override that wraps onto a second line still applies there: the
/// line resolves inside the RLO the previous line opened.
#[test]
fn an_override_applies_on_every_line_it_spans() {
    let fonts = stack();
    let text = format!("start {RLO}abcd efgh ijkl mnop{POP_DIRECTIONAL} end");
    let w = |s: &str| {
        shape_text(fonts.face("latin").unwrap(), s, ShapingDirection::Ltr, PX).total_advance
    };
    let para = lay(
        &fonts,
        &text,
        &[],
        w("start abcd efgh "),
        ShapingDirection::Ltr,
    );
    assert!(para.lines.len() >= 2);
    for (i, l) in para.lines.iter().enumerate() {
        for r in &l.runs {
            let piece = &text[r.source_range.start as usize..r.source_range.end as usize];
            let inside = r.source_range.start as usize > text.find(RLO).unwrap()
                && (r.source_range.end as usize) <= text.find(POP_DIRECTIONAL).unwrap();
            if inside && piece.chars().any(|c| c.is_ascii_alphabetic()) {
                assert_eq!(r.direction, ShapingDirection::Rtl, "line {i}: {piece:?}");
            }
        }
    }
    /* An embedding: Latin inside an RLE stays LTR text but sits at an
    odd-embedded level — its digits / letters keep their order. */
    let emb = format!("x {RLE}AB 12{POP_DIRECTIONAL} y");
    let para = lay(&fonts, &emb, &[], 1000.0, ShapingDirection::Ltr);
    assert_eq!(para.lines.len(), 1);
}
