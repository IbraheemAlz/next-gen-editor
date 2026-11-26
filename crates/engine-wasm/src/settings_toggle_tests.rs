//! Issue #354 — `word/settings.xml` `<w:trackRevisions/>` and
//! `<w:mirrorMargins/>`, end to end: `.docx` → reader → the engine's
//! review mode / page geometry → paint, hit-testing, PDF → save.

use super::*;
use format_docx::test_fixtures::{
    MIRROR_MARGINS_TEXTS, TRACK_REVISIONS_TEXTS, mirror_margins_docx, track_revisions_docx,
    track_revisions_settings_xml,
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

/// Native engine with the Latin test face at scale 1 (1 layout unit =
/// 1 pt), holding an empty document.
fn engine() -> Engine {
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
    e.undo = UndoStack::new(DocumentTree::from_text(""), UNDO_CAP);
    e.selection = Some(SelectionState {
        anchor: bpos_top(0, 0),
        caret: bpos_top(0, 0),
        ideal_x: None,
        kind: SelectionKind::Linear,
    });
    e.review_date = "2026-01-01T00:00:00Z".into();
    e
}

fn open(e: &mut Engine, bytes: Vec<u8>, format: DocFormat) {
    let evt = apply(
        e,
        Command::OpenDocument {
            bytes,
            format,
            name: Some("settings.docx".into()),
            defaults: None,
            limits: None,
            password: None,
        },
    );
    assert!(matches!(evt, Event::DocumentLoaded { .. }), "{evt:?}");
}

fn tracking(e: &Engine) -> bool {
    let Event::SelectionChanged {
        is_tracking_changes,
        ..
    } = e.selection_changed()
    else {
        panic!("selection_changed");
    };
    is_tracking_changes
}

fn caret_at(e: &mut Engine, block: u32, offset: u32) -> Event {
    let pos = BridgeLogicalPos {
        path: BridgeBlockPath {
            steps: vec![BridgePathStep::Block { idx: block }],
        },
        offset,
    };
    apply(
        e,
        Command::SetSelection {
            range: BridgeLogicalRange {
                start: pos.clone(),
                end: pos.clone(),
            },
            caret: pos,
        },
    )
}

fn save(e: &mut Engine) -> Vec<u8> {
    match apply(
        e,
        Command::SaveDocument {
            format: DocFormat::Docx,
        },
    ) {
        Event::DocumentSaved { bytes, .. } => bytes,
        other => panic!("save: {other:?}"),
    }
}

/// Every entry of a package, by name.
fn entries(zip: &[u8]) -> std::collections::BTreeMap<String, Vec<u8>> {
    use std::io::Read;
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(zip)).expect("zip");
    (0..z.len())
        .map(|i| {
            let mut f = z.by_index(i).expect("entry");
            let mut out = Vec::new();
            f.read_to_end(&mut out).expect("read");
            (f.name().to_string(), out)
        })
        .collect()
}

fn settings_of(zip: &[u8]) -> String {
    String::from_utf8(
        entries(zip)
            .remove("word/settings.xml")
            .expect("settings part"),
    )
    .expect("utf8")
}

/// A document saved with Track Changes on opens in review mode (the
/// toolbar's toggle reads `is_tracking_changes`), typing records an
/// insertion, and a save that left the toggle alone keeps the settings
/// part byte-identical.
#[test]
fn track_revisions_opens_in_review_mode_and_records_insertions() {
    let mut e = engine();
    assert!(!tracking(&e));
    open(&mut e, track_revisions_docx(), DocFormat::Docx);
    assert!(e.undo.current().settings.track_revisions, "read");
    assert!(tracking(&e), "review mode is on from the open");

    let end = TRACK_REVISIONS_TEXTS[1].len() as u32;
    caret_at(&mut e, 1, end);
    let evt = apply(
        &mut e,
        Command::InsertText {
            at: None,
            text: " Yes.".into(),
        },
    );
    assert!(
        matches!(
            evt,
            Event::SelectionChanged {
                is_tracking_changes: true,
                ..
            }
        ),
        "{evt:?}"
    );
    let p = e
        .undo
        .current()
        .paragraph_at_path(&engine::BlockPath::top(1))
        .expect("paragraph 1");
    assert!(
        p.revisions
            .iter()
            .any(|r| r.kind == engine::RevisionKind::Insert && (r.start, r.end) == (end, end + 5)),
        "{:?}",
        p.revisions
    );

    let saved = save(&mut e);
    assert_eq!(
        settings_of(&saved),
        track_revisions_settings_xml(),
        "an unchanged toggle keeps the part byte-identical"
    );
    let doc = String::from_utf8(entries(&saved).remove("word/document.xml").unwrap()).unwrap();
    assert!(
        doc.contains("<w:ins "),
        "the typing is a tracked insertion: {doc}"
    );
}

/// Toggling review mode off is written back — exactly the element goes,
/// every other part and byte stays — and the saved file reopens with it
/// off; toggling it on again restores the source part byte for byte.
#[test]
fn toggling_review_mode_is_written_back_to_settings() {
    let source = entries(&track_revisions_docx());
    let mut e = engine();
    open(&mut e, track_revisions_docx(), DocFormat::Docx);
    let evt = apply(&mut e, Command::ToggleTrackChanges { enabled: false });
    assert!(
        matches!(
            evt,
            Event::SelectionChanged {
                is_tracking_changes: false,
                ..
            }
        ),
        "{evt:?}"
    );
    assert!(
        e.undo.current().settings.track_revisions,
        "the toggle is no document edit (no undo step)"
    );
    let off = save(&mut e);
    let saved = entries(&off);
    assert_eq!(
        settings_of(&off),
        track_revisions_settings_xml().replacen("<w:trackRevisions/>", "", 1)
    );
    assert_eq!(saved.len(), source.len(), "no part added or dropped");
    for (name, bytes) in &source {
        if name != "word/settings.xml" {
            assert_eq!(saved.get(name), Some(bytes), "{name} byte-identical");
        }
    }

    /* The saved file reopens with review mode off. */
    let mut reopened = engine();
    apply(&mut reopened, Command::ToggleTrackChanges { enabled: true });
    open(&mut reopened, off.clone(), DocFormat::Docx);
    assert!(!reopened.undo.current().settings.track_revisions);
    assert!(!tracking(&reopened), "the open replaces the previous mode");

    /* On again: the element is re-inserted at its schema rank — the
    source part exactly. */
    apply(&mut reopened, Command::ToggleTrackChanges { enabled: true });
    let on = save(&mut reopened);
    assert_eq!(settings_of(&on), track_revisions_settings_xml());
}

/// A package without a settings part gets one — related and declared —
/// only when review mode is on at save.
#[test]
fn review_mode_on_a_new_document_mints_the_settings_part() {
    let mut e = engine();
    open(&mut e, b"plain text".to_vec(), DocFormat::PlainText);
    assert!(!tracking(&e));
    let plain = entries(&save(&mut e));
    assert!(
        !plain.contains_key("word/settings.xml"),
        "nothing on: no part"
    );

    apply(&mut e, Command::ToggleTrackChanges { enabled: true });
    let bytes = save(&mut e);
    let saved = entries(&bytes);
    assert!(settings_of(&bytes).contains("<w:trackRevisions/>"));
    let ct = String::from_utf8(saved["[Content_Types].xml"].clone()).unwrap();
    assert!(ct.contains("/word/settings.xml"), "{ct}");
    let rels = String::from_utf8(saved["word/_rels/document.xml.rels"].clone()).unwrap();
    assert!(rels.contains("relationships/settings"), "{rels}");

    let mut reopened = engine();
    open(&mut reopened, bytes, DocFormat::Docx);
    assert!(tracking(&reopened));
}

/// `trackedChanges` protection forces review mode on, but that is the
/// restriction's, not the document's Track Changes setting: a save keeps
/// the part as it was read.
#[test]
fn forced_review_mode_does_not_write_track_revisions() {
    let bytes = format_docx::test_fixtures::protected_docx(
        "<w:p><w:r><w:t>alpha</w:t></w:r></w:p>",
        "<w:documentProtection w:edit=\"trackedChanges\" w:enforcement=\"1\"/>",
    );
    let source = entries(&bytes)["word/settings.xml"].clone();
    let mut e = engine();
    open(&mut e, bytes, DocFormat::Docx);
    assert!(tracking(&e), "forced on by the protection");
    assert!(!e.undo.current().settings.track_revisions);
    assert_eq!(entries(&save(&mut e))["word/settings.xml"], source);
}

/// The three pages of the mirror fixture, laid out at scale 1.
fn mirror_pages(e: &Engine) -> Vec<PageBox> {
    let (pages, _, _, info) = e.build_pages(1.0, false, None).expect("layout");
    assert!(info.degradations.is_empty(), "{:?}", info.degradations);
    assert_eq!(pages.len(), 3);
    pages
}

const INSIDE: f32 = format_docx::test_fixtures::MIRROR_LEFT_TWIPS as f32 / 20.0;
const OUTSIDE: f32 = format_docx::test_fixtures::MIRROR_RIGHT_TWIPS as f32 / 20.0;

/// Page-local x of every page's first text line.
fn line_starts(pages: &[PageBox]) -> Vec<f32> {
    pages
        .iter()
        .map(|p| {
            let LayoutBlock::Paragraph(para) = &p.blocks[0] else {
                panic!("a paragraph");
            };
            p.margins.left + para.origin.x + para.lines[0].origin.x
        })
        .collect()
}

/// `<w:mirrorMargins/>`: the even page lays out with the inside (`w:left`)
/// margin on the right — its text starts at the outside margin — and the
/// box tree, the caret and the PDF all follow.
#[test]
fn mirror_margins_swap_inside_and_outside_on_even_pages() {
    let mut e = engine();
    open(&mut e, mirror_margins_docx(), DocFormat::Docx);
    assert!(e.undo.current().settings.mirror_margins, "read");
    let pages = mirror_pages(&e);
    assert_eq!(
        pages.iter().map(|p| p.margins.left).collect::<Vec<_>>(),
        vec![INSIDE, OUTSIDE, INSIDE]
    );
    assert_eq!(
        pages.iter().map(|p| p.margins.right).collect::<Vec<_>>(),
        vec![OUTSIDE, INSIDE, OUTSIDE]
    );
    assert_eq!(line_starts(&pages), vec![INSIDE, OUTSIDE, INSIDE]);

    /* The caret at each paragraph's start (one per page). */
    for (block, want) in [(0, INSIDE), (1, OUTSIDE), (2, INSIDE)] {
        let Event::SelectionChanged { caret, .. } = caret_at(&mut e, block, 0) else {
            panic!("caret");
        };
        assert!(
            (caret.x - want).abs() < 0.5,
            "paragraph {block} ({:?}): caret x {} != {want}",
            MIRROR_MARGINS_TEXTS[block as usize],
            caret.x
        );
    }

    /* PDF export reads the box tree: each page's leftmost glyph. */
    let pdf = match e.do_export_pdf(format_pdf::PdfProfile::Plain) {
        Event::PdfExported { bytes, .. } => bytes,
        other => panic!("export: {other:?}"),
    };
    let leftmost: Vec<f32> = format_pdf::test_support::content_streams(&pdf)
        .iter()
        .map(|s| {
            let text = String::from_utf8_lossy(s);
            let tokens: Vec<&str> = text.split_whitespace().collect();
            tokens
                .iter()
                .enumerate()
                .filter(|(_, t)| **t == "Tm")
                .filter_map(|(i, _)| tokens.get(i.checked_sub(2)?)?.parse::<f32>().ok())
                .fold(f32::INFINITY, f32::min)
        })
        .collect();
    assert_eq!(leftmost.len(), 3, "{leftmost:?}");
    for (got, want) in leftmost.iter().zip([INSIDE, OUTSIDE, INSIDE]) {
        assert!((got - want).abs() < 0.5, "{leftmost:?}");
    }
}

/// Without the setting the same pages keep `w:left` on the left, and the
/// geometry differs from the mirrored layout exactly by the even page.
#[test]
fn without_mirror_margins_every_page_keeps_its_left_margin() {
    let mut e = engine();
    open(&mut e, mirror_margins_docx(), DocFormat::Docx);
    let mirrored = mirror_pages(&e);
    let mut doc = e.undo.current().clone();
    doc.settings.mirror_margins = false;
    e.undo = UndoStack::new(doc, UNDO_CAP);
    let plain = mirror_pages(&e);
    assert_eq!(line_starts(&plain), vec![INSIDE, INSIDE, INSIDE]);
    assert_ne!(
        layout::geometry_fingerprint(&plain),
        layout::geometry_fingerprint(&mirrored)
    );
    assert_eq!(
        layout::geometry_fingerprint(&plain[..1]),
        layout::geometry_fingerprint(&mirrored[..1]),
        "an odd page is unchanged"
    );
}

/// The mirror fixture's zero-edit save is byte-identical, every part.
#[test]
fn mirror_margins_zero_edit_save_is_byte_identical() {
    let mut e = engine();
    open(&mut e, mirror_margins_docx(), DocFormat::Docx);
    let saved = entries(&save(&mut e));
    let source = entries(&mirror_margins_docx());
    assert_eq!(
        saved.keys().collect::<Vec<_>>(),
        source.keys().collect::<Vec<_>>()
    );
    for (name, bytes) in &source {
        assert_eq!(&saved[name], bytes, "{name}");
    }
}
