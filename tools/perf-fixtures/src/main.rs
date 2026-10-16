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
//!
//! ## Keeping `Np` honest (issue #476)
//!
//! `Np.docx` must LAY OUT to about `N` pages in the editor - the memory
//! budgets (`tools/memory-profile`) are per-page and the open-50p timing
//! (`tools/perf`) is "a 50-page document" - so [`PARAS_PER_PAGE`] is a
//! calibration against the shell's boot text metrics and the shipped font
//! set, not a property of the text. It drifts whenever the line pitch or the
//! font stack changes (#329 Word line metrics, #463/#475 one font-derived
//! pitch: the old 20 per page laid out 167 pages for the "50p" file). To
//! re-calibrate, run `node tools/memory-profile/run.mjs` (it prints each
//! fixture's laid-out page count, and `--budgets` fails when one is further
//! than its tolerance from nominal), set `PARAS_PER_PAGE` to
//! `old * nominal / measured`, and regenerate with
//! `cargo run -p perf-fixtures --release`. The output is deterministic, so
//! a no-op run leaves the working tree clean.

use engine::{Block, DocumentTree, Paragraph, Table, TableCell, TableRow};
use format_docx::read_docx;
use format_docx::writer::build_minimal_docx;
use std::fs;
use std::path::Path;

/// Filler paragraphs per nominal page: a page of this ~110-character filler
/// at the editor's boot metrics (24 px text on the Word-derived pitch, A4)
/// holds six paragraphs of three to four lines. Issue #476 - it was 20
/// before #329 / #475 and laid out ~3.3x the nominal pages.
const PARAS_PER_PAGE: usize = 6;

/// Mixed Latin + Arabic filler so a loaded document exercises the BiDi and
/// shaping paths a real corpus document would.
const FILLER: &str = "The quick brown fox jumps over the lazy dog. \
    مرحبا بالعالم، هذا نص حشو لقياس استهلاك الذاكرة.";

/// Issue #379 — short mixed-script cell text for the table fixture.
const CELL_TEXT: &str = "fox مرحبا";

/// Issue #379 — nested towers in the table fixture: one tower per page at
/// the editor's boot text metrics, so 30 towers lay out to the nominal 30
/// pages (issue #476: 22 towers laid out to 22 pages, `tools/perf` now
/// reports and checks it like the `Np` files).
const TABLE_TOWERS: usize = 30;
/// Nesting depth of every tower.
const TOWER_DEPTH: usize = 3;

fn main() {
    let out_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/perf");
    fs::create_dir_all(&out_dir).expect("create tests/perf");

    for pages in [50usize, 100, 250, 500] {
        let count = pages * PARAS_PER_PAGE;
        let bytes = filler_docx(count, &format!("{pages}p"));

        let path = out_dir.join(format!("{pages}p.docx"));
        fs::write(&path, &bytes).expect("write .docx");
        println!(
            "[perf-fixtures] {pages}p -> {count} paragraphs, {} bytes",
            bytes.len()
        );
    }

    write_table_fixture(&out_dir);
    write_engine_anchor();
}

/// `count` filler paragraphs as a minimal `.docx`. Sanity: the file must
/// parse back, or the harness's `LOAD_DOCX` fails.
fn filler_docx(count: usize, label: &str) -> Vec<u8> {
    let texts = (0..count).map(|i| format!("Paragraph {i:05}. {FILLER}"));
    let doc = DocumentTree::from_paragraphs(texts);
    let bytes = build_minimal_docx(&doc).expect("build .docx");
    let parsed = read_docx(&bytes).expect("re-read generated .docx");
    assert_eq!(
        parsed.document.paragraph_count() as usize,
        count,
        "round-trip paragraph count mismatch for {label}",
    );
    bytes
}

/// Paragraphs in the engine-test anchor: what `50p.docx` held before issue
/// #476 re-calibrated it (50 pages x the old 20 per page).
const ENGINE_ANCHOR_PARAGRAPHS: usize = 1000;

/// Issue #476 - `crates/engine-wasm/tests/fixtures/long_single_section.docx`:
/// the long single-section document `engine-wasm`'s layout tests pin
/// (`viewport_cull_stops_single_section_layout`, the geometry fingerprints
/// of the widow-control and bridge tests). Those tests used to read
/// `tests/perf/50p.docx`, which made every re-calibration of the perf
/// fixture a re-pin of unrelated layout anchors. The anchor has its own
/// file now, byte-identical to the pre-#476 `50p.docx` (same text, same
/// writer), so no pin moved. Regenerating it must leave the file unchanged.
fn write_engine_anchor() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/engine-wasm/tests/fixtures/long_single_section.docx");
    fs::create_dir_all(path.parent().expect("fixture dir")).expect("create fixture dir");
    let bytes = filler_docx(ENGINE_ANCHOR_PARAGRAPHS, "long_single_section");
    fs::write(&path, &bytes).expect("write engine anchor");
    println!(
        "[perf-fixtures] long_single_section -> {ENGINE_ANCHOR_PARAGRAPHS} paragraphs, {} bytes",
        bytes.len()
    );
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
