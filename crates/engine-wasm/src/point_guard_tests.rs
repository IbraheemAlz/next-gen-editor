//! Issue #503 (item 2) - the command path's twin of the reader's measure
//! clamp, for the page / paragraph point fields.
//!
//! `SetPageMargins`, `SetColumns`, `SetParagraphIndent`, `SetLineSpacing`,
//! `SetTabStops` and `ModifyStyle`'s paragraph patch used to take any
//! FINITE number of points (`1e33` for a margin, `f32::MAX` for an indent).
//! Word holds each to 22 in (1584 pt); `point_guard::check_command` (run by
//! `apply_command` right after the font-size guard) refuses a value outside
//! with `OutOfRange` naming the command and the field, `do_set_page_margins`
//! refuses margins that swallow the section's page, and the #341 invariant
//! holds: the document, selection, story and undo depth are untouched.
//!
//! Gated on `fuzz-native` like the other bridge-level tests that drive real
//! `Command`s (`Engine::apply_sync`, the fuzz fingerprint).

use super::*;
use bridge::{
    BridgeParaPropertiesPatch, BridgeStyleProperties, BridgeTabKind, BridgeTabStop, ErrorKind,
};

fn engine_over(text: &str) -> Engine {
    let mut doc = DocumentTree::from_text(text);
    // `ModifyStyle` needs a style to modify.
    doc.styles.insert(
        "Normal".into(),
        engine::ParagraphStyle {
            id: "Normal".into(),
            name: "Normal".into(),
            ..Default::default()
        },
    );
    engine_with(doc)
}

fn engine_with(doc: DocumentTree) -> Engine {
    let mut e = Engine::new_headless(doc);
    e.selection = Some(SelectionState {
        anchor: bpos_top(0, 0),
        caret: bpos_top(0, 0),
        ideal_x: None,
        kind: SelectionKind::Linear,
    });
    e
}

fn whole() -> BridgeLogicalRange {
    BridgeLogicalRange {
        start: bpos_top(0, 0),
        end: bpos_top(0, 5),
    }
}

/// Assert `evt` is the #503 refusal for `field` of `command`, and that
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
    assert_eq!(e.check_point_fields_for_fuzzing(), Ok(()), "{message}");
}

fn assert_accepted(e: &Engine, evt: &Event, what: &str) {
    assert!(
        !matches!(evt, Event::Error { .. }),
        "{what}: refused with {evt:?}"
    );
    assert_eq!(e.check_point_fields_for_fuzzing(), Ok(()), "{what}");
}

/// Values the unsigned 22 in bound (margins, gutters) pivots on: below
/// zero, one point and a hair past the bound, the extremes.
const UNSIGNED_BAD: [f32; 8] = [
    -1.0,
    -0.01,
    1584.5,
    1585.0,
    1e33,
    1.0951575e33,
    f32::MAX,
    f32::MIN,
];

/// The signed 22 in bound (indents, tab positions).
const SIGNED_BAD: [f32; 8] = [
    -1585.0,
    -1584.5,
    1584.5,
    1585.0,
    -1e33,
    1e33,
    f32::MAX,
    f32::MIN,
];

fn first_paragraph(e: &Engine) -> engine::Paragraph {
    e.undo.current().blocks[0]
        .as_paragraph()
        .expect("paragraph")
        .clone()
}

// ---- SetPageMargins -------------------------------------------------------

fn margins(top: f32, right: f32, bottom: f32, left: f32) -> Command {
    Command::SetPageMargins {
        at: bpos_top(0, 0),
        top_pt: top,
        right_pt: right,
        bottom_pt: bottom,
        left_pt: left,
    }
}

#[test]
fn set_page_margins_refuses_each_margin_outside_zero_to_1584_naming_the_field() {
    for bad in UNSIGNED_BAD {
        for (field, cmd) in [
            ("top_pt", margins(bad, 36.0, 36.0, 36.0)),
            ("right_pt", margins(36.0, bad, 36.0, 36.0)),
            ("bottom_pt", margins(36.0, 36.0, bad, 36.0)),
            ("left_pt", margins(36.0, 36.0, 36.0, bad)),
        ] {
            let mut e = engine_over("hello");
            let before = e.state_fingerprint_for_fuzzing();
            let evt = e.apply_sync(cmd);
            assert_refused(&e, &evt, before, "SetPageMargins", field);
        }
    }
}

#[test]
fn set_page_margins_accepts_zero_and_ordinary_margins() {
    for m in [0.0, 18.0, 72.0, 144.0] {
        let mut e = engine_over("hello");
        let evt = e.apply_sync(margins(m, m, m, m));
        assert_accepted(&e, &evt, &format!("{m}"));
        let g = e.undo.current().body_section.geometry;
        assert_eq!(
            [g.margin_top, g.margin_right, g.margin_bottom, g.margin_left],
            [m; 4]
        );
    }
}

/// Every margin at the 1584 bound passes the per-field guard; whether the
/// SET succeeds then depends on the page, which only the section-aware
/// check can see - one point under the text-area limit works, one over
/// does not, on a page as big as Word allows.
#[test]
fn the_bound_per_field_and_the_text_area_per_page_are_separate_checks() {
    for ok in [0.0, 1584.0] {
        assert_eq!(
            crate::point_guard::check_command(&margins(ok, ok, ok, ok)).map_err(|_| ()),
            Ok(()),
            "{ok}"
        );
    }
    assert!(crate::point_guard::check_command(&margins(1584.5, 0.0, 0.0, 0.0)).is_err());

    let mut doc = DocumentTree::from_text("hello");
    doc.body_section.geometry.width = 1584.0;
    doc.body_section.geometry.height = 1584.0;
    // 1584 - 7.2 = 1576.8 pt of margin on one axis is the most that fits.
    let mut e = engine_with(doc.clone());
    let evt = e.apply_sync(margins(0.0, 0.0, 0.0, 1576.0));
    assert_accepted(&e, &evt, "1576 on a 22 in page");
    assert_eq!(e.undo.current().body_section.geometry.margin_left, 1576.0);

    for (cmd, field) in [
        (margins(0.0, 0.0, 0.0, 1584.0), "left_pt + right_pt"),
        (margins(0.0, 800.0, 0.0, 800.0), "left_pt + right_pt"),
        (margins(1584.0, 0.0, 1584.0, 0.0), "top_pt + bottom_pt"),
        (margins(790.0, 0.0, 790.0, 0.0), "top_pt + bottom_pt"),
    ] {
        let mut e = engine_with(doc.clone());
        let before = e.state_fingerprint_for_fuzzing();
        let evt = e.apply_sync(cmd);
        assert_refused(&e, &evt, before, "SetPageMargins", field);
    }
}

#[test]
fn margins_that_swallow_a_letter_page_are_refused_and_the_section_is_untouched() {
    let mut e = engine_over("hello");
    let page = e.undo.current().body_section.geometry;
    let before = e.state_fingerprint_for_fuzzing();
    // A4: 595.3 pt wide - 300 + 300 leaves 0 pt.
    let evt = e.apply_sync(margins(36.0, 300.0, 36.0, 300.0));
    assert_refused(&e, &evt, before, "SetPageMargins", "left_pt + right_pt");
    assert_eq!(e.undo.current().body_section.geometry, page);
}

// ---- SetColumns -----------------------------------------------------------

#[test]
fn set_columns_refuses_a_gutter_outside_zero_to_1584() {
    for bad in UNSIGNED_BAD {
        let mut e = engine_over("hello");
        let before = e.state_fingerprint_for_fuzzing();
        let evt = e.apply_sync(Command::SetColumns {
            at: bpos_top(0, 0),
            count: 2,
            gutter_pt: bad,
        });
        assert_refused(&e, &evt, before, "SetColumns", "gutter_pt");
    }
    for ok in [0.0, 18.0, 36.0, 1584.0] {
        let mut e = engine_over("hello");
        let evt = e.apply_sync(Command::SetColumns {
            at: bpos_top(0, 0),
            count: 2,
            gutter_pt: ok,
        });
        assert_accepted(&e, &evt, &format!("{ok}"));
        assert_eq!(e.undo.current().body_section.columns.gutter_pt, ok);
    }
}

// ---- SetParagraphIndent ---------------------------------------------------

fn indent(start: f32, end: f32, first_line: f32) -> Command {
    Command::SetParagraphIndent {
        range: whole(),
        start_pt: start,
        end_pt: end,
        first_line_pt: first_line,
    }
}

#[test]
fn set_paragraph_indent_refuses_each_field_outside_plus_minus_1584() {
    for bad in SIGNED_BAD {
        for (field, cmd) in [
            ("start_pt", indent(bad, 0.0, 0.0)),
            ("end_pt", indent(0.0, bad, 0.0)),
            ("first_line_pt", indent(0.0, 0.0, bad)),
        ] {
            let mut e = engine_over("hello");
            let before = e.state_fingerprint_for_fuzzing();
            let evt = e.apply_sync(cmd);
            assert_refused(&e, &evt, before, "SetParagraphIndent", field);
        }
    }
}

#[test]
fn set_paragraph_indent_accepts_the_bound_and_negative_outdents() {
    for v in [-1584.0, -36.0, 0.0, 36.0, 1584.0] {
        let mut e = engine_over("hello");
        let evt = e.apply_sync(indent(v, v, v));
        assert_accepted(&e, &evt, &format!("{v}"));
        let ind = first_paragraph(&e).props.indent;
        let twips = (v * 20.0) as i32;
        assert_eq!(ind.start_twips, twips, "{v}");
        assert_eq!(ind.end_twips, twips, "{v}");
        // The sign of the first line picks first-line vs hanging.
        assert_eq!(
            ind.first_line_twips.max(ind.hanging_twips),
            twips.abs(),
            "{v}"
        );
    }
}

// ---- SetLineSpacing -------------------------------------------------------

#[test]
fn set_line_spacing_refuses_a_multiplier_outside_the_bound() {
    for bad in [0.01, 0.059, 132.5, 133.0, 1e33, 1.0951575e33, f32::MAX] {
        let mut e = engine_over("hello");
        let before = e.state_fingerprint_for_fuzzing();
        let evt = e.apply_sync(Command::SetLineSpacing {
            range: whole(),
            multiplier: bad,
        });
        assert_refused(&e, &evt, before, "SetLineSpacing", "multiplier");
    }
}

#[test]
fn set_line_spacing_accepts_the_range_and_keeps_zero_and_below_as_clear() {
    for (m, want) in [
        (0.06, Some(14)),
        (1.0, Some(240)),
        (1.5, Some(360)),
        (132.0, Some(31_680)),
        // The documented "clear the override" sentinel.
        (0.0, None),
        (-1.0, None),
        (f32::MIN, None),
    ] {
        let mut e = engine_over("hello");
        // Give it something to clear.
        e.apply_sync(Command::SetLineSpacing {
            range: whole(),
            multiplier: 2.0,
        });
        let evt = e.apply_sync(Command::SetLineSpacing {
            range: whole(),
            multiplier: m,
        });
        assert_accepted(&e, &evt, &format!("{m}"));
        let got = match first_paragraph(&e).props.line_height {
            Some(engine::LineHeight::Auto { twips }) => Some(twips),
            None => None,
            other => panic!("{m}: {other:?}"),
        };
        assert_eq!(got, want, "{m}");
    }
}

// ---- SetTabStops ----------------------------------------------------------

fn stop(position_pt: f32) -> BridgeTabStop {
    BridgeTabStop {
        position_pt,
        kind: BridgeTabKind::Left,
        leader: None,
    }
}

#[test]
fn set_tab_stops_refuses_a_position_outside_plus_minus_1584_naming_the_index() {
    for bad in SIGNED_BAD {
        for (index, stops) in [
            (0, vec![stop(bad)]),
            (1, vec![stop(36.0), stop(bad)]),
            (2, vec![stop(36.0), stop(72.0), stop(bad), stop(108.0)]),
        ] {
            let mut e = engine_over("hello");
            let before = e.state_fingerprint_for_fuzzing();
            let evt = e.apply_sync(Command::SetTabStops {
                range: whole(),
                stops,
            });
            assert_refused(
                &e,
                &evt,
                before,
                "SetTabStops",
                &format!("stops[{index}].position_pt"),
            );
        }
    }
}

#[test]
fn set_tab_stops_accepts_the_bound() {
    for v in [-1584.0, 0.0, 36.0, 1584.0] {
        let mut e = engine_over("hello");
        let evt = e.apply_sync(Command::SetTabStops {
            range: whole(),
            stops: vec![stop(v)],
        });
        assert_accepted(&e, &evt, &format!("{v}"));
        assert_eq!(first_paragraph(&e).props.tab_stops[0].position_pt, v);
    }
}

// ---- ModifyStyle ----------------------------------------------------------

fn style_with(patch: BridgeParaPropertiesPatch) -> Command {
    Command::ModifyStyle {
        style_id: "Normal".into(),
        properties: BridgeStyleProperties {
            para_props: Some(patch),
            ..Default::default()
        },
    }
}

#[test]
fn modify_style_bounds_its_paragraph_patch_like_the_direct_commands() {
    type Build = fn(f32) -> BridgeParaPropertiesPatch;
    let fields: [(&str, Build, &[f32]); 4] = [
        (
            "properties.para_props.indent_start_pt",
            |v| BridgeParaPropertiesPatch {
                indent_start_pt: Some(v),
                ..Default::default()
            },
            &SIGNED_BAD,
        ),
        (
            "properties.para_props.indent_end_pt",
            |v| BridgeParaPropertiesPatch {
                indent_end_pt: Some(v),
                ..Default::default()
            },
            &SIGNED_BAD,
        ),
        (
            "properties.para_props.first_line_pt",
            |v| BridgeParaPropertiesPatch {
                first_line_pt: Some(v),
                ..Default::default()
            },
            &SIGNED_BAD,
        ),
        (
            "properties.para_props.line_spacing_multiplier",
            |v| BridgeParaPropertiesPatch {
                line_spacing_multiplier: Some(v),
                ..Default::default()
            },
            &[0.01, 132.5, 1e33, f32::MAX],
        ),
    ];
    for (field, build, bad) in fields {
        for &v in bad {
            let mut e = engine_over("hello");
            let before = e.state_fingerprint_for_fuzzing();
            let evt = e.apply_sync(style_with(build(v)));
            assert_refused(&e, &evt, before, "ModifyStyle", field);
        }
    }
    for (field, build, _) in fields {
        for v in [0.0, 1.0, 36.0] {
            let mut e = engine_over("hello");
            let evt = e.apply_sync(style_with(build(v)));
            assert!(
                !matches!(
                    evt,
                    Event::Error {
                        kind: Some(ErrorKind::OutOfRange),
                        ..
                    }
                ),
                "{field} = {v}: refused with {evt:?}"
            );
            assert_eq!(e.check_point_fields_for_fuzzing(), Ok(()), "{field} = {v}");
        }
    }
    // The bound itself, and the "no change" sentinel for the multiplier.
    for v in [-1584.0, 1584.0] {
        let mut e = engine_over("hello");
        let evt = e.apply_sync(style_with(BridgeParaPropertiesPatch {
            indent_start_pt: Some(v),
            ..Default::default()
        }));
        assert_accepted(&e, &evt, &format!("{v}"));
        let stored = e.undo.current().styles["Normal"].para.indent.start_twips;
        assert_eq!(
            stored,
            (v * 20.0) as i32,
            "the style holds the bound itself"
        );
    }
    let mut e = engine_over("hello");
    let evt = e.apply_sync(style_with(BridgeParaPropertiesPatch {
        line_spacing_multiplier: Some(-3.0),
        ..Default::default()
    }));
    assert_accepted(
        &e,
        &evt,
        "a non-positive multiplier is the no-change sentinel",
    );
}

// ---- other doors ------------------------------------------------------------

/// HTML paste reads none of these properties, so hostile CSS cannot reach
/// the model through it - pinned, so a future CSS importer that does read
/// `margin-left` / `text-indent` / `line-height` has to bound it.
#[test]
fn html_paste_cannot_carry_a_point_field_into_the_model() {
    let mut e = engine_over("hello");
    let evt = e.apply_sync(Command::PasteHtml {
        html: concat!(
            r#"<p style="margin-left:1e30pt;margin-right:-1e30px;text-indent:-9e99pt;"#,
            r#"line-height:1e33;margin-top:1e33pt;padding-left:1e33px">pasted</p>"#,
            r#"<table><tr><td style="width:1e33px;padding:1e33pt">cell</td></tr></table>"#
        )
        .into(),
    });
    assert_accepted(&e, &evt, "hostile CSS");
}

/// The shared constants are the numbers the reader's `measure` clamps to.
#[test]
fn the_shared_constants_are_words() {
    assert_eq!(engine::MAX_POINT_FIELD_PT, 1584.0);
    assert_eq!(engine::MAX_POINT_FIELD_TWIPS, 31_680);
    assert_eq!(engine::MIN_PAGE_SIDE_PT, 7.2);
    assert_eq!(engine::MIN_TEXT_AREA_PT, 7.2);
    assert_eq!(engine::MAX_LINE_SPACING_MULTIPLE, 132.0);
}

/// Whatever else a session does, the fresh document is inside the bound.
#[test]
fn a_fresh_document_is_inside_every_bound() {
    let e = engine_over("hello");
    assert_eq!(e.check_point_fields_for_fuzzing(), Ok(()));
}
