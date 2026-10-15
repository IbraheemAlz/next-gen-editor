//! Issue #283 — tracked-change (revision) ranges follow their text through
//! every byte-moving edit, end to end through real `Command`s on the
//! headless `fuzz-native` engine: typing (plain and tracked), deletion,
//! Enter, a paragraph merge, a multi-line paste, an IME commit and a
//! formatting change before a tracked insertion leave the revision on the
//! SAME text, `Engine::check_invariants_for_fuzzing` (which now checks
//! every revision range lies inside its paragraph on char boundaries)
//! holds after every command, and accepting / rejecting afterwards acts
//! on the right bytes.

use super::*;

const INS: &str = "INSERTED";

/// `"lead " + INS + " tail"` with a tracked insertion by Bob over `INS`,
/// on a headless engine with the caret at `(0, 0)`.
fn engine_with_insertion(lead: &str) -> Engine {
    let text = format!("{lead}{INS} tail");
    let mut doc = DocumentTree::from_text(&text);
    let start = lead.len() as u32;
    let mut blocks = doc.blocks.clone();
    if let Some(engine::Block::Paragraph(p)) = blocks.get_mut(0) {
        p.revisions.push(engine::Revision {
            start,
            end: start + INS.len() as u32,
            kind: engine::RevisionKind::Insert,
            author: "Bob".to_string(),
            date: "2026-01-01T00:00:00Z".to_string(),
            id: Some(7),
            prev_attrs: None,
            move_name: None,
        });
    }
    doc.blocks = blocks;
    Engine::new_headless(doc)
}

fn run(e: &mut Engine, cmd: Command) -> Event {
    let name = format!("{cmd:?}");
    let evt = e.apply_sync(cmd);
    assert!(!matches!(evt, Event::Error { .. }), "{name}: {evt:?}");
    if let Err(why) = e.check_invariants_for_fuzzing() {
        panic!("after {name}: {why}");
    }
    evt
}

fn pos(block: u32, offset: u32) -> BridgeLogicalPos {
    bpos_top(block, offset)
}

fn range(a: (u32, u32), b: (u32, u32)) -> BridgeLogicalRange {
    BridgeLogicalRange {
        start: pos(a.0, a.1),
        end: pos(b.0, b.1),
    }
}

fn place(e: &mut Engine, block: u32, offset: u32) {
    run(
        e,
        Command::SetSelection {
            range: range((block, offset), (block, offset)),
            caret: pos(block, offset),
        },
    );
}

/// The text every revision of `author` covers, per top-level paragraph.
fn covered(e: &Engine, author: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    for (i, b) in e.undo.current().blocks.iter().enumerate() {
        if let engine::Block::Paragraph(p) = b {
            for r in p.revisions.iter().filter(|r| r.author == author) {
                out.push((i, p.text[r.start as usize..r.end as usize].to_string()));
            }
        }
    }
    out
}

fn text(e: &Engine, block: u32) -> String {
    e.undo
        .current()
        .paragraph_text(block)
        .unwrap_or_default()
        .to_string()
}

#[test]
fn typing_before_a_tracked_insertion_keeps_it_on_its_text() {
    let mut e = engine_with_insertion("lead ");
    place(&mut e, 0, 2);
    run(
        &mut e,
        Command::InsertText {
            at: None,
            text: "é€x".to_string(),
        },
    );
    assert_eq!(covered(&e, "Bob"), vec![(0, INS.to_string())]);
    /* Accept-all keeps the right bytes, reject-all removes exactly them. */
    let mut rejected = engine_with_insertion("lead ");
    place(&mut rejected, 0, 2);
    run(
        &mut rejected,
        Command::InsertText {
            at: None,
            text: "é€x".to_string(),
        },
    );
    run(&mut rejected, Command::RejectAllRevisions);
    assert_eq!(text(&rejected, 0), "leé€xad  tail");
    run(&mut e, Command::AcceptAllRevisions);
    assert_eq!(text(&e, 0), format!("leé€xad {INS} tail"));
    assert!(covered(&e, "Bob").is_empty());
}

#[test]
fn tracked_typing_before_another_reviewers_insertion_shifts_it() {
    let mut e = engine_with_insertion("lead ");
    run(&mut e, Command::ToggleTrackChanges { enabled: true });
    place(&mut e, 0, 0);
    run(
        &mut e,
        Command::InsertText {
            at: None,
            text: "ab".to_string(),
        },
    );
    assert_eq!(covered(&e, "Bob"), vec![(0, INS.to_string())]);
    assert_eq!(covered(&e, "You"), vec![(0, "ab".to_string())]);
    /* Typing right at the revision's start stays outside it. */
    place(&mut e, 0, 7);
    run(
        &mut e,
        Command::InsertText {
            at: None,
            text: "Z".to_string(),
        },
    );
    assert_eq!(covered(&e, "Bob"), vec![(0, INS.to_string())]);
}

#[test]
fn deleting_before_a_tracked_insertion_shifts_it_left() {
    let mut e = engine_with_insertion("lead ");
    run(
        &mut e,
        Command::DeleteRange {
            range: range((0, 0), (0, 3)),
        },
    );
    assert_eq!(text(&e, 0), format!("d {INS} tail"));
    assert_eq!(covered(&e, "Bob"), vec![(0, INS.to_string())]);
    /* Backspace at the caret too. */
    place(&mut e, 0, 1);
    run(
        &mut e,
        Command::DeleteAtCaret {
            forward: false,
            by_word: false,
        },
    );
    assert_eq!(covered(&e, "Bob"), vec![(0, INS.to_string())]);
}

#[test]
fn enter_and_merge_carry_the_revision_with_its_text() {
    let mut e = engine_with_insertion("lead ");
    place(&mut e, 0, 2);
    run(&mut e, Command::SplitParagraph { at: None });
    assert_eq!(text(&e, 0), "le");
    assert_eq!(covered(&e, "Bob"), vec![(1, INS.to_string())]);
    /* Merge back: delete the paragraph break. */
    run(
        &mut e,
        Command::DeleteRange {
            range: range((0, 2), (1, 0)),
        },
    );
    assert_eq!(covered(&e, "Bob"), vec![(0, INS.to_string())]);
    run(&mut e, Command::RejectAllRevisions);
    assert_eq!(text(&e, 0), "lead  tail");
}

#[test]
fn paste_before_a_tracked_insertion_shifts_it() {
    let mut e = engine_with_insertion("lead ");
    place(&mut e, 0, 1);
    run(
        &mut e,
        Command::PastePlain {
            text: "one\ntwo".to_string(),
        },
    );
    assert_eq!(covered(&e, "Bob"), vec![(1, INS.to_string())]);
    /* Tracked paste too. */
    let mut t = engine_with_insertion("lead ");
    run(&mut t, Command::ToggleTrackChanges { enabled: true });
    place(&mut t, 0, 1);
    run(
        &mut t,
        Command::PastePlain {
            text: "xyz".to_string(),
        },
    );
    assert_eq!(covered(&t, "Bob"), vec![(0, INS.to_string())]);
}

#[test]
fn ime_commit_before_a_tracked_insertion_shifts_it() {
    let mut e = engine_with_insertion("lead ");
    place(&mut e, 0, 3);
    run(&mut e, Command::BeginComposition { at: None });
    run(
        &mut e,
        Command::UpdateComposition {
            text: "日本".to_string(),
            target_range: None,
        },
    );
    run(&mut e, Command::EndComposition { commit: true });
    assert_eq!(text(&e, 0), format!("lea日本d {INS} tail"));
    assert_eq!(covered(&e, "Bob"), vec![(0, INS.to_string())]);
}

#[test]
fn a_tracked_formatting_change_follows_its_text_too() {
    let mut e = engine_with_insertion("lead ");
    run(&mut e, Command::ToggleTrackChanges { enabled: true });
    /* Bold " tail" with review mode on: a FormatChange revision. */
    let len = text(&e, 0).len() as u32;
    run(
        &mut e,
        Command::ApplyFormatting {
            range: Some(range((0, len - 4), (0, len))),
            attrs: bridge::TextAttrsPatch {
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
            },
        },
    );
    let formatted = |e: &Engine| -> Vec<String> {
        let doc = e.undo.current();
        let p = doc.nth_paragraph(0).unwrap();
        p.revisions
            .iter()
            .filter(|r| r.kind == engine::RevisionKind::FormatChange)
            .map(|r| p.text[r.start as usize..r.end as usize].to_string())
            .collect()
    };
    assert_eq!(formatted(&e), vec!["tail".to_string()]);
    run(&mut e, Command::ToggleTrackChanges { enabled: false });
    place(&mut e, 0, 0);
    run(
        &mut e,
        Command::InsertText {
            at: None,
            text: "ÄÖ".to_string(),
        },
    );
    assert_eq!(formatted(&e), vec!["tail".to_string()]);
    assert_eq!(covered(&e, "Bob"), vec![(0, INS.to_string())]);
}

#[test]
fn a_restored_snapshot_with_broken_revision_ranges_is_repaired() {
    let mut doc = DocumentTree::from_text("aé");
    let mut blocks = doc.blocks.clone();
    if let Some(engine::Block::Paragraph(p)) = blocks.get_mut(0) {
        for (start, end) in [(0, 2), (1, 2), (2, 40)] {
            p.revisions.push(engine::Revision {
                start,
                end,
                kind: engine::RevisionKind::Delete,
                author: "Mallory".to_string(),
                date: String::new(),
                id: None,
                prev_attrs: None,
                move_name: None,
            });
        }
    }
    doc.blocks = blocks;
    assert!(engine::check_revision_ranges(&doc).is_err());
    let hostile = Engine::new_headless(doc);
    let bytes = hostile.snapshot_bytes().expect("snapshot");
    let mut e = Engine::new_headless(DocumentTree::from_text("fresh"));
    e.restore_from_bytes(&bytes).expect("restore");
    e.check_invariants_for_fuzzing()
        .expect("repaired on restore");
    let doc = e.undo.current();
    let p = doc.nth_paragraph(0).unwrap();
    /* [0, 2) snaps to [0, 1); [1, 2) to nothing; [2, 40) to [1, 3). */
    let ranges: Vec<(u32, u32)> = p.revisions.iter().map(|r| (r.start, r.end)).collect();
    assert_eq!(ranges, vec![(0, 1), (1, 3)]);
}
