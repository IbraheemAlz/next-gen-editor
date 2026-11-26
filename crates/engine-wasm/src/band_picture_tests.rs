//! Issue #573 — the pictures of the header / footer being EDITED are
//! objects (clean-room memo `onlyoffice-cursor-memo.md` §2 "Drawings are
//! layer-scoped", §8, §9): only the active story's drawing layer is
//! hit-testable, so
//!
//! 1. `GetImageRects` lists a header / footer part's pictures (inline and
//!    floating) only while that part is the active story — on its anchor
//!    page, with story-rooted paths and `story_area` / `story_rid` naming
//!    the part — and never while the body is edited;
//! 2. the image commands address such a picture with an empty `story`
//!    (positions resolve in the part being edited): a resize / move is ONE
//!    undo step of the part, the body picture at the same address is
//!    untouched, a missing address or a text-box chain is refused;
//! 3. the shell's Delete path — select the sentinel in the story, delete the
//!    selection — removes the picture in one undo step.

use super::*;
use bridge::HeaderFooterArea;

fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    use std::task::{Context, Poll, Waker};
    let mut cx = Context::from_waker(Waker::noop());
    let mut fut = Box::pin(fut);
    match fut.as_mut().poll(&mut cx) {
        Poll::Ready(v) => v,
        Poll::Pending => panic!("Engine::apply suspended in a native test"),
    }
}

fn send(e: &mut Engine, cmd: Command) -> Event {
    block_on(e.apply(cmd))
}

fn apply(e: &mut Engine, cmd: Command) -> Event {
    let evt = send(e, cmd);
    assert!(!matches!(evt, Event::Error { .. }), "{evt:?}");
    evt
}

fn blob() -> engine::ImageBlob {
    engine::ImageBlob {
        content_type: "image/png".into(),
        data: format_pdf::test_images::png_rgba(2, 2, &[200; 16]),
    }
}

const HEADER_RID: &str = "rIdH";
const FOOTER_RID: &str = "rIdF";
/// "Header " / "Footer ": the inline picture's sentinel byte.
const INLINE_AT: u32 = 7;

/// A part (`label` + an inline 1 x 1/2 in picture; then a paragraph holding
/// only a floating 1 x 1/2 in picture, 4 in from the page's left edge,
/// `top_emu` below its top) minted into `doc`'s media.
fn part_with_pictures(doc: &mut DocumentTree, label: &str, top_emu: i64) -> Vec<engine::Block> {
    let mut part = doc.clone();
    part.blocks = DocumentTree::from_paragraphs([label.to_string(), "\u{FFFC}".to_string()]).blocks;
    part = part.insert_inline_image_at(
        EnginePos::new(EngineBlockPath::top(0), INLINE_AT),
        blob(),
        914_400,
        457_200,
    );
    if let Some(p) = part.blocks[1].as_paragraph_mut() {
        p.inline_objects.push(engine::InlineObject {
            at: 0,
            kind: engine::InlineKind::Image {
                rel_id: format!("float_{label}"),
                width_emu: 914_400,
                height_emu: 457_200,
                media_key: None,
            },
            anchor: Some(Box::new(engine::FloatAnchor {
                position_h: engine::HPosition {
                    relative_from: engine::HRelativeFrom::Page,
                    offset: engine::FloatOffset::Emu(4 * 914_400),
                },
                position_v: engine::VPosition {
                    relative_from: engine::VRelativeFrom::Page,
                    offset: engine::FloatOffset::Emu(top_emu),
                },
                ..engine::FloatAnchor::default()
            })),
            source_xml: None,
        });
    }
    doc.media = part.media.clone();
    part.blocks.iter().cloned().collect()
}

/// Body: a text line, then a paragraph that STARTS with an inline picture
/// (`Block(1)` @ 0 — the same address a header picture could have), then
/// text. Header and footer parts each hold an inline and a floating
/// picture.
fn doc() -> DocumentTree {
    let mut doc = DocumentTree::from_paragraphs([
        "Body text above".to_string(),
        " after the picture".to_string(),
        "Body text below".to_string(),
    ]);
    doc = doc.insert_inline_image_at(
        EnginePos::new(EngineBlockPath::top(1), 0),
        blob(),
        914_400,
        457_200,
    );
    let header = part_with_pictures(&mut doc, "Header ", 228_600);
    let footer = part_with_pictures(&mut doc, "Footer ", 10_400_000);
    doc.headers.insert(HEADER_RID.into(), header);
    doc.footers.insert(FOOTER_RID.into(), footer);
    doc.body_section.header_refs.default = Some(HEADER_RID.into());
    doc.body_section.footer_refs.default = Some(FOOTER_RID.into());
    doc
}

fn enter(e: &mut Engine, area: HeaderFooterArea) {
    match apply(e, Command::EnterHeaderFooter { page: 0, area }) {
        Event::SelectionChanged {
            editing_story: Some(s),
            ..
        } => assert_eq!(s.area, area),
        other => panic!("EnterHeaderFooter answered {other:?}"),
    }
}

fn rects(e: &Engine) -> Vec<bridge::ImageRect> {
    e.image_geometry().expect("image geometry")
}

fn band_rects(e: &Engine) -> Vec<bridge::ImageRect> {
    rects(e)
        .into_iter()
        .filter(|r| r.story_area.is_some())
        .collect()
}

/// The part's paragraph `block` as the model holds it.
fn part_paragraph(e: &Engine, area: HeaderFooterArea, block: usize) -> engine::Paragraph {
    let doc = e.undo.current();
    let (parts, rid) = match area {
        HeaderFooterArea::Header => (&doc.headers, HEADER_RID),
        _ => (&doc.footers, FOOTER_RID),
    };
    parts[rid][block]
        .as_paragraph()
        .expect("a paragraph")
        .clone()
}

fn image_extent(p: &engine::Paragraph, at: u32) -> (i64, i64) {
    p.inline_objects
        .iter()
        .find_map(|io| match &io.kind {
            engine::InlineKind::Image {
                width_emu,
                height_emu,
                ..
            } if io.at == at => Some((*width_emu, *height_emu)),
            _ => None,
        })
        .expect("a picture at that offset")
}

fn undo_depth(e: &Engine) -> u32 {
    e.undo.depth()
}

/// Case 1 — listed only while the part is edited, with story-rooted
/// addresses, inside its band of the anchor page.
#[test]
fn a_band_part_pictures_are_listed_only_while_the_part_is_edited() {
    let mut e = tests::test_engine_with_doc(doc());
    let body_only = rects(&e);
    assert_eq!(
        body_only.len(),
        1,
        "editing the body: only its picture {body_only:?}"
    );
    assert_eq!(body_only[0].story_area, None);
    assert_eq!(body_only[0].path, BridgeBlockPath::top(1));
    let body_y = body_only[0].rect.y;

    for area in [HeaderFooterArea::Header, HeaderFooterArea::Footer] {
        enter(&mut e, area);
        let rid = if area == HeaderFooterArea::Header {
            HEADER_RID
        } else {
            FOOTER_RID
        };
        let band = band_rects(&e);
        assert_eq!(
            band.len(),
            2,
            "{area:?}: its inline + floating picture {band:?}"
        );
        for r in &band {
            assert_eq!(r.story_area, Some(area));
            assert_eq!(r.story_rid, rid);
            assert!(r.story.is_empty(), "no text-box chain: {r:?}");
            assert_eq!((r.width_emu, r.height_emu), (914_400, 457_200), "{r:?}");
        }
        let inline = band
            .iter()
            .find(|r| !r.floating)
            .expect("the inline picture");
        assert_eq!(inline.path, BridgeBlockPath::top(0));
        assert_eq!(inline.at, INLINE_AT);
        let float = band
            .iter()
            .find(|r| r.floating)
            .expect("the floating picture");
        assert_eq!(float.path, BridgeBlockPath::top(1));
        assert_eq!(float.at, 0);
        /* 4 in from the page's left edge (test scale 2). */
        let inch = engine::emu_to_pt(914_400) * 2.0;
        assert!((float.rect.x - 4.0 * inch).abs() < 0.5, "{float:?}");
        /* In its band: the header's above the body picture, the footer's
        below it. */
        for r in &band {
            if area == HeaderFooterArea::Header {
                assert!(r.rect.y + r.rect.h <= body_y, "{r:?} above the body");
            } else {
                assert!(r.rect.y > body_y, "{r:?} below the body");
            }
        }
        /* The body picture is still listed (a double-click on it leaves
        the part — the shell's business), untagged. */
        assert_eq!(
            rects(&e).iter().filter(|r| r.story_area.is_none()).count(),
            1
        );
        apply(&mut e, Command::ExitHeaderFooter);
        assert!(
            band_rects(&e).is_empty(),
            "{area:?} left: its pictures are inert again"
        );
    }
}

/// Case 2 — resize / move / wrap address the part being edited, each one
/// undo step; the body picture at the same `(Block(1), 0)` address as the
/// header float is untouched.
#[test]
fn image_commands_with_an_empty_story_edit_the_part_being_edited() {
    let mut e = tests::test_engine_with_doc(doc());
    enter(&mut e, HeaderFooterArea::Header);
    let depth = undo_depth(&e);

    apply(
        &mut e,
        Command::ResizeImage {
            path: BridgeBlockPath::top(0),
            at: INLINE_AT,
            width_emu: 1_828_800,
            height_emu: 914_400,
            story: Vec::new(),
        },
    );
    assert_eq!(undo_depth(&e), depth + 1, "one undo step");
    assert_eq!(
        image_extent(&part_paragraph(&e, HeaderFooterArea::Header, 0), INLINE_AT),
        (1_828_800, 914_400)
    );
    let inline = band_rects(&e)
        .into_iter()
        .find(|r| !r.floating)
        .expect("listed");
    assert_eq!((inline.width_emu, inline.height_emu), (1_828_800, 914_400));
    assert!(
        (inline.rect.w - engine::emu_to_pt(1_828_800) * 2.0).abs() < 0.5,
        "re-laid out at the new size: {inline:?}"
    );

    /* The header float sits at `(Block(1), 0)` of the part — so does the
    body picture of the body. A move addresses the part's. */
    let before = band_rects(&e)
        .into_iter()
        .find(|r| r.floating)
        .expect("listed");
    apply(
        &mut e,
        Command::MoveImage {
            path: BridgeBlockPath::top(1),
            at: 0,
            offset_h_emu: 914_400,
            offset_v_emu: 228_600,
            story: Vec::new(),
        },
    );
    assert_eq!(undo_depth(&e), depth + 2);
    let after = band_rects(&e)
        .into_iter()
        .find(|r| r.floating)
        .expect("listed");
    assert!(
        after.rect.x < before.rect.x,
        "moved left: {before:?} -> {after:?}"
    );
    let body = rects(&e)
        .into_iter()
        .find(|r| r.story_area.is_none())
        .expect("the body picture");
    assert_eq!((body.width_emu, body.height_emu), (914_400, 457_200));
    assert!(!body.floating, "the body picture is still inline");

    apply(
        &mut e,
        Command::SetImageWrap {
            path: BridgeBlockPath::top(1),
            at: 0,
            wrap: bridge::ImageWrapMode::BehindText,
            story: Vec::new(),
        },
    );
    let wrapped = band_rects(&e)
        .into_iter()
        .find(|r| r.floating)
        .expect("listed");
    assert_eq!(wrapped.wrap, Some(bridge::ImageWrapMode::BehindText));

    /* Undo walks the part back, one command at a time. */
    apply(&mut e, Command::Undo);
    apply(&mut e, Command::Undo);
    apply(&mut e, Command::Undo);
    assert_eq!(
        image_extent(&part_paragraph(&e, HeaderFooterArea::Header, 0), INLINE_AT),
        (914_400, 457_200)
    );

    /* Honest refusals, nothing pushed: no picture there, or a text-box
    chain (a band's text boxes are not addressable). */
    let depth = undo_depth(&e);
    let missing = send(
        &mut e,
        Command::ResizeImage {
            path: BridgeBlockPath::top(0),
            at: 0,
            width_emu: 1,
            height_emu: 1,
            story: Vec::new(),
        },
    );
    assert!(
        matches!(
            missing,
            Event::Error {
                kind: Some(bridge::ErrorKind::NoSuchTarget),
                ..
            }
        ),
        "{missing:?}"
    );
    let chain = send(
        &mut e,
        Command::ResizeImage {
            path: BridgeBlockPath::top(0),
            at: INLINE_AT,
            width_emu: 1,
            height_emu: 1,
            story: vec![bridge::TextBoxHop {
                path: BridgeBlockPath::top(0),
                at: 0,
            }],
        },
    );
    assert!(matches!(chain, Event::Error { .. }), "{chain:?}");
    assert_eq!(undo_depth(&e), depth);
}

/// Case 3 — the shell's Delete for a story picture: select exactly the
/// sentinel in the story, delete the selection. One undo step; undo
/// brings the picture back.
#[test]
fn a_band_picture_is_deleted_through_its_sentinel_in_one_undo_step() {
    let mut e = tests::test_engine_with_doc(doc());
    enter(&mut e, HeaderFooterArea::Header);
    let at = BridgeLogicalPos {
        path: BridgeBlockPath::top(0),
        offset: INLINE_AT,
    };
    let end = BridgeLogicalPos {
        path: BridgeBlockPath::top(0),
        offset: INLINE_AT + 3,
    };
    match apply(
        &mut e,
        Command::SetSelection {
            range: BridgeLogicalRange {
                start: at.clone(),
                end: end.clone(),
            },
            caret: end,
        },
    ) {
        Event::SelectionChanged {
            range,
            editing_story: Some(s),
            ..
        } => {
            assert_eq!(s.area, HeaderFooterArea::Header);
            assert_eq!(
                (range.start.offset, range.end.offset),
                (INLINE_AT, INLINE_AT + 3)
            );
        }
        other => panic!("{other:?}"),
    }
    let depth = undo_depth(&e);
    apply(
        &mut e,
        Command::DeleteAtCaret {
            forward: false,
            by_word: false,
        },
    );
    assert_eq!(undo_depth(&e), depth + 1, "one undo step");
    let p = part_paragraph(&e, HeaderFooterArea::Header, 0);
    assert_eq!(p.text, "Header ");
    assert!(p.inline_objects.is_empty(), "{:?}", p.inline_objects);
    assert_eq!(band_rects(&e).len(), 1, "only the float is left");
    apply(&mut e, Command::Undo);
    assert_eq!(band_rects(&e).len(), 2, "undo brings it back");
}
