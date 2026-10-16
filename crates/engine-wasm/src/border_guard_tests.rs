//! Issue #503 (residue) - the command path's twin of the reader's border
//! width clamp and tab-stop list cap.
//!
//! `SetCellBorders` / `SetParagraphBorders` carried a `u16` width (8191 pt;
//! Word draws 1/4 pt ..= 6 pt = `size_eighth_pt` 2..=48) and `SetTabStops`
//! any number of stops (Word keeps 64 per paragraph).
//! `border_guard::check_command` / `point_guard::check_command` refuse a
//! value outside with `OutOfRange` naming the command and the field, and
//! the #341 invariant holds: the document, selection, story and undo depth
//! are untouched.
//!
//! Gated on `fuzz-native` like the other bridge-level tests that drive real
//! `Command`s (`Engine::apply_sync`, the fuzz fingerprint).

use super::*;
use bridge::{
    BridgeBorderStroke, BridgeBorderStyle, BridgeCellBorders, BridgeTabKind, BridgeTabStop,
    ErrorKind,
};

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

/// An engine over a one-paragraph document followed by a 2 x 2 table
/// (block 1) - what `SetCellBorders` addresses.
fn engine_with_table() -> Engine {
    let mut e = engine_over("hello");
    let evt = e.apply_sync(Command::InsertTable {
        at: BridgeBlockPath::top(1),
        rows: 2,
        cols: 2,
    });
    assert!(!matches!(evt, Event::Error { .. }), "{evt:?}");
    e
}

fn whole() -> BridgeLogicalRange {
    BridgeLogicalRange {
        start: bpos_top(0, 0),
        end: bpos_top(0, 5),
    }
}

fn pen(style: BridgeBorderStyle, size: u16) -> BridgeBorderStroke {
    BridgeBorderStroke {
        style,
        size_eighth_pt: size,
        color: None,
    }
}

fn cell_borders(borders: BridgeCellBorders) -> Command {
    Command::SetCellBorders {
        table_path: BridgeBlockPath::top(1),
        row: 0,
        col: 0,
        borders,
    }
}

fn paragraph_borders(borders: BridgeCellBorders) -> Command {
    Command::SetParagraphBorders {
        range: whole(),
        borders,
    }
}

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

/// A drawn line outside 2..=48 eighths of a point.
const DRAWN_BAD: [u16; 8] = [0, 1, 49, 50, 96, 4000, 8191, u16::MAX];
/// ...and inside it, edges included.
const DRAWN_OK: [u16; 5] = [2, 3, 4, 47, 48];

fn edge_cases(size: u16, style: BridgeBorderStyle) -> [(&'static str, BridgeCellBorders); 4] {
    let p = || pen(style.clone(), size);
    [
        (
            "borders.top.size_eighth_pt",
            BridgeCellBorders {
                top: Some(p()),
                ..Default::default()
            },
        ),
        (
            "borders.left.size_eighth_pt",
            BridgeCellBorders {
                left: Some(p()),
                ..Default::default()
            },
        ),
        (
            "borders.bottom.size_eighth_pt",
            BridgeCellBorders {
                bottom: Some(p()),
                ..Default::default()
            },
        ),
        (
            "borders.right.size_eighth_pt",
            BridgeCellBorders {
                right: Some(p()),
                ..Default::default()
            },
        ),
    ]
}

#[test]
fn set_cell_borders_refuses_each_edge_outside_2_to_48_naming_the_field() {
    for style in [
        BridgeBorderStyle::Single,
        BridgeBorderStyle::Double,
        BridgeBorderStyle::Dotted,
        BridgeBorderStyle::Dashed,
    ] {
        for bad in DRAWN_BAD {
            for (field, borders) in edge_cases(bad, style.clone()) {
                let mut e = engine_with_table();
                let before = e.state_fingerprint_for_fuzzing();
                let evt = e.apply_sync(cell_borders(borders));
                assert_refused(&e, &evt, before, "SetCellBorders", field);
            }
        }
    }
}

#[test]
fn set_cell_borders_accepts_the_bound_itself() {
    for ok in DRAWN_OK {
        for (field, borders) in edge_cases(ok, BridgeBorderStyle::Single) {
            let mut e = engine_with_table();
            let evt = e.apply_sync(cell_borders(borders));
            assert_accepted(&e, &evt, &format!("{field} = {ok}"));
        }
    }
    /* The stored width is the one sent - the guard does not rewrite. */
    let mut e = engine_with_table();
    let evt = e.apply_sync(cell_borders(BridgeCellBorders {
        top: Some(pen(BridgeBorderStyle::Single, 48)),
        bottom: Some(pen(BridgeBorderStyle::Single, 2)),
        ..Default::default()
    }));
    assert_accepted(&e, &evt, "6 pt and 1/4 pt");
    let doc = e.undo.current();
    let Some(engine::Block::Table(t)) = doc.blocks.get(1) else {
        panic!("table")
    };
    let b = t.rows[0].cells[0].props.borders.as_ref().expect("borders");
    assert_eq!(b.top.as_ref().map(|s| s.size_eighth_pt), Some(48));
    assert_eq!(b.bottom.as_ref().map(|s| s.size_eighth_pt), Some(2));
}

/// A `None` stroke paints nothing: 0 is its natural width, 48 the ceiling.
#[test]
fn a_none_stroke_may_be_zero_but_not_past_the_ceiling() {
    for ok in [0u16, 1, 2, 48] {
        let mut e = engine_with_table();
        let evt = e.apply_sync(cell_borders(BridgeCellBorders {
            top: Some(pen(BridgeBorderStyle::None, ok)),
            ..Default::default()
        }));
        assert_accepted(&e, &evt, &format!("none {ok}"));
    }
    for bad in [49u16, 96, u16::MAX] {
        let mut e = engine_with_table();
        let before = e.state_fingerprint_for_fuzzing();
        let evt = e.apply_sync(cell_borders(BridgeCellBorders {
            left: Some(pen(BridgeBorderStyle::None, bad)),
            ..Default::default()
        }));
        assert_refused(
            &e,
            &evt,
            before,
            "SetCellBorders",
            "borders.left.size_eighth_pt",
        );
    }
}

#[test]
fn set_paragraph_borders_refuses_and_accepts_the_same_range() {
    for bad in DRAWN_BAD {
        for (field, borders) in edge_cases(bad, BridgeBorderStyle::Single) {
            let mut e = engine_over("hello");
            let before = e.state_fingerprint_for_fuzzing();
            let evt = e.apply_sync(paragraph_borders(borders));
            assert_refused(&e, &evt, before, "SetParagraphBorders", field);
        }
    }
    for ok in DRAWN_OK {
        for (field, borders) in edge_cases(ok, BridgeBorderStyle::Single) {
            let mut e = engine_over("hello");
            let evt = e.apply_sync(paragraph_borders(borders));
            assert_accepted(&e, &evt, &format!("{field} = {ok}"));
        }
    }
    /* An all-`None` edge set clears the borders and is always fine. */
    let mut e = engine_over("hello");
    let evt = e.apply_sync(paragraph_borders(BridgeCellBorders::default()));
    assert_accepted(&e, &evt, "clearing");
}

/// The first bad edge is the one named when several are.
#[test]
fn the_first_bad_edge_is_named() {
    let mut e = engine_over("hello");
    let before = e.state_fingerprint_for_fuzzing();
    let evt = e.apply_sync(paragraph_borders(BridgeCellBorders {
        top: Some(pen(BridgeBorderStyle::Single, 4)),
        left: Some(pen(BridgeBorderStyle::Single, 4)),
        bottom: Some(pen(BridgeBorderStyle::Single, 4000)),
        right: Some(pen(BridgeBorderStyle::Single, 4000)),
    }));
    assert_refused(
        &e,
        &evt,
        before,
        "SetParagraphBorders",
        "borders.bottom.size_eighth_pt",
    );
}

// ---- HTML paste -------------------------------------------------------------

/// A pasted `border: Npx solid` is clamped, never refused (a clipboard is
/// not the user's to re-type), and the model holds the bound afterwards.
#[test]
fn html_paste_clamps_a_border_width_into_the_bound() {
    for css in [
        "1e30px", "99999px", "9px", "0px", "0.01px", "-5px", "inf px", "NaNpx",
    ] {
        let mut e = engine_over("hello");
        let evt = e.apply_sync(Command::PasteHtml {
            html: format!(
                r#"<table><tr><td style="border-top:{css} solid #000;border-left:{css} dashed #f00">c</td></tr></table>"#
            ),
        });
        assert_accepted(&e, &evt, css);
    }
    let mut e = engine_over("hello");
    let evt = e.apply_sync(Command::PasteHtml {
        html: r#"<table><tr><td style="border-top:1e30px solid #000">c</td></tr></table>"#.into(),
    });
    assert_accepted(&e, &evt, "1e30px");
    let mut widths = vec![];
    for block in e.undo.current().blocks.iter() {
        if let engine::Block::Table(t) = block {
            for row in &t.rows {
                for cell in &row.cells {
                    if let Some(s) = cell.props.borders.as_ref().and_then(|b| b.top.as_ref()) {
                        widths.push(s.size_eighth_pt);
                    }
                }
            }
        }
    }
    assert_eq!(widths, vec![48], "the huge width pasted as 6 pt");
}

// ---- SetTabStops: the list length --------------------------------------------

fn stops(n: usize) -> Vec<BridgeTabStop> {
    (0..n)
        .map(|i| BridgeTabStop {
            position_pt: 10.0 + i as f32,
            kind: BridgeTabKind::Left,
            leader: None,
        })
        .collect()
}

#[test]
fn set_tab_stops_refuses_more_than_64_stops() {
    for n in [65usize, 66, 100, 1000] {
        let mut e = engine_over("hello");
        let before = e.state_fingerprint_for_fuzzing();
        let evt = e.apply_sync(Command::SetTabStops {
            range: whole(),
            stops: stops(n),
        });
        assert_refused(&e, &evt, before, "SetTabStops", "stops");
        let Event::Error { message, .. } = &evt else {
            unreachable!()
        };
        assert!(message.contains(&n.to_string()), "{message}");
    }
}

#[test]
fn set_tab_stops_accepts_up_to_64_stops() {
    for n in [0usize, 1, 63, 64] {
        let mut e = engine_over("hello");
        let evt = e.apply_sync(Command::SetTabStops {
            range: whole(),
            stops: stops(n),
        });
        assert_accepted(&e, &evt, &format!("{n} stops"));
        let held = e.undo.current().blocks[0]
            .as_paragraph()
            .expect("paragraph")
            .props
            .tab_stops
            .len();
        assert_eq!(held, n);
    }
}

/// A bad POSITION inside a too-long list: the count is the first refusal
/// (it is the cheaper check), and either way nothing changes.
#[test]
fn a_too_long_list_is_refused_before_its_positions_are_looked_at() {
    let mut list = stops(65);
    list[3].position_pt = 1e33;
    let mut e = engine_over("hello");
    let before = e.state_fingerprint_for_fuzzing();
    let evt = e.apply_sync(Command::SetTabStops {
        range: whole(),
        stops: list,
    });
    assert_refused(&e, &evt, before, "SetTabStops", "stops");
}

/// The shared constants are Word's numbers.
#[test]
fn the_shared_constants_are_words() {
    assert_eq!(engine::MAX_BORDER_EIGHTH_PT, 48);
    assert_eq!(engine::MIN_BORDER_EIGHTH_PT, 2);
    assert_eq!(engine::MAX_TAB_STOPS, 64);
}
