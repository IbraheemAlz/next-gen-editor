//! Issue #560 acceptance — paragraphs the ENGINE mints (an inserted
//! table's cells and escape paragraph, a new row, Enter inside a fresh
//! cell and at the end of the document) take the document's style cascade
//! when they are created (`StyleCascade::new_paragraph`), so the document
//! lays out exactly as its saved package re-opens: `docDefaults` + the
//! default paragraph style (`Normal`) — their spacing, line rule and
//! direction — instead of none of it until a save + reopen. Pinned with
//! the layout fingerprint, like the typed-document reopen of #475
//! (`line_pitch_tests::a_typed_document_reopens_from_its_docx_with_identical_geometry`).
//!
//! Driven through real `Command`s on `Engine::new_headless` (`fuzz-native`).

use super::*;
use format_docx::test_fixtures::{
    DEFAULT_CHAR_TEXTS, DEFAULT_STYLE_TEXTS, default_char_style_docx, default_style_rtl_docx,
};

fn top(idx: u32) -> BridgeBlockPath {
    BridgeBlockPath::top(idx)
}

fn cell(table: u32, row: u32, col: u32) -> BridgeBlockPath {
    BridgeBlockPath {
        steps: vec![
            BridgePathStep::Block { idx: table },
            BridgePathStep::Cell { row, col },
            BridgePathStep::Block { idx: 0 },
        ],
    }
}

fn pos(path: BridgeBlockPath, offset: u32) -> BridgeLogicalPos {
    BridgeLogicalPos { path, offset }
}

fn ok(e: &mut Engine, cmd: Command) -> Event {
    let evt = e.apply_sync(cmd);
    assert!(!matches!(evt, Event::Error { .. }), "{evt:?}");
    evt
}

fn place(e: &mut Engine, at: BridgeLogicalPos) {
    ok(
        e,
        Command::SetSelection {
            range: BridgeLogicalRange {
                start: at.clone(),
                end: at.clone(),
            },
            caret: at,
        },
    );
}

fn open(bytes: Vec<u8>) -> Engine {
    let mut e = Engine::new_headless(DocumentTree::new());
    let evt = e.apply_sync(Command::OpenDocument {
        bytes,
        format: DocFormat::Docx,
        name: None,
        defaults: None,
        limits: None,
        password: None,
    });
    assert!(matches!(evt, Event::DocumentLoaded { .. }), "{evt:?}");
    e
}

/// Every paragraph's resolved properties, in document order (cells at
/// every depth included).
fn all_props(doc: &DocumentTree) -> Vec<(String, engine::ParaProperties)> {
    let mut out = Vec::new();
    engine::for_each_paragraph_nested(doc.blocks.iter(), &mut |_, p| {
        out.push((p.text.clone(), p.props.clone()));
    });
    out
}

/// Insert a 2 × 2 table after paragraph 2 through the toolbar's command,
/// type into its first cell, press Enter there and type again, add a row
/// below, then press Enter at the end of the document and type.
fn edit(mut e: Engine, paragraphs: u32) -> Engine {
    ok(
        &mut e,
        Command::InsertTable {
            at: top(3),
            rows: 2,
            cols: 2,
        },
    );
    place(&mut e, pos(cell(3, 0, 0), 0));
    ok(
        &mut e,
        Command::InsertText {
            at: None,
            text: "cell text".into(),
        },
    );
    ok(&mut e, Command::SplitParagraph { at: None });
    ok(
        &mut e,
        Command::InsertText {
            at: None,
            text: "second line".into(),
        },
    );
    ok(
        &mut e,
        Command::InsertRow {
            table_path: top(3),
            row: 1,
            side: bridge::InsertSide::After,
        },
    );
    /* The table pushed the source paragraphs after it down by one. */
    let last = paragraphs;
    let len = e
        .undo
        .current()
        .paragraph_at_path(&bridge_to_engine_path(top(last)))
        .map_or(0, |p| p.text.len() as u32);
    place(&mut e, pos(top(last), len));
    ok(&mut e, Command::SplitParagraph { at: None });
    ok(
        &mut e,
        Command::InsertText {
            at: None,
            text: "after".into(),
        },
    );
    e
}

#[test]
fn inserted_tables_and_enter_lay_out_as_the_saved_package_reopens() {
    for (name, fixture, paragraphs) in [
        (
            "default_style_rtl",
            default_style_rtl_docx(),
            DEFAULT_STYLE_TEXTS.len() as u32,
        ),
        (
            "default_char_style",
            default_char_style_docx(),
            DEFAULT_CHAR_TEXTS.len() as u32,
        ),
    ] {
        let mut e = edit(open(fixture), paragraphs);
        let doc = e.undo.current().clone();
        let cascade = doc.resolve_style_cascade(None);
        for (row, col) in [(0, 1), (1, 0), (2, 1)] {
            let p = doc
                .paragraph_at_path(&bridge_to_engine_path(cell(3, row, col)))
                .unwrap_or_else(|| panic!("{name}: cell ({row}, {col})"));
            assert_eq!(
                p.props, cascade,
                "{name}: minted cell ({row}, {col}) carries docDefaults + Normal"
            );
        }
        let (typed, ..) = e.build_pages(1.0, false, None).expect("layout");

        let Event::DocumentSaved { bytes, .. } = e.apply_sync(Command::SaveDocx) else {
            panic!("{name}: SaveDocx did not answer DocumentSaved");
        };
        let reopened = open(bytes);
        let back = reopened.undo.current();
        assert_eq!(
            all_props(back),
            all_props(&doc),
            "{name}: every paragraph re-reads to the properties it had"
        );
        let (again, ..) = reopened.build_pages(1.0, false, None).expect("layout");
        assert_eq!(again.len(), typed.len(), "{name}: page count moved");
        assert_eq!(
            layout::geometry_fingerprint(&again),
            layout::geometry_fingerprint(&typed),
            "{name}: insert table + Enter -> SaveDocx -> reopen changed the geometry"
        );
    }
}
