//! Issue #505 — entering a header / footer parks the caret on the part's
//! first line INSIDE the band of the entered page, even when the part is
//! empty (a fresh part materialised by the entry, or an existing part that
//! holds only an empty paragraph).
//!
//! An empty part paints no band, so the story geometry used to have no
//! lines and `selection_changed` fell back to the BODY margin of page 0:
//! the caret overlay sat on the first body line, outside the band, until
//! the first keystroke built one (live repro, 2026-10-23). The entry also
//! reveals the caret now, so the toolbar's Footer button scrolls an
//! off-screen footer into view.
//!
//! Gated on `fuzz-native` like the other bridge-level tests that drive real
//! `Command`s through `Engine::apply_sync`.

use super::*;
use bridge::HeaderFooterArea;

/// Page `idx`'s absolute top, height and top / bottom margins (device px)
/// from the layout the caret geometry walked.
fn page_frame(e: &Engine, idx: usize) -> (f32, f32, f32, f32) {
    let snap = e.layout_snapshot.borrow();
    let pages = &snap.as_ref().expect("a laid-out snapshot").pages;
    assert!(
        pages.len() > idx,
        "page {idx} is not laid out ({} pages)",
        pages.len()
    );
    let gap = render::scene::PAGE_GAP_PT * e.scale();
    let top: f32 = pages[..idx].iter().map(|p| p.size.height + gap).sum();
    let p = &pages[idx];
    (top, p.size.height, p.margins.top, p.margins.bottom)
}

/// The `SelectionChanged` an entry answers: its caret rect and whether it
/// asks the shell to reveal the caret. The caret must come from the
/// story's own first LINE (the primary fix), not from the story-aware
/// fallback rect (the second line of defence, tested on its own below).
fn enter(e: &mut Engine, page: u32, area: HeaderFooterArea) -> (BridgeRect, bool) {
    match e.apply_sync(Command::EnterHeaderFooter { page, area }) {
        Event::SelectionChanged {
            caret,
            reveal_caret,
            editing_story,
            ..
        } => {
            let story = editing_story.expect("the entry names the active story");
            assert_eq!(story.area, area);
            assert_eq!(story.page, page);
            let geom = e.document_geometry().expect("story geometry");
            let first = geom.first().unwrap_or_else(|| {
                panic!("{area:?} on page {page}: the story has no laid-out line")
            });
            assert_eq!(first.path, BridgeBlockPath::top(0));
            assert!(
                (caret.y - first.y_top).abs() < 0.01 && (caret.h - first.height).abs() < 0.01,
                "caret {caret:?} is not on the story's first line (y {}, h {})",
                first.y_top,
                first.height
            );
            (caret, reveal_caret)
        }
        other => panic!("EnterHeaderFooter answered {other:?}"),
    }
}

/// `caret` lies inside the header band (page top .. body top) of the page
/// whose frame is `frame`.
fn assert_in_header_band(caret: &BridgeRect, frame: (f32, f32, f32, f32)) {
    let (top, _h, margin_top, _mb) = frame;
    assert!(caret.h > 0.0, "a caret with a height: {caret:?}");
    assert!(
        caret.y >= top && caret.y + caret.h <= top + margin_top,
        "caret {caret:?} is not inside the header band {top}..{}",
        top + margin_top
    );
}

/// `caret` lies inside the footer band (body bottom .. page bottom).
fn assert_in_footer_band(caret: &BridgeRect, frame: (f32, f32, f32, f32)) {
    let (top, h, _mt, margin_bottom) = frame;
    assert!(caret.h > 0.0, "a caret with a height: {caret:?}");
    assert!(
        caret.y >= top + h - margin_bottom && caret.y + caret.h <= top + h,
        "caret {caret:?} is not inside the footer band {}..{}",
        top + h - margin_bottom,
        top + h
    );
}

/// Two pages (a page break between them), no header or footer part.
fn two_page_engine() -> Engine {
    Engine::new_headless(DocumentTree::from_text("page one\u{c}page two"))
}

#[test]
fn entering_an_absent_header_puts_the_caret_inside_the_entered_pages_band() {
    for page in [0u32, 1] {
        let mut e = two_page_engine();
        assert!(e.undo.current().headers.is_empty());
        let (caret, reveal) = enter(&mut e, page, HeaderFooterArea::Header);
        assert_in_header_band(&caret, page_frame(&e, page as usize));
        assert!(reveal, "the entry reveals its caret");
        /* The part was materialised (one empty paragraph) and the caret
        sits at its start. */
        let sel = e.selection.clone().expect("a selection");
        assert_eq!(sel.caret, bpos_top(0, 0));
        assert_eq!(e.undo.current().headers.len(), 1);
    }
}

#[test]
fn entering_an_absent_footer_puts_the_caret_inside_the_entered_pages_band() {
    for page in [0u32, 1] {
        let mut e = two_page_engine();
        let (caret, reveal) = enter(&mut e, page, HeaderFooterArea::Footer);
        assert_in_footer_band(&caret, page_frame(&e, page as usize));
        assert!(reveal, "the entry reveals its caret");
        assert_eq!(e.undo.current().footers.len(), 1);
    }
}

/// The second entry finds the part the first one materialised: it exists
/// but is still empty, so no entry path builds a band for it.
#[test]
fn re_entering_an_existing_empty_part_keeps_the_caret_in_the_band() {
    let mut e = two_page_engine();
    for area in [HeaderFooterArea::Header, HeaderFooterArea::Footer] {
        enter(&mut e, 0, area);
        let evt = e.apply_sync(Command::ExitHeaderFooter);
        assert!(matches!(evt, Event::SelectionChanged { .. }), "{evt:?}");
    }
    let parts = (
        e.undo.current().headers.len(),
        e.undo.current().footers.len(),
    );
    let (header_caret, _) = enter(&mut e, 1, HeaderFooterArea::Header);
    assert_in_header_band(&header_caret, page_frame(&e, 1));
    e.apply_sync(Command::ExitHeaderFooter);
    let (footer_caret, _) = enter(&mut e, 1, HeaderFooterArea::Footer);
    assert_in_footer_band(&footer_caret, page_frame(&e, 1));
    let now = (
        e.undo.current().headers.len(),
        e.undo.current().footers.len(),
    );
    assert_eq!(now, parts, "re-entry materialises nothing new");
}

/// The empty part's caret is where its first typed character lands: the
/// line laid out for the empty part and the painted band agree (same
/// builder, same bottom anchoring for the footer).
#[test]
fn the_empty_parts_caret_line_is_the_line_the_first_character_paints_on() {
    for area in [HeaderFooterArea::Header, HeaderFooterArea::Footer] {
        let mut e = two_page_engine();
        let (empty, _) = enter(&mut e, 0, area);
        let typed = match e.apply_sync(Command::InsertText {
            at: None,
            text: "F".to_string(),
        }) {
            Event::SelectionChanged { caret, .. } => caret,
            other => panic!("InsertText answered {other:?}"),
        };
        assert!(
            (typed.y - empty.y).abs() < 0.5 && (typed.h - empty.h).abs() < 0.5,
            "{area:?}: empty caret {empty:?} vs after typing {typed:?}"
        );
        /* x is the paragraph's start edge, which follows its resolved
        direction (an empty paragraph takes the configured default, a
        typed Latin letter makes it LTR) — only the box is pinned. */
        let snap = e.layout_snapshot.borrow();
        let page = &snap.as_ref().expect("laid out").pages[0];
        for caret in [&empty, &typed] {
            assert!(
                caret.x >= page.margins.left - 0.5
                    && caret.x <= page.size.width - page.margins.right + 0.5,
                "{area:?}: caret {caret:?} outside the content box"
            );
        }
    }
}

/// Second line of defence: with no laid-out line at all for the story
/// (the part vanished), the fallback rect is still in the band of the
/// story's page, never the body margin of page 0.
#[test]
fn the_story_caret_fallback_is_the_band_of_the_anchor_page() {
    for area in [HeaderFooterArea::Header, HeaderFooterArea::Footer] {
        let mut e = two_page_engine();
        enter(&mut e, 1, area);
        let frame = page_frame(&e, 1);
        let fallback = e.caret_fallback_rect();
        match area {
            HeaderFooterArea::Header => assert_in_header_band(&fallback, frame),
            _ => assert_in_footer_band(&fallback, frame),
        }
    }
}
