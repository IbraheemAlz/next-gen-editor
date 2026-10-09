//! Issues #301 / #298 — structural tracked edits end to end through
//! `Engine::apply`: Enter with review mode on records an inserted
//! paragraph mark, a delete across paragraph marks records deleted marks,
//! and Accept / Reject (all, or the single mark row) resolve them as one
//! undo step each.

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

fn seed_layout(e: &mut Engine) {
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
}

fn apply(e: &mut Engine, cmd: Command) -> Event {
    let evt = block_on(e.apply(cmd));
    assert!(!matches!(evt, Event::Error { .. }), "{evt:?}");
    evt
}

/// A laid-out engine over `paras`, review mode ON, caret at `(0, 0)`.
fn tracking_engine(paras: &[&str]) -> Engine {
    let doc = DocumentTree::from_paragraphs(paras.iter().map(|s| s.to_string()));
    let mut e = assemble_engine(None, None);
    seed_layout(&mut e);
    e.undo = UndoStack::new(doc, 100);
    caret(&mut e, 0, 0);
    apply(&mut e, Command::ToggleTrackChanges { enabled: true });
    e
}

fn caret(e: &mut Engine, block: u32, offset: u32) {
    select(e, (block, offset), (block, offset));
}

fn select(e: &mut Engine, anchor: (u32, u32), caret: (u32, u32)) {
    e.selection = Some(SelectionState {
        anchor: bpos_top(anchor.0, anchor.1),
        caret: bpos_top(caret.0, caret.1),
        ideal_x: None,
        kind: SelectionKind::Linear,
    });
}

fn texts(e: &Engine) -> Vec<String> {
    e.undo
        .current()
        .blocks
        .iter()
        .filter_map(engine::Block::as_paragraph)
        .map(|p| p.text.clone())
        .collect()
}

fn mark_kinds(e: &Engine) -> Vec<Vec<engine::RevisionKind>> {
    e.undo
        .current()
        .blocks
        .iter()
        .filter_map(engine::Block::as_paragraph)
        .map(|p| p.mark_revisions.iter().map(|r| r.kind).collect())
        .collect()
}

fn caret_pos(e: &Engine) -> (u32, u32) {
    let c = &e.selection.as_ref().expect("selection").caret;
    (c.path.last_block_index().unwrap_or(0), c.offset)
}

fn selection_valid(e: &Engine) -> bool {
    match &e.selection {
        None => true,
        Some(sel) => e.with_selection_doc(|d| {
            clamp_pos(d, sel.anchor.clone()) == sel.anchor
                && clamp_pos(d, sel.caret.clone()) == sel.caret
        }),
    }
}

/* ======================= issue #301 — tracked Enter ==== */

#[test]
fn tracked_enter_marks_the_new_paragraph_mark_inserted() {
    let mut e = tracking_engine(&["alpha beta"]);
    caret(&mut e, 0, 5);
    let depth = e.undo.depth();
    apply(&mut e, Command::SplitParagraph { at: None });
    assert_eq!(e.undo.depth(), depth + 1);
    assert_eq!(texts(&e), vec!["alpha", " beta"]);
    assert_eq!(
        mark_kinds(&e),
        vec![vec![engine::RevisionKind::Insert], vec![]]
    );
    let mark = &e.undo.current().nth_paragraph(0).unwrap().mark_revisions[0];
    assert_eq!(mark.author, e.review_author);
    assert_eq!(caret_pos(&e), (1, 0));
    /* Reject-all merges the halves back as ONE undo step. */
    apply(&mut e, Command::RejectAllRevisions);
    assert_eq!(texts(&e), vec!["alpha beta"]);
    assert!(!e.undo.current().has_revisions());
    assert!(selection_valid(&e));
    apply(&mut e, Command::Undo);
    assert_eq!(texts(&e), vec!["alpha", " beta"]);
    /* Accept-all keeps the break. */
    apply(&mut e, Command::AcceptAllRevisions);
    assert_eq!(texts(&e), vec!["alpha", " beta"]);
    assert!(!e.undo.current().has_revisions());
}

#[test]
fn rejecting_the_mark_row_merges_a_tracked_enter() {
    let mut e = tracking_engine(&["alpha beta"]);
    caret(&mut e, 0, 5);
    apply(&mut e, Command::SplitParagraph { at: None });
    apply(
        &mut e,
        Command::RejectRevision {
            block: 0,
            start: 5,
            end: 5,
            revision_id: None,
        },
    );
    assert_eq!(texts(&e), vec!["alpha beta"]);
    assert!(selection_valid(&e));
}

#[test]
fn tracked_enter_over_a_selection_marks_it_deleted_then_breaks() {
    let mut e = tracking_engine(&["alpha beta gamma"]);
    select(&mut e, (0, 6), (0, 10));
    apply(&mut e, Command::SplitParagraph { at: None });
    /* The selection stays (struck), the break lands at its start. */
    assert_eq!(texts(&e), vec!["alpha ", "beta gamma"]);
    assert_eq!(
        mark_kinds(&e),
        vec![vec![engine::RevisionKind::Insert], vec![]]
    );
    let p1 = e.undo.current().nth_paragraph(1).unwrap();
    assert_eq!(
        p1.revisions
            .iter()
            .map(|r| (r.kind, r.start, r.end))
            .collect::<Vec<_>>(),
        vec![(engine::RevisionKind::Delete, 0, 4)]
    );
    apply(&mut e, Command::RejectAllRevisions);
    assert_eq!(texts(&e), vec!["alpha beta gamma"]);
    assert!(!e.undo.current().has_revisions());
}

#[test]
fn enter_with_review_mode_off_stays_plain() {
    let mut e = tracking_engine(&["alpha beta"]);
    apply(&mut e, Command::ToggleTrackChanges { enabled: false });
    caret(&mut e, 0, 5);
    apply(&mut e, Command::SplitParagraph { at: None });
    assert_eq!(texts(&e), vec!["alpha", " beta"]);
    assert!(!e.undo.current().has_revisions());
}

/* ======================= issue #298 — cross-paragraph tracked delete ==== */

fn delete_at_caret(e: &mut Engine, forward: bool) -> Event {
    apply(
        e,
        Command::DeleteAtCaret {
            forward,
            by_word: false,
        },
    )
}

#[test]
fn deleting_a_selection_across_paragraphs_marks_it_instead_of_doing_nothing() {
    let mut e = tracking_engine(&["alpha beta", "middle", "gamma delta"]);
    select(&mut e, (0, 6), (2, 6));
    let depth = e.undo.depth();
    delete_at_caret(&mut e, false);
    assert_eq!(e.undo.depth(), depth + 1, "one undo step");
    assert_eq!(texts(&e), vec!["alpha beta", "middle", "gamma delta"]);
    assert_eq!(
        mark_kinds(&e),
        vec![
            vec![engine::RevisionKind::Delete],
            vec![engine::RevisionKind::Delete],
            vec![]
        ]
    );
    assert_eq!(
        caret_pos(&e),
        (2, 6),
        "the caret lands after the struck text"
    );
    apply(&mut e, Command::AcceptAllRevisions);
    assert_eq!(texts(&e), vec!["alpha delta"]);
    assert!(selection_valid(&e));
    apply(&mut e, Command::Undo);
    apply(&mut e, Command::RejectAllRevisions);
    assert_eq!(texts(&e), vec!["alpha beta", "middle", "gamma delta"]);
    assert!(!e.undo.current().has_revisions());
}

#[test]
fn backspace_at_a_paragraph_start_marks_the_mark_and_steps_left() {
    let mut e = tracking_engine(&["one", "two"]);
    caret(&mut e, 1, 0);
    delete_at_caret(&mut e, false);
    assert_eq!(texts(&e), vec!["one", "two"]);
    assert_eq!(
        mark_kinds(&e),
        vec![vec![engine::RevisionKind::Delete], vec![]]
    );
    /* Word-like: the caret steps over the struck mark, so the next
    Backspace reaches the character before it (not the same mark). */
    assert_eq!(caret_pos(&e), (0, 3));
    delete_at_caret(&mut e, false);
    assert_eq!(caret_pos(&e), (0, 2));
    let p0 = e.undo.current().nth_paragraph(0).unwrap();
    assert_eq!(
        p0.revisions
            .iter()
            .map(|r| (r.kind, r.start, r.end))
            .collect::<Vec<_>>(),
        vec![(engine::RevisionKind::Delete, 2, 3)]
    );
    apply(&mut e, Command::AcceptAllRevisions);
    assert_eq!(texts(&e), vec!["ontwo"]);
}

#[test]
fn forward_delete_at_a_paragraph_end_marks_the_mark() {
    let mut e = tracking_engine(&["one", "two"]);
    caret(&mut e, 0, 3);
    delete_at_caret(&mut e, true);
    assert_eq!(
        mark_kinds(&e),
        vec![vec![engine::RevisionKind::Delete], vec![]]
    );
    assert_eq!(caret_pos(&e), (1, 0));
    apply(
        &mut e,
        Command::RejectRevision {
            block: 0,
            start: 3,
            end: 3,
            revision_id: None,
        },
    );
    assert_eq!(texts(&e), vec!["one", "two"]);
    assert!(!e.undo.current().has_revisions());
}

#[test]
fn backspace_over_your_own_tracked_enter_removes_it() {
    let mut e = tracking_engine(&["alpha beta"]);
    caret(&mut e, 0, 5);
    apply(&mut e, Command::SplitParagraph { at: None });
    assert_eq!(caret_pos(&e), (1, 0));
    delete_at_caret(&mut e, false);
    assert_eq!(texts(&e), vec!["alpha beta"]);
    assert!(!e.undo.current().has_revisions());
    assert_eq!(caret_pos(&e), (0, 5));
}

#[test]
fn typing_over_a_cross_paragraph_selection_marks_and_inserts() {
    let mut e = tracking_engine(&["one", "two"]);
    select(&mut e, (0, 1), (1, 1));
    apply(
        &mut e,
        Command::InsertText {
            at: None,
            text: "X".into(),
        },
    );
    assert_eq!(texts(&e), vec!["oXne", "two"]);
    apply(&mut e, Command::AcceptAllRevisions);
    assert_eq!(texts(&e), vec!["oXwo"]);
}

/* ======================= issue #365 — tracked table rows ==== */

/// `["before", <2 × 2 table "r{r}c{c}">, "after"]`, review mode on.
fn table_engine() -> Engine {
    let mut doc = DocumentTree::from_paragraphs(["before".to_string(), "after".to_string()])
        .insert_table(engine::BlockPath::top(1), 2, 2);
    for r in 0..2 {
        for c in 0..2 {
            let at = engine::LogicalPos::new(bridge_to_engine_path(cell_path(r, c)), 0);
            doc = doc.insert_text(at, &format!("r{r}c{c}"));
        }
    }
    let mut e = tracking_engine(&["x"]);
    e.undo = UndoStack::new(doc, 100);
    caret(&mut e, 0, 0);
    e
}

/// The bridge path of cell `(row, col)`'s first paragraph of the table at
/// block 1.
fn cell_path(row: u32, col: u32) -> BridgeBlockPath {
    BridgeBlockPath {
        steps: vec![
            BridgePathStep::Block { idx: 1 },
            BridgePathStep::Cell { row, col },
            BridgePathStep::Block { idx: 0 },
        ],
    }
}

fn row_kinds(e: &Engine) -> Vec<Vec<engine::RevisionKind>> {
    e.undo
        .current()
        .table_at_path(&engine::BlockPath::top(1))
        .map(|t| {
            t.rows
                .iter()
                .map(|r| r.props.revisions.iter().map(|x| x.kind).collect())
                .collect()
        })
        .unwrap_or_default()
}

fn shape(e: &Engine) -> Vec<String> {
    e.undo
        .current()
        .blocks
        .iter()
        .map(|b| match b {
            engine::Block::Paragraph(p) => p.text.clone(),
            engine::Block::Table(t) => format!("<table {}>", t.rows.len()),
        })
        .collect()
}

#[test]
fn a_tracked_delete_over_a_table_marks_its_rows() {
    let mut e = table_engine();
    let depth = e.undo.depth();
    apply(
        &mut e,
        Command::DeleteRange {
            range: BridgeLogicalRange {
                start: bpos_top(0, 2),
                end: bpos_top(2, 2),
            },
        },
    );
    assert_eq!(e.undo.depth(), depth + 1, "one undo step");
    assert_eq!(shape(&e), vec!["before", "<table 2>", "after"]);
    assert_eq!(row_kinds(&e), vec![vec![DEL], vec![DEL]]);
    apply(&mut e, Command::AcceptAllRevisions);
    assert_eq!(shape(&e), vec!["beter"]);
    assert!(selection_valid(&e));
    apply(&mut e, Command::Undo);
    apply(&mut e, Command::RejectAllRevisions);
    assert_eq!(shape(&e), vec!["before", "<table 2>", "after"]);
    assert!(!e.undo.current().has_revisions());
}

/// Select two rows (start of the first cell to the end of the last),
/// Delete: both rows are marked deleted; the review rows list each under
/// its own id; accepting one by id removes exactly that row.
#[test]
fn deleting_two_selected_rows_marks_them_and_accept_removes_them() {
    let mut e = table_engine();
    e.selection = Some(SelectionState {
        anchor: BridgeLogicalPos {
            path: cell_path(0, 0),
            offset: 0,
        },
        caret: BridgeLogicalPos {
            path: cell_path(1, 1),
            offset: 4,
        },
        ideal_x: None,
        kind: SelectionKind::Linear,
    });
    delete_at_caret(&mut e, true);
    assert_eq!(row_kinds(&e), vec![vec![DEL], vec![DEL]]);
    let rows: Vec<_> = revision_rows(e.undo.current())
        .into_iter()
        .filter(|r| r.row.is_some())
        .collect();
    assert_eq!(
        rows.iter()
            .map(|r| (r.block, r.row, r.kind, r.start, r.end))
            .collect::<Vec<_>>(),
        vec![(1, Some(0), "delete", 0, 0), (1, Some(1), "delete", 0, 0)]
    );
    apply(
        &mut e,
        Command::AcceptRevision {
            block: 1,
            start: 0,
            end: 0,
            revision_id: Some(rows[0].revision_id),
        },
    );
    assert_eq!(shape(&e), vec!["before", "<table 1>", "after"]);
    assert_eq!(row_kinds(&e), vec![vec![DEL]]);
    assert!(selection_valid(&e));
    apply(&mut e, Command::AcceptAllRevisions);
    assert_eq!(shape(&e), vec!["before", "after"]);
    assert!(selection_valid(&e));
}

/// Across two cells of one row: each cell's sub-range, no row change.
#[test]
fn deleting_across_cells_of_a_row_marks_each_cell() {
    let mut e = table_engine();
    e.selection = Some(SelectionState {
        anchor: BridgeLogicalPos {
            path: cell_path(1, 0),
            offset: 2,
        },
        caret: BridgeLogicalPos {
            path: cell_path(1, 1),
            offset: 2,
        },
        ideal_x: None,
        kind: SelectionKind::Linear,
    });
    delete_at_caret(&mut e, false);
    assert_eq!(row_kinds(&e), vec![vec![], vec![]]);
    apply(&mut e, Command::AcceptAllRevisions);
    let t = e
        .undo
        .current()
        .table_at_path(&engine::BlockPath::top(1))
        .unwrap()
        .clone();
    let texts: Vec<&str> = t.rows[1]
        .cells
        .iter()
        .filter_map(|c| c.blocks[0].as_paragraph().map(|p| p.text.as_str()))
        .collect();
    assert_eq!(texts, vec!["r1", "c1"]);
}

/* ======================= issue #366 — paste + IME commit ==== */

fn text_revs(e: &Engine) -> Vec<Vec<(engine::RevisionKind, u32, u32)>> {
    e.undo
        .current()
        .blocks
        .iter()
        .filter_map(engine::Block::as_paragraph)
        .map(|p| {
            let mut v: Vec<_> = p
                .revisions
                .iter()
                .map(|r| (r.kind, r.start, r.end))
                .collect();
            v.sort_by_key(|r| (r.1, r.2));
            v
        })
        .collect()
}

const INS: engine::RevisionKind = engine::RevisionKind::Insert;
const DEL: engine::RevisionKind = engine::RevisionKind::Delete;

#[test]
fn a_multiline_plain_paste_is_tracked_and_rejects_to_the_original() {
    let mut e = tracking_engine(&["alpha beta"]);
    caret(&mut e, 0, 5);
    let depth = e.undo.depth();
    apply(
        &mut e,
        Command::PastePlain {
            text: "one\r\ntwo".into(),
        },
    );
    assert_eq!(e.undo.depth(), depth + 1, "one undo step");
    assert_eq!(texts(&e), vec!["alphaone", "two beta"]);
    assert_eq!(text_revs(&e), vec![vec![(INS, 5, 8)], vec![(INS, 0, 3)]]);
    assert_eq!(mark_kinds(&e), vec![vec![INS], vec![]]);
    let mark = &e.undo.current().nth_paragraph(0).unwrap().mark_revisions[0];
    assert_eq!(mark.author, e.review_author);
    assert_eq!(caret_pos(&e), (1, 3));
    apply(&mut e, Command::RejectAllRevisions);
    assert_eq!(texts(&e), vec!["alpha beta"]);
    assert!(!e.undo.current().has_revisions());
    assert!(selection_valid(&e));
    apply(&mut e, Command::Undo);
    apply(&mut e, Command::AcceptAllRevisions);
    assert_eq!(texts(&e), vec!["alphaone", "two beta"]);
    assert!(!e.undo.current().has_revisions());
}

#[test]
fn a_single_line_plain_paste_is_a_tracked_insertion() {
    let mut e = tracking_engine(&["alpha beta"]);
    caret(&mut e, 0, 6);
    apply(
        &mut e,
        Command::PastePlain {
            text: "new ".into(),
        },
    );
    assert_eq!(texts(&e), vec!["alpha new beta"]);
    assert_eq!(text_revs(&e), vec![vec![(INS, 6, 10)]]);
    apply(&mut e, Command::RejectAllRevisions);
    assert_eq!(texts(&e), vec!["alpha beta"]);
}

/// Pasting over a selection: the selection is marked deleted first — the
/// reviewer's own pending insertion inside it is removed outright (#265)
/// — then the paste lands at its start as an insertion.
#[test]
fn pasting_over_a_selection_marks_it_deleted_and_drops_your_own_insertion() {
    let mut e = tracking_engine(&["alpha beta"]);
    caret(&mut e, 0, 6);
    apply(
        &mut e,
        Command::InsertText {
            at: None,
            text: "XX".into(),
        },
    );
    assert_eq!(texts(&e), vec!["alpha XXbeta"]);
    select(&mut e, (0, 6), (0, 12));
    apply(
        &mut e,
        Command::PastePlain {
            text: "1\n2".into(),
        },
    );
    /* "XX" (own) is gone; "beta" is struck behind the paste. */
    assert_eq!(texts(&e), vec!["alpha 1", "2beta"]);
    assert_eq!(
        text_revs(&e),
        vec![vec![(INS, 6, 7)], vec![(INS, 0, 1), (DEL, 1, 5)]]
    );
    assert_eq!(mark_kinds(&e), vec![vec![INS], vec![]]);
    apply(&mut e, Command::RejectAllRevisions);
    assert_eq!(texts(&e), vec!["alpha beta"]);
    apply(&mut e, Command::Undo);
    apply(&mut e, Command::AcceptAllRevisions);
    assert_eq!(texts(&e), vec!["alpha 1", "2"]);
}

#[test]
fn an_html_paste_is_tracked_and_rejects_to_the_original() {
    let mut e = tracking_engine(&["alpha beta"]);
    caret(&mut e, 0, 5);
    apply(
        &mut e,
        Command::PasteHtml {
            html: "<p>one</p><p>two</p>".into(),
        },
    );
    assert_eq!(texts(&e), vec!["alphaone", "two beta"]);
    assert_eq!(text_revs(&e), vec![vec![(INS, 5, 8)], vec![(INS, 0, 3)]]);
    assert_eq!(mark_kinds(&e), vec![vec![INS], vec![]]);
    apply(&mut e, Command::RejectAllRevisions);
    assert_eq!(texts(&e), vec!["alpha beta"]);
    assert!(!e.undo.current().has_revisions());
    apply(&mut e, Command::Undo);
    apply(&mut e, Command::AcceptAllRevisions);
    assert_eq!(texts(&e), vec!["alphaone", "two beta"]);
}

#[test]
fn an_html_paste_over_a_selection_marks_it_deleted() {
    let mut e = tracking_engine(&["alpha beta"]);
    select(&mut e, (0, 0), (0, 5));
    apply(
        &mut e,
        Command::PasteHtml {
            html: "<p>A</p><p>B</p>".into(),
        },
    );
    assert_eq!(texts(&e), vec!["A", "Balpha beta"]);
    assert_eq!(
        text_revs(&e),
        vec![vec![(INS, 0, 1)], vec![(INS, 0, 1), (DEL, 1, 6)]]
    );
    apply(&mut e, Command::AcceptAllRevisions);
    assert_eq!(texts(&e), vec!["A", "B beta"]);
}

#[test]
fn pasting_with_review_mode_off_stays_plain() {
    let mut e = tracking_engine(&["alpha beta"]);
    apply(&mut e, Command::ToggleTrackChanges { enabled: false });
    caret(&mut e, 0, 5);
    apply(
        &mut e,
        Command::PastePlain {
            text: "1\n2".into(),
        },
    );
    apply(
        &mut e,
        Command::PasteHtml {
            html: "<p>3</p><p>4</p>".into(),
        },
    );
    assert_eq!(texts(&e), vec!["alpha1", "23", "4 beta"]);
    assert!(!e.undo.current().has_revisions());
}

/// An IME commit goes through the tracked typing path: the composed text
/// is the reviewer's insertion (one undo step), reject removes it.
#[test]
fn an_ime_commit_is_a_tracked_insertion() {
    let mut e = tracking_engine(&["alpha beta"]);
    caret(&mut e, 0, 6);
    let depth = e.undo.depth();
    apply(&mut e, Command::BeginComposition { at: None });
    apply(
        &mut e,
        Command::UpdateComposition {
            text: "に".into(),
            target_range: None,
        },
    );
    apply(
        &mut e,
        Command::UpdateComposition {
            text: "日本".into(),
            target_range: None,
        },
    );
    assert_eq!(e.undo.depth(), depth, "the preview is no edit");
    apply(&mut e, Command::EndComposition { commit: true });
    assert_eq!(e.undo.depth(), depth + 1);
    assert_eq!(texts(&e), vec!["alpha 日本beta"]);
    assert_eq!(text_revs(&e), vec![vec![(INS, 6, 12)]]);
    assert_eq!(caret_pos(&e), (0, 12));
    apply(&mut e, Command::RejectAllRevisions);
    assert_eq!(texts(&e), vec!["alpha beta"]);
    /* A cancelled composition records nothing. */
    let depth = e.undo.depth();
    apply(&mut e, Command::BeginComposition { at: None });
    apply(
        &mut e,
        Command::UpdateComposition {
            text: "x".into(),
            target_range: None,
        },
    );
    apply(&mut e, Command::EndComposition { commit: false });
    assert_eq!(e.undo.depth(), depth);
    assert!(!e.undo.current().has_revisions());
}

/// An IME commit over a selection marks the selection deleted first.
#[test]
fn an_ime_commit_over_a_selection_marks_it_deleted() {
    let mut e = tracking_engine(&["alpha beta"]);
    select(&mut e, (0, 6), (0, 10));
    apply(&mut e, Command::BeginComposition { at: None });
    apply(
        &mut e,
        Command::UpdateComposition {
            text: "日本".into(),
            target_range: None,
        },
    );
    apply(&mut e, Command::EndComposition { commit: true });
    assert_eq!(texts(&e), vec!["alpha 日本beta"]);
    assert_eq!(text_revs(&e), vec![vec![(INS, 6, 12), (DEL, 12, 16)]]);
    apply(&mut e, Command::AcceptAllRevisions);
    assert_eq!(texts(&e), vec!["alpha 日本"]);
}
