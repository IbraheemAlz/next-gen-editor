//! Issue #501 item 3 - the viewport cull clamps a line taller than its page.
//!
//! The lazy band stops at the first block boundary where "finished pages +
//! the paginator's in-progress cursor" reaches the cull budget. A line
//! taller than its page (clipped by the oversize guard) leaves the cursor
//! at the line's full height, so a size no page can hold - the model still
//! can: a restored snapshot, an old logged `ApplyFormatting` from before
//! the font-size bound - put the running height past EVERY finite target:
//! the band stopped right behind that block for ever (#499). The cull now
//! counts at most the page (`Paginator::cursor_y_within_page`), so each
//! such block adds one page and a finite target keeps working.

use super::*;
use crate::tests::test_engine_with_doc;

/// `lines` one-line paragraphs; paragraph `tall` carries `size` pt on
/// every character (`size` is written straight into the tree - the
/// command path would refuse it).
fn doc_with_tall_paragraph(lines: usize, tall: usize, size: f32) -> DocumentTree {
    let mut d = DocumentTree::from_text("Line 1");
    for i in 2..=lines {
        d.blocks
            .push_back(engine::Block::Paragraph(engine::Paragraph {
                text: format!("Line {i}"),
                ..Default::default()
            }));
    }
    if let Some(p) = d.blocks.get_mut(tall).and_then(|b| b.as_paragraph_mut()) {
        let len = p.text.len() as u32;
        let style = engine::SpanStyle {
            font_size: Some(size),
            font_size_cs: Some(size),
            ..Default::default()
        };
        p.spans.push(engine::StyleRun {
            start: 0,
            end: len,
            style,
        });
    }
    d
}

fn small_band(engine: &mut Engine) {
    engine.lazy_layout = LazyLayoutState {
        viewport_y: 0.0,
        viewport_h: 100.0,
        min_target_y: INITIAL_COLD_OPEN_BUDGET_PT,
    };
    engine.invalidate_layout_snapshot();
}

/// (pages laid, is the layout full) after a build at `target_pt`.
fn band_at(engine: &mut Engine, target_pt: f32) -> (usize, bool) {
    let scale = engine.scale();
    engine
        .ensure_layout_snapshot(scale, false, Some(target_pt * scale))
        .map_err(|e| format!("{e:?}"))
        .expect("layout builds");
    let snap = engine.layout_snapshot.borrow();
    let s = snap.as_ref().expect("snapshot");
    (s.pages.len(), s.info.is_full_layout)
}

/// With a page-tall line at the top, a growing finite target grows the
/// band, and a large one reaches the whole document.
fn assert_band_grows_with_the_target(size: f32) {
    let mut engine = test_engine_with_doc(doc_with_tall_paragraph(80, 0, size));
    small_band(&mut engine);
    let (first, first_full) = band_at(&mut engine, 2_000.0);
    assert!(!first_full, "{size}: a 2000 pt target is a band");
    let (second, second_full) = band_at(&mut engine, 12_000.0);
    assert!(
        second > first || second_full,
        "{size}: the band must grow with the target ({first} -> {second} pages)"
    );
    let (_, full) = band_at(&mut engine, 1.0e6);
    assert!(full, "{size}: a target past the document lays it all out");
}

/// A legitimately page-tall line (Word's largest size, 1638 pt, on a
/// default page): the band follows the target.
#[test]
fn a_word_maximum_line_keeps_a_finite_target_working() {
    assert_band_grows_with_the_target(1638.0);
}

/// The #499 trigger as a restored tree can hold it: the band used to stop
/// behind block 0 whatever the target was.
#[test]
fn a_line_taller_than_any_target_no_longer_pins_the_band() {
    assert_band_grows_with_the_target(1.0951575e33);
}

/// The clamp is inert for a normal document: a cursor inside the page is
/// returned as is, so the band the cull stops at does not move.
#[test]
fn a_cursor_inside_the_page_is_counted_unclamped() {
    let mut engine = test_engine_with_doc(doc_with_tall_paragraph(300, usize::MAX, 11.0));
    small_band(&mut engine);
    let (a, a_full) = band_at(&mut engine, 2_000.0);
    let (b, b_full) = band_at(&mut engine, 20_000.0);
    assert!(!a_full && a >= 1);
    assert!(b > a || b_full, "a plain document grows with its target");
}

fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    use std::task::{Context, Poll, Waker};
    let mut cx = Context::from_waker(Waker::noop());
    let mut fut = Box::pin(fut);
    match fut.as_mut().poll(&mut cx) {
        Poll::Ready(v) => v,
        Poll::Pending => panic!("Engine::apply suspended in a native test"),
    }
}

/// What an `ExpandLayout` answered: (pages, full layout, the degradation
/// reasons it carried).
fn expand(engine: &mut Engine, target_y: f32) -> (u32, bool, Vec<bridge::LayoutDegradeReason>) {
    match block_on(engine.apply(Command::ExpandLayout { target_y })) {
        Event::Painted {
            page_count,
            is_full_layout,
            layout_degraded,
            ..
        } => (
            page_count,
            is_full_layout,
            layout_degraded.iter().map(|d| d.reason).collect(),
        ),
        other => panic!("ExpandLayout answers Painted, got {other:?}"),
    }
}

/// Item 2, the nominal path: a scroll-driven `ExpandLayout` over a document
/// holding a page-tall line grows the band with the target and completes
/// without a stall note - the structural clamp already delivered it.
#[test]
fn expand_layout_over_a_page_tall_line_grows_the_band_without_a_note() {
    for size in [1638.0, 1.0951575e33] {
        let mut engine = test_engine_with_doc(doc_with_tall_paragraph(80, 0, size));
        small_band(&mut engine);
        let (first, _, _) = expand(&mut engine, 2_000.0);
        let (second, second_full, notes) = expand(&mut engine, 12_000.0);
        assert!(second > first || second_full, "{size}: {first} -> {second}");
        assert!(
            !notes.contains(&bridge::LayoutDegradeReason::FastPathMismatch),
            "{size}: {notes:?}"
        );
        let (_, full, _) = expand(&mut engine, 1.0e6);
        assert!(full, "{size}: an ExpandLayout past the end lays it all out");
    }
}
