//! Issues #277 / #276 — paragraph-style and character-formatting
//! continuity across the interactive edit commands (Enter, typing),
//! driven end to end through `Engine::apply`.

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

fn engine_with(doc: DocumentTree) -> Engine {
    let mut e = assemble_engine(None, None);
    let bytes = include_bytes!("../../../ts/fonts/LiberationSans-Regular.ttf").to_vec();
    let font = LoadedFont::parse("test-latin".to_string(), bytes).expect("parse test font");
    e.fonts.insert("test-latin".to_string(), Arc::new(font));
    e.layout_cfg = Some(RenderConfig {
        font_id: "test-latin".to_string(),
        base_direction: ShapingDirection::Ltr,
        px_size: 16.0,
        line_height: 26.0,
        alignment: Alignment::Start,
        scale: 1.0,
        base_scale: 1.0,
        zoom: 1.0,
    });
    e.undo = UndoStack::new(doc, 100);
    e
}

fn caret(e: &mut Engine, para: u32, offset: u32) {
    apply(
        e,
        Command::SetSelection {
            range: BridgeLogicalRange {
                start: bpos_top(para, offset),
                end: bpos_top(para, offset),
            },
            caret: bpos_top(para, offset),
        },
    );
}

/// A Heading1 paragraph (bold via the style) followed by body text.
fn heading_doc() -> DocumentTree {
    let mut d = DocumentTree::new();
    d.styles.insert(
        "Heading1".into(),
        engine::ParagraphStyle {
            id: "Heading1".into(),
            name: "heading 1".into(),
            para: engine::ParaProperties {
                keep_next: Some(true),
                ..Default::default()
            },
            run: SpanStyle {
                bold: Some(true),
                font_size: Some(24.0),
                ..Default::default()
            },
            ..Default::default()
        },
    );
    let mut h = engine::Paragraph {
        text: "Chapter One".into(),
        style_id: Some("Heading1".into()),
        ..Default::default()
    };
    engine::recompute_paragraph_props(&mut h, &d.styles, &d.style_defaults);
    let body = engine::Paragraph {
        text: "Body".into(),
        ..Default::default()
    };
    d.blocks = vec![engine::Block::Paragraph(h), engine::Block::Paragraph(body)]
        .into_iter()
        .collect();
    d
}

fn style_of(e: &Engine, idx: u32) -> Option<String> {
    e.undo
        .current()
        .nth_paragraph(idx)
        .and_then(|p| p.style_id.clone())
}

/// #277 — Enter in the middle of a heading keeps Heading1 on both halves.
#[test]
fn enter_mid_heading_keeps_the_style_on_both_halves() {
    let mut e = engine_with(heading_doc());
    caret(&mut e, 0, 7);
    apply(&mut e, Command::SplitParagraph { at: None });
    let doc = e.undo.current();
    assert_eq!(doc.nth_paragraph(0).unwrap().text, "Chapter");
    assert_eq!(doc.nth_paragraph(1).unwrap().text, " One");
    assert_eq!(style_of(&e, 0).as_deref(), Some("Heading1"), "left half");
    assert_eq!(style_of(&e, 1).as_deref(), Some("Heading1"), "right half");
    /* The resolved cascade still paints the right half as a heading. */
    assert_eq!(
        e.undo.current().nth_paragraph(1).unwrap().props.keep_next,
        Some(true)
    );
}

fn with_next(mut d: DocumentTree, next: Option<&str>) -> DocumentTree {
    d.styles.insert(
        "Normal".into(),
        engine::ParagraphStyle {
            id: "Normal".into(),
            name: "Normal".into(),
            ..Default::default()
        },
    );
    if let Some(h) = d.styles.get_mut("Heading1") {
        h.next = next.map(str::to_owned);
    }
    d
}

fn toolbar_bold(e: &Engine) -> bool {
    let Event::SelectionChanged { attrs_at_caret, .. } = e.selection_changed() else {
        panic!("expected SelectionChanged");
    };
    attrs_at_caret.bold
}

/// #277 — Enter at the very end of a heading gives the NEW paragraph
/// the style's `<w:next>` (Heading1 → Normal); the heading keeps its
/// style, and the new paragraph's resolved props drop the heading's
/// style-level `keep_next`.
#[test]
fn enter_at_heading_end_applies_the_next_style() {
    let mut e = engine_with(with_next(heading_doc(), Some("Normal")));
    caret(&mut e, 0, 11);
    apply(&mut e, Command::SplitParagraph { at: None });
    let doc = e.undo.current();
    assert_eq!(doc.nth_paragraph(0).unwrap().text, "Chapter One");
    assert_eq!(doc.nth_paragraph(1).unwrap().text, "");
    assert_eq!(style_of(&e, 0).as_deref(), Some("Heading1"));
    assert_eq!(style_of(&e, 1).as_deref(), Some("Normal"), "next style");
    assert_eq!(doc.nth_paragraph(0).unwrap().props.keep_next, Some(true));
    assert_eq!(doc.nth_paragraph(1).unwrap().props.keep_next, None);
    /* The toolbar reads the new paragraph's cascade: Normal is not bold. */
    assert!(!toolbar_bold(&e));

    /* Mid-heading still keeps Heading1 on both halves, even with a next. */
    let mut e = engine_with(with_next(heading_doc(), Some("Normal")));
    caret(&mut e, 0, 3);
    apply(&mut e, Command::SplitParagraph { at: None });
    assert_eq!(style_of(&e, 0).as_deref(), Some("Heading1"));
    assert_eq!(style_of(&e, 1).as_deref(), Some("Heading1"));
}

/// #277 — no `<w:next>` (or one naming an undefined style) ⇒ the new
/// paragraph keeps the same style.
#[test]
fn enter_at_heading_end_without_next_keeps_the_style() {
    for next in [None, Some("NoSuchStyle")] {
        let mut e = engine_with(with_next(heading_doc(), next));
        caret(&mut e, 0, 11);
        apply(&mut e, Command::SplitParagraph { at: None });
        assert_eq!(style_of(&e, 0).as_deref(), Some("Heading1"), "{next:?}");
        assert_eq!(style_of(&e, 1).as_deref(), Some("Heading1"), "{next:?}");
        assert!(toolbar_bold(&e), "still a heading {next:?}");
    }
}

/// #277 — Backspace inside a heading (`delete_text`) keeps the style;
/// it used to clear it exactly like `split_at`.
#[test]
fn backspace_inside_heading_keeps_the_style() {
    let mut e = engine_with(heading_doc());
    caret(&mut e, 0, 5);
    apply(
        &mut e,
        Command::DeleteAtCaret {
            forward: false,
            by_word: false,
        },
    );
    assert_eq!(
        e.undo.current().nth_paragraph(0).unwrap().text,
        "Chapter One".replacen('t', "", 1)
    );
    assert_eq!(style_of(&e, 0).as_deref(), Some("Heading1"));
}

/* ---- Issue #276 — typing continues the formatting before the caret ---- */

fn pos0(offset: u32) -> engine::LogicalPos {
    engine::LogicalPos {
        path: engine::BlockPath::top(0),
        offset,
    }
}

/// "plain BOLD italic": bytes 6..10 bold, 11..17 italic.
fn formatted_doc() -> DocumentTree {
    let d = DocumentTree::from_text("plain BOLD italic");
    let d = d.apply_style(
        pos0(6),
        pos0(10),
        SpanStyle {
            bold: Some(true),
            ..Default::default()
        },
    );
    d.apply_style(
        pos0(11),
        pos0(17),
        SpanStyle {
            italic: Some(true),
            ..Default::default()
        },
    )
}

/// Interactive typing at the live caret (the `at` only seeds a missing
/// selection).
fn type_text(e: &mut Engine, text: &str) {
    let at = e.selection.as_ref().map(|s| s.caret.clone());
    apply(
        e,
        Command::InsertText {
            at,
            text: text.into(),
        },
    );
}

fn style_at(e: &Engine, offset: u32) -> SpanStyle {
    e.undo.current().nth_paragraph(0).unwrap().style_at(offset)
}

fn toolbar(e: &Engine) -> TextAttrs {
    let Event::SelectionChanged { attrs_at_caret, .. } = e.selection_changed() else {
        panic!("expected SelectionChanged");
    };
    attrs_at_caret
}

#[test]
fn typing_after_a_bold_run_is_bold() {
    let mut e = engine_with(formatted_doc());
    caret(&mut e, 0, 10);
    /* Engine and toolbar agree BEFORE the keystroke … */
    assert!(toolbar(&e).bold, "toolbar previews the inherited bold");
    type_text(&mut e, "ER");
    let p = e.undo.current().nth_paragraph(0).unwrap().clone();
    assert_eq!(p.text, "plain BOLDER italic");
    assert_eq!(style_at(&e, 10).bold, Some(true));
    assert_eq!(style_at(&e, 11).bold, Some(true));
    /* … one span, not a bold run followed by a plain one … */
    assert_eq!((p.spans[0].start, p.spans[0].end), (6, 12));
    /* … and after it. */
    assert!(toolbar(&e).bold);
    /* The italic span slid right untouched; the space stays plain. */
    assert_eq!(style_at(&e, 13).italic, Some(true));
    assert_eq!(style_at(&e, 12), SpanStyle::default());
}

#[test]
fn typing_after_plain_text_before_a_bold_run_stays_plain() {
    let mut e = engine_with(formatted_doc());
    caret(&mut e, 0, 6);
    assert!(!toolbar(&e).bold);
    type_text(&mut e, "x");
    assert_eq!(style_at(&e, 6), SpanStyle::default());
    assert_eq!(style_at(&e, 7).bold, Some(true));
}

#[test]
fn typing_at_paragraph_start_before_italic_text_is_italic() {
    let d = DocumentTree::from_text("italic body").apply_style(
        pos0(0),
        pos0(6),
        SpanStyle {
            italic: Some(true),
            ..Default::default()
        },
    );
    let mut e = engine_with(d);
    caret(&mut e, 0, 0);
    assert!(
        toolbar(&e).italic,
        "toolbar previews the char after the caret"
    );
    type_text(&mut e, "An ");
    assert_eq!(
        e.undo.current().nth_paragraph(0).unwrap().text,
        "An italic body"
    );
    assert_eq!(style_at(&e, 0).italic, Some(true));
    assert_eq!(style_at(&e, 8).italic, Some(true));
    assert_eq!(style_at(&e, 9), SpanStyle::default());
}

#[test]
fn pending_bold_off_then_typing_is_plain() {
    let mut e = engine_with(formatted_doc());
    caret(&mut e, 0, 10);
    apply(
        &mut e,
        Command::ApplyFormatting {
            range: None,
            attrs: TextAttrsPatch {
                bold: Some(false),
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
            },
        },
    );
    assert!(!toolbar(&e).bold, "toolbar previews the pending bold-off");
    type_text(&mut e, "x");
    assert_ne!(style_at(&e, 10).bold, Some(true), "pending wins");
    assert_eq!(
        style_at(&e, 9).bold,
        Some(true),
        "the run itself is untouched"
    );
    assert!(!toolbar(&e).bold);
}

/// Typing over a selection takes the FIRST replaced character's
/// formatting (what the toolbar shows for the range), not the character
/// before the selection; `ReplaceRange` (find/replace) follows suit.
#[test]
fn typing_over_a_selection_takes_the_first_replaced_characters_style() {
    let mut e = engine_with(formatted_doc());
    apply(
        &mut e,
        Command::SetSelection {
            range: BridgeLogicalRange {
                start: bpos_top(0, 6),
                end: bpos_top(0, 10),
            },
            caret: bpos_top(0, 10),
        },
    );
    assert!(toolbar(&e).bold, "range toolbar reads the selection start");
    type_text(&mut e, "HEAVY");
    assert_eq!(
        e.undo.current().nth_paragraph(0).unwrap().text,
        "plain HEAVY italic"
    );
    assert_eq!(style_at(&e, 6).bold, Some(true));
    assert_eq!(style_at(&e, 10).bold, Some(true));
    assert_eq!(style_at(&e, 5), SpanStyle::default());

    /* Replacing the plain space right after the bold run stays plain:
    the first replaced char is plain even though the one before is bold. */
    let mut e = engine_with(formatted_doc());
    apply(
        &mut e,
        Command::ReplaceRange {
            range: BridgeLogicalRange {
                start: bpos_top(0, 10),
                end: bpos_top(0, 11),
            },
            text: "_".into(),
        },
    );
    assert_eq!(style_at(&e, 10), SpanStyle::default(), "find/replace");
    assert_eq!(style_at(&e, 9).bold, Some(true));
}
