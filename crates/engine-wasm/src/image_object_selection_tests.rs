//! Issue #506 — the engine facts the shell's picture OBJECT selection rests
//! on (clean-room memo `onlyoffice-cursor-memo.md` §2 / §9). The object
//! selection itself is shell-side (`ts/src/state/engine-store.ts`); these
//! tests pin what it derives from the engine:
//!
//! 1. after `InsertImage` the picture sits at the command's `at` and the
//!    reply's caret is just past it — the shell object-selects the picture
//!    at that address, exactly as if it had been clicked;
//! 2. a picture copied through its sentinel range carries its media key in
//!    the clipboard HTML, a cut of that range is ONE undo step, and pasting
//!    the HTML back in the same document re-inserts the picture (the media
//!    blob outlives the delete);
//! 3. a press on a picture inside a text box, made from the body, enters
//!    the box that OWNS the picture (innermost box for a nested one), so
//!    one click can enter the box and select its picture.

use super::*;

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
    let evt = block_on(e.apply(cmd));
    assert!(!matches!(evt, Event::Error { .. }), "{evt:?}");
    evt
}

fn pos(block: u32, offset: u32) -> BridgeLogicalPos {
    BridgeLogicalPos {
        path: BridgeBlockPath::top(block),
        offset,
    }
}

fn png(width: u32, height: u32) -> bridge::ImageBlob {
    bridge::ImageBlob {
        bytes: vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a],
        mime: "image/png".to_string(),
        width,
        height,
    }
}

fn caret_at(e: &mut Engine, at: BridgeLogicalPos) {
    apply(
        e,
        Command::SetSelection {
            range: BridgeLogicalRange {
                start: at.clone(),
                end: at.clone(),
            },
            caret: at,
        },
    );
}

fn insert_at(e: &mut Engine, at: BridgeLogicalPos) -> Event {
    apply(
        e,
        Command::InsertImage {
            at,
            image: png(160, 100),
            fit: bridge::ImageFit::FitWidth,
        },
    )
}

/// `(block, at, media_key)` of every listed body picture, in order.
fn pictures(e: &Engine) -> Vec<(u32, u32, String)> {
    e.image_geometry()
        .expect("image geometry")
        .iter()
        .filter(|r| r.story.is_empty())
        .map(|r| {
            let block = match r.path.steps.first() {
                Some(bridge::PathStep::Block { idx }) => *idx,
                other => panic!("a top-level picture, got {other:?}"),
            };
            (block, r.at, r.media_key.clone())
        })
        .collect()
}

fn undo_depth(evt: &Event) -> u32 {
    match evt {
        Event::SelectionChanged { undo_depth, .. } => *undo_depth,
        other => panic!("expected SelectionChanged, got {other:?}"),
    }
}

/// Case 1 — the address the shell object-selects after an insert is the insert's
/// own `at`, and the reply's caret sits just past the 3-byte sentinel.
#[test]
fn insert_image_puts_the_picture_at_its_at_and_answers_with_the_caret_past_it() {
    for (text, offset) in [("", 0_u32), ("abcd", 2)] {
        let mut e = tests::test_engine_with_doc(DocumentTree::from_paragraphs([
            "above".to_string(),
            text.to_string(),
        ]));
        caret_at(&mut e, pos(1, offset));
        let evt = insert_at(&mut e, pos(1, offset));
        let Event::SelectionChanged { range, caret, .. } = &evt else {
            panic!("InsertImage answers SelectionChanged, got {evt:?}");
        };
        assert_eq!(range.start, range.end, "a collapsed caret ({text:?})");
        assert_eq!(
            range.end,
            pos(1, offset + 3),
            "caret just past the sentinel ({text:?})"
        );
        let listed = pictures(&e);
        assert_eq!(listed.len(), 1, "{listed:?}");
        assert_eq!(
            (listed[0].0, listed[0].1),
            (1, offset),
            "picture at the insert's at ({text:?})"
        );
        /* The reply's caret rect is the picture's right edge, on its line. */
        let rect = e.image_geometry().expect("geometry")[0].rect;
        assert!(
            (caret.x - (rect.x + rect.w)).abs() < 1.0,
            "{caret:?} vs {rect:?}"
        );
    }
}

/// Case 2 — copy / cut / paste of a picture through its sentinel range.
#[test]
fn a_picture_copied_by_its_sentinel_carries_its_media_key_and_cut_then_paste_restores_it() {
    let mut e = tests::test_engine_with_doc(DocumentTree::from_paragraphs([
        "above".to_string(),
        String::new(),
    ]));
    caret_at(&mut e, pos(1, 0));
    insert_at(&mut e, pos(1, 0));
    let [(_, _, key)] = pictures(&e).try_into().expect("one picture");

    /* What the shell's Ctrl+C / Ctrl+X do first: select the sentinel. */
    let selected = apply(
        &mut e,
        Command::SetSelection {
            range: BridgeLogicalRange {
                start: pos(1, 0),
                end: pos(1, 3),
            },
            caret: pos(1, 3),
        },
    );
    let Event::ClipboardPayload {
        plain,
        html,
        docx_fragment,
    } = apply(
        &mut e,
        Command::GetSelectionAsClipboard {
            include_docx: Some(true),
        },
    )
    else {
        panic!("expected ClipboardPayload");
    };
    assert_eq!(
        plain, "\u{FFFC}",
        "the copy is the picture, not the text above it"
    );
    /* An inserted picture's media key is its `rel_id` (`data-rel-id`). */
    assert!(
        html.contains("<img ") && html.contains(&format!("=\"{key}\"")),
        "the HTML carries the picture and names its media: {html}"
    );
    assert!(!html.contains("above"), "{html}");
    assert!(!docx_fragment.is_empty());

    /* Cut = delete the selected sentinel: ONE undo snapshot. */
    let before = undo_depth(&selected);
    let cut = apply(
        &mut e,
        Command::DeleteAtCaret {
            forward: false,
            by_word: false,
        },
    );
    assert_eq!(undo_depth(&cut), before + 1, "the cut is one edit");
    assert!(pictures(&e).is_empty(), "the picture is gone");
    /* The blob outlives the delete (undo needs it too), so a same-document
    paste can paint the picture again — whatever prunes unreferenced media
    (#299) must keep this working. */
    assert!(
        e.undo.current().media.contains_key(&key),
        "the blob outlives the delete"
    );

    /* Paste of the copied HTML puts the same picture back. */
    let pasted = apply(&mut e, Command::PasteHtml { html: html.clone() });
    assert_eq!(undo_depth(&pasted), before + 2, "the paste is one edit");
    let back = pictures(&e);
    assert_eq!(back.len(), 1, "{back:?}");
    assert_eq!(
        back[0].2, key,
        "the pasted picture paints from the same media"
    );
    assert_eq!(paragraph_text(&e, 0), "above", "nothing else moved");

    /* One undo takes the paste back, a second one the cut. */
    apply(&mut e, Command::Undo);
    assert!(pictures(&e).is_empty());
    apply(&mut e, Command::Undo);
    assert_eq!(
        pictures(&e).len(),
        1,
        "undoing the cut restores the picture"
    );
}

fn paragraph_text(e: &Engine, block: u32) -> String {
    e.undo
        .current()
        .paragraph_text(block)
        .map(str::to_owned)
        .unwrap_or_default()
}

/// Case 3 — a press on a text-box picture made from the BODY enters the box that
/// owns that picture — the outer box for its picture, the nested box for
/// the nested one (`text_box_pictures.docx`, #206).
#[test]
fn a_press_on_a_text_box_picture_from_the_body_enters_the_box_that_owns_it() {
    let bytes = include_bytes!("../../format-docx/tests/fixtures/text_box_pictures.docx");
    let archive = format_docx::read_docx(bytes).expect("fixture");
    let mut e = tests::test_engine_with_doc(archive.document);
    let rects = e.image_geometry().expect("geometry");
    assert_eq!(rects.len(), 2, "{rects:?}");
    let body_press = BridgePoint {
        x: 1.0 * 72.0 * e.scale(),
        y: 1.0 * 72.0 * e.scale(),
    };
    for r in &rects {
        /* Start from the body each time. */
        let back = apply(
            &mut e,
            Command::PlaceCaretAtPoint {
                page: 0,
                at: body_press,
            },
        );
        let Event::SelectionChanged { editing_story, .. } = &back else {
            panic!("{back:?}");
        };
        assert!(
            editing_story.is_none(),
            "a press in the body is a body press"
        );

        let centre = BridgePoint {
            x: r.rect.x + r.rect.w / 2.0,
            y: r.rect.y + r.rect.h / 2.0,
        };
        let evt = apply(
            &mut e,
            Command::PlaceCaretAtPoint {
                page: 0,
                at: centre,
            },
        );
        let Event::SelectionChanged { editing_story, .. } = &evt else {
            panic!("{evt:?}");
        };
        let story = editing_story.as_ref().expect("the press entered a story");
        assert_eq!(story.area, bridge::HeaderFooterArea::TextBox);
        assert_eq!(
            story.rid, r.story_rid,
            "the press on the picture enters the box that owns it"
        );
    }
}
