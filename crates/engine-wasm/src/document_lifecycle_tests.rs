//! Document lifecycle commands: issue #338 (`CloseDocument` resets to the
//! seeded empty document instead of answering a `phase3_stub` error) and
//! issue #339 (`OpenDocument` with `PlainText` / `Html`).

use super::*;
use bridge::{DefaultPageSize as WirePageSize, HeaderFooterArea};

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

fn range(p: u32, a: u32, b: u32) -> BridgeLogicalRange {
    BridgeLogicalRange {
        start: bpos_top(p, a),
        end: bpos_top(p, b),
    }
}

/// Issue #338 — typing, a comment, a name and track-changes all go; the
/// answer is a real `SelectionChanged` over the seeded empty document
/// with nothing to undo, at a moved document revision.
#[test]
fn close_document_resets_to_the_seeded_empty_document() {
    let mut e = engine_with(DocumentTree::from_text("hello world"));
    apply(
        &mut e,
        Command::InsertText {
            at: None,
            text: "abc".into(),
        },
    );
    let commented = apply(
        &mut e,
        Command::InsertComment {
            range: range(0, 0, 3),
            text: "note".into(),
            author: "Tester".into(),
        },
    );
    assert!(!matches!(commented, Event::Error { .. }), "{commented:?}");
    assert!(!e.undo.current().comment_defs.is_empty());
    assert!(e.undo.can_undo());
    e.document_name = Some("letter.docx".into());
    e.tracking_changes = true;
    let before = e.mutation_seq;

    let Event::SelectionChanged {
        range: sel,
        can_undo,
        can_redo,
        undo_depth,
        is_tracking_changes,
        editing_story,
        document_revision,
        ..
    } = apply(&mut e, Command::CloseDocument)
    else {
        panic!("CloseDocument answers SelectionChanged");
    };
    assert!(!can_undo && !can_redo, "undo history cleared");
    assert_eq!(undo_depth, e.undo.depth());
    assert!(!is_tracking_changes, "a new document records nothing");
    assert!(editing_story.is_none());
    assert_eq!(sel.start, bpos_top(0, 0));
    assert_eq!(sel.end, bpos_top(0, 0));
    assert!(e.mutation_seq > before, "the close is a document mutation");
    assert_eq!(document_revision, e.mutation_seq);

    let doc = e.undo.current();
    assert_eq!(doc.paragraph_count(), 1);
    assert_eq!(doc.to_plain_text(), "");
    assert!(doc.comment_defs.is_empty() && doc.comment_ranges.is_empty());
    assert!(doc.media.is_empty());
    assert!(doc.source_package.is_none());
    assert!(e.document_name.is_none());
    /* The session survives: fonts + layout config (zoom, direction). */
    assert!(e.layout_cfg.is_some());
    assert!(e.fonts.contains_key("test-latin"));
    /* And the engine keeps working on the fresh document. */
    let typed = apply(
        &mut e,
        Command::InsertText {
            at: None,
            text: "new".into(),
        },
    );
    assert!(matches!(typed, Event::SelectionChanged { .. }), "{typed:?}");
    assert_eq!(e.undo.current().to_plain_text(), "new");
}

/// Issue #338 — an opened `.docx`'s retained source package (#134) is
/// dropped: the next save goes through the minimal-package writer.
#[test]
fn close_document_drops_the_retained_source_package() {
    let mut e = engine_with(DocumentTree::from_text(""));
    let bytes = format_docx::build_minimal_docx(&DocumentTree::from_text("from a file"))
        .expect("build a .docx");
    let loaded = apply(&mut e, Command::LoadDocx { bytes });
    assert!(matches!(loaded, Event::DocumentLoaded { .. }), "{loaded:?}");
    assert!(e.undo.current().source_package.is_some());

    apply(&mut e, Command::CloseDocument);
    assert!(e.undo.current().source_package.is_none());
    assert!(e.detached_package.borrow().is_none());
    let saved = apply(&mut e, Command::SaveDocx);
    assert!(matches!(saved, Event::DocumentSaved { .. }), "{saved:?}");
}

/// Issue #338 — `StoryPolicy::ExitsStory`: closing from inside a header
/// leaves the story first instead of being rejected by the story gate.
#[test]
fn close_document_exits_an_active_story() {
    let mut e = engine_with(DocumentTree::from_text("body text"));
    apply(
        &mut e,
        Command::EnterHeaderFooter {
            page: 0,
            area: HeaderFooterArea::Header,
        },
    );
    assert!(e.story_active());
    let closed = apply(&mut e, Command::CloseDocument);
    let Event::SelectionChanged { editing_story, .. } = closed else {
        panic!("CloseDocument answers SelectionChanged, got {closed:?}");
    };
    assert!(editing_story.is_none());
    assert!(!e.story_active());
    assert_eq!(e.undo.current().to_plain_text(), "");
}

fn open(e: &mut Engine, bytes: &[u8], format: DocFormat, name: &str) -> Event {
    apply(
        e,
        Command::OpenDocument {
            bytes: bytes.to_vec(),
            format,
            name: Some(name.into()),
            defaults: None,
            limits: None,
            password: None,
        },
    )
}

fn paragraphs(e: &Engine) -> Vec<(String, Option<engine::TextDirection>)> {
    e.undo
        .current()
        .blocks
        .iter()
        .filter_map(|b| b.as_paragraph())
        .map(|p| (p.text.clone(), p.props.direction))
        .collect()
}

/// Issue #339 — a `.txt` opens one paragraph per line (any line-break
/// convention, BOM dropped, one trailing break absorbed), each with its
/// auto direction: Arabic lines RTL, Latin lines LTR, neutral lines
/// following the paragraph before them.
#[test]
fn open_plain_text_splits_lines_and_auto_directs_them() {
    use engine::TextDirection::{Ltr, Rtl};
    let mut e = engine_with(DocumentTree::from_text("old"));
    let mut bytes = vec![0xEF, 0xBB, 0xBF];
    bytes.extend_from_slice("Hello\r\nمرحبا بالعالم\n\n123\rEnd\n".as_bytes());
    let loaded = open(&mut e, &bytes, DocFormat::PlainText, "dir/notes.txt");
    assert!(
        matches!(
            loaded,
            Event::DocumentLoaded {
                paragraph_count: 5,
                ..
            }
        ),
        "{loaded:?}"
    );
    assert_eq!(
        paragraphs(&e),
        vec![
            ("Hello".to_string(), Some(Ltr)),
            ("مرحبا بالعالم".to_string(), Some(Rtl)),
            (String::new(), Some(Rtl)),
            ("123".to_string(), Some(Rtl)),
            ("End".to_string(), Some(Ltr)),
        ]
    );
    let doc = e.undo.current();
    let arabic = doc.blocks[1].as_paragraph().unwrap();
    assert_eq!(
        arabic.direct_overrides.direction,
        Some(Rtl),
        "direct, so the .docx writer and ApplyStyle keep it"
    );
    assert!(doc.source_package.is_none(), "nothing to preserve");
    assert_eq!(e.document_name.as_deref(), Some("notes.txt"));
    assert!(!e.undo.can_undo(), "a fresh document");
    let saved = apply(&mut e, Command::SaveDocx);
    assert!(matches!(saved, Event::DocumentSaved { .. }), "{saved:?}");
}

/// Issue #339 — UTF-16 (either BOM) decodes; invalid UTF-8 is replaced,
/// never rejected; an empty file is one empty paragraph.
#[test]
fn open_plain_text_decodes_utf16_and_tolerates_invalid_utf8() {
    let mut e = engine_with(DocumentTree::from_text(""));
    let mut le = vec![0xFF, 0xFE];
    for u in "سلام\nhi".encode_utf16() {
        le.extend_from_slice(&u.to_le_bytes());
    }
    open(&mut e, &le, DocFormat::PlainText, "le.txt");
    let texts: Vec<String> = paragraphs(&e).into_iter().map(|(t, _)| t).collect();
    assert_eq!(texts, ["سلام", "hi"]);

    let mut be = vec![0xFE, 0xFF];
    for u in "ok".encode_utf16() {
        be.extend_from_slice(&u.to_be_bytes());
    }
    open(&mut e, &be, DocFormat::PlainText, "be.txt");
    assert_eq!(paragraphs(&e)[0].0, "ok");

    open(
        &mut e,
        b"caf\xE9 au lait",
        DocFormat::PlainText,
        "cp1252.txt",
    );
    assert_eq!(paragraphs(&e)[0].0, "caf\u{FFFD} au lait");

    let loaded = open(&mut e, b"", DocFormat::PlainText, "empty.txt");
    assert!(
        matches!(
            loaded,
            Event::DocumentLoaded {
                paragraph_count: 1,
                ..
            }
        ),
        "{loaded:?}"
    );
}

/// Issue #339 — a `.html` opens through the paste parser: head metadata
/// skipped, `dir` honoured, tables kept, undeclared paragraphs auto-directed;
/// the host's page-size default applies.
#[test]
fn open_html_keeps_tables_and_directions() {
    use engine::TextDirection::{Ltr, Rtl};
    let mut e = engine_with(DocumentTree::from_text(""));
    let html = "<!DOCTYPE html><html><head><title>Ignored</title></head><body>\
                <p dir=\"rtl\">Hello مرحبا</p>\
                <table><tr><td>a</td><td>b</td></tr><tr><td>c</td><td>d</td></tr></table>\
                <p>بعد الجدول</p><p>After</p></body></html>";
    let loaded = apply(
        &mut e,
        Command::OpenDocument {
            bytes: html.as_bytes().to_vec(),
            format: DocFormat::Html,
            name: Some("page.html".into()),
            defaults: Some(DocumentDefaults {
                page_size: Some(WirePageSize::Letter),
                widow_control: None,
            }),
            limits: None,
            password: None,
        },
    );
    assert!(matches!(loaded, Event::DocumentLoaded { .. }), "{loaded:?}");
    let doc = e.undo.current();
    assert_eq!(
        doc.blocks.len(),
        4,
        "paragraph, table, paragraph, paragraph"
    );
    let engine::Block::Table(t) = &doc.blocks[1] else {
        panic!("the table survives");
    };
    assert_eq!((t.rows.len(), t.rows[0].cells.len()), (2, 2));
    assert_eq!(
        paragraphs(&e),
        vec![
            ("Hello مرحبا".to_string(), Some(Rtl)),
            ("بعد الجدول".to_string(), Some(Rtl)),
            ("After".to_string(), Some(Ltr)),
        ],
        "explicit dir wins over first-strong; the rest auto-direct"
    );
    assert!(!doc.to_plain_text().contains("Ignored"));
    assert_eq!(doc.body_section.geometry, engine::PageGeometry::letter());
    assert_eq!(
        doc.settings.default_page_size,
        engine::DefaultPageSize::Letter
    );
    assert!(doc.source_package.is_none());
}

/// Issue #339 — PDF is an export format: an honest, specific error, and
/// the current document is untouched.
#[test]
fn open_pdf_is_an_honest_error() {
    let mut e = engine_with(DocumentTree::from_text("keep me"));
    let evt = open(&mut e, b"%PDF-1.7", DocFormat::Pdf, "x.pdf");
    let Event::Error { message, .. } = evt else {
        panic!("PDF import answers Error, got {evt:?}");
    };
    assert!(message.contains("export format"), "{message}");
    assert_eq!(e.undo.current().to_plain_text(), "keep me");
}

/// Issues #339 / #348 — the host's `OpenDocument.limits` bound a `.txt` /
/// `.html` file like one package part: an oversized file is refused before
/// decoding, typed `PackageTooLarge`, and the open document is untouched.
#[test]
fn open_text_honours_the_host_package_limits() {
    let mut e = engine_with(DocumentTree::from_text("keep me"));
    for format in [DocFormat::PlainText, DocFormat::Html] {
        let evt = apply(
            &mut e,
            Command::OpenDocument {
                bytes: b"0123456789".to_vec(),
                format,
                name: Some("big".into()),
                defaults: None,
                limits: Some(bridge::PackageLimitsOverride {
                    max_part_bytes: Some(4),
                    ..Default::default()
                }),
                password: None,
            },
        );
        let Event::Error { message, kind, .. } = evt else {
            panic!("an oversized file answers Error, got {evt:?}");
        };
        assert_eq!(kind, Some(bridge::ErrorKind::PackageTooLarge), "{message}");
        assert_eq!(e.undo.current().to_plain_text(), "keep me");
    }
    /* Under the bound it opens. */
    let evt = apply(
        &mut e,
        Command::OpenDocument {
            bytes: b"0123".to_vec(),
            format: DocFormat::PlainText,
            name: None,
            defaults: None,
            limits: Some(bridge::PackageLimitsOverride {
                max_part_bytes: Some(4),
                ..Default::default()
            }),
            password: None,
        },
    );
    assert!(matches!(evt, Event::DocumentLoaded { .. }), "{evt:?}");
}

/// Issue #406 — the reader's warning report reaches the open reply: a
/// clamped page margin and an unusable one ride `DocumentLoaded.warnings`
/// (typed kinds, the attribute + raw value as detail), and a clean open
/// carries none.
#[test]
fn open_docx_reports_the_reader_warnings_on_the_reply() {
    let mut e = engine_with(DocumentTree::from_text("old"));
    let document = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
        <w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\
        <w:body><w:p><w:r><w:t>margins</w:t></w:r></w:p>\
        <w:sectPr><w:pgSz w:w=\"11906\" w:h=\"16838\"/>\
        <w:pgMar w:top=\"99999\" w:right=\"NaN\" w:bottom=\"1440\" w:left=\"1440\" \
        w:header=\"720\" w:footer=\"720\"/></w:sectPr></w:body></w:document>";
    let bytes = format_docx::test_fixtures::package_with_document_xml(document, &[]);
    let loaded = open(&mut e, &bytes, DocFormat::Docx, "margins.docx");
    let Event::DocumentLoaded { warnings, .. } = loaded else {
        panic!("expected DocumentLoaded, got {loaded:?}");
    };
    let clamped = warnings
        .iter()
        .find(|w| w.kind == bridge::ReadWarningKind::MeasureClamped)
        .expect("the 99999-twip top margin is clamped");
    assert_eq!(clamped.detail, "w:pgMar/@w:top = \"99999\" → 31680 twips");
    assert_eq!(clamped.count, 1);
    let invalid = warnings
        .iter()
        .find(|w| w.kind == bridge::ReadWarningKind::InvalidMeasure)
        .expect("the NaN right margin is ignored");
    assert_eq!(invalid.detail, "w:pgMar/@w:right = \"NaN\"");

    let clean =
        format_docx::test_fixtures::docx_with_body("<w:p><w:r><w:t>clean</w:t></w:r></w:p>");
    let loaded = open(&mut e, &clean, DocFormat::Docx, "clean.docx");
    assert!(
        matches!(&loaded, Event::DocumentLoaded { warnings, .. } if warnings.is_empty()),
        "{loaded:?}"
    );
}
