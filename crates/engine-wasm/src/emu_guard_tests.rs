//! Issue #459 - the command path's twin of the reader's 22 in clamp.
//!
//! `Command::ResizeImage` / `MoveImage` carry integer EMU fields; the #407
//! finiteness guard cannot see them, so `i64::MAX` used to reach layout.
//! `emu_guard::check_command` (run by `apply_command` before any gate or
//! handler) refuses a field outside Word's 22 in with `OutOfRange` naming
//! the field - extents `1..=MAX_DRAWING_EMU`, offsets `+-MAX_DRAWING_EMU` -
//! and the #341 invariant holds: the document, selection, story and undo
//! depth are untouched.
//!
//! Gated on `fuzz-native` like the other bridge-level tests that drive real
//! `Command`s (`Engine::apply_sync`, the fuzz fingerprint).

use super::*;
use bridge::ErrorKind;
use engine::MAX_DRAWING_EMU as BOUND;

fn image(at: u32, anchor: Option<engine::FloatAnchor>) -> engine::InlineObject {
    engine::InlineObject {
        at,
        kind: engine::InlineKind::Image {
            rel_id: "nge_img_1".to_string(),
            width_emu: 914_400,
            height_emu: 457_200,
            media_key: None,
        },
        anchor: anchor.map(Box::new),
        source_xml: None,
    }
}

/// A headless engine over `"a" + U+FFFC + "b"` holding one picture at
/// byte 1: floating (`anchor`) or inline.
fn engine_with_image(anchor: Option<engine::FloatAnchor>) -> Engine {
    let mut doc = DocumentTree::from_text("a\u{FFFC}b");
    if let Some(p) = doc.blocks[0].as_paragraph_mut() {
        p.inline_objects.push(image(1, anchor));
    }
    let mut e = Engine::new_headless(doc);
    e.selection = Some(SelectionState {
        anchor: bpos_top(0, 0),
        caret: bpos_top(0, 0),
        ideal_x: None,
        kind: SelectionKind::Linear,
    });
    e
}

fn resize(e: &mut Engine, w: i64, h: i64) -> Event {
    e.apply_sync(Command::ResizeImage {
        path: bridge::BlockPath::top(0),
        at: 1,
        width_emu: w,
        height_emu: h,
        story: Vec::new(),
    })
}

fn move_to(e: &mut Engine, h: i64, v: i64) -> Event {
    e.apply_sync(Command::MoveImage {
        path: bridge::BlockPath::top(0),
        at: 1,
        offset_h_emu: h,
        offset_v_emu: v,
        story: Vec::new(),
    })
}

fn extent(e: &Engine) -> (i64, i64) {
    let doc = e.undo.current();
    let io = &doc.blocks[0].as_paragraph().unwrap().inline_objects[0];
    match &io.kind {
        engine::InlineKind::Image {
            width_emu,
            height_emu,
            ..
        } => (*width_emu, *height_emu),
        other => panic!("not an image: {other:?}"),
    }
}

fn offsets(e: &Engine) -> (engine::FloatOffset, engine::FloatOffset) {
    let doc = e.undo.current();
    let io = &doc.blocks[0].as_paragraph().unwrap().inline_objects[0];
    let a = io.anchor.as_deref().expect("a floating image");
    (a.position_h.offset, a.position_v.offset)
}

/// Assert `evt` is the #459 refusal for `field` of `command`, and that
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

const OUT_OF_RANGE_EXTENTS: [i64; 7] = [
    i64::MAX,
    i64::MIN,
    0,
    -1,
    BOUND + 1,
    i64::MAX - 1,
    i64::MIN + 1,
];

#[test]
fn resize_refuses_an_extent_outside_one_to_the_bound_naming_the_field() {
    for bad in OUT_OF_RANGE_EXTENTS {
        for (field, w, h) in [
            ("width_emu", bad, 457_200),
            ("height_emu", 914_400, bad),
            /* the first offender is the one named */
            ("width_emu", bad, bad),
        ] {
            let mut e = engine_with_image(None);
            let before = e.state_fingerprint_for_fuzzing();
            let evt = resize(&mut e, w, h);
            assert_refused(&e, &evt, before, "ResizeImage", field);
            assert_eq!(extent(&e), (914_400, 457_200), "{bad}: document unchanged");
            assert!(e.check_image_bounds_for_fuzzing().is_ok());
        }
    }
}

#[test]
fn resize_accepts_one_and_the_bound_exactly() {
    for (w, h) in [
        (1, 1),
        (BOUND, BOUND),
        (BOUND - 1, 2),
        (1, BOUND),
        (914_400, 457_200),
    ] {
        let mut e = engine_with_image(None);
        let evt = resize(&mut e, w, h);
        assert!(
            matches!(evt, Event::SelectionChanged { .. }),
            "{w} x {h}: {evt:?}"
        );
        assert_eq!(extent(&e), (w, h));
        /* Whatever the model holds, no laid-out rect passes the bound. */
        e.check_image_bounds_for_fuzzing()
            .expect("inside the bound");
    }
}

#[test]
fn a_resize_at_the_bound_lays_out_inside_it() {
    let mut e = engine_with_image(None);
    resize(&mut e, BOUND, BOUND);
    let rects = e.image_geometry().expect("geometry");
    assert_eq!(rects.len(), 1);
    let limit = engine::emu_to_pt(BOUND) * e.scale() * 1.001 + 1.0;
    assert!(
        rects[0].rect.w <= limit && rects[0].rect.h <= limit,
        "{:?} vs {limit}",
        rects[0].rect
    );
    assert_eq!((rects[0].width_emu, rects[0].height_emu), (BOUND, BOUND));
}

#[test]
fn move_refuses_an_offset_outside_the_signed_bound_naming_the_field() {
    for bad in [i64::MAX, i64::MIN, BOUND + 1, -BOUND - 1, i64::MAX - 1] {
        for (field, h, v) in [
            ("offset_h_emu", bad, 0),
            ("offset_v_emu", 0, bad),
            ("offset_h_emu", bad, bad),
        ] {
            let mut e = engine_with_image(Some(engine::FloatAnchor::default()));
            let before = e.state_fingerprint_for_fuzzing();
            let evt = move_to(&mut e, h, v);
            assert_refused(&e, &evt, before, "MoveImage", field);
            assert_eq!(
                offsets(&e),
                (engine::FloatOffset::Emu(0), engine::FloatOffset::Emu(0)),
                "{bad}: document unchanged"
            );
        }
    }
}

#[test]
fn move_accepts_zero_and_the_bound_on_both_sides() {
    for (h, v) in [
        (0, 0),
        (BOUND, BOUND),
        (-BOUND, -BOUND),
        (BOUND - 1, -BOUND + 1),
        (-1, 1),
    ] {
        let mut e = engine_with_image(Some(engine::FloatAnchor::default()));
        let evt = move_to(&mut e, h, v);
        assert!(
            matches!(evt, Event::SelectionChanged { .. }),
            "{h}, {v}: {evt:?}"
        );
        assert_eq!(
            offsets(&e),
            (engine::FloatOffset::Emu(h), engine::FloatOffset::Emu(v))
        );
        e.check_image_bounds_for_fuzzing()
            .expect("inside the bound");
    }
}

/// The guard runs before the story gate and the address lookup, so a
/// text-box chain (or an address holding no picture) cannot turn an
/// out-of-range number into a different, accepted-looking reply.
#[test]
fn the_guard_runs_before_story_and_address_resolution() {
    let mut e = engine_with_image(None);
    let before = e.state_fingerprint_for_fuzzing();
    let evt = e.apply_sync(Command::ResizeImage {
        path: bridge::BlockPath::top(7),
        at: 99,
        width_emu: i64::MAX,
        height_emu: 1,
        story: vec![bridge::TextBoxHop {
            path: bridge::BlockPath::top(0),
            at: 0,
        }],
    });
    assert_refused(&e, &evt, before, "ResizeImage", "width_emu");
    let evt = e.apply_sync(Command::MoveImage {
        path: bridge::BlockPath::top(7),
        at: 99,
        offset_h_emu: 0,
        offset_v_emu: i64::MIN,
        story: Vec::new(),
    });
    assert_refused(&e, &evt, before, "MoveImage", "offset_v_emu");
}

/// The model's own setters clamp too: a direct caller of the document API
/// (or a replayed old log entry that slipped past) never stores a value
/// past the bound.
#[test]
fn the_model_setters_clamp_for_direct_callers() {
    let mut e = engine_with_image(Some(engine::FloatAnchor::default()));
    let doc = e.undo.current().clone();
    let path = engine::BlockPath::top(0);
    let resized = doc.resize_inline_image_at(&path, 1, i64::MAX, i64::MIN);
    let moved = resized.move_floating_image_at(&path, 1, i64::MAX, i64::MIN);
    assert!(engine::first_image_beyond_bound(&moved).is_none());
    e.undo = UndoStack::new(moved, UNDO_CAP);
    assert_eq!(extent(&e), (BOUND, 1));
    assert_eq!(
        offsets(&e),
        (
            engine::FloatOffset::Emu(BOUND),
            engine::FloatOffset::Emu(-BOUND)
        )
    );
}

#[test]
fn the_fuzz_invariant_sees_an_out_of_bound_picture() {
    let mut e = engine_with_image(None);
    let mut doc = e.undo.current().clone();
    if let Some(p) = doc.blocks[0].as_paragraph_mut()
        && let engine::InlineKind::Image { width_emu, .. } = &mut p.inline_objects[0].kind
    {
        *width_emu = i64::MAX;
    }
    e.undo = UndoStack::new(doc, UNDO_CAP);
    let why = e.check_image_bounds_for_fuzzing().expect_err("must fire");
    assert!(why.contains("width_emu"), "{why}");
}

fn insert_image(e: &mut Engine, w: u32, h: u32, fit: bridge::ImageFit) -> Event {
    e.apply_sync(Command::InsertImage {
        at: bridge::LogicalPos {
            path: bridge::BlockPath::top(0),
            offset: 0,
        },
        image: bridge::ImageBlob {
            bytes: vec![0x89, b'P', b'N', b'G'],
            mime: "image/png".to_string(),
            width: w,
            height: h,
        },
        fit,
    })
}

/// `InsertImage` is the other door a huge extent could come through (a
/// decoder-reported `u32` px size under `Original`): it scales the natural
/// size down to the bound, keeping the aspect ratio.
#[test]
fn insert_image_scales_a_natural_size_past_the_bound_down_to_it() {
    let mut e = engine_with_image(None);
    let evt = insert_image(&mut e, u32::MAX, u32::MAX / 2, bridge::ImageFit::Original);
    assert!(matches!(evt, Event::SelectionChanged { .. }), "{evt:?}");
    let doc = e.undo.current();
    let inserted = doc.blocks[0]
        .as_paragraph()
        .unwrap()
        .inline_objects
        .iter()
        .find(|io| io.at == 0)
        .expect("the inserted picture");
    let engine::InlineKind::Image {
        width_emu,
        height_emu,
        ..
    } = &inserted.kind
    else {
        panic!("not an image")
    };
    assert_eq!(*width_emu, BOUND, "the longer side lands on the bound");
    assert!(
        (*height_emu - BOUND / 2).abs() <= 1,
        "aspect kept: {width_emu} x {height_emu}"
    );
    e.check_image_bounds_for_fuzzing()
        .expect("inside the bound");

    /* A tall sliver under FitWidth: the 6 in width would need a height
    past the bound; the pair is scaled down together. */
    let mut e = engine_with_image(None);
    insert_image(&mut e, 1, u32::MAX, bridge::ImageFit::FitWidth);
    e.check_image_bounds_for_fuzzing()
        .expect("inside the bound");

    /* An ordinary photo is untouched. */
    let mut e = engine_with_image(None);
    insert_image(&mut e, 200, 100, bridge::ImageFit::Original);
    let doc = e.undo.current();
    let inserted = doc.blocks[0]
        .as_paragraph()
        .unwrap()
        .inline_objects
        .iter()
        .find(|io| io.at == 0)
        .unwrap();
    assert!(matches!(
        &inserted.kind,
        engine::InlineKind::Image {
            width_emu: 1_905_000,
            height_emu: 952_500,
            ..
        }
    ));
}
