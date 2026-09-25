//! Issue #194 — the engine's own "the document changed" signal. The worker
//! used to decide when to rebroadcast the accessibility tree from a
//! hand-kept allowlist of command types that drifted from the bridge; it
//! now compares `Engine::document_mutation_seq` across a command. This
//! table pins the contract per command CLASS: every document mutation
//! bumps the counter, every query / view / selection command leaves it
//! alone — so the worker needs no per-command bookkeeping.

use super::*;
use bridge::{
    BlockPath as WirePath, FieldKind, HeaderFooterArea, InsertSide, ListKind, TocSwitches,
};

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
    e.undo = UndoStack::new(doc, 100);
    e.selection = Some(SelectionState {
        anchor: bpos_top(0, 0),
        caret: bpos_top(0, 0),
        ideal_x: None,
        kind: SelectionKind::Linear,
    });
    e.review_date = "2026-01-01T00:00:00Z".into();
    e
}

/// `"hello world"` / `"second"` — two paragraphs, caret at the start.
fn text_doc() -> DocumentTree {
    DocumentTree::from_text("hello worldsecond").split_paragraph(engine::LogicalPos {
        path: EngineBlockPath::top(0),
        offset: 11,
    })
}

/// `[Table 2×2, Paragraph "tail"]`.
fn table_doc() -> DocumentTree {
    DocumentTree::from_text("tail").insert_table(EngineBlockPath::top(0), 2, 2)
}

fn range(p: u32, a: u32, b: u32) -> BridgeLogicalRange {
    BridgeLogicalRange {
        start: bpos_top(p, a),
        end: bpos_top(p, b),
    }
}

fn no_attrs() -> TextAttrsPatch {
    TextAttrsPatch {
        bold: None,
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
    }
}

fn insert(text: &str) -> Command {
    Command::InsertText {
        at: None,
        text: text.into(),
    }
}

fn image() -> BridgeImageBlob {
    BridgeImageBlob {
        bytes: vec![0x89, b'P', b'N', b'G'],
        mime: "image/png".into(),
        width: 10,
        height: 10,
    }
}

fn red() -> Color {
    Color {
        r: 255,
        g: 0,
        b: 0,
        a: 255,
    }
}

fn viewport() -> BridgeRect {
    BridgeRect {
        x: 0.0,
        y: 0.0,
        w: 600.0,
        h: 800.0,
    }
}

fn docx_fixture() -> Vec<u8> {
    include_bytes!("../../format-docx/tests/fixtures/simple_text.docx").to_vec()
}

struct Case {
    label: &'static str,
    doc: fn() -> DocumentTree,
    setup: Vec<Command>,
    cmd: Command,
}

fn case(label: &'static str, doc: fn() -> DocumentTree, setup: Vec<Command>, cmd: Command) -> Case {
    Case {
        label,
        doc,
        setup,
        cmd,
    }
}

/// Commands that change the document — one per mutation class the old
/// worker allowlist tracked, plus every class it missed (#194).
fn mutating_cases() -> Vec<Case> {
    let t = WirePath::top(0);
    vec![
        case("text: insert", text_doc, vec![], insert("x")),
        case(
            "text: delete range",
            text_doc,
            vec![],
            Command::DeleteRange {
                range: range(0, 0, 5),
            },
        ),
        case(
            "text: replace range",
            text_doc,
            vec![],
            Command::ReplaceRange {
                range: range(0, 0, 5),
                text: "howdy".into(),
            },
        ),
        case(
            "text: delete at caret (merge)",
            text_doc,
            vec![Command::SetSelection {
                range: range(1, 0, 0),
                caret: bpos_top(1, 0),
            }],
            Command::DeleteAtCaret {
                forward: false,
                by_word: false,
            },
        ),
        case(
            "text: split",
            text_doc,
            vec![],
            Command::SplitParagraph {
                at: Some(bpos_top(0, 5)),
            },
        ),
        case(
            "text: paste plain",
            text_doc,
            vec![],
            Command::PastePlain { text: "p".into() },
        ),
        case(
            "text: paste html",
            text_doc,
            vec![],
            Command::PasteHtml {
                html: "<p>h</p>".into(),
            },
        ),
        case(
            "ime: commit",
            text_doc,
            vec![
                Command::BeginComposition {
                    at: Some(bpos_top(0, 0)),
                },
                Command::UpdateComposition {
                    text: "ع".into(),
                    target_range: None,
                },
            ],
            Command::EndComposition { commit: true },
        ),
        case(
            "format: run attrs",
            text_doc,
            vec![],
            Command::ApplyFormatting {
                range: Some(range(0, 0, 5)),
                attrs: TextAttrsPatch {
                    bold: Some(true),
                    ..no_attrs()
                },
            },
        ),
        case(
            "paragraph: align",
            text_doc,
            vec![],
            Command::SetParagraphAlign {
                range: range(0, 0, 0),
                align: BridgeAlignment::Center,
            },
        ),
        case(
            "paragraph: direction",
            text_doc,
            vec![],
            Command::SetParagraphDirection {
                range: range(0, 0, 0),
                direction: Direction::Rtl,
            },
        ),
        case(
            "paragraph: indent",
            text_doc,
            vec![],
            Command::SetParagraphIndent {
                range: range(0, 0, 0),
                start_pt: 18.0,
                end_pt: 0.0,
                first_line_pt: 0.0,
            },
        ),
        case(
            "paragraph: line spacing",
            text_doc,
            vec![],
            Command::SetLineSpacing {
                range: range(0, 0, 0),
                multiplier: 2.0,
            },
        ),
        case(
            "paragraph: shading",
            text_doc,
            vec![],
            Command::SetParagraphShading {
                range: range(0, 0, 0),
                color: Some(red()),
            },
        ),
        case(
            "style: apply",
            text_doc,
            vec![],
            Command::ApplyStyle {
                range: range(0, 0, 0),
                style_id: Some("Heading1".into()),
            },
        ),
        case(
            "list: toggle",
            text_doc,
            vec![],
            Command::ToggleList {
                range: range(0, 0, 0),
                kind: ListKind::Bullet,
            },
        ),
        case(
            "list: level",
            text_doc,
            vec![Command::ToggleList {
                range: range(0, 0, 0),
                kind: ListKind::Number,
            }],
            Command::ChangeListLevel {
                range: range(0, 0, 0),
                delta: 1,
            },
        ),
        case(
            "section: page break",
            text_doc,
            vec![],
            Command::InsertPageBreak { at: bpos_top(0, 5) },
        ),
        case(
            "section: section break",
            text_doc,
            vec![],
            Command::InsertSectionBreak {
                at: bpos_top(0, 5),
                kind: SectionBreakKind::NextPage,
            },
        ),
        case(
            "section: orientation",
            text_doc,
            vec![],
            Command::SetPageOrientation {
                at: bpos_top(0, 0),
                orientation: BridgePageOrientation::Landscape,
            },
        ),
        case(
            "section: margins",
            text_doc,
            vec![],
            Command::SetPageMargins {
                at: bpos_top(0, 0),
                top_pt: 36.0,
                right_pt: 36.0,
                bottom_pt: 36.0,
                left_pt: 36.0,
            },
        ),
        case(
            "field: insert",
            text_doc,
            vec![],
            Command::InsertField {
                at: bpos_top(0, 0),
                kind: FieldKind::Page,
            },
        ),
        case(
            "field: toc",
            text_doc,
            vec![],
            Command::InsertToc {
                at: bpos_top(0, 0),
                switches: TocSwitches::default(),
            },
        ),
        case(
            "note: footnote",
            text_doc,
            vec![],
            Command::InsertFootnote { at: bpos_top(0, 5) },
        ),
        case(
            "note: endnote",
            text_doc,
            vec![],
            Command::InsertEndnote { at: bpos_top(0, 5) },
        ),
        case(
            "story: text box",
            text_doc,
            vec![],
            Command::InsertTextBox {
                at: bpos_top(0, 5),
                width_emu: 914_400,
                height_emu: 457_200,
            },
        ),
        case(
            "story: header edit",
            text_doc,
            vec![Command::EnterHeaderFooter {
                page: 0,
                area: HeaderFooterArea::Header,
            }],
            insert("head"),
        ),
        case(
            "image: insert",
            text_doc,
            vec![],
            Command::InsertImage {
                at: bpos_top(0, 5),
                image: image(),
                fit: ImageFit::Original,
            },
        ),
        case(
            "table: insert",
            text_doc,
            vec![],
            Command::InsertTable {
                at: WirePath::top(1),
                rows: 2,
                cols: 2,
            },
        ),
        case(
            "table: delete",
            table_doc,
            vec![],
            Command::DeleteTable { path: t.clone() },
        ),
        case(
            "table: row",
            table_doc,
            vec![],
            Command::InsertRow {
                table_path: t.clone(),
                row: 0,
                side: InsertSide::After,
            },
        ),
        case(
            "table: column",
            table_doc,
            vec![],
            Command::DeleteColumn {
                table_path: t.clone(),
                col: 1,
            },
        ),
        case(
            "table: merge",
            table_doc,
            vec![],
            Command::MergeCells {
                table_path: t.clone(),
                from_row: 0,
                from_col: 0,
                to_row: 0,
                to_col: 1,
            },
        ),
        case(
            "table: shading",
            table_doc,
            vec![],
            Command::SetCellShading {
                table_path: t,
                row: 0,
                col: 0,
                color: Some(red()),
            },
        ),
        case(
            "review: comment",
            text_doc,
            vec![],
            Command::InsertComment {
                range: range(0, 0, 5),
                text: "note".into(),
                author: "A".into(),
            },
        ),
        case("history: undo", text_doc, vec![insert("x")], Command::Undo),
        case(
            "history: redo",
            text_doc,
            vec![insert("x"), Command::Undo],
            Command::Redo,
        ),
        case(
            "load: docx",
            text_doc,
            vec![],
            Command::LoadDocx {
                bytes: docx_fixture(),
            },
        ),
        case(
            "load: render page reset",
            text_doc,
            vec![],
            Command::RenderPage {
                text: "fresh".into(),
                font_id: "test-latin".into(),
                base_direction: "LTR".into(),
                px_size: 16.0,
                line_height: 26.0,
                align: "START".into(),
                device_pixel_ratio: None,
            },
        ),
    ]
}

/// Commands that never change the document.
fn query_cases() -> Vec<Case> {
    vec![
        case("ping", text_doc, vec![], Command::Ping),
        case("stats", text_doc, vec![], Command::RequestStats),
        case(
            "hit test",
            text_doc,
            vec![],
            Command::HitTest {
                at: BridgePoint { x: 10.0, y: 10.0 },
            },
        ),
        case(
            "selection: set",
            text_doc,
            vec![],
            Command::SetSelection {
                range: range(0, 0, 5),
                caret: bpos_top(0, 5),
            },
        ),
        case(
            "selection: move caret",
            text_doc,
            vec![],
            Command::MoveCaret {
                direction: MoveDirection::Right,
                extend: false,
            },
        ),
        case("selection: all", text_doc, vec![], Command::SelectAll),
        case(
            "clipboard: copy",
            text_doc,
            vec![Command::SelectAll],
            Command::GetSelectionAsClipboard { include_docx: None },
        ),
        case(
            "a11y: delta",
            text_doc,
            vec![],
            Command::RequestAccessibilityDelta,
        ),
        case(
            "view: paint",
            text_doc,
            vec![],
            Command::RequestPaint {
                viewport: viewport(),
                dirty: None,
            },
        ),
        case(
            "view: viewport",
            text_doc,
            vec![],
            Command::SetViewport { rect: viewport() },
        ),
        case(
            "view: zoom",
            text_doc,
            vec![],
            Command::SetZoom { scale: 1.5 },
        ),
        case(
            "view: field codes",
            text_doc,
            vec![],
            Command::SetFieldCodeView { enabled: true },
        ),
        case("save: docx", text_doc, vec![], Command::SaveDocx),
        case(
            "snapshot",
            text_doc,
            vec![],
            Command::Snapshot { seq: None },
        ),
        case("images: rects", text_doc, vec![], Command::GetImageRects),
        case(
            "review: toggle tracking",
            text_doc,
            vec![],
            Command::ToggleTrackChanges { enabled: true },
        ),
        case(
            "ime: preview only",
            text_doc,
            vec![Command::BeginComposition {
                at: Some(bpos_top(0, 0)),
            }],
            Command::UpdateComposition {
                text: "ع".into(),
                target_range: None,
            },
        ),
    ]
}

fn run(c: Case) -> (u64, u64, Event) {
    let mut e = engine_with((c.doc)());
    for s in c.setup {
        let evt = apply(&mut e, s);
        assert!(
            !matches!(evt, Event::Error { .. }),
            "{}: setup failed: {evt:?}",
            c.label
        );
    }
    let before = e.mutation_seq;
    let evt = apply(&mut e, c.cmd);
    (before, e.mutation_seq, evt)
}

#[test]
fn every_mutating_command_class_bumps_the_mutation_seq() {
    for c in mutating_cases() {
        let label = c.label;
        let (before, after, evt) = run(c);
        assert!(!matches!(evt, Event::Error { .. }), "{label}: {evt:?}");
        assert!(after > before, "{label}: mutation_seq {before} -> {after}");
    }
}

#[test]
fn queries_leave_the_mutation_seq_alone() {
    for c in query_cases() {
        let label = c.label;
        let (before, after, evt) = run(c);
        assert!(!matches!(evt, Event::Error { .. }), "{label}: {evt:?}");
        assert_eq!(before, after, "{label}: a query bumped mutation_seq");
    }
}

/// A no-op attempt at a mutation (nothing to undo) is not a mutation.
#[test]
fn a_no_op_undo_is_not_a_mutation() {
    let mut e = engine_with(text_doc());
    let before = e.mutation_seq;
    apply(&mut e, Command::Undo);
    assert_eq!(e.mutation_seq, before);
}

/// The counter is monotonic across stack swaps: a reload restarts the undo
/// revision at 0, and the seq must not follow it down (nor collide with a
/// value the worker already broadcast for).
#[test]
fn mutation_seq_is_monotonic_across_a_document_reload() {
    let mut e = engine_with(text_doc());
    apply(&mut e, insert("a"));
    apply(&mut e, insert("b"));
    let before = e.mutation_seq;
    apply(
        &mut e,
        Command::LoadDocx {
            bytes: docx_fixture(),
        },
    );
    let after_load = e.mutation_seq;
    assert!(after_load > before);
    apply(&mut e, insert("c"));
    assert!(e.mutation_seq > after_load);
}

/// `Event::Painted` carries the counter (the explicit paint and the
/// `SetViewport` synthetic alike), so a consumer that only sees paints can
/// tell a content change from a scroll.
#[test]
fn painted_carries_the_mutation_seq() {
    let mut e = engine_with(text_doc());
    apply(&mut e, insert("a"));
    let seq = e.mutation_seq;
    assert!(seq > 0);
    let Event::Painted { mutation_seq, .. } = apply(
        &mut e,
        Command::RequestPaint {
            viewport: viewport(),
            dirty: None,
        },
    ) else {
        panic!("RequestPaint answers Painted");
    };
    assert_eq!(mutation_seq, seq);
    let Event::Painted { mutation_seq, .. } =
        apply(&mut e, Command::SetViewport { rect: viewport() })
    else {
        panic!("SetViewport answers Painted");
    };
    assert_eq!(mutation_seq, seq);
}

/// End to end on the engine side: the delta the worker requests after a
/// class the old allowlist missed (a field insert) is a real
/// single-paragraph `Update`, never a full-tree replace.
#[test]
fn a_missed_class_now_yields_a_fine_grained_a11y_update() {
    let mut e = engine_with(text_doc());
    let _ = e.build_a11y_delta();
    let before = e.mutation_seq;
    apply(
        &mut e,
        Command::InsertField {
            at: bpos_top(0, 0),
            kind: FieldKind::Page,
        },
    );
    assert!(e.mutation_seq > before);
    let patches = e.build_a11y_delta();
    assert!(
        matches!(patches.as_slice(), [A11yPatch::Update { index: 0, .. }]),
        "{patches:?}"
    );
}
