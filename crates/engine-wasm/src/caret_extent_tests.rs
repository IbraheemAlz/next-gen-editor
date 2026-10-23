//! Issue #557 (clean-room memo §11) — the caret is as tall as the TEXT at
//! it: the caret run's face ascent + descent at its size (an empty
//! paragraph: the paragraph mark's), hung off the line's baseline. Never the
//! line box: the face's line gap and any line-spacing extra sit above the
//! text and stay out of the caret, so an empty cell and the same cell
//! holding Latin text of the mark's formatting show one caret height.
//!
//! These run on `Engine::new_headless` — the shell's boot config with
//! Liberation Sans (Word's metrics: ascent 1854, descent 434, line gap 67
//! per 2048 em) — so the numbers are what the editor shows.

use super::*;

fn top(idx: u32) -> BridgeBlockPath {
    BridgeBlockPath {
        steps: vec![BridgePathStep::Block { idx }],
    }
}

fn cell(table: u32, row: u32, col: u32) -> BridgeBlockPath {
    BridgeBlockPath {
        steps: vec![
            BridgePathStep::Block { idx: table },
            BridgePathStep::Cell { row, col },
            BridgePathStep::Block { idx: 0 },
        ],
    }
}

fn pos(path: BridgeBlockPath, offset: u32) -> BridgeLogicalPos {
    BridgeLogicalPos { path, offset }
}

fn range(path: &BridgeBlockPath, start: u32, end: u32) -> BridgeLogicalRange {
    BridgeLogicalRange {
        start: pos(path.clone(), start),
        end: pos(path.clone(), end),
    }
}

/// The caret rectangle the reply to `cmd` carries.
fn caret_after(e: &mut Engine, cmd: Command) -> BridgeRect {
    match e.apply_sync(cmd) {
        Event::SelectionChanged { caret, .. } => caret,
        other => panic!("expected SelectionChanged, got {other:?}"),
    }
}

/// Put a collapsed caret at `at` (setup) and return its rectangle.
fn caret_at(e: &mut Engine, at: BridgeLogicalPos) -> BridgeRect {
    caret_after(
        e,
        Command::SetSelection {
            range: BridgeLogicalRange {
                start: at.clone(),
                end: at.clone(),
            },
            caret: at,
        },
    )
}

/// The laid-out line holding `at` (its box, for comparison).
fn line_at(e: &Engine, at: &BridgeLogicalPos) -> (f32, f32, f32) {
    let geom = e.document_geometry().expect("geometry");
    let line = geom
        .iter()
        .find(|l| l.path == at.path && at.offset >= l.start_byte && at.offset <= l.end_byte)
        .unwrap_or_else(|| panic!("no line holds {at:?}"));
    (line.y_top, line.height, line.baseline)
}

/// Liberation Sans' `(ascent, descent)` in device px for a run of `size`
/// layout units (a `font_size`) on this engine.
fn liberation(e: &Engine, size: f32) -> (f32, f32) {
    let px = size * e.scale();
    (1854.0 / 2048.0 * px, 434.0 / 2048.0 * px)
}

/// The boot size (layout px) every paragraph of these documents uses.
fn boot_px(e: &Engine) -> f32 {
    e.layout_cfg.as_ref().expect("layout config").px_size
}

fn size_patch(pt: f32) -> TextAttrsPatch {
    TextAttrsPatch {
        bold: None,
        italic: None,
        underline: None,
        strike: None,
        font_family: None,
        font_size: Some(pt),
        color: None,
        bg_color: None,
        script: None,
        language: None,
        caps: None,
        small_caps: None,
        font_slot: None,
    }
}

fn near(a: f32, b: f32, what: &str) {
    assert!((a - b).abs() < 0.05, "{what}: {a} vs {b}");
}

#[test]
fn an_empty_cell_and_the_same_cell_with_latin_text_share_one_caret_height() {
    let mut e = Engine::new_headless(DocumentTree::from_text("Hello world"));
    let evt = e.apply_sync(Command::InsertTable {
        at: top(1),
        rows: 2,
        cols: 2,
    });
    assert!(!matches!(evt, Event::Error { .. }), "{evt:?}");
    let at = pos(cell(1, 1, 0), 0);
    let empty = caret_at(&mut e, at.clone());
    let (y_top, line_h, baseline) = line_at(&e, &at);
    let (ascent, descent) = liberation(&e, boot_px(&e));

    /* The mark's face, without its line gap: strictly shorter than the
    line box, hanging off the baseline. */
    near(
        empty.h,
        ascent + descent,
        "empty cell caret = ascent + descent",
    );
    assert!(
        empty.h < line_h - 0.5,
        "the caret ({}) must be shorter than the line box ({line_h}): the gap is not text",
        empty.h
    );
    near(empty.y, baseline - ascent, "caret top = baseline - ascent");
    near(
        empty.y + empty.h,
        y_top + line_h,
        "caret bottom = line bottom",
    );

    /* Latin text typed there takes the mark's formatting: same face, same
    size, same caret. */
    let typed = caret_after(
        &mut e,
        Command::InsertText {
            at: None,
            text: "ab".into(),
        },
    );
    near(typed.h, empty.h, "typed caret height");
    near(typed.y, empty.y, "typed caret top");
    assert!(typed.x > empty.x, "the caret advanced past `ab`");
}

#[test]
fn an_empty_paragraph_caret_matches_the_caret_in_its_text() {
    let mut e = Engine::new_headless(DocumentTree::from_text("Hello world"));
    let in_text = caret_at(&mut e, pos(top(0), 3));
    caret_at(&mut e, pos(top(0), 11));
    let empty = caret_after(&mut e, Command::SplitParagraph { at: None });
    assert_eq!(e.selection.as_ref().unwrap().caret, pos(top(1), 0));
    near(empty.h, in_text.h, "empty paragraph vs text caret");
    let typed = caret_after(
        &mut e,
        Command::InsertText {
            at: None,
            text: "ab".into(),
        },
    );
    near(typed.h, empty.h, "after typing");
    near(typed.y, empty.y, "after typing (top)");
}

/// Double line spacing doubles the line box, the extra above the text
/// (Word); the caret stays the text's height, on the baseline.
#[test]
fn line_spacing_extra_stays_out_of_the_caret() {
    let mut e = Engine::new_headless(DocumentTree::from_text("Hello world"));
    let single = caret_at(&mut e, pos(top(0), 3));
    let (_, single_line_h, _) = line_at(&e, &pos(top(0), 3));
    let evt = e.apply_sync(Command::SetLineSpacing {
        range: range(&top(0), 0, 11),
        multiplier: 2.0,
    });
    assert!(!matches!(evt, Event::Error { .. }), "{evt:?}");
    let double = caret_at(&mut e, pos(top(0), 3));
    let (y_top, line_h, baseline) = line_at(&e, &pos(top(0), 3));
    assert!(
        line_h > 1.9 * single_line_h,
        "the line doubled: {line_h} vs {single_line_h}"
    );
    near(double.h, single.h, "caret height under double spacing");
    near(
        double.y + double.h,
        y_top + line_h,
        "caret bottom on the line bottom",
    );
    near(
        double.y,
        baseline - liberation(&e, boot_px(&e)).0,
        "caret top under double spacing",
    );
    /* An empty paragraph under the same spacing: the mark's height. */
    caret_at(&mut e, pos(top(0), 11));
    let empty = caret_after(&mut e, Command::SplitParagraph { at: None });
    near(empty.h, single.h, "empty double-spaced paragraph caret");
}

/// In a line mixing sizes the caret takes ITS run's font, not the line's
/// tallest; at the seam it inherits from the left (memo §1).
#[test]
fn the_caret_takes_its_own_runs_size_in_a_mixed_line() {
    let mut e = Engine::new_headless(DocumentTree::from_text("small BIG"));
    let evt = e.apply_sync(Command::ApplyFormatting {
        range: Some(range(&top(0), 6, 9)),
        attrs: size_patch(48.0),
    });
    assert!(!matches!(evt, Event::Error { .. }), "{evt:?}");
    let (_, line_h, baseline) = line_at(&e, &pos(top(0), 2));
    let small = caret_at(&mut e, pos(top(0), 2));
    let big = caret_at(&mut e, pos(top(0), 9));
    let seam = caret_at(&mut e, pos(top(0), 6));
    let px = boot_px(&e);
    let (sa, sd) = liberation(&e, px);
    near(small.h, sa + sd, "caret in the small run");
    near(small.y, baseline - sa, "small caret top");
    assert!(small.h < line_h * 0.75, "not the line box ({line_h})");
    /* `font_size` is in layout units, like the boot `px_size`. */
    let (ba, bd) = liberation(&e, 48.0);
    near(big.h, ba + bd, "caret in the big run");
    near(big.y, baseline - ba, "big caret top");
    near(seam.h, small.h, "the seam inherits the run on its left");
}
