//! Issue #503 (item 1) - the restore / replay door of Word's size and point
//! bounds.
//!
//! The command guards (#501 font sizes, #503 point fields) refuse a hostile
//! value on `apply_command`, and the `.docx` reader / HTML paste clamp it.
//! A crash-recovery snapshot is a door neither sees: `Engine::restore`
//! installed whatever bytes IndexedDB handed back, so a `1e33` pt run or a
//! `1e9` pt margin reached layout. Now `restore_snapshot` clamps every font
//! size, margin, indent, spacing, tab stop and gutter into the shared
//! bounds (`DocumentTree::repair_bounds`), `Event::Recovered.bounds_repaired`
//! counts them, and a logged tail command the guards refuse is skipped and
//! counted as `refused_commands`.
//!
//! Gated on `fuzz-native` like the other bridge-level tests that drive real
//! `Command`s (`Engine::apply_sync`, the fuzz invariants).

use super::*;
use engine::{StyleRun, TabKind, TabLeader, TabStop};

fn engine_over(doc: DocumentTree) -> Engine {
    let mut e = Engine::new_headless(doc);
    e.selection = Some(SelectionState {
        anchor: bpos_top(0, 0),
        caret: bpos_top(0, 0),
        ideal_x: None,
        kind: SelectionKind::Linear,
    });
    e
}

fn sized(pt: f32) -> SpanStyle {
    SpanStyle {
        font_size: Some(pt),
        ..Default::default()
    }
}

/// A document that carries what no command could have produced: a `1e33`
/// pt run, a `1e9` pt pair of margins, an absurd indent and tab stop and a
/// 5000 pt style size.
fn hostile_doc() -> DocumentTree {
    let mut doc = DocumentTree::from_text("hello world");
    {
        let p = doc.blocks[0].as_paragraph_mut().expect("paragraph");
        p.spans.push(StyleRun {
            start: 0,
            end: 5,
            style: sized(1.0e33),
        });
        p.props.indent.start_twips = i32::MAX;
        p.props.tab_stops.push(TabStop {
            position_pt: -1e30,
            kind: TabKind::Left,
            leader: TabLeader::None,
        });
    }
    doc.body_section.geometry.margin_left = 1.0e9;
    doc.body_section.geometry.margin_right = 1.0e9;
    doc.style_run_defaults = sized(5000.0);
    doc
}

fn hostile_snapshot() -> Vec<u8> {
    Engine::new_headless(hostile_doc())
        .snapshot_bytes()
        .expect("snapshot")
}

fn recover(e: &mut Engine, snapshot: Vec<u8>, log_tail: Vec<Command>) -> Event {
    e.apply_sync(Command::Recover {
        snapshot,
        log_tail,
        renderer_downgrade: None,
        package: None,
    })
}

fn counts(evt: &Event) -> (u32, u32, u32) {
    let Event::Recovered {
        applied_commands,
        bounds_repaired,
        refused_commands,
        ..
    } = evt
    else {
        panic!("expected Recovered, got {evt:?}");
    };
    (*applied_commands, *bounds_repaired, *refused_commands)
}

fn size_command(pt: f32) -> Command {
    Command::ApplyFormatting {
        range: Some(BridgeLogicalRange {
            start: bpos_top(0, 0),
            end: bpos_top(0, 5),
        }),
        attrs: TextAttrsPatch {
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
        },
    }
}

fn insert(text: &str) -> Command {
    Command::InsertText {
        at: None,
        text: text.into(),
    }
}

#[test]
fn the_hostile_fixture_really_is_out_of_bounds_before_the_restore() {
    let doc = hostile_doc();
    assert!(engine::first_font_size_beyond_bound(&doc).is_some());
    assert!(engine::first_point_field_beyond_bound(&doc).is_some());
}

#[test]
fn restore_clamps_a_snapshot_into_word_s_bounds() {
    let mut e = engine_over(DocumentTree::from_text("fresh"));
    e.restore_for_fuzzing(&hostile_snapshot(), None)
        .expect("restore");
    assert_eq!(e.check_font_sizes_for_fuzzing(), Ok(()));
    assert_eq!(e.check_point_fields_for_fuzzing(), Ok(()));
    let doc = e.undo.current();
    let p = doc.blocks[0].as_paragraph().expect("paragraph");
    assert_eq!(p.spans[0].style.font_size, Some(engine::MAX_FONT_SIZE_PT));
    assert_eq!(
        p.props.indent.start_twips,
        engine::MAX_POINT_FIELD_TWIPS,
        "an indent clamps to 22 in"
    );
    assert_eq!(
        p.props.tab_stops[0].position_pt,
        -engine::MAX_POINT_FIELD_PT
    );
    /* The text itself is not the repair's business. */
    assert_eq!(doc.nth_paragraph(0).unwrap().text, "hello world");
    /* ... and layout of the restored document terminates and paints. */
    e.ensure_layout_for_fuzzing().expect("layout");
    assert!(e.layout_page_count_for_fuzzing() < 50);
}

#[test]
fn recover_reports_the_clamped_values_on_the_reply() {
    let mut e = engine_over(DocumentTree::from_text("fresh"));
    let evt = recover(&mut e, hostile_snapshot(), Vec::new());
    let (applied, repaired, refused) = counts(&evt);
    /* run size + margins (fit) + indent + tab stop + the style default:
    at least the 1e33 pt run and the 1e9 pt margin the issue names. */
    assert!(repaired >= 2, "{repaired}");
    assert_eq!((applied, refused), (0, 0));
    assert_eq!(e.check_font_sizes_for_fuzzing(), Ok(()));
    assert_eq!(e.check_point_fields_for_fuzzing(), Ok(()));
}

#[test]
fn an_engine_authored_snapshot_repairs_nothing() {
    let mut a = engine_over(DocumentTree::from_text("hello world"));
    /* Word's own limit is inside the bound, so it is not repaired. */
    let _ = a.apply_sync(size_command(1638.0));
    assert_eq!(a.check_font_sizes_for_fuzzing(), Ok(()));
    let bytes = a.snapshot_bytes().expect("snapshot");
    let mut b = engine_over(DocumentTree::from_text("fresh"));
    let evt = recover(&mut b, bytes, vec![insert("X")]);
    assert_eq!(counts(&evt), (1, 0, 0));
}

#[test]
fn an_undo_step_does_not_bring_a_hostile_value_back() {
    /* A hostile value in an UNDO entry is repaired too (and not counted:
    the user's current document is what is reported). */
    let clean = DocumentTree::from_text("hello world");
    let mut stack = engine::UndoStack::new(hostile_doc(), 10);
    stack.push(clean);
    let mut a = Engine::new_headless(DocumentTree::from_text("x"));
    a.install_undo_stack(stack);
    let bytes = a.snapshot_bytes().expect("snapshot");
    let mut b = engine_over(DocumentTree::from_text("fresh"));
    let evt = recover(&mut b, bytes, Vec::new());
    assert_eq!(counts(&evt).1, 0, "the current document was clean");
    let _ = b.apply_sync(Command::Undo);
    assert_eq!(b.check_font_sizes_for_fuzzing(), Ok(()));
    assert_eq!(b.check_point_fields_for_fuzzing(), Ok(()));
}

#[test]
fn a_pending_format_and_the_layout_size_are_clamped_too() {
    let mut a = Engine::new_headless(DocumentTree::from_text("hello world"));
    a.pending_format = Some(sized(1.0e30));
    a.layout_cfg = a.layout_cfg.take().map(|mut c| {
        c.px_size = 9.0e9;
        c
    });
    let bytes = a.snapshot_bytes().expect("snapshot");
    let mut b = engine_over(DocumentTree::from_text("fresh"));
    let evt = recover(&mut b, bytes, Vec::new());
    assert!(counts(&evt).1 >= 1);
    assert_eq!(
        b.pending_format.as_ref().and_then(|s| s.font_size),
        Some(engine::MAX_FONT_SIZE_PT)
    );
    if let Some(cfg) = b.layout_cfg.as_ref() {
        assert!(engine::font_size_in_bound(cfg.px_size), "{}", cfg.px_size);
    }
}

#[test]
fn a_refused_log_tail_command_is_skipped_and_counted() {
    let mut a = engine_over(DocumentTree::from_text("hello world"));
    let bytes = a.snapshot_bytes().expect("snapshot");
    let mut b = engine_over(DocumentTree::from_text("fresh"));
    let evt = recover(
        &mut b,
        bytes,
        vec![insert("X"), size_command(1.0e33), insert("Y")],
    );
    /* All three are replayed; the hostile size is refused, not applied. */
    assert_eq!(counts(&evt), (3, 0, 1));
    assert_eq!(b.check_font_sizes_for_fuzzing(), Ok(()));
    let text = b.undo.current().nth_paragraph(0).unwrap().text.clone();
    assert!(text.contains('X') && text.contains('Y'), "{text}");
    let _ = &mut a;
}

#[test]
fn every_guard_kind_counts_in_the_tail() {
    let mut a = engine_over(DocumentTree::from_text("hello world"));
    let bytes = a.snapshot_bytes().expect("snapshot");
    let mut b = engine_over(DocumentTree::from_text("fresh"));
    let hostile_margins = Command::SetPageMargins {
        at: bpos_top(0, 0),
        top_pt: 1e33,
        right_pt: 72.0,
        bottom_pt: 72.0,
        left_pt: 72.0,
    };
    let evt = recover(
        &mut b,
        bytes,
        vec![
            hostile_margins,
            size_command(f32::INFINITY),
            insert("\u{FFFE}"),
            insert("ok"),
        ],
    );
    /* OutOfRange (point guard), InvalidArgument (finiteness) and
    InvalidArgument (forbidden text) are refused; the plain insert lands. */
    assert_eq!(counts(&evt), (4, 0, 3));
    assert_eq!(b.check_point_fields_for_fuzzing(), Ok(()));
    let _ = &mut a;
}
