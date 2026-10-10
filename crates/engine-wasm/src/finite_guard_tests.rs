//! Issue #407 — the command-boundary finiteness guard: every numeric
//! command field that arrives `NaN` / `±inf` is refused with a typed
//! `Event::Error { kind: InvalidArgument }` naming the field, before any
//! gate or handler runs, and nothing changes.

use super::*;
use bridge::{
    BridgeParaPropertiesPatch, BridgeSpanStylePatch, BridgeStyleProperties, BridgeTabKind,
    BridgeTabStop, Point as WirePoint, Rect as WireRect,
};

fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    use std::task::{Context, Poll, Waker};
    let mut cx = Context::from_waker(Waker::noop());
    let mut fut = Box::pin(fut);
    match fut.as_mut().poll(&mut cx) {
        Poll::Ready(v) => v,
        Poll::Pending => panic!("Engine::apply suspended in a native test"),
    }
}

fn apply(e: &mut Engine, cmd: Command) -> Event {
    block_on(e.apply(cmd))
}

fn engine_with(doc: DocumentTree) -> Engine {
    let mut e = assemble_engine(None, None);
    let bytes = include_bytes!("../../../ts/fonts/LiberationSans-Regular.ttf").to_vec();
    let font = LoadedFont::parse("test-latin".to_string(), bytes).expect("parse test font");
    e.fonts.insert("test-latin".to_string(), Arc::new(font));
    e.layout_cfg = Some(RenderConfig {
        font_id: "test-latin".to_string(),
        base_direction: ShapingDirection::Ltr,
        px_size: 16.0,
        line_height: 26.0,
        alignment: Alignment::Start,
        scale: 1.0,
        base_scale: 1.0,
        zoom: 1.0,
    });
    e.undo = UndoStack::new(doc, UNDO_CAP);
    e.selection = Some(SelectionState {
        anchor: bpos_top(0, 0),
        caret: bpos_top(0, 0),
        ideal_x: None,
        kind: SelectionKind::Linear,
    });
    e.review_date = "2026-01-01T00:00:00Z".into();
    e
}

fn range(a: u32, b: u32) -> BridgeLogicalRange {
    BridgeLogicalRange {
        start: bpos_top(0, a),
        end: bpos_top(0, b),
    }
}

fn font_size_patch(size: f32) -> TextAttrsPatch {
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

/// The message of an `InvalidArgument` refusal, or `None` for any other
/// reply.
fn invalid_argument(evt: &Event) -> Option<&str> {
    match evt {
        Event::Error {
            message,
            kind: Some(bridge::ErrorKind::InvalidArgument),
            ..
        } => Some(message),
        _ => None,
    }
}

/// One command per float-carrying shape on the wire — a top-level field,
/// a field of a nested struct, an `Option<f32>`, a sequence element and
/// an `f64` — each carrying `bad`, with the field the refusal must name.
fn poisoned(bad: f32) -> Vec<(Command, &'static str)> {
    vec![
        (Command::SetZoom { scale: bad }, "SetZoom: scale is "),
        (
            Command::SetDeviceScale { scale: bad },
            "SetDeviceScale: scale is ",
        ),
        (
            Command::ExpandLayout { target_y: bad },
            "ExpandLayout: target_y is ",
        ),
        (
            Command::ApplyFormatting {
                range: Some(range(0, 5)),
                attrs: font_size_patch(bad),
            },
            "ApplyFormatting: attrs.font_size is ",
        ),
        (
            Command::SetParagraphIndent {
                range: range(0, 0),
                start_pt: 0.0,
                end_pt: bad,
                first_line_pt: 0.0,
            },
            "SetParagraphIndent: end_pt is ",
        ),
        (
            Command::SetLineSpacing {
                range: range(0, 0),
                multiplier: bad,
            },
            "SetLineSpacing: multiplier is ",
        ),
        (
            Command::SetColumns {
                at: bpos_top(0, 0),
                count: 2,
                gutter_pt: bad,
            },
            "SetColumns: gutter_pt is ",
        ),
        (
            Command::SetPageMargins {
                at: bpos_top(0, 0),
                top_pt: 72.0,
                right_pt: bad,
                bottom_pt: 72.0,
                left_pt: 72.0,
            },
            "SetPageMargins: right_pt is ",
        ),
        (
            Command::SetTabStops {
                range: range(0, 0),
                stops: vec![
                    BridgeTabStop {
                        position_pt: 36.0,
                        kind: BridgeTabKind::Left,
                        leader: None,
                    },
                    BridgeTabStop {
                        position_pt: bad,
                        kind: BridgeTabKind::Right,
                        leader: None,
                    },
                ],
            },
            "SetTabStops: stops[1].position_pt is ",
        ),
        (
            Command::ModifyStyle {
                style_id: "Normal".into(),
                properties: BridgeStyleProperties {
                    para_props: Some(BridgeParaPropertiesPatch {
                        indent_start_pt: Some(bad),
                        ..Default::default()
                    }),
                    run_props: Some(BridgeSpanStylePatch {
                        font_size: Some(12.0),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            },
            "ModifyStyle: properties.para_props.indent_start_pt is ",
        ),
        (
            Command::HitTest {
                at: WirePoint { x: bad, y: 0.0 },
            },
            "HitTest: at.x is ",
        ),
        (
            Command::RequestPaint {
                viewport: WireRect {
                    x: 0.0,
                    y: 0.0,
                    w: 100.0,
                    h: bad,
                },
                dirty: None,
            },
            "RequestPaint: viewport.h is ",
        ),
        (
            Command::Tick {
                now_ms: f64::from(bad),
            },
            "Tick: now_ms is ",
        ),
    ]
}

#[test]
fn every_float_carrying_command_refuses_nan_and_infinity_and_changes_nothing() {
    for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        for (cmd, prefix) in poisoned(bad) {
            let mut e = engine_with(DocumentTree::from_text("hello world"));
            let revision = e.mutation_seq;
            let styles_before = format!("{:?}", e.undo.current().styles);
            let (zoom, scale, base) = {
                let cfg = e.layout_cfg.as_ref().unwrap();
                (cfg.zoom, cfg.scale, cfg.base_scale)
            };
            let evt = apply(&mut e, cmd.clone());
            let message = invalid_argument(&evt)
                .unwrap_or_else(|| panic!("{cmd:?} must be refused, got {evt:?}"));
            assert!(
                message.starts_with(prefix),
                "{message:?} must start with {prefix:?}"
            );
            assert!(
                message.ends_with("(every numeric argument must be a finite number)"),
                "{message:?}"
            );
            assert_eq!(e.undo.current().to_plain_text(), "hello world");
            assert_eq!(e.mutation_seq, revision, "{cmd:?} moved the revision");
            assert!(!e.undo.can_undo(), "{cmd:?} pushed an undo step");
            assert_eq!(format!("{:?}", e.undo.current().styles), styles_before);
            let cfg = e.layout_cfg.as_ref().unwrap();
            assert_eq!(
                (cfg.zoom, cfg.scale, cfg.base_scale),
                (zoom, scale, base),
                "{cmd:?} touched the view"
            );
        }
    }
}

/// The same commands with finite numbers are NOT refused by the guard
/// (whatever their handler then answers).
#[test]
fn finite_arguments_pass_the_guard() {
    for (cmd, _) in poisoned(12.0) {
        let mut e = engine_with(DocumentTree::from_text("hello world"));
        let evt = apply(&mut e, cmd.clone());
        assert!(invalid_argument(&evt).is_none(), "{cmd:?} → {evt:?}");
    }
    let mut e = engine_with(DocumentTree::from_text("hello world"));
    let evt = apply(
        &mut e,
        Command::ApplyFormatting {
            range: Some(range(0, 5)),
            attrs: font_size_patch(24.0),
        },
    );
    assert!(matches!(evt, Event::SelectionChanged { .. }), "{evt:?}");
    assert!(e.undo.can_undo());
}

/// The guard runs before the #345 protection gate: a NaN argument on a
/// read-only document is an `InvalidArgument`, not a `Protected` refusal
/// (the argument is wrong whatever the document allows), and a finite one
/// still meets the protection gate.
#[test]
fn the_guard_runs_before_the_protection_gate() {
    let mut e = engine_with(DocumentTree::from_text(""));
    let bytes = format_docx::test_fixtures::protected_docx(
        "<w:p><w:r><w:t>locked</w:t></w:r></w:p>",
        "<w:documentProtection w:edit=\"readOnly\" w:enforcement=\"1\"/>",
    );
    let opened = apply(
        &mut e,
        Command::OpenDocument {
            bytes,
            format: DocFormat::Docx,
            name: Some("locked.docx".into()),
            defaults: None,
            limits: None,
            password: None,
        },
    );
    assert!(matches!(opened, Event::DocumentLoaded { .. }), "{opened:?}");
    let nan = apply(
        &mut e,
        Command::ApplyFormatting {
            range: Some(range(0, 3)),
            attrs: font_size_patch(f32::NAN),
        },
    );
    assert!(invalid_argument(&nan).is_some(), "{nan:?}");
    let finite = apply(
        &mut e,
        Command::ApplyFormatting {
            range: Some(range(0, 3)),
            attrs: font_size_patch(20.0),
        },
    );
    assert!(
        matches!(
            finite,
            Event::Error {
                kind: Some(bridge::ErrorKind::Protected),
                ..
            }
        ),
        "{finite:?}"
    );
}

/// A logged non-finite command does not refuse the whole recovery: the
/// tail replays around it (the bad command is refused on its own), and a
/// logged NaN zoom never reaches the restored view.
#[test]
fn recovery_replays_around_a_logged_non_finite_command() {
    let a = engine_with(DocumentTree::from_text("alpha"));
    let snapshot = a.snapshot_bytes().unwrap();
    let mut b = engine_with(DocumentTree::from_text(""));
    let evt = apply(
        &mut b,
        Command::Recover {
            snapshot,
            log_tail: vec![
                Command::SetZoom { scale: 2.0 },
                Command::SetZoom { scale: f32::NAN },
                Command::ApplyFormatting {
                    range: Some(range(0, 5)),
                    attrs: font_size_patch(f32::INFINITY),
                },
                Command::InsertText {
                    at: None,
                    text: "!".into(),
                },
            ],
            renderer_downgrade: None,
            package: None,
        },
    );
    let Event::Recovered {
        applied_commands,
        zoom,
        ..
    } = evt
    else {
        panic!("expected Recovered, got {evt:?}");
    };
    assert_eq!(applied_commands, 4);
    assert_eq!(zoom, 2.0, "the NaN zoom is not folded into the view");
    /* The insert landed at the restored caret (offset 0). */
    assert_eq!(b.undo.current().to_plain_text(), "!alpha");
    let cfg = b.layout_cfg.as_ref().unwrap();
    assert!(cfg.zoom.is_finite() && cfg.scale.is_finite());
}

/// Issue #469 - a refused command's `Event::Error` carries the command's
/// wire name as a field (early refusals included); the message prefix is no longer the only carrier.
#[test]
fn refusal_carries_the_command_wire_name() {
    let mut e = engine_with(DocumentTree::from_text("hello"));
    let evt = apply(
        &mut e,
        Command::ApplyFormatting {
            range: Some(range(0, 5)),
            attrs: font_size_patch(f32::NAN),
        },
    );
    assert!(
        matches!(&evt, Event::Error { command: Some(c), kind: Some(bridge::ErrorKind::InvalidArgument), .. } if c == "APPLY_FORMATTING"),
        "{evt:?}"
    );
    /* A story-gated / protection-gated / handler refusal goes through the
    same choke point: every Error reply names its command. */
    for (bad, _) in poisoned(f32::INFINITY) {
        let wire_name = bad.kind().wire_name();
        let evt = apply(&mut e, bad);
        assert!(
            matches!(&evt, Event::Error { command: Some(c), .. } if *c == wire_name),
            "{wire_name}: {evt:?}"
        );
    }
}
