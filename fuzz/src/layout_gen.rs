//! Random paragraph/table/section tree generator for `layout_paginate`
//! (D5.5, issue #90).
//!
//! Bypasses OOXML entirely — the point of this target is the layout
//! engine's robustness against extreme or malformed STRUCTURAL trees
//! (zero/negative-size cells, a table row whose cell count doesn't match
//! its `<w:tblGrid>`, degenerate page geometry), independent of whatever
//! `rpc_command` covers on the `Command` surface.

use crate::util::pick;
use arbitrary::Unstructured;
use engine::{
    Alignment, Block, DocumentTree, Indent, PageGeometry, ParaProperties, Paragraph, Spacing,
    Table, TableCell, TableRow, TextDirection,
};

fn small(u: &mut Unstructured, max: u32) -> u32 {
    u.int_in_range(0..=max).unwrap_or(0)
}

/// A signed twips-ish value, deliberately allowed to go negative or huge —
/// both are things a hand-edited or buggy `.docx` can carry, and the
/// layout solver must not panic on either.
fn small_i32(u: &mut Unstructured, max: i32) -> i32 {
    u.int_in_range(-max..=max).unwrap_or(0)
}

fn gen_text(u: &mut Unstructured) -> String {
    const POOL: &[&str] = &[
        "",
        "hello world",
        "a fairly ordinary sentence that should wrap across more than one line",
        "السلام عليكم ورحمة الله وبركاته، هذه فقرة عربية طويلة لاختبار الالتفاف",
        "supercalifragilisticexpialidocious_no_spaces_at_all_to_break_on_whatsoever_keep_going",
        "line one\nline two\nline three",
        "🙂🙂🙂 emoji paragraph 🙂🙂🙂",
    ];
    pick(u, POOL).to_string()
}

fn gen_para_properties(u: &mut Unstructured) -> ParaProperties {
    let mut props = ParaProperties {
        alignment: *pick(
            u,
            &[
                None,
                Some(Alignment::Start),
                Some(Alignment::Center),
                Some(Alignment::Justify),
                Some(Alignment::End),
            ],
        ),
        indent: Indent {
            start_twips: small_i32(u, 20_000),
            end_twips: small_i32(u, 20_000),
            first_line_twips: small_i32(u, 20_000),
            hanging_twips: small_i32(u, 20_000),
        },
        spacing: Spacing {
            before_twips: small_i32(u, 20_000),
            after_twips: small_i32(u, 20_000),
        },
        direction: *pick(
            u,
            &[None, Some(TextDirection::Ltr), Some(TextDirection::Rtl)],
        ),
        ..Default::default()
    };
    props.page_break_before = u.ratio(1, 4).unwrap_or(false);
    /* Issue #178 — tri-state now; the generator still only exercises the
    two explicit states (never `None`) to keep the existing bias. */
    props.keep_next = Some(u.ratio(1, 6).unwrap_or(false));
    props.keep_lines = Some(u.ratio(1, 6).unwrap_or(false));
    props
}

fn gen_paragraph(u: &mut Unstructured) -> Paragraph {
    Paragraph {
        text: gen_text(u),
        props: gen_para_properties(u),
        ..Default::default()
    }
}

fn gen_cell(u: &mut Unstructured) -> TableCell {
    let n = small(u, 2) + 1;
    let mut blocks = Vec::with_capacity(n as usize);
    for _ in 0..n {
        blocks.push(Block::Paragraph(gen_paragraph(u)));
    }
    TableCell {
        props: Default::default(),
        blocks,
    source_markup: None,}
}

fn gen_table(u: &mut Unstructured) -> Table {
    let cols = small(u, 6) + 1;
    let grid: Vec<i32> = (0..cols).map(|_| small_i32(u, 8_000)).collect();
    let row_count = small(u, 8);
    let mut rows = Vec::with_capacity(row_count as usize);
    for _ in 0..row_count {
        // Deliberately allow a row's cell count to diverge from `grid`'s
        // length — a mismatched grid/row is a real hostile-but-parseable
        // shape (a hand-edited `.docx`, or another writer's bug).
        let cell_count = small(u, cols + 1);
        let cells = (0..cell_count).map(|_| gen_cell(u)).collect();
        rows.push(TableRow {
            props: Default::default(),
            cells,
        source_markup: None,});
    }
    Table {
        grid,
        props: Default::default(),
        rows,
        dirty: false,
        source_xml: None,
        body_xml: None,
    source_markup: None,}
}

fn gen_block(u: &mut Unstructured, allow_table: bool) -> Block {
    if allow_table && u.ratio(1, 4).unwrap_or(false) {
        Block::Table(gen_table(u))
    } else {
        Block::Paragraph(gen_paragraph(u))
    }
}

/// Build a `DocumentTree` with a random top-level block sequence
/// (paragraphs and at-most-one-level-deep tables) and, some of the time,
/// degenerate page geometry (near-zero or negative content area) — exactly
/// the inputs a paginator's "keep making forward progress" invariant needs
/// to survive.
pub fn gen_document_tree(u: &mut Unstructured) -> DocumentTree {
    let mut doc = DocumentTree::new();
    let block_count = small(u, 12);
    for _ in 0..block_count {
        doc.blocks.push_back(gen_block(u, true));
    }
    if u.ratio(1, 3).unwrap_or(false) {
        let mut geometry = PageGeometry::a4();
        if u.ratio(1, 2).unwrap_or(false) {
            // Points, not twips — small values here (including 0) shrink
            // the content area toward nothing, which is exactly the
            // "does the paginator still terminate" edge case.
            geometry.width = small(u, 2000) as f32;
            geometry.height = small(u, 2000) as f32;
        }
        if u.ratio(1, 2).unwrap_or(false) {
            geometry.margin_top = small(u, 800) as f32;
            geometry.margin_right = small(u, 800) as f32;
            geometry.margin_bottom = small(u, 800) as f32;
            geometry.margin_left = small(u, 800) as f32;
        }
        doc.body_section.geometry = geometry;
    }
    doc
}
