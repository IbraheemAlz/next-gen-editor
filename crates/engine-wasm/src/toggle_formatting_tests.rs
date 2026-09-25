//! Issue #286 — `Command::ToggleFormatting` derives the target state
//! engine-side (the #276 typing rule + pending style, i.e. exactly what
//! `SelectionChanged.attrs_at_caret` reports), so a toggle posted before
//! the previous reply lands can never flip against a stale mirror.
//! Issue #296 — header/footer stories arm and apply pending formatting
//! through the same path as the body.

use super::*;
use bridge::{FormattingToggle, HeaderFooterArea};

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

fn select(e: &mut Engine, start: u32, end: u32) {
    apply(
        e,
        Command::SetSelection {
            range: BridgeLogicalRange {
                start: bpos_top(0, start),
                end: bpos_top(0, end),
            },
            caret: bpos_top(0, end),
        },
    );
}

fn toggle(e: &mut Engine, attr: FormattingToggle) -> Event {
    apply(
        e,
        Command::ToggleFormatting {
            attr,
            underline_style: None,
        },
    )
}

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

fn is_bold(e: &Engine, offset: u32) -> bool {
    style_at(e, offset).bold == Some(true)
}

fn toolbar(e: &Engine) -> TextAttrs {
    let Event::SelectionChanged { attrs_at_caret, .. } = e.selection_changed() else {
        panic!("expected SelectionChanged");
    };
    attrs_at_caret
}

fn pos0(offset: u32) -> engine::LogicalPos {
    engine::LogicalPos {
        path: engine::BlockPath::top(0),
        offset,
    }
}

fn bold_style() -> SpanStyle {
    SpanStyle {
        bold: Some(true),
        ..Default::default()
    }
}

/// The #286 repro, engine side: toggle, type, toggle, type — with no
/// reply read in between — yields a bold run then a plain run.
#[test]
fn toggle_type_toggle_type_gives_two_runs() {
    let mut e = engine_with(DocumentTree::from_text("x "));
    select(&mut e, 2, 2);
    toggle(&mut e, FormattingToggle::Bold);
    type_text(&mut e, "bold");
    toggle(&mut e, FormattingToggle::Bold);
    type_text(&mut e, " end");
    assert_eq!(e.undo.current().to_plain_text().trim_end(), "x bold end");
    for off in 2..6 {
        assert!(is_bold(&e, off), "byte {off} of 'bold' is bold");
    }
    for off in 6..10 {
        assert!(!is_bold(&e, off), "byte {off} of ' end' is plain");
    }
    assert!(!toolbar(&e).bold);
}

/// No pending style armed: the caret after a bold run inherits bold
/// (#276), so the toggle turns it OFF — the same value the toolbar reads.
#[test]
fn toggle_after_a_bold_run_turns_bold_off() {
    let d = DocumentTree::from_text("plain BOLD").apply_style(pos0(6), pos0(10), bold_style());
    let mut e = engine_with(d);
    select(&mut e, 10, 10);
    assert!(toolbar(&e).bold);
    toggle(&mut e, FormattingToggle::Bold);
    assert!(!toolbar(&e).bold, "pending bold-off previewed");
    type_text(&mut e, "x");
    assert!(!is_bold(&e, 10));
    assert!(is_bold(&e, 9), "the run itself is untouched");
}

/// A paragraph bold through its style cascade (Heading-like) toggles to
/// explicit bold-off.
#[test]
fn toggle_reads_the_style_cascade() {
    let mut d = DocumentTree::new();
    d.styles.insert(
        "Heading1".into(),
        engine::ParagraphStyle {
            id: "Heading1".into(),
            name: "heading 1".into(),
            run: bold_style(),
            ..Default::default()
        },
    );
    let mut h = engine::Paragraph {
        text: "Title".into(),
        style_id: Some("Heading1".into()),
        ..Default::default()
    };
    engine::recompute_paragraph_props(&mut h, &d.styles, &d.style_defaults);
    d.blocks = vec![engine::Block::Paragraph(h)].into_iter().collect();
    let mut e = engine_with(d);
    select(&mut e, 5, 5);
    assert!(toolbar(&e).bold);
    toggle(&mut e, FormattingToggle::Bold);
    assert!(!toolbar(&e).bold);
    type_text(&mut e, "x");
    assert_eq!(style_at(&e, 5).bold, Some(false), "explicit bold-off");
}

/// Ranges: all-bold → off; plain → on; mixed → on (Word).
#[test]
fn ranged_toggle_follows_words_rule() {
    let d = DocumentTree::from_text("plain BOLD").apply_style(pos0(6), pos0(10), bold_style());
    let mut e = engine_with(d.clone());
    select(&mut e, 6, 10);
    toggle(&mut e, FormattingToggle::Bold);
    assert!((6..10).all(|o| !is_bold(&e, o)), "all-bold range → off");

    let mut e = engine_with(d.clone());
    select(&mut e, 0, 5);
    toggle(&mut e, FormattingToggle::Bold);
    assert!((0..5).all(|o| is_bold(&e, o)), "plain range → on");

    let mut e = engine_with(d);
    select(&mut e, 3, 8);
    toggle(&mut e, FormattingToggle::Bold);
    assert!((3..8).all(|o| is_bold(&e, o)), "mixed range → on");
    assert!(!is_bold(&e, 2));
    assert!(e.selection.as_ref().is_some_and(|s| s.anchor != s.caret));
}

#[test]
fn underline_script_and_caps_toggles() {
    let mut e = engine_with(DocumentTree::from_text("abcdef"));
    select(&mut e, 0, 3);
    apply(
        &mut e,
        Command::ToggleFormatting {
            attr: FormattingToggle::Underline,
            underline_style: Some(UnderlineStyle::Double),
        },
    );
    assert_eq!(
        style_at(&e, 1).underline,
        Some(engine::UnderlineStyle::Double)
    );
    toggle(&mut e, FormattingToggle::Underline);
    assert_eq!(
        style_at(&e, 1).underline,
        Some(engine::UnderlineStyle::None)
    );

    toggle(&mut e, FormattingToggle::Superscript);
    assert_eq!(
        style_at(&e, 1).vert_align,
        Some(engine::VertAlign::Superscript)
    );
    toggle(&mut e, FormattingToggle::Subscript);
    assert_eq!(
        style_at(&e, 1).vert_align,
        Some(engine::VertAlign::Subscript)
    );
    toggle(&mut e, FormattingToggle::Subscript);
    assert_eq!(
        style_at(&e, 1).vert_align,
        Some(engine::VertAlign::Baseline)
    );

    toggle(&mut e, FormattingToggle::Caps);
    assert_eq!(style_at(&e, 1).caps, Some(true));
    toggle(&mut e, FormattingToggle::SmallCaps);
    assert_eq!(style_at(&e, 1).small_caps, Some(true));
    assert_eq!(style_at(&e, 1).caps, Some(false), "caps pair is exclusive");
    toggle(&mut e, FormattingToggle::Italic);
    toggle(&mut e, FormattingToggle::Strike);
    assert_eq!(style_at(&e, 1).italic, Some(true));
    assert_eq!(style_at(&e, 1).strike, Some(true));
}

#[test]
fn toggle_without_a_selection_is_a_typed_error() {
    let mut e = engine_with(DocumentTree::from_text("abc"));
    let evt = block_on(e.apply(Command::ToggleFormatting {
        attr: FormattingToggle::Bold,
        underline_style: None,
    }));
    assert!(matches!(evt, Event::Error { .. }), "{evt:?}");
}

fn header_para_style(e: &Engine, offset: u32) -> SpanStyle {
    let StoryTarget::Header { rid, .. } = &e.active_story else {
        panic!("expected a header story");
    };
    e.undo.current().headers[rid]
        .iter()
        .find_map(|b| b.as_paragraph())
        .expect("header paragraph")
        .style_at(offset)
}

/// #296 — Ctrl+B then typing inside a header produces a bold run (the
/// toolbar preview and the typed text agree), and the second toggle
/// ends it; leaving the story discards the armed style.
#[test]
fn header_story_toggle_arms_and_applies_pending_formatting() {
    let mut e = engine_with(DocumentTree::from_text("body text"));
    select(&mut e, 0, 0);
    apply(
        &mut e,
        Command::EnterHeaderFooter {
            page: 0,
            area: HeaderFooterArea::Header,
        },
    );
    assert!(e.story_active());
    toggle(&mut e, FormattingToggle::Bold);
    assert!(toolbar(&e).bold, "toolbar previews the armed bold");
    type_text(&mut e, "Hi");
    toggle(&mut e, FormattingToggle::Bold);
    type_text(&mut e, " there");
    assert_eq!(header_para_style(&e, 0).bold, Some(true));
    assert_eq!(header_para_style(&e, 1).bold, Some(true));
    assert_ne!(header_para_style(&e, 3).bold, Some(true));
    /* The body is untouched. */
    assert!((0..9).all(|o| !is_bold(&e, o)));

    toggle(&mut e, FormattingToggle::Italic);
    assert!(e.pending_format.is_some());
    apply(&mut e, Command::ExitHeaderFooter);
    assert!(e.pending_format.is_none(), "exit discards the armed style");
}
