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
        source_markup: None,
    }
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
            source_markup: None,
        });
    }
    Table {
        grid,
        props: Default::default(),
        rows,
        dirty: false,
        source_xml: None,
        body_xml: None,
        source_markup: None,
    }
}

fn gen_block(u: &mut Unstructured, allow_table: bool) -> Block {
    if allow_table && u.ratio(1, 4).unwrap_or(false) {
        Block::Table(gen_table(u))
    } else {
        Block::Paragraph(gen_paragraph(u))
    }
}

/// Issue #318 — leading bytes that switch `layout_paginate` into the
/// nested-table shape ([`gen_nested_tower`]). A prefix, not a generator
/// arm, so every pre-existing seed (and its libFuzzer descendants)
/// decodes exactly as before; mutations of a `NEST` seed keep exploring
/// deep nesting.
pub const NESTING_MAGIC: &[u8; 4] = b"NEST";

/// Issue #318 — deepest tower [`gen_nested_tower`] builds: far past the
/// engine's layout cap (`MAX_TABLE_LAYOUT_DEPTH`, 32) and the reader's
/// typed cap (64) — this tree bypasses the reader, so nothing but the
/// layout bounds it.
pub const MAX_FUZZ_NESTING: usize = 256;

/// Issue #318 — the document `layout_paginate` lays out for `data`: a
/// [`NESTING_MAGIC`]-prefixed input is a nested tower, anything else
/// [`gen_document_tree`].
pub fn gen_layout_document(data: &[u8]) -> DocumentTree {
    match data.strip_prefix(NESTING_MAGIC.as_slice()) {
        Some(rest) => gen_nested_tower(&mut Unstructured::new(rest)),
        None => gen_document_tree(&mut Unstructured::new(data)),
    }
}

/// Issue #318 — the committed seed for a `depth`-level tower: the magic
/// and the depth (u16 LE). Every level's knobs then decode from exhausted
/// entropy: a one-column autofit table whose cell holds `"level i"` and
/// the next level — the shape of the corpus's `table_nested_200_deep.docx`.
pub fn nesting_seed(depth: u16) -> Vec<u8> {
    let mut seed = NESTING_MAGIC.to_vec();
    seed.extend_from_slice(&depth.to_le_bytes());
    seed
}

/// Issue #318 — a document holding one tower of `depth` nested tables
/// (u16 LE, clamped to `1..=MAX_FUZZ_NESTING`), each level's column
/// count, grid, autofit / fixed layout and text drawn from the rest of
/// the input. The nested table rides cell 0 after the level's paragraph.
/// Built bottom-up: no recursion here, whatever the depth.
pub fn gen_nested_tower(u: &mut Unstructured) -> DocumentTree {
    let depth = match u.bytes(2) {
        Ok(b) => u16::from_le_bytes([b[0], b[1]]) as usize,
        Err(_) => 1,
    }
    .clamp(1, MAX_FUZZ_NESTING);
    struct Level {
        cols: u32,
        grid: Vec<i32>,
        fixed: bool,
        text: String,
    }
    let levels: Vec<Level> = (0..depth)
        .map(|i| {
            /* One knob byte per level; exhausted entropy reads 0 — the
            plain one-column autofit level (`Unstructured::ratio` would
            read `true` there, so it is not used for these switches). */
            let knobs: u8 = u.arbitrary().unwrap_or(0);
            let cols = 1 + u32::from(knobs >> 3) % 3;
            let grid = if knobs & 1 != 0 {
                (0..cols).map(|_| small_i32(u, 8_000)).collect()
            } else {
                Vec::new()
            };
            Level {
                cols,
                grid,
                fixed: (knobs >> 1) & 3 == 3,
                text: format!("level {i} {}", gen_text(u)).trim_end().to_string(),
            }
        })
        .collect();
    let mut inner: Option<Table> = None;
    for level in levels.into_iter().rev() {
        let mut first = vec![Block::Paragraph(Paragraph {
            text: level.text,
            ..Default::default()
        })];
        if let Some(t) = inner.take() {
            first.push(Block::Table(t));
        }
        let mut cells = vec![TableCell {
            props: Default::default(),
            blocks: first,
            source_markup: None,
        }];
        for _ in 1..level.cols {
            cells.push(TableCell {
                props: Default::default(),
                blocks: vec![Block::Paragraph(Paragraph::default())],
                source_markup: None,
            });
        }
        let mut table = Table {
            grid: level.grid,
            props: Default::default(),
            rows: vec![TableRow {
                props: Default::default(),
                cells,
                source_markup: None,
            }],
            dirty: false,
            source_xml: None,
            body_xml: None,
            source_markup: None,
        };
        if level.fixed {
            table.props.layout = engine::TableLayout::Fixed;
        }
        inner = Some(table);
    }
    let mut doc = DocumentTree::new();
    doc.blocks.clear();
    doc.blocks.push_back(Block::Paragraph(Paragraph {
        text: "intro".to_string(),
        ..Default::default()
    }));
    if let Some(t) = inner {
        doc.blocks.push_back(Block::Table(t));
    }
    doc
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// libFuzzer's per-input `-timeout` in `fuzz-nightly.yml`.
    const PER_INPUT_BUDGET: Duration = Duration::from_secs(30);

    fn seed_path() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("corpus/layout_paginate/seed_nested_200")
    }

    /// Nesting levels of the tower `doc` holds (0 = no table).
    fn tower_depth(doc: &DocumentTree) -> usize {
        let mut depth = 0;
        let mut table = doc.blocks.iter().find_map(|b| match b {
            Block::Table(t) => Some(t),
            Block::Paragraph(_) => None,
        });
        while let Some(t) = table {
            depth += 1;
            table = t.rows[0].cells[0].blocks.iter().find_map(|b| match b {
                Block::Table(t) => Some(t),
                Block::Paragraph(_) => None,
            });
        }
        depth
    }

    /// Issue #318 — the committed seed is `nesting_seed(200)` and decodes
    /// to a 200-deep tower of plain one-column autofit tables (the corpus
    /// `table_nested_200_deep.docx` shape, without the reader's 64-level
    /// cap). Regenerate with `cargo run --manifest-path fuzz/Cargo.toml
    /// --example regen-seeds`.
    #[test]
    fn committed_nesting_seed_decodes_to_a_200_deep_tower() {
        let committed = std::fs::read(seed_path()).expect("seed_nested_200");
        assert_eq!(committed, nesting_seed(200), "seed_nested_200 is stale");
        let doc = gen_layout_document(&committed);
        assert_eq!(tower_depth(&doc), 200);
        let Some(Block::Table(top)) = doc.blocks.get(1) else {
            panic!("intro, then the tower");
        };
        assert!(top.grid.is_empty() && top.rows[0].cells.len() == 1);
        assert!(matches!(top.props.layout, engine::TableLayout::Autofit));
        /* Pre-existing seeds keep their decoding (no magic → the plain
        generator, byte for byte). */
        let mixed = [0x0c, 0x0c, 0x0c, 0x0c, 0x0c, 0x0c, 0x0c, 0x0c, 3, 3, 3, 3];
        let (a, b) = (
            gen_layout_document(&mixed),
            gen_document_tree(&mut Unstructured::new(&mixed)),
        );
        assert_eq!(a.blocks.len(), b.blocks.len());
        assert_eq!(a.to_plain_text(), b.to_plain_text());
        assert_eq!(
            tower_depth(&gen_layout_document(&nesting_seed(u16::MAX))),
            MAX_FUZZ_NESTING
        );
    }

    /// Issue #318 acceptance — the 200-deep seed lays out (the layout used
    /// to cost `F(400)` grid layouts: it never returned) inside the
    /// per-input budget, degraded with a reported `NestingCapped`, and the
    /// `layout_paginate` invariants hold.
    #[test]
    fn nesting_seed_lays_out_under_the_budget_with_nesting_capped() {
        let seed = std::fs::read(seed_path()).expect("seed_nested_200");
        let t0 = Instant::now();
        let mut engine = engine_wasm::Engine::new_headless(gen_layout_document(&seed));
        engine.ensure_layout_for_fuzzing().expect("layout");
        let took = t0.elapsed();
        let probe = engine.layout_probe_for_fuzzing().expect("a snapshot");
        eprintln!("[#318] seed_nested_200: {took:?}, {probe:?}");
        assert!(
            took < PER_INPUT_BUDGET,
            "{took:?} over the per-input budget"
        );
        assert!(
            probe.degradations.iter().any(|d| d == "NestingCapped"),
            "{:?}",
            probe.degradations
        );
        assert!(probe.page_count >= 1);
        crate::run_layout_paginate(&seed);
    }
}
