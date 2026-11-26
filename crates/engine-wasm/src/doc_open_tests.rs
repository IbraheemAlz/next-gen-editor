//! Issue #567 — a legacy Word 97-2003 `.doc` opens through the real
//! dispatcher (`Engine::apply_sync`, `fuzz-native`): `OpenDocument` with the
//! `.docx` format the shell sends for every Word file is routed by magic
//! bytes to `format-doc`. The images are built in memory
//! (`format_doc::test_builder`); no binary fixture is committed.

use super::*;
use format_doc::test_builder::{DocSpec, Para, Tc, build, run, sprm, streams};

fn open(engine: &mut Engine, bytes: Vec<u8>, name: &str) -> Event {
    engine.apply_sync(Command::OpenDocument {
        bytes,
        format: DocFormat::Docx,
        name: Some(name.into()),
        defaults: None,
        limits: None,
        password: None,
    })
}

fn paragraph(doc: &DocumentTree, i: usize) -> engine::Paragraph {
    match doc.blocks.get(i) {
        Some(engine::Block::Paragraph(p)) => p.clone(),
        other => panic!("block {i} is not a paragraph: {other:?}"),
    }
}

/// "Hello bold world" (a bold run), an Arabic right-to-left paragraph, a
/// 2x2 table with single borders (issue #576), a list paragraph (its
/// marker is not imported: reported), on a landscape Letter page.
fn sample() -> Vec<u8> {
    let cat = |v: &[Vec<u8>]| v.concat();
    let row_end = || {
        Para::row_end(cat(&[
            sprm::def_table(&[-108, 2772, 5652], &[Tc::default(), Tc::default()]),
            sprm::table_borders80(sprm::brc80(4, 0x01, 0)),
        ]))
    };
    let mut spec = DocSpec::new(vec![
        Para::new(
            vec![
                run("Hello ", Vec::new()),
                run("bold", sprm::bold(true)),
                run(" world", Vec::new()),
            ],
            Vec::new(),
        ),
        Para::new(
            vec![run("مرحبا بالعالم", sprm::rtl_run(true))],
            cat(&[sprm::rtl_paragraph(true), sprm::jc(1)]),
        ),
        Para::cell("A1", Vec::new()),
        Para::cell("B1", Vec::new()),
        row_end(),
        Para::cell("A2", Vec::new()),
        Para::cell("B2", Vec::new()),
        row_end(),
        Para::new(vec![run("listed", Vec::new())], sprm::list(1)),
        Para::plain("tail."),
    ]);
    /* One section over the whole main text. */
    spec.sections = vec![(
        spec.ccp_text(),
        cat(&[
            sprm::page_size(15840, 12240),
            sprm::margins(1080, 1080, 1440, 1440),
        ]),
    )];
    build(&spec)
}

#[test]
fn a_doc_opens_with_its_text_bold_run_rtl_paragraph_and_page_setup() {
    let bytes = sample();
    assert_eq!(&bytes[..8], &format_docx::opc::cfb::CFB_SIGNATURE);
    let mut engine = Engine::new_headless(DocumentTree::from_text("previous"));
    let evt = open(&mut engine, bytes, "C:\\docs\\report.doc");
    let Event::DocumentLoaded {
        paragraph_count,
        warnings,
        ..
    } = evt
    else {
        panic!("expected DocumentLoaded, got {evt:?}");
    };
    assert_eq!(paragraph_count, 4, "the table is one block, not paragraphs");
    /* Honest UX: the list marker that was not imported is reported; the
    table was imported, so it is not (issue #576). */
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert_eq!(
        warnings[0].kind,
        bridge::ReadWarningKind::LegacyContentNotImported
    );
    assert!(
        warnings[0].detail.starts_with("list numbering"),
        "{}",
        warnings[0].detail
    );
    assert!(warnings.iter().all(|w| !w.detail.starts_with("tables")));

    let doc = engine.undo.current().clone();
    let p0 = paragraph(&doc, 0);
    assert_eq!(p0.text, "Hello bold world");
    let bold: Vec<_> = p0
        .spans
        .iter()
        .filter(|s| s.style.bold == Some(true))
        .map(|s| &p0.text[s.start as usize..s.end as usize])
        .collect();
    assert_eq!(bold, vec!["bold"]);
    let p1 = paragraph(&doc, 1);
    assert_eq!(p1.text, "مرحبا بالعالم");
    assert_eq!(p1.props.direction, Some(engine::TextDirection::Rtl));
    assert_eq!(p1.props.alignment, Some(engine::Alignment::Center));
    assert!(p1.spans.iter().all(|s| s.style.forces_complex_script()));
    let Some(engine::Block::Table(t)) = doc.blocks.get(2) else {
        panic!("block 2 is the table: {:?}", doc.blocks.get(2));
    };
    assert_eq!((t.rows.len(), t.grid.clone()), (2, vec![2880, 2880]));
    assert_eq!(paragraph(&doc, 4).text, "tail.");
    assert_eq!(
        (
            doc.body_section.geometry.width,
            doc.body_section.geometry.height
        ),
        (792.0, 612.0),
        "the SEPX page setup, not the host default"
    );
    assert_eq!(doc.body_section.geometry.margin_left, 54.0);
    assert!(doc.source_package.is_none());
    /* A save writes .docx: the name says so (FILENAME, the shell's
    download). */
    assert_eq!(engine.document_name.as_deref(), Some("report.docx"));
}

#[test]
fn a_doc_saves_as_a_docx_that_keeps_text_bold_and_rtl() {
    let mut engine = Engine::new_headless(DocumentTree::from_text(""));
    open(&mut engine, sample(), "report.doc");
    let Event::DocumentSaved { bytes, .. } = engine.apply_sync(Command::SaveDocument {
        format: DocFormat::Docx,
    }) else {
        panic!("the save answers DocumentSaved");
    };
    assert_eq!(&bytes[..2], b"PK", "a .docx package");
    let archive = format_docx::read_docx(&bytes).expect("the saved .docx reads");
    let p0 = paragraph(&archive.document, 0);
    assert_eq!(p0.text, "Hello bold world");
    assert!(p0.spans.iter().any(|s| s.style.bold == Some(true)));
    let p1 = paragraph(&archive.document, 1);
    assert_eq!(p1.props.direction, Some(engine::TextDirection::Rtl));
    assert!(p1.spans.iter().all(|s| s.style.forces_complex_script()));
    assert_eq!(archive.document.body_section.geometry.width, 792.0);
    /* Issue #576 — the table survives the save as a `<w:tbl>`. */
    let Some(engine::Block::Table(t)) = archive.document.blocks.get(2) else {
        panic!("the saved .docx keeps the table");
    };
    assert_eq!(t.grid, vec![2880, 2880]);
    assert_eq!(t.props.indent_twips, -108);
    assert_eq!(t.props.layout, engine::TableLayout::Fixed);
    let texts: Vec<Vec<String>> = t
        .rows
        .iter()
        .map(|r| r.cells.iter().map(|c| plain_of(&c.blocks)).collect())
        .collect();
    assert_eq!(texts, vec![vec!["A1", "B1"], vec!["A2", "B2"]]);
}

/// The text of a cell's paragraphs, one line each.
fn plain_of(blocks: &[engine::Block]) -> String {
    blocks
        .iter()
        .filter_map(engine::Block::as_paragraph)
        .map(|p| p.text.clone())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Issue #576 — the opened document's text has the table's shape: one
/// line per row, its cells separated by tabs, between the paragraphs
/// around it (`DocumentTree::to_plain_text`); a Select All copy carries
/// the table as HTML (`documentText` in `ts/e2e/helpers/editor.ts` reads
/// the clipboard's PLAIN text, which drops a table spanned by a body
/// range — issue #561); the table keeps the model's rules.
#[test]
fn a_doc_table_reads_back_as_rows_of_tab_separated_cells() {
    let mut engine = Engine::new_headless(DocumentTree::from_text(""));
    let evt = open(&mut engine, sample(), "table.doc");
    assert!(matches!(evt, Event::DocumentLoaded { .. }), "{evt:?}");
    assert_eq!(
        engine.undo.current().to_plain_text(),
        "Hello bold world\nمرحبا بالعالم\nA1\tB1\nA2\tB2\nlisted\ntail."
    );
    engine.apply_sync(Command::SelectAll);
    let Event::ClipboardPayload { html, .. } =
        engine.apply_sync(Command::GetSelectionAsClipboard { include_docx: None })
    else {
        panic!("the clipboard answers a payload");
    };
    assert!(html.contains("<table"), "{html}");
    for cell in ["A1", "B1", "A2", "B2"] {
        assert!(html.contains(cell), "{cell} in {html}");
    }
    assert_eq!(engine.undo.current().ragged_table_rows(), 0);
    assert!(engine.check_invariants_for_fuzzing().is_ok());
    assert!(engine.check_point_fields_for_fuzzing().is_ok());
}

#[test]
fn the_dnd_load_docx_path_opens_a_doc_too() {
    let mut engine = Engine::new_headless(DocumentTree::from_text(""));
    let evt = engine.apply_sync(Command::LoadDocx { bytes: sample() });
    assert!(matches!(evt, Event::DocumentLoaded { .. }), "{evt:?}");
    assert_eq!(paragraph(engine.undo.current(), 4).text, "tail.");
}

fn assert_refused(engine: &mut Engine, bytes: Vec<u8>, kind: bridge::ErrorKind) -> String {
    let before = engine.state_fingerprint_for_fuzzing();
    let name_before = engine.document_name.clone();
    let evt = open(engine, bytes, "refused.doc");
    let Event::Error {
        kind: got, message, ..
    } = evt
    else {
        panic!("expected a refusal, got {evt:?}");
    };
    assert_eq!(got, Some(kind), "{message}");
    assert_eq!(engine.state_fingerprint_for_fuzzing(), before, "{message}");
    assert_eq!(engine.document_name, name_before);
    message
}

#[test]
fn encrypted_damaged_and_foreign_compound_files_are_refused_by_kind() {
    let mut engine = Engine::new_headless(DocumentTree::from_text("keep me"));

    let encrypted = build(&DocSpec {
        encrypted: true,
        ..DocSpec::new(vec![Para::plain("zzsecret")])
    });
    let m = assert_refused(&mut engine, encrypted, bridge::ErrorKind::EncryptedDocument);
    assert!(!m.contains("zzsecret"), "{m}");

    /* A piece table cut short: damaged, named by stream and structure. */
    let (word, mut table) = streams(&DocSpec::new(vec![Para::plain("zzsecret")]));
    table[1..5].copy_from_slice(&3u32.to_le_bytes());
    let damaged =
        format_docx::opc::cfb::test_writer::build(&[("WordDocument", &word), ("1Table", &table)]);
    let m = assert_refused(&mut engine, damaged, bridge::ErrorKind::DocumentUnreadable);
    assert!(m.starts_with("OpenDocument: 1Table: Clx: "), "{m}");
    assert!(!m.contains("zzsecret"), "{m}");

    let workbook = format_docx::opc::cfb::test_writer::build(&[("Workbook", &[0u8; 64])]);
    let m = assert_refused(&mut engine, workbook, bridge::ErrorKind::InvalidDocument);
    assert!(m.contains("not a Word document"), "{m}");

    assert_eq!(engine.undo.current().to_plain_text(), "keep me");
}

#[test]
fn legacy_names_become_docx_names() {
    assert_eq!(docx_name_for_legacy("a.doc"), "a.docx");
    assert_eq!(docx_name_for_legacy("A.DOC"), "A.docx");
    assert_eq!(docx_name_for_legacy("t.dot"), "t.docx");
    assert_eq!(docx_name_for_legacy("x.docx"), "x.docx");
    assert_eq!(docx_name_for_legacy(".doc"), ".doc");
    assert_eq!(docx_name_for_legacy("plain"), "plain");
}
