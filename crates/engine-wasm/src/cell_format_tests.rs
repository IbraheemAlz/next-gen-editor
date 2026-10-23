//! Issue #509 - formatting reaches EVERY paragraph of a range that spans
//! cells, rows, a body -> table boundary or a nested table, and a
//! `TABLE_CELLS` selection formats exactly its rectangle. Before, a range
//! whose two ends had different parents styled the first paragraph only
//! (`DocumentTree::apply_style` -> `apply_style_single`), a bullet on a
//! cell paragraph set `list_item` but the marker resolver walked
//! top-level paragraphs only (so no marker ever painted), and every
//! paragraph-level command (alignment, indent, borders, ...) had the same
//! first-paragraph-only cross-container fallback.
//!
//! Gated on `fuzz-native` like the other bridge-level tests that drive real
//! `Command`s through `Engine::apply_sync` (a headless engine with a real
//! font, so layout runs).

use super::*;
use bridge::{
    BridgeBorderStroke, BridgeBorderStyle, BridgeCellBorders, FormattingToggle, ListKind,
};
use engine::{Block, LogicalPos, PathStep};

/// `[Paragraph "head", Table rows x cols, Paragraph "tail"]`.
fn with_table(rows: u32, cols: u32) -> DocumentTree {
    let doc = DocumentTree::from_text("head").insert_table(EngineBlockPath::top(1), rows, cols);
    doc.insert_text(
        LogicalPos {
            path: EngineBlockPath::top(2),
            offset: 0,
        },
        "tail",
    )
}

/// `[head, Table 2 x 2, tail]`: the cells read "A1" "A2" / "B1" "B2".
fn doc_2x2() -> DocumentTree {
    let mut doc = with_table(2, 2);
    for (r, c, text) in [(0, 0, "A1"), (0, 1, "A2"), (1, 0, "B1"), (1, 1, "B2")] {
        doc = doc.insert_text(
            LogicalPos {
                path: cell_path(r, c),
                offset: 0,
            },
            text,
        );
    }
    doc
}

/// `[head, Table 3 x 3 (cells "r<R>c<C>"), tail]`.
fn doc_3x3() -> DocumentTree {
    let mut doc = with_table(3, 3);
    for r in 0..3u32 {
        for c in 0..3u32 {
            doc = doc.insert_text(
                LogicalPos {
                    path: cell_path(r, c),
                    offset: 0,
                },
                &format!("r{r}c{c}"),
            );
        }
    }
    doc
}

fn engine_of(doc: DocumentTree) -> Engine {
    Engine::new_headless(doc)
}

fn cell_path(r: u32, c: u32) -> EngineBlockPath {
    EngineBlockPath {
        steps: vec![
            PathStep::Block(1),
            PathStep::Cell { row: r, col: c },
            PathStep::Block(0),
        ],
    }
}

fn cell_pos(r: u32, c: u32, offset: u32) -> BridgeLogicalPos {
    to_bridge_pos(LogicalPos {
        path: cell_path(r, c),
        offset,
    })
}

fn range(a: BridgeLogicalPos, b: BridgeLogicalPos) -> BridgeLogicalRange {
    BridgeLogicalRange { start: a, end: b }
}

fn ok(e: &mut Engine, cmd: Command) -> Event {
    let evt = e.apply_sync(cmd);
    assert!(!matches!(evt, Event::Error { .. }), "{evt:?}");
    evt
}

/// Anchor at `a`, caret at `b` (what a drag / shift+click leaves).
fn select(e: &mut Engine, a: BridgeLogicalPos, b: BridgeLogicalPos) {
    ok(
        e,
        Command::SetSelection {
            range: range(a, b.clone()),
            caret: b,
        },
    );
}

fn selection_kind(e: &Engine) -> SelectionKind {
    e.selection.as_ref().expect("a selection").kind.clone()
}

fn patch() -> TextAttrsPatch {
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
        font_slot: None,
    }
}

fn format(e: &mut Engine, range: Option<BridgeLogicalRange>, attrs: TextAttrsPatch) {
    ok(e, Command::ApplyFormatting { range, attrs });
}

fn toggle(e: &mut Engine, attr: FormattingToggle) {
    ok(
        e,
        Command::ToggleFormatting {
            attr,
            underline_style: None,
        },
    );
}

fn para<'a>(e: &'a Engine, path: &EngineBlockPath) -> &'a engine::Paragraph {
    e.undo
        .current()
        .paragraph_at_path(path)
        .unwrap_or_else(|| panic!("no paragraph at {path:?}"))
}

fn body(e: &Engine, idx: u32) -> &engine::Paragraph {
    para(e, &EngineBlockPath::top(idx))
}

fn cell(e: &Engine, r: u32, c: u32) -> &engine::Paragraph {
    para(e, &cell_path(r, c))
}

/// Every byte of the paragraph satisfies `pred` on its span style.
fn all_bytes(p: &engine::Paragraph, pred: impl Fn(&SpanStyle) -> bool) -> bool {
    !p.text.is_empty()
        && (0..p.text.len() as u32)
            .filter(|&i| p.text.is_char_boundary(i as usize))
            .all(|i| pred(&p.style_at(i)))
}

/// No byte of the paragraph satisfies `pred`.
fn no_bytes(p: &engine::Paragraph, pred: impl Fn(&SpanStyle) -> bool) -> bool {
    (0..p.text.len() as u32)
        .filter(|&i| p.text.is_char_boundary(i as usize))
        .all(|i| !pred(&p.style_at(i)))
}

fn is_bold(s: &SpanStyle) -> bool {
    s.bold == Some(true)
}

fn is_36(s: &SpanStyle) -> bool {
    s.font_size == Some(36.0)
}

fn bold_patch() -> TextAttrsPatch {
    TextAttrsPatch {
        bold: Some(true),
        ..patch()
    }
}

/* ---------------------------------------------------------------------
Character formatting over a range
--------------------------------------------------------------------- */

#[test]
fn bold_across_two_cells_formats_both_paragraphs() {
    let mut e = engine_of(doc_2x2());
    /* Shift+click from the end of A1 into A2: the engine calls it a
    TABLE_CELLS selection (two cells of one table). */
    select(&mut e, cell_pos(0, 0, 0), cell_pos(0, 1, 2));
    assert!(matches!(
        selection_kind(&e),
        SelectionKind::TableCells { .. }
    ));
    toggle(&mut e, FormattingToggle::Bold);
    assert!(all_bytes(cell(&e, 0, 0), is_bold), "A1");
    assert!(all_bytes(cell(&e, 0, 1), is_bold), "A2");
    assert!(no_bytes(cell(&e, 1, 0), is_bold), "B1 is outside the rectangle");
    assert!(no_bytes(cell(&e, 1, 1), is_bold), "B2 is outside the rectangle");
    assert!(no_bytes(body(&e, 0), is_bold) && no_bytes(body(&e, 2), is_bold));

    /* Every character already bold -> the toggle clears it (Word). */
    toggle(&mut e, FormattingToggle::Bold);
    assert!(no_bytes(cell(&e, 0, 0), is_bold), "A1 cleared");
    assert!(no_bytes(cell(&e, 0, 1), is_bold), "A2 cleared");
}

#[test]
fn toggle_over_a_mixed_range_sets_then_clears_everything() {
    let mut e = engine_of(doc_2x2());
    /* Bold A1 alone, then select A1..A2: A2 is not bold -> mixed -> ON. */
    select(&mut e, cell_pos(0, 0, 0), cell_pos(0, 0, 2));
    toggle(&mut e, FormattingToggle::Bold);
    assert!(all_bytes(cell(&e, 0, 0), is_bold));
    select(&mut e, cell_pos(0, 0, 0), cell_pos(0, 1, 2));
    toggle(&mut e, FormattingToggle::Bold);
    assert!(all_bytes(cell(&e, 0, 0), is_bold), "stays bold");
    assert!(all_bytes(cell(&e, 0, 1), is_bold), "mixed range turns it ON");
    toggle(&mut e, FormattingToggle::Bold);
    assert!(no_bytes(cell(&e, 0, 0), is_bold));
    assert!(no_bytes(cell(&e, 0, 1), is_bold));
}

#[test]
fn table_cells_selection_formats_all_four_cells() {
    let mut e = engine_of(doc_2x2());
    select(&mut e, cell_pos(0, 0, 0), cell_pos(1, 1, 2));
    assert!(matches!(
        selection_kind(&e),
        SelectionKind::TableCells { .. }
    ));
    format(
        &mut e,
        None,
        TextAttrsPatch {
            font_size: Some(36.0),
            ..patch()
        },
    );
    for (r, c) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
        assert!(all_bytes(cell(&e, r, c), is_36), "cell ({r},{c})");
    }
    assert!(no_bytes(body(&e, 0), is_36) && no_bytes(body(&e, 2), is_36));
    /* The selection survives the edit. */
    assert!(matches!(
        selection_kind(&e),
        SelectionKind::TableCells { .. }
    ));
}

#[test]
fn table_cells_selection_is_the_rectangle_not_the_document_order_range() {
    let mut e = engine_of(doc_3x3());
    /* Anchor at the top-right of the 2x2 corner, caret bottom-left: the
    document-order range (0,1)..(1,0) would skip (0,0) and (1,1) and
    swallow (0,2); the rectangle is rows 0..=1 x cols 0..=1. */
    select(&mut e, cell_pos(0, 1, 0), cell_pos(1, 0, 4));
    assert!(matches!(
        selection_kind(&e),
        SelectionKind::TableCells { .. }
    ));
    toggle(&mut e, FormattingToggle::Bold);
    for r in 0..3u32 {
        for c in 0..3u32 {
            let inside = r <= 1 && c <= 1;
            assert_eq!(
                all_bytes(cell(&e, r, c), is_bold),
                inside,
                "cell ({r},{c}) bold == inside the rectangle"
            );
            if !inside {
                assert!(no_bytes(cell(&e, r, c), is_bold), "cell ({r},{c}) untouched");
            }
        }
    }
}

#[test]
fn every_character_attribute_reaches_every_cell_of_a_rectangle() {
    let teal = bridge::Color {
        r: 0,
        g: 128,
        b: 128,
        a: 255,
    };
    let yellow = bridge::Color {
        r: 255,
        g: 255,
        b: 0,
        a: 255,
    };
    type Check = fn(&SpanStyle) -> bool;
    let cases: Vec<(&str, TextAttrsPatch, Check)> = vec![
        ("bold", bold_patch(), |s| s.bold == Some(true)),
        (
            "italic",
            TextAttrsPatch {
                italic: Some(true),
                ..patch()
            },
            |s| s.italic == Some(true),
        ),
        (
            "underline",
            TextAttrsPatch {
                underline: Some(bridge::UnderlineStyle::Single),
                ..patch()
            },
            |s| s.underline == Some(engine::UnderlineStyle::Single),
        ),
        (
            "strike",
            TextAttrsPatch {
                strike: Some(true),
                ..patch()
            },
            |s| s.strike == Some(true),
        ),
        (
            "size",
            TextAttrsPatch {
                font_size: Some(36.0),
                ..patch()
            },
            |s| s.font_size == Some(36.0),
        ),
        (
            "family",
            TextAttrsPatch {
                font_family: Some("amiri".into()),
                ..patch()
            },
            |s| s.font_family == Some(engine::FontFamily::Amiri),
        ),
        (
            "colour",
            TextAttrsPatch {
                color: Some(teal),
                ..patch()
            },
            |s| s.color == Some([0, 128, 128, 255]),
        ),
        (
            "highlight",
            TextAttrsPatch {
                bg_color: Some(yellow),
                ..patch()
            },
            |s| s.bg_color == Some([255, 255, 0, 255]),
        ),
        (
            "superscript",
            TextAttrsPatch {
                script: Some(bridge::VerticalScript::Superscript),
                ..patch()
            },
            |s| s.vert_align == Some(engine::VertAlign::Superscript),
        ),
        (
            "subscript",
            TextAttrsPatch {
                script: Some(bridge::VerticalScript::Subscript),
                ..patch()
            },
            |s| s.vert_align == Some(engine::VertAlign::Subscript),
        ),
        (
            "caps",
            TextAttrsPatch {
                caps: Some(true),
                ..patch()
            },
            |s| s.caps == Some(true),
        ),
        (
            "small caps",
            TextAttrsPatch {
                small_caps: Some(true),
                ..patch()
            },
            |s| s.small_caps == Some(true),
        ),
    ];
    for (name, attrs, check) in cases {
        let mut e = engine_of(doc_2x2());
        select(&mut e, cell_pos(0, 0, 0), cell_pos(1, 1, 2));
        format(&mut e, None, attrs);
        for (r, c) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
            assert!(all_bytes(cell(&e, r, c), check), "{name}: cell ({r},{c})");
        }
        assert!(no_bytes(body(&e, 0), check), "{name}: head untouched");
    }
}

#[test]
fn toggle_every_flag_over_cells_sets_then_clears() {
    for attr in [
        FormattingToggle::Bold,
        FormattingToggle::Italic,
        FormattingToggle::Strike,
        FormattingToggle::Underline,
        FormattingToggle::Superscript,
        FormattingToggle::Subscript,
        FormattingToggle::Caps,
        FormattingToggle::SmallCaps,
    ] {
        let flag = |s: &SpanStyle| match attr {
            FormattingToggle::Bold => s.bold == Some(true),
            FormattingToggle::Italic => s.italic == Some(true),
            FormattingToggle::Strike => s.strike == Some(true),
            FormattingToggle::Underline => {
                s.underline.is_some_and(|u| u != engine::UnderlineStyle::None)
            }
            FormattingToggle::Superscript => s.vert_align == Some(engine::VertAlign::Superscript),
            FormattingToggle::Subscript => s.vert_align == Some(engine::VertAlign::Subscript),
            FormattingToggle::Caps => s.caps == Some(true),
            FormattingToggle::SmallCaps => s.small_caps == Some(true),
        };
        let mut e = engine_of(doc_2x2());
        select(&mut e, cell_pos(0, 0, 0), cell_pos(1, 1, 2));
        toggle(&mut e, attr);
        for (r, c) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
            assert!(all_bytes(cell(&e, r, c), flag), "{attr:?} on: ({r},{c})");
        }
        toggle(&mut e, attr);
        for (r, c) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
            assert!(no_bytes(cell(&e, r, c), flag), "{attr:?} off: ({r},{c})");
        }
    }
}

#[test]
fn an_explicit_range_across_cells_is_document_order_with_partial_ends() {
    let mut e = engine_of(doc_2x2());
    /* The live selection is a caret elsewhere, so the explicit range is
    not a rectangle: A2 from byte 1, all of B1, B2 up to byte 1. */
    select(&mut e, bpos_top(0, 0), bpos_top(0, 0));
    format(
        &mut e,
        Some(range(cell_pos(0, 1, 1), cell_pos(1, 1, 1))),
        bold_patch(),
    );
    assert!(no_bytes(cell(&e, 0, 0), is_bold), "A1 precedes the range");
    assert!(!is_bold(&cell(&e, 0, 1).style_at(0)), "A2 byte 0 precedes it");
    assert!(is_bold(&cell(&e, 0, 1).style_at(1)), "A2 byte 1 is the start");
    assert!(all_bytes(cell(&e, 1, 0), is_bold), "B1 is a whole middle paragraph");
    assert!(is_bold(&cell(&e, 1, 1).style_at(0)), "B2 byte 0 is inside");
    assert!(!is_bold(&cell(&e, 1, 1).style_at(1)), "B2 ends at byte 1");
    assert!(no_bytes(body(&e, 2), is_bold), "tail follows the range");
}

#[test]
fn a_body_to_cell_range_formats_the_body_tail_and_the_cells_it_reaches() {
    let mut e = engine_of(doc_2x2());
    select(&mut e, bpos_top(0, 0), bpos_top(0, 0));
    /* "head" from byte 2, through A1 and A2 whole, to B1's first byte. */
    format(
        &mut e,
        Some(range(bpos_top(0, 2), cell_pos(1, 0, 1))),
        bold_patch(),
    );
    assert!(!is_bold(&body(&e, 0).style_at(1)) && is_bold(&body(&e, 0).style_at(2)));
    assert!(all_bytes(cell(&e, 0, 0), is_bold), "A1");
    assert!(all_bytes(cell(&e, 0, 1), is_bold), "A2");
    assert!(is_bold(&cell(&e, 1, 0).style_at(0)) && !is_bold(&cell(&e, 1, 0).style_at(1)));
    assert!(no_bytes(cell(&e, 1, 1), is_bold), "B2 is past the range");
    assert!(no_bytes(body(&e, 2), is_bold), "tail is past the range");
}

#[test]
fn a_range_around_the_table_formats_the_whole_table() {
    let mut e = engine_of(doc_2x2());
    select(&mut e, bpos_top(0, 0), bpos_top(0, 0));
    format(
        &mut e,
        Some(range(bpos_top(0, 0), bpos_top(2, 4))),
        bold_patch(),
    );
    assert!(all_bytes(body(&e, 0), is_bold), "head: {:?} {:?}", body(&e, 0).text, body(&e, 0).spans);
    assert!(all_bytes(body(&e, 2), is_bold), "tail: {:?} {:?}", body(&e, 2).text, body(&e, 2).spans);
    for (r, c) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
        assert!(all_bytes(cell(&e, r, c), is_bold), "cell ({r},{c})");
    }
}

#[test]
fn a_cell_range_inside_a_nested_table_reaches_the_nested_paragraphs() {
    /* [head, Table(2x2) whose cell (0,1) holds [para "A2", Table 1x2], tail]. */
    let mut doc = doc_2x2();
    let nested = DocumentTree::from_text("")
        .insert_table(EngineBlockPath::top(0), 1, 2)
        .blocks
        .iter()
        .find_map(|b| b.as_table().cloned())
        .expect("a nested table");
    let Block::Table(outer) = doc.blocks.get_mut(1).expect("table") else {
        panic!("block 1 is the table");
    };
    outer.rows[0].cells[1].blocks.push(Block::Table(nested));
    let nested_cell = |c: u32| EngineBlockPath {
        steps: vec![
            PathStep::Block(1),
            PathStep::Cell { row: 0, col: 1 },
            PathStep::Block(1),
            PathStep::Cell { row: 0, col: c },
            PathStep::Block(0),
        ],
    };
    for c in 0..2 {
        doc = doc.insert_text(
            LogicalPos {
                path: nested_cell(c),
                offset: 0,
            },
            if c == 0 { "n0" } else { "n1" },
        );
    }
    let mut e = engine_of(doc);
    /* Bold a range from A2 (outer cell) through B1: the nested table
    sits between them and its paragraphs must be reached. */
    select(&mut e, bpos_top(0, 0), bpos_top(0, 0));
    format(
        &mut e,
        Some(range(cell_pos(0, 1, 0), cell_pos(1, 0, 2))),
        bold_patch(),
    );
    assert!(all_bytes(cell(&e, 0, 1), is_bold), "A2");
    assert!(all_bytes(para(&e, &nested_cell(0)), is_bold), "nested n0");
    assert!(all_bytes(para(&e, &nested_cell(1)), is_bold), "nested n1");
    assert!(all_bytes(cell(&e, 1, 0), is_bold), "B1");
    assert!(no_bytes(cell(&e, 0, 0), is_bold), "A1 precedes");

    /* A rectangle selection over the OUTER table formats the nested
    cells inside the selected cell too. */
    let mut e2 = engine_of(e.undo.current().clone());
    select(&mut e2, cell_pos(0, 0, 0), cell_pos(1, 1, 2));
    format(
        &mut e2,
        None,
        TextAttrsPatch {
            font_size: Some(36.0),
            ..patch()
        },
    );
    assert!(all_bytes(para(&e2, &nested_cell(0)), is_36), "nested n0");
    assert!(all_bytes(para(&e2, &nested_cell(1)), is_36), "nested n1");
}

#[test]
fn an_empty_cell_in_the_selection_keeps_the_formatting_for_typing() {
    let mut doc = with_table(1, 2);
    doc = doc.insert_text(
        LogicalPos {
            path: cell_path(0, 0),
            offset: 0,
        },
        "full",
    );
    let mut e = engine_of(doc);
    /* Select both cells (the second is empty), bold, then type in the
    empty one: Word types with the cell mark's formatting. */
    select(&mut e, cell_pos(0, 0, 0), cell_pos(0, 1, 0));
    toggle(&mut e, FormattingToggle::Bold);
    assert!(all_bytes(cell(&e, 0, 0), is_bold));
    assert_eq!(
        cell(&e, 0, 1).mark_style.as_deref().and_then(|m| m.bold),
        Some(true),
        "the empty cell's mark carries the bold"
    );
    select(&mut e, cell_pos(0, 1, 0), cell_pos(0, 1, 0));
    ok(
        &mut e,
        Command::InsertText {
            at: None,
            text: "typed".into(),
        },
    );
    assert!(all_bytes(cell(&e, 0, 1), is_bold), "typed text is bold");
    /* ...and a second toggle over the same cells reads "all bold" and
    clears the flag everywhere. */
    select(&mut e, cell_pos(0, 0, 0), cell_pos(0, 1, 5));
    toggle(&mut e, FormattingToggle::Bold);
    assert!(no_bytes(cell(&e, 0, 0), is_bold) && no_bytes(cell(&e, 0, 1), is_bold));
}

#[test]
fn a_tracked_format_change_across_cells_records_one_revision_per_paragraph() {
    let mut e = engine_of(doc_2x2());
    ok(&mut e, Command::ToggleTrackChanges { enabled: true });
    select(&mut e, cell_pos(0, 0, 0), cell_pos(0, 1, 2));
    toggle(&mut e, FormattingToggle::Bold);
    for (r, c) in [(0, 0), (0, 1)] {
        let revs = &cell(&e, r, c).revisions;
        assert_eq!(revs.len(), 1, "cell ({r},{c}): {revs:?}");
        assert_eq!(revs[0].kind, engine::RevisionKind::FormatChange);
        assert_eq!((revs[0].start, revs[0].end), (0, 2));
    }
    assert!(cell(&e, 1, 0).revisions.is_empty() && cell(&e, 1, 1).revisions.is_empty());
}

#[test]
fn formatting_is_one_undo_step_and_leaves_untouched_blocks_shared() {
    let mut e = engine_of(doc_2x2());
    let depth = e.undo.depth();
    select(&mut e, cell_pos(0, 0, 0), cell_pos(1, 1, 2));
    toggle(&mut e, FormattingToggle::Bold);
    assert_eq!(e.undo.depth(), depth + 1, "one undo step for the whole range");
    ok(&mut e, Command::Undo);
    for (r, c) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
        assert!(no_bytes(cell(&e, r, c), is_bold), "undone: ({r},{c})");
    }
}

/* ---------------------------------------------------------------------
Paragraph formatting in cells
--------------------------------------------------------------------- */

fn bullet_marker(p: &engine::Paragraph) -> Option<&str> {
    p.list_item.and(p.resolved_marker.as_deref())
}

fn pages(e: &Engine) -> Vec<PageBox> {
    let (pages, _, _, info) = e.build_pages(1.0, false, None).expect("layout");
    assert!(info.degradations.is_empty(), "{:?}", info.degradations);
    pages
}

/// The laid-out paragraph box of cell `(row, col)` block 0 on page 1.
fn laid_out_cell_paragraph(pages: &[PageBox], row: usize, col: usize) -> &ParagraphBox {
    let table = pages[0]
        .blocks
        .iter()
        .find_map(|b| match b {
            LayoutBlock::Table(t) => Some(t),
            LayoutBlock::Paragraph(_) => None,
        })
        .expect("the table");
    match &table.rows[row].cells[col].content[0] {
        LayoutBlock::Paragraph(p) => p,
        LayoutBlock::Table(_) => panic!("a table where the paragraph should be"),
    }
}

#[test]
fn a_bullet_on_a_cell_paragraph_gets_a_marker_that_paints_inside_the_cell() {
    let mut e = engine_of(doc_2x2());
    /* A collapsed caret in A2, the Bullet button. */
    select(&mut e, cell_pos(0, 1, 1), cell_pos(0, 1, 1));
    ok(
        &mut e,
        Command::ToggleList {
            range: range(cell_pos(0, 1, 1), cell_pos(0, 1, 1)),
            kind: ListKind::Bullet,
        },
    );
    let a2 = cell(&e, 0, 1);
    assert!(a2.list_item.is_some(), "the paragraph joined a list");
    assert_eq!(bullet_marker(a2), Some("\u{2022}"), "...and was given a marker");
    assert!(a2.resolved_list_indent.is_some(), "...and the level's indent");
    assert!(
        cell(&e, 0, 0).list_item.is_none() && cell(&e, 1, 1).list_item.is_none(),
        "the neighbours are not in the list"
    );
    /* The layout carries the marker: a shaped glyph run to the left of
    the cell's text, inside the cell. */
    let pages = pages(&e);
    let laid = laid_out_cell_paragraph(&pages, 0, 1);
    let marker = laid.marker.as_ref().expect("a laid-out marker");
    assert_eq!(marker.text, "\u{2022}");
    assert!(!marker.run.glyphs.is_empty(), "the marker is shaped");
    assert!(
        marker.origin.x >= 0.0 && marker.origin.x + marker.width <= laid.size.width,
        "the marker sits inside the paragraph box: x {} w {} of {}",
        marker.origin.x,
        marker.width,
        laid.size.width
    );
    assert!(
        laid_out_cell_paragraph(&pages, 0, 0).marker.is_none(),
        "A1 has no marker"
    );
    /* And it reaches the display list: more glyphs than the text alone. */
    let plain = engine_of(doc_2x2());
    let glyphs = |p: &[PageBox]| {
        render::scene::build_document_scene(p, 0.0)
            .cmds
            .iter()
            .filter(|c| matches!(c, render::scene::DisplayCmd::DrawGlyphRun(_)))
            .count()
    };
    assert!(
        glyphs(&pages) > glyphs(&self::pages(&plain)),
        "the bullet adds a painted glyph run"
    );

    /* Toggling Off removes the marker again. */
    ok(
        &mut e,
        Command::ToggleList {
            range: range(cell_pos(0, 1, 1), cell_pos(0, 1, 1)),
            kind: ListKind::Off,
        },
    );
    assert!(cell(&e, 0, 1).list_item.is_none() && cell(&e, 0, 1).resolved_marker.is_none());
}

#[test]
fn a_numbered_list_runs_through_cells_and_the_body_in_document_order() {
    let mut e = engine_of(doc_2x2());
    /* A selection over A1..B2 numbers them 1.-4. in row-major order; the
    rectangle is the whole table. */
    select(&mut e, cell_pos(0, 0, 0), cell_pos(1, 1, 2));
    ok(
        &mut e,
        Command::ToggleList {
            range: range(cell_pos(0, 0, 0), cell_pos(1, 1, 2)),
            kind: ListKind::Number,
        },
    );
    let markers: Vec<_> = [(0, 0), (0, 1), (1, 0), (1, 1)]
        .iter()
        .map(|&(r, c)| cell(&e, r, c).resolved_marker.clone())
        .collect();
    assert_eq!(
        markers,
        vec![
            Some("1.".to_string()),
            Some("2.".to_string()),
            Some("3.".to_string()),
            Some("4.".to_string())
        ]
    );
    /* A body paragraph after the table joins the same list and continues it. */
    select(&mut e, bpos_top(2, 0), bpos_top(2, 0));
    ok(
        &mut e,
        Command::ToggleList {
            range: range(bpos_top(2, 0), bpos_top(2, 0)),
            kind: ListKind::Number,
        },
    );
    assert_eq!(body(&e, 2).resolved_marker.as_deref(), Some("5."));
    /* Enter in a cell list item renumbers the rest (the #50 refresh now
    walks cells). */
    select(&mut e, cell_pos(0, 0, 1), cell_pos(0, 0, 1));
    ok(&mut e, Command::SplitParagraph { at: None });
    let first_cell_markers: Vec<_> = (0..2)
        .map(|i| {
            let path = EngineBlockPath {
                steps: vec![
                    PathStep::Block(1),
                    PathStep::Cell { row: 0, col: 0 },
                    PathStep::Block(i),
                ],
            };
            para(&e, &path).resolved_marker.clone()
        })
        .collect();
    assert_eq!(
        first_cell_markers,
        vec![Some("1.".to_string()), Some("2.".to_string())]
    );
    assert_eq!(cell(&e, 0, 1).resolved_marker.as_deref(), Some("3."));
    assert_eq!(body(&e, 2).resolved_marker.as_deref(), Some("6."));
}

#[test]
fn a_bullet_over_a_rectangle_lists_exactly_the_selected_cells() {
    let mut e = engine_of(doc_3x3());
    select(&mut e, cell_pos(0, 0, 0), cell_pos(1, 1, 4));
    ok(
        &mut e,
        Command::ToggleList {
            range: range(cell_pos(0, 0, 0), cell_pos(1, 1, 4)),
            kind: ListKind::Bullet,
        },
    );
    for r in 0..3u32 {
        for c in 0..3u32 {
            let inside = r <= 1 && c <= 1;
            assert_eq!(
                cell(&e, r, c).list_item.is_some(),
                inside,
                "cell ({r},{c}) listed == inside"
            );
            assert_eq!(bullet_marker(cell(&e, r, c)).is_some(), inside, "({r},{c})");
        }
    }
}

#[test]
fn paragraph_formatting_reaches_cell_paragraphs_singly_and_over_a_rectangle() {
    use engine::Alignment;
    let mut e = engine_of(doc_3x3());
    /* One cell: centre, indent, shading, borders, line spacing. */
    let at = cell_pos(1, 1, 1);
    select(&mut e, at.clone(), at.clone());
    ok(
        &mut e,
        Command::SetParagraphAlign {
            range: range(at.clone(), at.clone()),
            align: bridge::Alignment::Center,
        },
    );
    ok(
        &mut e,
        Command::SetParagraphIndent {
            range: range(at.clone(), at.clone()),
            start_pt: 36.0,
            end_pt: 0.0,
            first_line_pt: 0.0,
        },
    );
    ok(
        &mut e,
        Command::SetParagraphShading {
            range: range(at.clone(), at.clone()),
            color: Some(bridge::Color {
                r: 255,
                g: 255,
                b: 0,
                a: 255,
            }),
        },
    );
    let edge = BridgeBorderStroke {
        style: BridgeBorderStyle::Single,
        size_eighth_pt: 8,
        color: None,
    };
    ok(
        &mut e,
        Command::SetParagraphBorders {
            range: range(at.clone(), at.clone()),
            borders: BridgeCellBorders {
                top: Some(edge.clone()),
                left: Some(edge.clone()),
                bottom: Some(edge.clone()),
                right: Some(edge),
            },
        },
    );
    ok(
        &mut e,
        Command::SetLineSpacing {
            range: range(at.clone(), at),
            multiplier: 2.0,
        },
    );
    let p = cell(&e, 1, 1);
    assert_eq!(p.props.alignment, Some(Alignment::Center));
    assert_eq!(p.props.indent.start_twips, 720);
    assert_eq!(p.props.shading, Some([255, 255, 0, 255]));
    assert!(p.props.borders.as_ref().is_some_and(|b| b.top.is_some()));
    assert!(p.props.line_height.is_some(), "line spacing set");
    for (r, c) in [(0, 0), (1, 0), (2, 2)] {
        assert_eq!(cell(&e, r, c).props.alignment, None, "({r},{c}) untouched");
    }

    /* A rectangle: right-align + style across rows 1..=2 x cols 1..=2. */
    select(&mut e, cell_pos(1, 1, 0), cell_pos(2, 2, 4));
    assert!(matches!(
        selection_kind(&e),
        SelectionKind::TableCells { .. }
    ));
    ok(
        &mut e,
        Command::SetParagraphAlign {
            range: range(cell_pos(1, 1, 0), cell_pos(2, 2, 4)),
            align: bridge::Alignment::End,
        },
    );
    for r in 0..3u32 {
        for c in 0..3u32 {
            let inside = r >= 1 && c >= 1;
            assert_eq!(
                cell(&e, r, c).props.alignment == Some(Alignment::End),
                inside,
                "cell ({r},{c}) aligned == inside"
            );
        }
    }
}

#[test]
fn a_docx_round_trip_keeps_a_cell_bullet_and_the_reader_numbers_cells() {
    let mut e = engine_of(doc_2x2());
    select(&mut e, cell_pos(0, 1, 0), cell_pos(0, 1, 0));
    ok(
        &mut e,
        Command::ToggleList {
            range: range(cell_pos(0, 1, 0), cell_pos(0, 1, 0)),
            kind: ListKind::Number,
        },
    );
    let bytes = format_docx::save_docx(e.undo.current()).expect("save");
    let reopened = format_docx::read_docx(&bytes).expect("reopen").document;
    let p = reopened
        .paragraph_at_path(&cell_path(0, 1))
        .expect("the cell paragraph");
    assert!(p.list_item.is_some(), "numPr survives the round trip");
    assert_eq!(
        p.resolved_marker.as_deref(),
        Some("1."),
        "the reader numbers a cell's list item (it used to skip tables)"
    );
}
