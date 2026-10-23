//! Issue #512 - a position-taking command never does arithmetic on a RAW
//! wire offset.
//!
//! `do_insert_field` built its caret as `at.offset + cached.len()` straight
//! from the wire: `offset: 4294967295` panicked `attempt to add with
//! overflow` (24 nightly `rpc_command` findings, #493 .. #549; in the
//! release wasm the add wraps to a tiny caret instead - a wrong caret
//! rather than a trap). The same command also took a block index past the
//! end of the document leniently: `insert_field_at` returns the tree
//! unchanged for it, and the handler still pushed an undo entry and
//! answered `SelectionChanged`.
//!
//! The fix routes `InsertField` and `InsertFootnote` / `InsertEndnote`
//! through the engine's ONE validated boundary for an explicit wire
//! position, `resolve_edit_pos` (issue #115): a path that addresses no
//! paragraph is refused `NotInParagraph` (nothing applied - the #341
//! "error => no mutation" invariant), and the offset is SNAPPED (capped at
//! the paragraph's length, floored to a char boundary) before any
//! `offset + len` is formed.
//!
//! The last test is the audit of every other command that carries a
//! position: each is driven with `u32::MAX`-class offsets and dangling
//! block indices, with and without a live selection, and must neither
//! panic (the test profile has overflow checks on, like the fuzz build)
//! nor break the `rpc_command` invariants.
//!
//! Gated on `fuzz-native` like the other bridge-level tests that drive real
//! `Command`s through `Engine::apply_sync` and read the fuzz fingerprint.

use super::*;
use bridge::{
    Alignment as BridgeAlignment, BridgeCellBorders, BridgeTabStop, Direction as BridgeDirection,
    ErrorKind, FieldKind, ImageFit, ListKind, PageOrientation, PathStep, SectionBreakKind,
    SelectionModifier, TocSwitches,
};

const MAX: u32 = u32::MAX;

const KINDS: [FieldKind; 6] = [
    FieldKind::Page,
    FieldKind::NumPages,
    FieldKind::Date,
    FieldKind::Time,
    FieldKind::FileName,
    FieldKind::Author,
];

fn engine_over(text: &str) -> Engine {
    Engine::new_headless(DocumentTree::from_text(text))
}

/// `evt` is the typed refusal of `cmd`, and nothing about the session moved.
fn assert_refused(e: &Engine, evt: &Event, before: [u64; 5], cmd: &Command, why: &str) {
    let Event::Error {
        kind,
        message,
        command,
    } = evt
    else {
        panic!("{why}: expected a refusal, got {evt:?}");
    };
    assert_eq!(*kind, Some(ErrorKind::NotInParagraph), "{why}: {message}");
    assert_eq!(
        command.as_deref(),
        Some(cmd.kind().wire_name().as_str()),
        "{why}: the refusal names its command"
    );
    assert_eq!(
        e.state_fingerprint_for_fuzzing(),
        before,
        "{why}: a refused command changes nothing"
    );
    assert_eq!(e.check_invariants_for_fuzzing(), Ok(()), "{why}");
}

fn field_command(at: BridgeLogicalPos, kind: FieldKind) -> Command {
    Command::InsertField { at, kind }
}

/// The 24 findings: a block that is not there, offset `u32::MAX`. Refused
/// once, at the door - before the field's text is even built - and not
/// merely "no panic": no undo entry, no selection move.
#[test]
fn an_insert_field_on_a_block_that_does_not_exist_is_refused_with_nothing_applied() {
    let mut e = engine_over("hello");
    for block in [1, 22, 7_364_864, 543_716_896, 3_909_091_511, MAX] {
        for offset in [0, 5, MAX - 2, MAX] {
            for kind in KINDS {
                let cmd = field_command(bpos_top(block, offset), kind);
                let before = e.state_fingerprint_for_fuzzing();
                let evt = e.apply_sync(cmd.clone());
                assert_refused(
                    &e,
                    &evt,
                    before,
                    &cmd,
                    &format!("{kind:?} at block {block} offset {offset}"),
                );
            }
        }
    }
    assert_eq!(e.undo_depth(), 1, "no refusal pushed an undo entry");
    assert_eq!(e.undo.current().paragraph_text(0), Some("hello"));
}

/// An offset far past the text of a REAL paragraph follows the engine-wide
/// snap policy (#115) like `ReplaceRange` / `InsertText`: the field lands at
/// the paragraph's end and the caret sits right after it.
#[test]
fn an_insert_field_far_past_the_text_lands_at_the_paragraph_end() {
    for offset in [6, 999_999, MAX - 2, MAX - 1, MAX] {
        let mut e = engine_over("hello");
        let evt = e.apply_sync(field_command(bpos_top(0, offset), FieldKind::Page));
        assert!(
            matches!(evt, Event::SelectionChanged { .. }),
            "offset {offset}: {evt:?}"
        );
        let p = e.undo.current().nth_paragraph(0).expect("paragraph 0");
        assert_eq!(p.text, "hello1", "offset {offset}");
        assert_eq!(p.fields.len(), 1, "offset {offset}");
        assert_eq!((p.fields[0].start, p.fields[0].end), (5, 6), "{offset}");
        let sel = e.selection.as_ref().expect("a selection");
        assert_eq!(sel.caret, bpos_top(0, 6), "offset {offset}");
        assert!(e.selection_is_valid(), "offset {offset}");
        assert_eq!(e.undo_depth(), 2, "offset {offset}: one undo entry");
    }
}

/// An offset inside a multi-byte scalar floors to the char boundary and the
/// caret is derived from THAT offset (the old `at.offset + len` was one byte
/// too far, then clamped by luck).
#[test]
fn an_insert_field_inside_a_scalar_snaps_down_and_the_caret_follows_the_snap() {
    let mut e = engine_over("بب");
    let evt = e.apply_sync(field_command(bpos_top(0, 3), FieldKind::Page));
    assert!(matches!(evt, Event::SelectionChanged { .. }), "{evt:?}");
    let p = e.undo.current().nth_paragraph(0).expect("paragraph 0");
    assert_eq!(p.text, "ب1ب");
    assert_eq!((p.fields[0].start, p.fields[0].end), (2, 3));
    assert_eq!(e.selection.as_ref().expect("selection").caret.offset, 3);
}

/// A table-cell path keeps its own, earlier refusal.
#[test]
fn an_insert_field_in_a_table_cell_path_is_still_refused_in_table_cell() {
    let mut e = engine_over("hello");
    let mut at = bpos_top(0, MAX);
    at.path.steps.push(PathStep::Block { idx: 0 });
    let before = e.state_fingerprint_for_fuzzing();
    let evt = e.apply_sync(field_command(at, FieldKind::Page));
    let Event::Error { kind, .. } = &evt else {
        panic!("expected a refusal, got {evt:?}");
    };
    assert_eq!(*kind, Some(ErrorKind::InTableCell));
    assert_eq!(e.state_fingerprint_for_fuzzing(), before);
}

/// The story branch resolves against the STORY tree (a page-number footer
/// is the primary use): a dangling block of the footer is refused with the
/// footer still active, a huge offset in its one paragraph snaps.
#[test]
fn an_insert_field_in_a_footer_resolves_against_the_footer() {
    let mut e = engine_over("body");
    let evt = e.apply_sync(Command::EnterHeaderFooter {
        page: 0,
        area: bridge::HeaderFooterArea::Footer,
    });
    assert!(matches!(evt, Event::SelectionChanged { .. }), "{evt:?}");
    assert!(e.story_active());

    for (block, offset) in [(1, 0), (7, MAX), (MAX, MAX)] {
        let cmd = field_command(bpos_top(block, offset), FieldKind::Page);
        let before = e.state_fingerprint_for_fuzzing();
        let evt = e.apply_sync(cmd.clone());
        assert_refused(&e, &evt, before, &cmd, &format!("footer block {block}"));
        assert!(e.story_active(), "the refusal leaves the footer active");
    }

    let evt = e.apply_sync(field_command(bpos_top(0, MAX), FieldKind::Page));
    assert!(matches!(evt, Event::SelectionChanged { .. }), "{evt:?}");
    let story = e.story_doc().expect("the footer story");
    assert_eq!(story.paragraph_text(0), Some("1"));
    assert_eq!(e.undo.current().paragraph_text(0), Some("body"));
    assert!(e.selection_is_valid());
}

/// `InsertFootnote` / `InsertEndnote` shared the shape: a validated path but
/// an unclamped offset, `at.offset + 3` for the caret after the reference.
#[test]
fn a_note_at_a_huge_offset_lands_at_the_paragraph_end() {
    for endnote in [false, true] {
        for offset in [MAX - 2, MAX - 1, MAX] {
            let mut e = engine_over("hello");
            let at = bpos_top(0, offset);
            let cmd = if endnote {
                Command::InsertEndnote { at }
            } else {
                Command::InsertFootnote { at }
            };
            let evt = e.apply_sync(cmd);
            assert!(
                matches!(evt, Event::SelectionChanged { .. }),
                "endnote {endnote} offset {offset}: {evt:?}"
            );
            let p = e.undo.current().nth_paragraph(0).expect("paragraph 0");
            assert_eq!(p.text, "hello\u{FFFC}", "endnote {endnote} offset {offset}");
            assert_eq!(p.inline_objects.len(), 1);
            assert_eq!(p.inline_objects[0].at, 5);
            assert_eq!(e.check_invariants_for_fuzzing(), Ok(()));
        }
    }
}

#[test]
fn a_note_on_a_block_that_does_not_exist_is_refused_with_nothing_applied() {
    for endnote in [false, true] {
        for (block, offset) in [(1, 0), (22, MAX), (MAX, MAX)] {
            let mut e = engine_over("hello");
            let at = bpos_top(block, offset);
            let cmd = if endnote {
                Command::InsertEndnote { at }
            } else {
                Command::InsertFootnote { at }
            };
            let before = e.state_fingerprint_for_fuzzing();
            let evt = e.apply_sync(cmd.clone());
            assert_refused(&e, &evt, before, &cmd, &format!("block {block}"));
        }
    }
}

/// Found by the audit below: `InsertSectionBreak` had the same shape - a
/// lenient no-op tree for a missing block, then a caret built by bumping the
/// RAW wire block index (`u32::MAX + 1`).
#[test]
fn a_section_break_on_a_block_that_does_not_exist_is_refused_with_nothing_applied() {
    let mut e = engine_over("hello");
    for (block, offset) in [(1, 0), (22, MAX), (MAX - 1, 0), (MAX, MAX)] {
        let cmd = Command::InsertSectionBreak {
            at: bpos_top(block, offset),
            kind: SectionBreakKind::NextPage,
        };
        let before = e.state_fingerprint_for_fuzzing();
        let evt = e.apply_sync(cmd.clone());
        assert_refused(&e, &evt, before, &cmd, &format!("block {block}"));
    }
    // A real paragraph with a huge offset snaps to its end, as before.
    let evt = e.apply_sync(Command::InsertSectionBreak {
        at: bpos_top(0, MAX),
        kind: SectionBreakKind::NextPage,
    });
    assert!(matches!(evt, Event::SelectionChanged { .. }), "{evt:?}");
    assert_eq!(e.undo.current().paragraph_text(0), Some("hello"));
    assert_eq!(e.check_invariants_for_fuzzing(), Ok(()));
}

fn pos_range(a: BridgeLogicalPos, b: BridgeLogicalPos) -> BridgeLogicalRange {
    BridgeLogicalRange { start: a, end: b }
}

fn attrs() -> TextAttrsPatch {
    TextAttrsPatch {
        bold: Some(true),
        italic: None,
        underline: None,
        strike: None,
        font_family: None,
        font_size: None,
        color: None,
        bg_color: None,
        script: None,
        language: None,
        caps: None,
        small_caps: None,
        font_slot: None,
    }
}

/// Every command of the bridge that carries a `LogicalPos` / `LogicalRange`,
/// built around the positions `from` .. `to` (`from` is also the single
/// `at`). Composition is a three-command sequence, so each entry is a list.
fn position_commands(from: BridgeLogicalPos, to: BridgeLogicalPos) -> Vec<Vec<Command>> {
    let range = || pos_range(from.clone(), to.clone());
    let at = || from.clone();
    let one = |c: Command| vec![c];
    vec![
        one(Command::InsertText {
            at: Some(at()),
            text: "x".into(),
        }),
        one(Command::DeleteRange { range: range() }),
        one(Command::ReplaceRange {
            range: range(),
            text: "yz".into(),
        }),
        one(Command::ApplyFormatting {
            range: Some(range()),
            attrs: attrs(),
        }),
        one(Command::SplitParagraph { at: Some(at()) }),
        one(Command::InsertImage {
            at: at(),
            image: bridge::ImageBlob {
                bytes: vec![0x89, b'P', b'N', b'G'],
                mime: "image/png".into(),
                width: 10,
                height: 10,
            },
            fit: ImageFit::Original,
        }),
        one(Command::SetSelection {
            range: range(),
            caret: to.clone(),
        }),
        one(Command::ExtendSelection {
            to: to.clone(),
            modifier: SelectionModifier::None,
        }),
        vec![
            Command::BeginComposition { at: Some(at()) },
            Command::UpdateComposition {
                text: "w".into(),
                target_range: Some(range()),
            },
            Command::EndComposition { commit: true },
        ],
        one(Command::SetParagraphAlign {
            range: range(),
            align: BridgeAlignment::Center,
        }),
        one(Command::SetParagraphDirection {
            range: range(),
            direction: BridgeDirection::Rtl,
        }),
        one(Command::SetColumns {
            at: at(),
            count: 2,
            gutter_pt: 12.0,
        }),
        one(Command::InsertPageBreak { at: at() }),
        one(Command::InsertSectionBreak {
            at: at(),
            kind: SectionBreakKind::NextPage,
        }),
        one(Command::InsertField {
            at: at(),
            kind: FieldKind::Page,
        }),
        one(Command::InsertFootnote { at: at() }),
        one(Command::InsertEndnote { at: at() }),
        one(Command::InsertTextBox {
            at: at(),
            width_emu: 1_000_000,
            height_emu: 500_000,
        }),
        one(Command::SetFieldInstruction {
            at: at(),
            instruction: "PAGE".into(),
        }),
        one(Command::InsertToc {
            at: at(),
            switches: TocSwitches::default(),
        }),
        one(Command::SetParagraphBorders {
            range: range(),
            borders: BridgeCellBorders::default(),
        }),
        one(Command::SetPageMargins {
            at: at(),
            top_pt: 72.0,
            right_pt: 72.0,
            bottom_pt: 72.0,
            left_pt: 72.0,
        }),
        one(Command::SetPageOrientation {
            at: at(),
            orientation: PageOrientation::Landscape,
        }),
        one(Command::ToggleList {
            range: range(),
            kind: ListKind::Bullet,
        }),
        one(Command::ChangeListLevel {
            range: range(),
            delta: 1,
        }),
        one(Command::SetParagraphIndent {
            range: range(),
            start_pt: 18.0,
            end_pt: 0.0,
            first_line_pt: 0.0,
        }),
        one(Command::SetLineSpacing {
            range: range(),
            multiplier: 1.5,
        }),
        one(Command::SetParagraphShading {
            range: range(),
            color: None,
        }),
        one(Command::InsertComment {
            range: range(),
            text: "note".into(),
            author: "a".into(),
        }),
        one(Command::SetTabStops {
            range: range(),
            stops: Vec::<BridgeTabStop>::new(),
        }),
        one(Command::ApplyStyle {
            range: range(),
            style_id: None,
        }),
    ]
}

/// Audit of the sibling `offset + len` sites the #512 triage listed
/// (`restyle_replacement`, `story_insert_text`, `do_insert_note`,
/// `do_insert_text_interactive`, `do_replace_range`, and the protection
/// gate's run-SDT replace): every command that carries a position takes a
/// `u32::MAX`-class offset or a dangling block index without panicking, and
/// keeps the `rpc_command` invariants - the selection stays inside its
/// document, the undo stack inside its bound, and a refusal changes nothing.
/// Run with and without a live selection (several commands consult their
/// explicit `at` only when none exists) and with review mode on (the tracked
/// twins are separate code paths).
#[test]
fn no_position_taking_command_overflows_on_a_u32_max_class_offset() {
    let positions = [
        (bpos_top(0, MAX), bpos_top(0, MAX)),
        (bpos_top(0, MAX - 2), bpos_top(0, MAX)),
        (bpos_top(0, 0), bpos_top(0, MAX)),
        (bpos_top(0, MAX), bpos_top(1, 3)),
        (bpos_top(22, MAX), bpos_top(22, MAX)),
        (bpos_top(0, 2), bpos_top(MAX, MAX)),
        (bpos_top(MAX, MAX), bpos_top(MAX, MAX)),
    ];
    let mut driven = 0;
    for (from, to) in positions {
        for (clear_selection, tracking) in [(false, false), (true, false), (false, true)] {
            for sequence in position_commands(from.clone(), to.clone()) {
                // "héllo" / "wörld": multi-byte text, two paragraphs.
                let mut e = engine_over("h\u{e9}llo w\u{f6}rld");
                let split = e.apply_sync(Command::SplitParagraph {
                    at: Some(bpos_top(0, 7)),
                });
                assert!(!matches!(split, Event::Error { .. }), "{split:?}");
                if tracking {
                    // The tracked twins (`tracked_insert_text`, the tracked
                    // section break, `tracked_delete`) are separate paths.
                    let on = e.apply_sync(Command::ToggleTrackChanges { enabled: true });
                    assert!(!matches!(on, Event::Error { .. }), "{on:?}");
                }
                if clear_selection {
                    e.selection = None;
                }
                for cmd in sequence {
                    let label = format!(
                        "{:?} from {from:?} to {to:?} (selection cleared: {clear_selection}, \
                         tracking: {tracking})",
                        cmd.kind().name()
                    );
                    let before = e.state_fingerprint_for_fuzzing();
                    let evt = e.apply_sync(cmd);
                    driven += 1;
                    /* The fuzz target's one exemption (`fuzz::
                    is_post_commit_report_error`): with the selection
                    cleared, an edit that already landed reports "no active
                    selection" from its reply step. */
                    if let Event::Error { message, .. } = &evt
                        && !message.starts_with("selection_changed: no active selection")
                    {
                        let after = e.state_fingerprint_for_fuzzing();
                        assert_eq!(after, before, "{label}: refused but changed: {message}");
                    }
                    assert_eq!(e.check_invariants_for_fuzzing(), Ok(()), "{label}: {evt:?}");
                }
            }
        }
    }
    assert!(driven > 400, "the audit drove {driven} commands");
}
