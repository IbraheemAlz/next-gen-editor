//! Issue #370 — an EMPTY paragraph's line is sized by its paragraph MARK
//! (`Paragraph::mark_style`, `<w:pPr><w:rPr>`, over the paragraph
//! style), not by the document default, through the production
//! `build_pages` (body, cache, cells) and the interactive Enter path.
//!
//! The test engine lays out at `line_height` 26 / `px_size` 16 (the
//! document default run: no `docDefaults` size); every layout here runs
//! at scale 1, so the nominal line is 26 pt.

use super::*;

const NOMINAL: f32 = 26.0;

fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    use std::task::{Context, Poll, Waker};
    let mut cx = Context::from_waker(Waker::noop());
    let mut fut = Box::pin(fut);
    match fut.as_mut().poll(&mut cx) {
        Poll::Ready(v) => v,
        Poll::Pending => panic!("Engine::apply suspended in a native test"),
    }
}

fn apply(e: &mut Engine, cmd: Command) {
    let evt = block_on(e.apply(cmd));
    assert!(!matches!(evt, Event::Error { .. }), "{evt:?}");
}

fn doc_of(paras: Vec<engine::Paragraph>) -> DocumentTree {
    DocumentTree::from_blocks(paras.into_iter().map(engine::Block::Paragraph))
}

fn para(text: &str) -> engine::Paragraph {
    engine::Paragraph {
        text: text.into(),
        ..Default::default()
    }
}

fn empty_with(mark: Option<SpanStyle>) -> engine::Paragraph {
    engine::Paragraph {
        mark_style: mark.map(Box::new),
        ..Default::default()
    }
}

fn sized(pt: f32) -> SpanStyle {
    SpanStyle {
        font_size: Some(pt),
        ..Default::default()
    }
}

/// `Above` / `p` / `Below`, laid out; returns the middle block's box.
fn middle_box(p: engine::Paragraph) -> ParagraphBox {
    let doc = doc_of(vec![para("Above"), p, para("Below")]);
    nth_box(&crate::tests::test_engine_with_doc(doc), 1)
}

fn nth_box(engine: &Engine, idx: usize) -> ParagraphBox {
    let (pages, _, _, _) = engine.build_pages(1.0, false, None).expect("layout");
    match &pages[0].blocks[idx] {
        LayoutBlock::Paragraph(p) => p.clone(),
        other => panic!("block {idx} is not a paragraph: {other:?}"),
    }
}

fn line(b: &ParagraphBox) -> (f32, f32) {
    assert_eq!(
        b.lines.len(),
        1,
        "an empty paragraph has one placeholder line"
    );
    (b.lines[0].baseline, b.lines[0].height)
}

#[test]
fn an_empty_paragraph_without_mark_properties_keeps_the_nominal_line() {
    let b = middle_box(empty_with(None));
    assert_eq!(line(&b), (NOMINAL, NOMINAL));
    assert_eq!(b.size.height.to_bits(), NOMINAL.to_bits());
    /* A mark that formats like the base (bold only, same size) too. */
    let bold = middle_box(empty_with(Some(SpanStyle {
        bold: Some(true),
        ..Default::default()
    })));
    assert_eq!(line(&bold), (NOMINAL, NOMINAL));
}

#[test]
fn a_small_mark_makes_a_thin_spacer_line() {
    let (baseline, height) = line(&middle_box(empty_with(Some(sized(4.0)))));
    /* 4 pt against the 16 pt base: a quarter of the nominal line. */
    assert!((height - NOMINAL / 4.0).abs() < 0.01, "{height}");
    assert_eq!(baseline, height);
}

#[test]
fn a_large_mark_grows_like_a_line_of_text_in_its_size() {
    let empty = middle_box(empty_with(Some(sized(48.0))));
    let (baseline, height) = line(&empty);
    assert!(height > NOMINAL * 1.5, "{height}");
    assert!(baseline < height, "{baseline} / {height}");
    /* The same paragraph holding one 48 pt character is exactly as tall:
    the mark never raises the floor past what text in its size gets. */
    let mut text = para("x");
    text.spans = vec![engine::StyleRun {
        start: 0,
        end: 1,
        style: sized(48.0),
    }];
    let text_box = middle_box(text);
    assert_eq!(height.to_bits(), text_box.lines[0].height.to_bits());
    assert_eq!(baseline.to_bits(), text_box.lines[0].baseline.to_bits());
}

#[test]
fn the_paragraph_style_size_is_the_marks_base() {
    let mut doc = doc_of(vec![para("Above"), para(""), para("Below")]);
    doc.styles.insert(
        "Small".into(),
        engine::ParagraphStyle {
            id: "Small".into(),
            name: "small".into(),
            run: sized(8.0),
            ..Default::default()
        },
    );
    if let Some(engine::Block::Paragraph(p)) = doc.blocks.get_mut(1) {
        p.style_id = Some("Small".into());
    }
    /* An 8 pt paragraph style with no mark properties: the mark IS the
    style, so its line shrinks with it (half the 16 pt default). */
    let (_, height) = line(&nth_box(&crate::tests::test_engine_with_doc(doc), 1));
    assert!((height - NOMINAL / 2.0).abs() < 0.01, "{height}");
}

#[test]
fn a_larger_paragraph_style_sizes_the_empty_line_like_its_text() {
    let styled = |text: &str| {
        let mut doc = doc_of(vec![para("Above"), para(text), para("Below")]);
        doc.styles.insert(
            "Big".into(),
            engine::ParagraphStyle {
                id: "Big".into(),
                name: "big".into(),
                run: sized(40.0),
                ..Default::default()
            },
        );
        if let Some(engine::Block::Paragraph(p)) = doc.blocks.get_mut(1) {
            p.style_id = Some("Big".into());
        }
        nth_box(&crate::tests::test_engine_with_doc(doc), 1)
    };
    let empty = styled("");
    let text = styled("x");
    assert!(line(&empty).1 > NOMINAL, "{:?}", line(&empty));
    assert_eq!(
        empty.lines[0].height.to_bits(),
        text.lines[0].height.to_bits()
    );
    assert_eq!(
        empty.lines[0].baseline.to_bits(),
        text.lines[0].baseline.to_bits()
    );
}

#[test]
fn exact_line_spacing_ignores_the_mark() {
    for mark in [sized(4.0), sized(48.0)] {
        let mut p = empty_with(Some(mark));
        p.props.line_height = Some(engine::LineHeight::Exact { twips: 240 });
        assert_eq!(line(&middle_box(p)), (12.0, 12.0));
    }
}

#[test]
fn at_least_spacing_keeps_its_floor() {
    let mut p = empty_with(Some(sized(4.0)));
    p.props.line_height = Some(engine::LineHeight::AtLeast { twips: 400 });
    assert_eq!(line(&middle_box(p)), (20.0, 20.0));
}

#[test]
fn auto_multiples_scale_the_marks_line() {
    let mut p = empty_with(Some(sized(4.0)));
    p.props.line_height = Some(engine::LineHeight::Auto { twips: 480 });
    let (_, height) = line(&middle_box(p));
    assert!((height - 2.0 * NOMINAL / 4.0).abs() < 0.01, "{height}");
}

#[test]
fn an_rtl_paragraphs_mark_uses_its_complex_script_size() {
    let mark = SpanStyle {
        font_size_cs: Some(4.0),
        ..Default::default()
    };
    let mut rtl = empty_with(Some(mark.clone()));
    rtl.props.direction = Some(engine::TextDirection::Rtl);
    let (_, height) = line(&middle_box(rtl));
    assert!((height - NOMINAL / 4.0).abs() < 0.01, "{height}");
    /* The same mark in an LTR paragraph: its Latin size is the default. */
    assert_eq!(
        line(&middle_box(empty_with(Some(mark)))),
        (NOMINAL, NOMINAL)
    );
}

#[test]
fn an_empty_cell_paragraph_is_sized_by_its_mark() {
    let mut doc = doc_of(vec![para("Above")]);
    let cell = |p: engine::Paragraph| engine::TableCell {
        blocks: vec![engine::Block::Paragraph(p)],
        ..Default::default()
    };
    doc.blocks.push_back(engine::Block::Table(engine::Table {
        grid: vec![2400, 2400],
        rows: vec![engine::TableRow {
            cells: vec![cell(empty_with(None)), cell(empty_with(Some(sized(4.0))))],
            ..Default::default()
        }],
        ..Default::default()
    }));
    let engine = crate::tests::test_engine_with_doc(doc);
    let (pages, _, _, _) = engine.build_pages(1.0, false, None).expect("layout");
    let LayoutBlock::Table(t) = &pages[0].blocks[1] else {
        panic!("block 1 is the table");
    };
    let heights: Vec<f32> = t.rows[0]
        .cells
        .iter()
        .map(|c| match &c.content[0] {
            LayoutBlock::Paragraph(p) => p.lines[0].height,
            other => panic!("cell content {other:?}"),
        })
        .collect();
    assert_eq!(heights[0], NOMINAL);
    assert!((heights[1] - NOMINAL / 4.0).abs() < 0.01, "{heights:?}");
}

#[test]
fn the_layout_cache_sees_a_mark_change() {
    let mut engine = crate::tests::test_engine_with_doc(doc_of(vec![
        para("Above"),
        empty_with(None),
        para("Below"),
    ]));
    assert_eq!(line(&nth_box(&engine, 1)), (NOMINAL, NOMINAL));
    /* Same text, same spans — only the mark differs: a key blind to it
    would serve the cached nominal box. */
    engine.undo.push(doc_of(vec![
        para("Above"),
        empty_with(Some(sized(4.0))),
        para("Below"),
    ]));
    let (_, height) = line(&nth_box(&engine, 1));
    assert!((height - NOMINAL / 4.0).abs() < 0.01, "{height}");
}

/// End to end: Enter at the end of a 4 pt run gives the new, empty
/// paragraph the run's formatting as its mark (#293), and its line the
/// mark's size (#370).
#[test]
fn enter_after_a_small_run_makes_a_thin_empty_line() {
    let mut first = para("tiny");
    first.spans = vec![engine::StyleRun {
        start: 0,
        end: 4,
        style: sized(4.0),
    }];
    let mut engine = crate::tests::test_engine_with_doc(doc_of(vec![first]));
    let at = bpos_top(0, 4);
    apply(
        &mut engine,
        Command::SetSelection {
            range: BridgeLogicalRange {
                start: at.clone(),
                end: at.clone(),
            },
            caret: at.clone(),
        },
    );
    apply(&mut engine, Command::SplitParagraph { at: Some(at) });
    let (_, height) = line(&nth_box(&engine, 1));
    assert!((height - NOMINAL / 4.0).abs() < 0.01, "{height}");
}
