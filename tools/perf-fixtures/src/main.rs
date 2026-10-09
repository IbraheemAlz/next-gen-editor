//! Generates the synthetic `.docx` load files the memory-profile harness
//! (D5.2) and the performance harness (D5.3) run against —
//! `tests/perf/{50,100,250,500}p.docx`, plus (issue #379) the table-heavy
//! `tests/perf/tables-30p.docx`.
//!
//! Each `Np.docx` holds `pages * PARAS_PER_PAGE` filler paragraphs. The
//! table fixture is ~30 pages of 3-deep nested autofit tables. The
//! documents are built with the engine's own `build_minimal_docx` writer
//! and verified to round-trip through `read_docx`, so the harness's
//! `LOAD_DOCX` command can always parse them.

use engine::{Block, DocumentTree, Paragraph, Table, TableCell, TableRow};
use format_docx::read_docx;
use format_docx::writer::build_minimal_docx;
use std::fs;
use std::path::Path;

/// Filler paragraphs per nominal page — roughly a page of body copy on A4.
const PARAS_PER_PAGE: usize = 20;

/// Mixed Latin + Arabic filler so a loaded document exercises the BiDi and
/// shaping paths a real corpus document would.
const FILLER: &str = "The quick brown fox jumps over the lazy dog. \
    مرحبا بالعالم، هذا نص حشو لقياس استهلاك الذاكرة.";

/// Issue #379 — short mixed-script cell text for the table fixture.
const CELL_TEXT: &str = "fox مرحبا";

/// Issue #379 — nested towers in the table fixture (~30 pages at the
/// editor's boot text metrics, 24 px on 36 px lines).
const TABLE_TOWERS: usize = 22;
/// Nesting depth of every tower.
const TOWER_DEPTH: usize = 3;

fn main() {
    let out_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/perf");
    fs::create_dir_all(&out_dir).expect("create tests/perf");

    for pages in [50usize, 100, 250, 500] {
        let count = pages * PARAS_PER_PAGE;
        let texts = (0..count).map(|i| format!("Paragraph {i:05}. {FILLER}"));
        let doc = DocumentTree::from_paragraphs(texts);

        let bytes = build_minimal_docx(&doc).expect("build .docx");

        /* Sanity: the file must parse back, or the harness LOAD_DOCX fails. */
        let parsed = read_docx(&bytes).expect("re-read generated .docx");
        assert_eq!(
            parsed.document.paragraph_count() as usize,
            count,
            "round-trip paragraph count mismatch for {pages}p",
        );

        let path = out_dir.join(format!("{pages}p.docx"));
        fs::write(&path, &bytes).expect("write .docx");
        println!(
            "[perf-fixtures] {pages}p -> {count} paragraphs, {} bytes",
            bytes.len()
        );
    }

    write_table_fixture(&out_dir);
}

fn para(text: String) -> Block {
    Block::Paragraph(Paragraph {
        text,
        ..Default::default()
    })
}

fn cell(blocks: Vec<Block>) -> TableCell {
    TableCell {
        blocks,
        ..Default::default()
    }
}

/// One level of tower `tag`: a 2 × 2 autofit table whose cell (0,0) holds
/// a heading and `inner` (the next level down); the other three cells
/// hold filler prose that wraps.
fn tower_level(tag: usize, level: usize, inner: Option<Table>) -> Table {
    let mut first = vec![para(format!("Tower {tag:03}, level {level}"))];
    if let Some(t) = inner {
        first.push(Block::Table(t));
    }
    let prose = |n: usize| cell(vec![para(format!("Cell {tag:03}.{level}.{n} {CELL_TEXT}"))]);
    Table {
        rows: vec![
            TableRow {
                cells: vec![cell(first), prose(1)],
                ..Default::default()
            },
            TableRow {
                cells: vec![prose(2), prose(3)],
                ..Default::default()
            },
        ],
        dirty: true,
        ..Default::default()
    }
}

fn tower(tag: usize) -> Table {
    let mut inner = None;
    for level in (0..TOWER_DEPTH).rev() {
        inner = Some(tower_level(tag, level, inner));
    }
    inner.expect("TOWER_DEPTH >= 1")
}

/// Max table nesting depth below `blocks` (0 = no table).
fn nesting(blocks: &[Block]) -> usize {
    blocks
        .iter()
        .filter_map(|b| match b {
            Block::Table(t) => Some(
                1 + t
                    .rows
                    .iter()
                    .flat_map(|r| &r.cells)
                    .map(|c| nesting(&c.blocks))
                    .max()
                    .unwrap_or(0),
            ),
            Block::Paragraph(_) => None,
        })
        .max()
        .unwrap_or(0)
}

/// Issue #379 — `tables-30p.docx`: an intro paragraph, then
/// `TABLE_TOWERS` 3-deep nested towers separated by body paragraphs —
/// the table-heavy repaint fixture `tools/perf` times.
fn write_table_fixture(out_dir: &Path) {
    let mut doc = DocumentTree::from_paragraphs([format!("Table-heavy fixture. {FILLER}")]);
    for tag in 0..TABLE_TOWERS {
        doc.blocks.push_back(Block::Table(tower(tag)));
        doc.blocks
            .push_back(para(format!("Between towers {tag:03}. {FILLER}")));
    }
    let bytes = build_minimal_docx(&doc).expect("build tables .docx");

    let parsed = read_docx(&bytes).expect("re-read generated tables .docx");
    let blocks: Vec<Block> = parsed.document.blocks.iter().cloned().collect();
    let tables = blocks
        .iter()
        .filter(|b| matches!(b, Block::Table(_)))
        .count();
    assert_eq!(tables, TABLE_TOWERS, "round-trip table count mismatch");
    assert_eq!(
        nesting(&blocks),
        TOWER_DEPTH,
        "round-trip nesting depth mismatch"
    );

    let path = out_dir.join("tables-30p.docx");
    fs::write(&path, &bytes).expect("write tables .docx");
    println!(
        "[perf-fixtures] tables-30p -> {TABLE_TOWERS} towers x {TOWER_DEPTH} levels, {} bytes",
        bytes.len()
    );
}
