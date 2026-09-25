//! Issue #203 — footnote and endnote stories in the accessibility mirror.
//!
//! Tree shape: a footnote is an `A11yNode::Note` region (`note_kind: Footnote`)
//! placed right AFTER the paragraph holding its first reference (after that
//! paragraph's text-box regions), in the same node list — top level for a
//! body paragraph, the cell's `nodes` for a cell paragraph. Endnotes are
//! `A11yNode::Note` regions (`note_kind: Endnote`) appended at the very END of
//! the top-level list in first-reference order; the mirror wraps that
//! contiguous suffix in one `role="doc-endnotes"` section. The reference
//! mark is an `A11yRun` carrying `note_ref` (the region id) with the note's
//! display marker as its text; a note body's self-mark reads as its marker.
//! Each note is its own node, so an edit inside it is one region `Update`.

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
    e.selection = Some(SelectionState {
        anchor: bpos_top(0, 0),
        caret: bpos_top(0, 0),
        ideal_x: None,
        kind: SelectionKind::Linear,
    });
    e.review_date = "2026-01-01T00:00:00Z".into();
    e
}

fn doc_of(texts: &[&str]) -> DocumentTree {
    let mut d = DocumentTree::new();
    d.blocks = texts
        .iter()
        .map(|t| {
            engine::Block::Paragraph(engine::Paragraph {
                text: (*t).to_owned(),
                ..Default::default()
            })
        })
        .collect();
    d
}

fn type_text(e: &mut Engine, text: &str) {
    apply(
        e,
        Command::InsertText {
            at: None,
            text: text.into(),
        },
    );
}

/// Insert a note of `kind` at body `(block, offset)`, type `text` into it,
/// and return to the body.
fn add_note(e: &mut Engine, kind: engine::NoteKind, block: u32, offset: u32, text: &str) {
    let at = bpos_top(block, offset);
    let cmd = match kind {
        engine::NoteKind::Footnote => Command::InsertFootnote { at },
        engine::NoteKind::Endnote => Command::InsertEndnote { at },
    };
    apply(e, cmd);
    type_text(e, text);
    apply(e, Command::ExitHeaderFooter);
}

fn run_texts(p: &A11yNode) -> Vec<String> {
    let A11yNode::Paragraph(p) = p else {
        panic!("expected a paragraph, got {p:?}");
    };
    p.runs.iter().map(|r| r.text.clone()).collect()
}

fn note_of(n: &A11yNode) -> &bridge::A11yNote {
    let A11yNode::Note(note) = n else {
        panic!("expected a note region, got {n:?}");
    };
    note
}

#[test]
fn a_footnote_region_follows_its_reference_paragraph() {
    let mut e = engine_with(doc_of(&["Alpha body text", "Second paragraph"]));
    add_note(&mut e, engine::NoteKind::Footnote, 0, 5, "note text");
    let nodes = e.build_a11y_nodes();
    assert_eq!(nodes.len(), 3, "{nodes:#?}");

    /* The reference mark is its own run: marker text + a link to the
    region; the U+FFFC placeholder never reaches the mirror. */
    assert_eq!(run_texts(&nodes[0]), vec!["Alpha", "1", " body text"]);
    let A11yNode::Paragraph(anchor) = &nodes[0] else {
        unreachable!()
    };
    let noteref = anchor.runs[1].note_ref.as_ref().expect("noteref run");
    assert_eq!(noteref.kind, bridge::A11yNoteKind::Footnote);
    assert_eq!(noteref.id, "footnote-1");
    assert!(anchor.runs[0].note_ref.is_none() && anchor.runs[2].note_ref.is_none());

    let note = note_of(&nodes[1]);
    assert_eq!(note.note_kind, bridge::A11yNoteKind::Footnote);
    assert_eq!(note.id, "footnote-1");
    assert_eq!(note.note_id, 1);
    assert_eq!(note.marker, "1");
    assert_eq!(note.nodes.len(), 1);
    /* The self-mark reads as the marker, as a plain run. */
    assert_eq!(run_texts(&note.nodes[0]), vec!["1", " note text"]);
    let A11yNode::Paragraph(np) = &note.nodes[0] else {
        unreachable!()
    };
    assert!(np.runs.iter().all(|r| r.note_ref.is_none()));

    assert_eq!(run_texts(&nodes[2]), vec!["Second paragraph"]);
}

#[test]
fn endnotes_collect_at_the_end_of_the_tree_in_reference_order() {
    let mut e = engine_with(doc_of(&["First para", "Second para"]));
    add_note(&mut e, engine::NoteKind::Endnote, 1, 6, "later note");
    add_note(&mut e, engine::NoteKind::Endnote, 0, 5, "earlier note");
    add_note(&mut e, engine::NoteKind::Footnote, 1, 0, "a footnote");
    let nodes = e.build_a11y_nodes();
    let kinds: Vec<&str> = nodes
        .iter()
        .map(|n| match n {
            A11yNode::Paragraph(_) => "p",
            A11yNode::Note(n) if n.note_kind == bridge::A11yNoteKind::Footnote => "fn",
            A11yNode::Note(_) => "en",
            _ => "other",
        })
        .collect();
    assert_eq!(kinds, vec!["p", "p", "fn", "en", "en"], "{nodes:#?}");

    /* Endnote w:id 2 is referenced first in the body, so it leads the
    section and carries the first marker. */
    let first = note_of(&nodes[3]);
    let second = note_of(&nodes[4]);
    assert_eq!((first.note_id, second.note_id), (2, 1));
    assert_eq!((first.marker.as_str(), second.marker.as_str()), ("1", "2"));
    assert!(run_texts(&first.nodes[0]).concat().contains("earlier note"));

    /* Endnote references link to the endnote region ids. */
    let A11yNode::Paragraph(p0) = &nodes[0] else {
        unreachable!()
    };
    let r = p0
        .runs
        .iter()
        .find_map(|r| r.note_ref.as_ref())
        .expect("endnote ref");
    assert_eq!(
        (r.kind, r.id.as_str()),
        (bridge::A11yNoteKind::Endnote, "endnote-2")
    );
}

#[test]
fn a_footnote_edit_is_one_region_update() {
    let mut e = engine_with(doc_of(&["Alpha body text", "Second paragraph"]));
    add_note(&mut e, engine::NoteKind::Footnote, 0, 5, "note");
    /* A second footnote on the second paragraph; the caret stays in it. */
    apply(&mut e, Command::InsertFootnote { at: bpos_top(1, 0) });
    let _ = e.build_a11y_delta();
    type_text(&mut e, "x");
    let patches = e.build_a11y_delta();
    let [A11yPatch::Update { index, node }] = patches.as_slice() else {
        panic!("{patches:#?}");
    };
    let note = note_of(node);
    assert_eq!(note.id, "footnote-2");
    assert_eq!(*index, 3, "right after the second paragraph");
    assert!(run_texts(&note.nodes[0]).concat().ends_with('x'));
}

#[test]
fn an_endnote_edit_is_one_region_update() {
    let mut e = engine_with(doc_of(&["Alpha body text"]));
    add_note(&mut e, engine::NoteKind::Endnote, 0, 5, "end");
    apply(&mut e, Command::InsertEndnote { at: bpos_top(0, 0) });
    let len = e.build_a11y_nodes().len();
    let _ = e.build_a11y_delta();
    type_text(&mut e, "y");
    let patches = e.build_a11y_delta();
    let [A11yPatch::Update { index, node }] = patches.as_slice() else {
        panic!("{patches:#?}");
    };
    assert_eq!(note_of(node).id, "endnote-2");
    assert_eq!(*index as usize, len - 2, "the first endnote in the section");
}

#[test]
fn note_paragraphs_resolve_their_own_direction() {
    let mut e = engine_with(doc_of(&["Latin anchor"]));
    add_note(&mut e, engine::NoteKind::Footnote, 0, 5, "مرحبا بالعالم");
    let nodes = e.build_a11y_nodes();
    let A11yNode::Paragraph(anchor) = &nodes[0] else {
        panic!("{nodes:?}");
    };
    assert_eq!(anchor.resolved_direction, Direction::Ltr);
    let note = note_of(&nodes[1]);
    let A11yNode::Paragraph(np) = &note.nodes[0] else {
        panic!("{note:?}");
    };
    /* The self-mark is neutral; the Arabic text is the first strong
    character, so the note paragraph resolves RTL on its own. */
    assert_eq!(np.resolved_direction, Direction::Rtl);
}

#[test]
fn a_footnote_in_a_table_cell_follows_the_cell_paragraph() {
    let mut e = engine_with(doc_of(&["Alpha"]));
    add_note(&mut e, engine::NoteKind::Footnote, 0, 5, "cell note");
    /* Move the referencing paragraph into a 1×1 table cell. */
    let doc = e.undo.current().clone();
    let engine::Block::Paragraph(p) = doc.blocks[0].clone() else {
        unreachable!()
    };
    let mut row = engine::TableRow::default();
    row.cells.push(engine::TableCell {
        blocks: vec![engine::Block::Paragraph(p)],
        ..Default::default()
    });
    let mut table = engine::Table::default();
    table.rows.push(row);
    let mut next = doc.clone();
    next.blocks = std::iter::once(engine::Block::Table(table)).collect();
    e.undo = UndoStack::new(next, 100);

    let nodes = e.build_a11y_nodes();
    assert_eq!(nodes.len(), 1, "no top-level footnote region: {nodes:#?}");
    let A11yNode::Table(t) = &nodes[0] else {
        panic!("{nodes:?}");
    };
    let cell_nodes = &t.rows[0].cells[0].nodes;
    assert_eq!(cell_nodes.len(), 2, "{cell_nodes:#?}");
    assert_eq!(note_of(&cell_nodes[1]).id, "footnote-1");
}

#[test]
fn entering_a_note_announces_it() {
    let mut e = engine_with(doc_of(&["Alpha body text"]));
    add_note(&mut e, engine::NoteKind::Footnote, 0, 5, "n");
    e.pending_announcements.clear();
    apply(&mut e, Command::InsertFootnote { at: bpos_top(0, 0) });
    assert!(
        e.pending_announcements
            .iter()
            .any(|(_, m)| m == "Editing footnote 1"),
        "{:?}",
        e.pending_announcements
    );
}
