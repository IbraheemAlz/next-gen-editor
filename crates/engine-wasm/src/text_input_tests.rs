//! Issue #467 — control characters and noncharacters at the input
//! boundary: every command that brings text into the model passes through
//! `text_input_guard` (one table, `engine::text_input`), so a pasted
//! `\u{1}` can no longer reach `<w:t>` and make the save malformed.
//!
//! A lone UTF-16 surrogate cannot be tested here: a Rust `String` cannot
//! hold one (the wasm-bindgen string glue turns it into `U+FFFD` before the
//! engine sees it), so the shell refuses it before dispatch — see
//! `ts/src/engine/text-guard.test.ts`.

use super::*;
use bridge::HeaderFooterArea;
use std::io::Read as _;

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
    block_on(e.apply(cmd))
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
    e.undo = UndoStack::new(doc, UNDO_CAP);
    e.selection = Some(SelectionState {
        anchor: bpos_top(0, 0),
        caret: bpos_top(0, 0),
        ideal_x: None,
        kind: SelectionKind::Linear,
    });
    e.review_date = "2026-01-01T00:00:00Z".into();
    e
}

fn insert(e: &mut Engine, text: &str) -> Event {
    apply(
        e,
        Command::InsertText {
            at: None,
            text: text.to_string(),
        },
    )
}

fn body_text(e: &Engine) -> String {
    e.undo.current().paragraph_text(0).unwrap_or("").to_string()
}

/// The saved package's `word/document.xml`, after the strict well-formed
/// check every regenerated part passes.
fn saved_document_xml(e: &mut Engine) -> String {
    let Event::DocumentSaved { bytes, .. } = apply(e, Command::SaveDocx) else {
        panic!("SaveDocx must succeed");
    };
    format_docx::check_part_xml_well_formed(&bytes, "word/document.xml")
        .expect("saved document.xml is well-formed");
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).expect("zip");
    let mut xml = String::new();
    zip.by_name("word/document.xml")
        .expect("main part")
        .read_to_string(&mut xml)
        .expect("utf-8");
    xml
}

fn refusal(evt: &Event) -> Option<&str> {
    match evt {
        Event::Error {
            message,
            kind: Some(bridge::ErrorKind::InvalidArgument),
            ..
        } => Some(message),
        _ => None,
    }
}

#[test]
fn a_c0_control_is_dropped_from_typed_text() {
    let mut e = engine_with(DocumentTree::from_text(""));
    let evt = insert(&mut e, "a\u{1}b");
    assert!(matches!(evt, Event::SelectionChanged { .. }), "{evt:?}");
    assert_eq!(body_text(&e), "ab");
    /* The caret sits after the CLEANED text, not after the raw length. */
    let Some(sel) = &e.selection else {
        panic!("selection")
    };
    assert_eq!(sel.caret.offset, 2);
}

#[test]
fn a_control_only_insert_inserts_nothing() {
    let mut e = engine_with(DocumentTree::from_text("seed"));
    insert(&mut e, "\u{1}\u{0}\u{1F}");
    assert_eq!(body_text(&e), "seed");
}

#[test]
fn tab_lf_and_cr_survive() {
    let mut e = engine_with(DocumentTree::from_text(""));
    insert(&mut e, "a\tb");
    assert_eq!(body_text(&e), "a\tb");
}

#[test]
fn a_form_feed_is_a_page_break_and_saves_as_one() {
    let mut e = engine_with(DocumentTree::from_text("x"));
    insert(&mut e, "a\u{C}b");
    assert_eq!(body_text(&e), "a\u{C}bx");
    let xml = saved_document_xml(&mut e);
    assert!(xml.contains(r#"<w:br w:type="page"/>"#), "{xml}");
    assert!(!xml.contains('\u{C}'));
}

#[test]
fn a_vertical_tab_is_a_line_break_and_saves_as_one() {
    let mut e = engine_with(DocumentTree::from_text("x"));
    insert(&mut e, "a\u{B}b");
    assert_eq!(body_text(&e), "a\u{2028}bx");
    let xml = saved_document_xml(&mut e);
    assert!(xml.contains("<w:br/>"), "{xml}");
    assert!(!xml.contains('\u{B}'));
}

#[test]
fn a_noncharacter_is_refused_and_nothing_changes() {
    for bad in ["\u{FFFE}", "ok\u{FFFF}ok"] {
        let mut e = engine_with(DocumentTree::from_text("seed"));
        let depth = e.undo.depth();
        let evt = insert(&mut e, bad);
        let msg = refusal(&evt)
            .unwrap_or_else(|| panic!("{bad:?}: expected InvalidArgument, got {evt:?}"));
        assert!(msg.starts_with("InsertText: text: "), "{msg}");
        assert!(msg.contains("U+FFF"), "{msg}");
        assert!(
            matches!(&evt, Event::Error { command: Some(c), .. } if c == "INSERT_TEXT"),
            "the reply names its command: {evt:?}"
        );
        assert_eq!(body_text(&e), "seed");
        assert_eq!(e.undo.depth(), depth, "no undo step for a refusal");
    }
}

#[test]
fn paste_plain_replace_range_and_composition_use_the_same_table() {
    let mut e = engine_with(DocumentTree::from_text("seed"));
    apply(
        &mut e,
        Command::PastePlain {
            text: "p\u{2}q\u{B}r".into(),
        },
    );
    assert_eq!(body_text(&e), "pq\u{2028}rseed");

    let mut e = engine_with(DocumentTree::from_text("seed"));
    let evt = apply(
        &mut e,
        Command::ReplaceRange {
            range: BridgeLogicalRange {
                start: bpos_top(0, 0),
                end: bpos_top(0, 4),
            },
            text: "n\u{3}ew".into(),
        },
    );
    assert!(matches!(evt, Event::SelectionChanged { .. }), "{evt:?}");
    assert_eq!(body_text(&e), "new");

    /* IME commit: the preview text and the committed text agree. */
    let mut e = engine_with(DocumentTree::from_text("seed"));
    apply(&mut e, Command::BeginComposition { at: None });
    let evt = apply(
        &mut e,
        Command::UpdateComposition {
            text: "ni\u{1}hao".into(),
            target_range: None,
        },
    );
    assert!(
        matches!(&evt, Event::CompositionUpdated { text, .. } if text == "nihao"),
        "{evt:?}"
    );
    apply(&mut e, Command::EndComposition { commit: true });
    assert_eq!(body_text(&e), "nihaoseed");

    /* Refusals leave every one of them untouched. */
    for cmd in [
        Command::PastePlain {
            text: "\u{FFFF}".into(),
        },
        Command::UpdateComposition {
            text: "\u{FFFE}".into(),
            target_range: None,
        },
    ] {
        let mut e = engine_with(DocumentTree::from_text("seed"));
        let evt = apply(&mut e, cmd);
        assert!(refusal(&evt).is_some(), "{evt:?}");
        assert_eq!(body_text(&e), "seed");
    }
}

#[test]
fn rich_paste_repairs_entities_and_control_characters() {
    let mut e = engine_with(DocumentTree::from_text(""));
    let evt = apply(
        &mut e,
        Command::PasteHtml {
            html: "<p>a&#1;b&#xFFFE;c\u{2}d</p>".into(),
        },
    );
    assert!(matches!(evt, Event::SelectionChanged { .. }), "{evt:?}");
    assert_eq!(body_text(&e), "ab\u{FFFD}cd");
    saved_document_xml(&mut e);
}

/// Markup attributes (a link target, a title) reach the package too; a
/// control character spelled in one must not corrupt any part.
#[test]
fn a_rich_paste_with_control_characters_in_attributes_still_saves() {
    let mut e = engine_with(DocumentTree::from_text(""));
    let evt = apply(
        &mut e,
        Command::PasteHtml {
            html: "<p><a href=\"http://example.com/a&#1;b\" title=\"t&#2;\">link</a> \
                   <span style=\"font-family:Ar&#3;ial\">x</span></p>"
                .into(),
        },
    );
    assert!(matches!(evt, Event::SelectionChanged { .. }), "{evt:?}");
    let Event::DocumentSaved { bytes, .. } = apply(&mut e, Command::SaveDocx) else {
        panic!("SaveDocx must succeed");
    };
    assert_eq!(format_docx::first_forbidden_character(&bytes), None);
    saved_document_xml(&mut e);
}

#[test]
fn a_header_story_gets_the_same_cleaning() {
    let mut e = engine_with(DocumentTree::from_text("body"));
    let evt = apply(
        &mut e,
        Command::EnterHeaderFooter {
            page: 0,
            area: HeaderFooterArea::Header,
        },
    );
    assert!(!matches!(evt, Event::Error { .. }), "{evt:?}");
    insert(&mut e, "h\u{1}i");
    let headers: Vec<String> = e
        .undo
        .current()
        .headers
        .values()
        .flat_map(|blocks| blocks.iter())
        .filter_map(|b| match b {
            engine::Block::Paragraph(p) => Some(p.text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(headers, vec!["hi".to_string()]);
    apply(&mut e, Command::ExitHeaderFooter);
    saved_document_xml(&mut e);
}

#[test]
fn tracked_insertion_is_cleaned_too() {
    let mut e = engine_with(DocumentTree::from_text(""));
    apply(&mut e, Command::ToggleTrackChanges { enabled: true });
    insert(&mut e, "t\u{1}r");
    assert_eq!(body_text(&e), "tr");
    saved_document_xml(&mut e);
}

#[test]
fn comment_and_review_identity_text_is_cleaned() {
    let mut e = engine_with(DocumentTree::from_text("seed"));
    apply(
        &mut e,
        Command::SetReviewIdentity {
            author: "Ann\u{1}\nSmith".into(),
            date: "2026-01-01T00:00:00Z".into(),
        },
    );
    assert_eq!(e.review_author, "Ann Smith");
    let evt = apply(
        &mut e,
        Command::InsertComment {
            range: BridgeLogicalRange {
                start: bpos_top(0, 0),
                end: bpos_top(0, 4),
            },
            text: "see\u{2} this".into(),
            author: "B\u{1}ob".into(),
        },
    );
    assert!(!matches!(evt, Event::Error { .. }), "{evt:?}");
    let comments = e.undo.current().comment_defs.clone();
    assert_eq!(comments.len(), 1);
    let c = comments.values().next().expect("comment");
    assert_eq!(c.paragraphs, vec!["see this".to_string()]);
    assert_eq!(c.author, "Bob");
    saved_document_xml(&mut e);

    let evt = apply(
        &mut e,
        Command::SetReviewIdentity {
            author: "x\u{FFFF}".into(),
            date: String::new(),
        },
    );
    assert!(refusal(&evt).is_some_and(|m| m.starts_with("SetReviewIdentity: author: ")));
    assert_eq!(e.review_author, "Ann Smith", "a refusal changes nothing");
}

#[test]
fn opening_a_text_file_repairs_its_controls() {
    let mut e = engine_with(DocumentTree::from_text("old"));
    let evt = apply(
        &mut e,
        Command::OpenDocument {
            bytes: b"one\x01two\x0Bthree\x0Cfour\nfive\xEF\xBF\xBE".to_vec(),
            format: bridge::DocFormat::PlainText,
            name: Some("n\u{1}ame.txt".into()),
            defaults: None,
            limits: None,
            password: None,
        },
    );
    assert!(matches!(evt, Event::DocumentLoaded { .. }), "{evt:?}");
    assert_eq!(
        e.undo.current().paragraph_text(0),
        Some("onetwo\u{2028}three\u{C}four")
    );
    assert_eq!(e.undo.current().paragraph_text(1), Some("five\u{FFFD}"));
    saved_document_xml(&mut e);
}

#[test]
fn clean_text_takes_the_fast_path_unchanged() {
    let mut e = engine_with(DocumentTree::from_text(""));
    insert(&mut e, "مرحبا بالعالم \u{200F}x\u{7F}\u{85}");
    assert_eq!(body_text(&e), "مرحبا بالعالم \u{200F}x\u{7F}\u{85}");
}

#[test]
fn a_save_after_a_hostile_paste_never_fails() {
    let mut e = engine_with(DocumentTree::from_text("seed"));
    for hostile in [
        "\u{1}",
        "a\u{0}b",
        "\u{B}\u{C}\u{1F}",
        "<![CDATA[ ]]> & &#0;",
        "\"'><\u{7f}",
    ] {
        insert(&mut e, hostile);
        saved_document_xml(&mut e);
    }
}
