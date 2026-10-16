//! Issue #471 - a merge that ENCLOSES an earlier merge (`gridSpan` and
//! `vMerge` regions inside the new rectangle are absorbed) saves to a
//! `.docx` whose re-read table has the same grid and the same cell shape,
//! every row's span sum still equal to the grid width; a rectangle that only
//! partly covers one is refused by `Table::check_merge_rect` before any
//! edit. Rides `--fixtures` as `table_merge_absorb.docx` (zero-edit resave)
//! and the default harness's step 61 (merge, save on both paths, re-read).

use super::{
    assert_document_xml_well_formed, build_table_fixture, extract_doc_xml, read_docx, write_docx,
};
use anyhow::{Context, Result, bail};
use engine::{BlockPath, DocumentTree, Table, TableError, VMergeRole};

/// 3 grid columns. Row 0: `A` spans columns 0-1, then `C`. Rows 1-2: three
/// plain cells, except column 2 which is a `vMerge` run (`F` restarts it,
/// row 2's last cell continues it with an empty paragraph).
const MERGE_TABLE: &str = concat!(
    r#"<w:tbl><w:tblGrid><w:gridCol w:w="1440"/><w:gridCol w:w="1440"/><w:gridCol w:w="1440"/></w:tblGrid>"#,
    r#"<w:tr><w:tc><w:tcPr><w:gridSpan w:val="2"/></w:tcPr><w:p><w:r><w:t xml:space="preserve">A</w:t></w:r></w:p></w:tc>"#,
    r#"<w:tc><w:p><w:r><w:t xml:space="preserve">C</w:t></w:r></w:p></w:tc></w:tr>"#,
    r#"<w:tr><w:tc><w:p><w:r><w:t xml:space="preserve">D</w:t></w:r></w:p></w:tc>"#,
    r#"<w:tc><w:p><w:r><w:t xml:space="preserve">E</w:t></w:r></w:p></w:tc>"#,
    r#"<w:tc><w:tcPr><w:vMerge w:val="restart"/></w:tcPr><w:p><w:r><w:t xml:space="preserve">F</w:t></w:r></w:p></w:tc></w:tr>"#,
    r#"<w:tr><w:tc><w:p><w:r><w:t xml:space="preserve">G</w:t></w:r></w:p></w:tc>"#,
    r#"<w:tc><w:p><w:r><w:t xml:space="preserve">H</w:t></w:r></w:p></w:tc>"#,
    r#"<w:tc><w:tcPr><w:vMerge/></w:tcPr><w:p/></w:tc></w:tr></w:tbl>"#,
);

pub(crate) fn build_table_merge_absorb_docx() -> Vec<u8> {
    build_table_fixture("intro", MERGE_TABLE)
}

/// `(grid_span, v_merge)` of every cell of every row.
type Shape = Vec<Vec<(usize, VMergeRole)>>;

fn shape(t: &Table) -> Shape {
    t.rows
        .iter()
        .map(|r| {
            r.cells
                .iter()
                .map(|c| (c.props.grid_span.max(1) as usize, c.props.v_merge))
                .collect()
        })
        .collect()
}

fn table(doc: &DocumentTree) -> Result<&Table> {
    doc.blocks
        .get(1)
        .and_then(engine::Block::as_table)
        .context("block 1 is not a table")
}

/// The paragraph texts of the cell at `(row, cell index)`.
fn cell_texts(t: &Table, row: usize, cell: usize) -> Vec<String> {
    t.rows[row].cells[cell]
        .blocks
        .iter()
        .map(|b| b.as_paragraph().map(|p| p.text.clone()).unwrap_or_default())
        .collect()
}

fn assert_square(label: &str, t: &Table) -> Result<()> {
    let width = t.grid.len();
    for (r, row) in t.rows.iter().enumerate() {
        let sum: usize = row
            .cells
            .iter()
            .map(|c| c.props.grid_span.max(1) as usize)
            .sum();
        if sum != width {
            bail!("{label}: row {r} spans sum to {sum}, the grid is {width} wide");
        }
    }
    if t.ragged_row_count() != 0 {
        bail!("{label}: ragged_row_count() != 0");
    }
    Ok(())
}

/// Save `doc` on BOTH save paths (`write_docx` over the source archive, the
/// UI's `save_docx`), check the part is well formed, re-read it.
fn save_and_reread(
    label: &str,
    archive: &format_docx::DocxArchive,
    doc: &DocumentTree,
) -> Result<Vec<DocumentTree>> {
    let mut out = Vec::new();
    for (path, bytes) in [
        (
            "write_docx",
            write_docx(archive, doc).context("write_docx")?,
        ),
        (
            "save_docx",
            format_docx::save_docx(doc).context("save_docx")?,
        ),
    ] {
        assert_document_xml_well_formed(&bytes).with_context(|| format!("{label} {path}"))?;
        let _ = extract_doc_xml(&bytes)?;
        out.push(
            read_docx(&bytes)
                .with_context(|| format!("{label} {path}: re-read"))?
                .document,
        );
    }
    Ok(out)
}

/// Issue #471 - step 61.
pub(crate) fn run_table_merge_roundtrip() -> Result<()> {
    let docx = build_table_merge_absorb_docx();
    let archive = read_docx(&docx).context("read table_merge_absorb")?;
    let t0 = table(&archive.document)?;
    let plain = (1, VMergeRole::None);
    let source_shape: Shape = vec![
        vec![(2, VMergeRole::None), plain],
        vec![plain, plain, (1, VMergeRole::Restart)],
        vec![plain, plain, (1, VMergeRole::Continue)],
    ];
    if shape(t0) != source_shape || t0.grid.len() != 3 {
        bail!(
            "step 61: the fixture read with the wrong shape: {:?}",
            shape(t0)
        );
    }
    assert_square("step 61 source", t0)?;

    /* (a) A rectangle that only partly covers a merged region is refused
    BEFORE any edit - A from its right grid column, F's run from above. */
    for ((r0, c0, r1, c1), (top, left, bottom, right)) in
        [((0, 1, 1, 2), (0, 0, 0, 1)), ((0, 2, 1, 2), (1, 2, 2, 2))]
    {
        match t0.check_merge_rect(r0, c0, r1, c1) {
            Err(TableError::CrossesMerge {
                top: t,
                left: l,
                bottom: b,
                right: r,
            }) if (t, l, b, r) == (top, left, bottom, right) => {}
            other => bail!(
                "step 61a: ({r0},{c0})-({r1},{c1}) should cross rows {top}-{bottom}, columns {left}-{right}: {other:?}"
            ),
        }
    }
    println!("[roundtrip] step 61a OK - a rectangle cutting a gridSpan / vMerge region is refused");

    /* (b) The whole table encloses both regions: A and F's run are absorbed
    into one cell spanning all three columns and all three rows. */
    let merged = archive.document.merge_cells(BlockPath::top(1), 0, 0, 2, 2);
    let tm = table(&merged)?;
    let want: Shape = vec![
        vec![(3, VMergeRole::Restart)],
        vec![(3, VMergeRole::Continue)],
        vec![(3, VMergeRole::Continue)],
    ];
    if shape(tm) != want || tm.grid.len() != 3 {
        bail!(
            "step 61b: absorbing merge has the wrong shape: {:?}",
            shape(tm)
        );
    }
    assert_square("step 61b merged", tm)?;
    let order: Vec<&str> = ["A", "C", "D", "E", "F", "G", "H"].to_vec();
    if cell_texts(tm, 0, 0) != order {
        bail!(
            "step 61b: owner content {:?} is not {order:?} (row-major, no blank for the vMerge placeholder)",
            cell_texts(tm, 0, 0)
        );
    }
    for re in save_and_reread("step 61b", &archive, &merged)? {
        let tr = table(&re)?;
        if shape(tr) != want || tr.grid != tm.grid {
            bail!(
                "step 61b: re-read shape {:?} != merged {:?}",
                shape(tr),
                want
            );
        }
        assert_square("step 61b re-read", tr)?;
        if cell_texts(tr, 0, 0) != order {
            bail!("step 61b: re-read owner content {:?}", cell_texts(tr, 0, 0));
        }
    }
    println!(
        "[roundtrip] step 61b OK - a merge enclosing a gridSpan and a vMerge region saves and re-reads with the same grid, every row square"
    );

    /* (c) A rectangle that holds only F's run (rows 1-2, columns 1-2) absorbs
    it and leaves A and the first column alone. */
    let merged = archive.document.merge_cells(BlockPath::top(1), 1, 1, 2, 2);
    let tm = table(&merged)?;
    let want: Shape = vec![
        vec![(2, VMergeRole::None), plain],
        vec![plain, (2, VMergeRole::Restart)],
        vec![plain, (2, VMergeRole::Continue)],
    ];
    if shape(tm) != want {
        bail!(
            "step 61c: partial absorb has the wrong shape: {:?}",
            shape(tm)
        );
    }
    assert_square("step 61c merged", tm)?;
    for re in save_and_reread("step 61c", &archive, &merged)? {
        let tr = table(&re)?;
        if shape(tr) != want || tr.grid != tm.grid {
            bail!(
                "step 61c: re-read shape {:?} != merged {:?}",
                shape(tr),
                want
            );
        }
        assert_square("step 61c re-read", tr)?;
        if cell_texts(tr, 1, 1) != ["E", "F", "H"] {
            bail!("step 61c: re-read owner content {:?}", cell_texts(tr, 1, 1));
        }
    }
    println!(
        "[roundtrip] step 61c OK - a merge enclosing only the vMerge run absorbs it; A and the first column are untouched"
    );
    Ok(())
}
