//! Issue #501 - the command path's twin of the reader's font-size clamp.
//!
//! `ApplyFormatting` (and `ModifyStyle`, and the harness `px_size`
//! fields) used to take any FINITE size: `1.0951575e33` pt made a line
//! taller than any page and stalled the lazy-band cull (#499). Word caps a
//! character at 1638 pt and floors it at 1 pt (`engine::MAX_FONT_SIZE_PT` /
//! `MIN_FONT_SIZE_PT`); `font_size_guard::check_command` (run by
//! `apply_command` right after the EMU guard) refuses a size outside it
//! with `OutOfRange` naming the field, and the #341 invariant holds: the
//! document, selection, story and undo depth are untouched.
//!
//! Gated on `fuzz-native` like the other bridge-level tests that drive real
//! `Command`s (`Engine::apply_sync`, the fuzz fingerprint).

use super::*;
use bridge::{BridgeSpanStylePatch, BridgeStyleProperties, ErrorKind};

fn engine_over(text: &str) -> Engine {
    let mut e = Engine::new_headless(DocumentTree::from_text(text));
    e.selection = Some(SelectionState {
        anchor: bpos_top(0, 0),
        caret: bpos_top(0, 0),
        ideal_x: None,
        kind: SelectionKind::Linear,
    });
    e
}

fn size_patch(size: f32) -> TextAttrsPatch {
    TextAttrsPatch {
        bold: None,
        italic: None,
        underline: None,
        strike: None,
        font_family: None,
        font_size: Some(size),
        color: None,
        bg_color: None,
        script: None,
        language: None,
        caps: None,
        small_caps: None,
        font_slot: None,
    }
}

fn format_range(e: &mut Engine, size: f32) -> Event {
    e.apply_sync(Command::ApplyFormatting {
        range: Some(BridgeLogicalRange {
            start: bpos_top(0, 0),
            end: bpos_top(0, 5),
        }),
        attrs: size_patch(size),
    })
}

fn modify_style(e: &mut Engine, size: f32) -> Event {
    e.apply_sync(Command::ModifyStyle {
        style_id: "Normal".into(),
        properties: BridgeStyleProperties {
            run_props: Some(BridgeSpanStylePatch {
                font_size: Some(size),
                ..Default::default()
            }),
            ..Default::default()
        },
    })
}

fn sizes_of_first_run(e: &Engine) -> (Option<f32>, Option<f32>) {
    let doc = e.undo.current();
    let style = doc.blocks[0].as_paragraph().expect("paragraph").style_at(0);
    (style.font_size, style.font_size_cs)
}

/// Assert `evt` is the #501 refusal for `field` of `command`, and that
/// nothing about the session moved.
fn assert_refused(e: &Engine, evt: &Event, before: [u64; 5], command: &str, field: &str) {
    let Event::Error { kind, message, .. } = evt else {
        panic!("expected an OutOfRange refusal of {field}, got {evt:?}");
    };
    assert_eq!(*kind, Some(ErrorKind::OutOfRange), "{message}");
    assert!(message.contains(command), "{message} should name {command}");
    assert!(message.contains(field), "{message} should name {field}");
    assert_eq!(
        e.state_fingerprint_for_fuzzing(),
        before,
        "a refused command changes nothing: {message}"
    );
}

/// The sizes the bound pivots on: below, at and past each edge, plus the
/// #501 trigger (`1.0951575e33`) and the negative / extreme finite ones.
const OUT_OF_RANGE: [f32; 9] = [
    0.0,
    0.5,
    0.999,
    1638.5,
    1639.0,
    1.0951575e33,
    1e33,
    f32::MAX,
    -12.0,
];

#[test]
fn apply_formatting_refuses_a_size_outside_one_to_1638_naming_the_field() {
    for bad in OUT_OF_RANGE {
        let mut e = engine_over("hello");
        let before = e.state_fingerprint_for_fuzzing();
        let evt = format_range(&mut e, bad);
        assert_refused(&e, &evt, before, "ApplyFormatting", "attrs.font_size");
        assert_eq!(sizes_of_first_run(&e), (None, None), "{bad}: unchanged");
    }
}

#[test]
fn apply_formatting_accepts_one_and_1638_exactly() {
    for ok in [1.0, 1.5, 11.0, 1637.5, 1638.0] {
        let mut e = engine_over("hello");
        let evt = format_range(&mut e, ok);
        assert!(
            !matches!(evt, Event::Error { .. }),
            "{ok}: refused with {evt:?}"
        );
        /* Engine-authored formatting sets both script slots. */
        assert_eq!(sizes_of_first_run(&e), (Some(ok), Some(ok)), "{ok}");
    }
}

#[test]
fn a_caret_size_change_is_bounded_too() {
    /* The collapsed-caret path arms a pending style through the same
    command: it must be refused before it can be armed. */
    let mut e = engine_over("hello");
    let before = e.state_fingerprint_for_fuzzing();
    let evt = e.apply_sync(Command::ApplyFormatting {
        range: None,
        attrs: size_patch(1e33),
    });
    assert_refused(&e, &evt, before, "ApplyFormatting", "attrs.font_size");
}

#[test]
fn modify_style_refuses_a_size_outside_the_bound_naming_the_field() {
    for bad in OUT_OF_RANGE {
        let mut e = engine_over("hello");
        let before = e.state_fingerprint_for_fuzzing();
        let evt = modify_style(&mut e, bad);
        assert_refused(
            &e,
            &evt,
            before,
            "ModifyStyle",
            "properties.run_props.font_size",
        );
    }
    for ok in [1.0, 14.0, 1638.0] {
        let mut e = engine_over("hello");
        let evt = modify_style(&mut e, ok);
        assert!(
            !matches!(
                evt,
                Event::Error {
                    kind: Some(ErrorKind::OutOfRange),
                    ..
                }
            ),
            "{ok}: refused with {evt:?}"
        );
    }
}

#[test]
fn the_harness_px_size_fields_share_the_bound() {
    for bad in [0.0, 0.5, 1638.5, 1e33] {
        for cmd in [
            Command::RenderPage {
                text: "hello".into(),
                font_id: "fuzz-latin".into(),
                base_direction: "ltr".into(),
                px_size: bad,
                line_height: 26.0,
                align: "start".into(),
                device_pixel_ratio: None,
            },
            Command::RasterizeGlyph {
                font_id: "fuzz-latin".into(),
                ch: "a".into(),
                px_size: bad,
            },
            Command::ShapeAndRasterize {
                text: "a".into(),
                font_id: "fuzz-latin".into(),
                direction: "ltr".into(),
                px_size: bad,
            },
        ] {
            let name = cmd.kind().name();
            let mut e = engine_over("hello");
            let before = e.state_fingerprint_for_fuzzing();
            let evt = e.apply_sync(cmd);
            assert_refused(&e, &evt, before, name, "px_size");
        }
    }
}

/// The bound is the same number the `.docx` reader and the HTML paste
/// door clamp to: a size pasted as HTML lands inside it.
#[test]
fn the_shared_constants_are_words() {
    assert_eq!(engine::MAX_FONT_SIZE_PT, 1638.0);
    assert_eq!(engine::MIN_FONT_SIZE_PT, 1.0);
}
