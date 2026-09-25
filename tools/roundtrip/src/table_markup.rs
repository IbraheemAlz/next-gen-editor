//! Issue #248 — table source markup: a regenerated `<w:tbl>` keeps its
//! attributes, pretty-print whitespace, verified property bytes,
//! `<w:tblPrEx>` (#103) and the row / cell-level content controls (#245's
//! `Bug66263-table.docx`), so an edit inside a table is a pure insertion.

use super::{
    INSERT_TEXT, assert_document_xml_well_formed, build_minimal_docx, extract_doc_xml,
    package_document_xml, read_docx, word_document_xml, write_docx,
};
use anyhow::{Context, Result, bail};
use engine::{BlockPath, LogicalPos, PathStep};

/// A Word-shaped, pretty-printed table: `<w:tblPr>` with unread
/// attributes (`<w:tblLook>` flags, a border's `w:space`), a
/// `<w:tblGridChange>`, row attributes (rsid + `w14:paraId`), a
/// `<w:tblPrEx>`, a `<w:trHeight>` without `w:hRule` (the regenerated
/// spelling differs), a `<w:shd w:themeFill>`, a multi-line `<w:p>` start
/// tag, a cell-level and a row-level `<w:sdt>`, and bookmarks between rows.
pub(crate) const TABLE_MARKUP_BODY: &str = r#"
  <w:p><w:r><w:t>intro</w:t></w:r></w:p>
  <w:tbl>
    <w:tblPr>
      <w:tblStyle w:val="TableGrid"/>
      <w:tblW w:w="0" w:type="auto"/>
      <w:tblBorders><w:top w:val="single" w:sz="4" w:space="0" w:color="auto"/></w:tblBorders>
      <w:tblLook w:val="04A0" w:firstRow="1" w:lastRow="0" w:firstColumn="1" w:lastColumn="0" w:noHBand="0" w:noVBand="1"/>
    </w:tblPr>
    <w:tblGrid>
      <w:gridCol w:w="2400"/>
      <w:gridCol w:w="2400"/>
      <w:tblGridChange w:id="7"><w:tblGrid><w:gridCol w:w="4800"/></w:tblGrid></w:tblGridChange>
    </w:tblGrid>
    <w:tr w:rsidR="00AA11BB" w14:paraId="1A2B3C4D" w14:textId="77777777">
      <w:tblPrEx><w:tblBorders><w:top w:val="double" w:sz="12" w:space="0" w:color="FF0000"/></w:tblBorders></w:tblPrEx>
      <w:trPr><w:cnfStyle w:val="100000000000"/><w:trHeight w:val="400"/></w:trPr>
      <w:tc>
        <w:tcPr><w:tcW w:w="2400" w:type="dxa"/><w:shd w:val="clear" w:color="auto" w:fill="FFEB78" w:themeFill="accent1"/></w:tcPr>
        <w:p w:rsidR="00AA11BB"
             w:rsidRDefault="00CC22DD">
          <w:r><w:t xml:space="preserve">A1</w:t></w:r>
        </w:p>
      </w:tc>
      <w:sdt><w:sdtPr><w:tag w:val="cell"/></w:sdtPr><w:sdtContent>
      <w:tc>
        <w:tcPr><w:tcW w:w="2400" w:type="dxa"/></w:tcPr>
        <w:p><w:r><w:t xml:space="preserve">B1</w:t></w:r></w:p>
      </w:tc>
      </w:sdtContent></w:sdt>
    </w:tr>
    <w:bookmarkStart w:id="5" w:name="between"/>
    <w:sdt>
      <w:sdtPr><w:id w:val="1001"/></w:sdtPr>
      <w:sdtContent>
        <w:tr w:rsidR="00EE33FF">
          <w:tc><w:p><w:r><w:t xml:space="preserve">A2</w:t></w:r></w:p></w:tc>
          <w:tc><w:p><w:r><w:t xml:space="preserve">B2</w:t></w:r></w:p></w:tc>
        </w:tr>
      </w:sdtContent>
    </w:sdt>
    <w:bookmarkEnd w:id="5"/>
  </w:tbl>
  <w:p><w:r><w:t>after</w:t></w:r></w:p>
"#;

pub(crate) fn build_table_source_markup_docx() -> Vec<u8> {
    package_document_xml(&word_document_xml(TABLE_MARKUP_BODY))
}

fn table_path() -> BlockPath {
    BlockPath::top(1)
}

fn cell(row: u32, col: u32) -> BlockPath {
    BlockPath {
        steps: vec![
            PathStep::Block(1),
            PathStep::Cell { row, col },
            PathStep::Block(0),
        ],
    }
}

/// `(original span, edited span)` between the longest common prefix and
/// suffix — `tools/corpus-native`'s `source_bytes_rewritten` is the first.
fn rewritten(orig: &[u8], edited: &[u8]) -> (usize, usize) {
    let prefix = orig.iter().zip(edited).take_while(|(a, b)| a == b).count();
    let max = orig.len().min(edited.len()) - prefix;
    let suffix = orig
        .iter()
        .rev()
        .zip(edited.iter().rev())
        .take(max)
        .take_while(|(a, b)| a == b)
        .count();
    (orig.len() - prefix - suffix, edited.len() - prefix - suffix)
}

fn save_both(
    archive: &format_docx::DocxArchive,
    doc: &engine::DocumentTree,
) -> Result<Vec<(&'static str, Vec<u8>)>> {
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
        assert_document_xml_well_formed(&bytes).with_context(|| path.to_string())?;
        out.push((path, extract_doc_xml(&bytes)?));
    }
    Ok(out)
}

/// Issue #248 — step 30: (a) zero-edit resave byte-identical on both save
/// paths; (b) typing in every cell — plain, inside a cell-level and a
/// row-level content control — saves as EXACTLY the source plus the
/// insert (tblPrEx, tblGridChange, trPr / tcPr spellings, attribute
/// whitespace all intact); (c) an inserted row is a pure insertion and a
/// deleted one a pure deletion; (d) a merge keeps the merged-away content and regenerates only the owner's
/// `<w:tcPr>`, adopting its unchanged children's source spelling.
pub(crate) fn run_table_markup_roundtrip() -> Result<()> {
    let xml = word_document_xml(TABLE_MARKUP_BODY);
    let docx = build_table_source_markup_docx();
    let archive = read_docx(&docx).context("read table_source_markup")?;

    /* (a) */
    let resaved = write_docx(&archive, &archive.document).context("write_docx")?;
    if extract_doc_xml(&resaved)? != xml.as_bytes() {
        bail!("step 30a: zero-edit archive resave is not byte-identical");
    }
    let ui = build_minimal_docx(&archive.document).context("build_minimal_docx")?;
    if extract_doc_xml(&ui)? != xml.as_bytes() {
        bail!("step 30a: zero-edit UI-path resave is not byte-identical");
    }
    println!("[roundtrip] step 30a OK — pretty-printed table resaves byte-identical");

    /* (b) */
    for (row, col, text) in [(0, 0, "A1"), (0, 1, "B1"), (1, 0, "A2"), (1, 1, "B2")] {
        let edited = archive
            .document
            .insert_text(LogicalPos::new(cell(row, col), 2), INSERT_TEXT);
        let expected = xml.replacen(
            &format!(">{text}</w:t>"),
            &format!(">{text}{INSERT_TEXT}</w:t>"),
            1,
        );
        for (path, got) in save_both(&archive, &edited)? {
            if got != expected.as_bytes() {
                bail!(
                    "step 30b {path}: edit in cell ({row}, {col}) is not source + insert\n--- expected ---\n{expected}\n--- got ---\n{}",
                    String::from_utf8_lossy(&got)
                );
            }
        }
    }
    println!(
        "[roundtrip] step 30b OK — typing in every cell (incl. row / cell content controls) is source + insert; tblPrEx (#103) byte-identical"
    );

    /* (c) */
    for at in 0..=2usize {
        let edited = archive.document.insert_row(table_path(), at);
        for (path, got) in save_both(&archive, &edited)? {
            let (orig_span, _) = rewritten(xml.as_bytes(), &got);
            if orig_span != 0 {
                bail!("step 30c {path}: insert_row({at}) rewrote {orig_span} source bytes");
            }
        }
    }
    for row in 0..=1u32 {
        let edited = archive.document.delete_row(table_path(), row);
        for (path, got) in save_both(&archive, &edited)? {
            let (_, new_span) = rewritten(xml.as_bytes(), &got);
            if new_span != 0 {
                bail!("step 30c {path}: delete_row({row}) wrote {new_span} new bytes");
            }
        }
    }
    println!(
        "[roundtrip] step 30c OK — inserted rows are pure insertions, deleted rows pure deletions"
    );

    /* (d) */
    let merged = archive.document.merge_cells(table_path(), 0, 0, 0, 1);
    let bytes = write_docx(&archive, &merged).context("write merged")?;
    assert_document_xml_well_formed(&bytes)?;
    let got = String::from_utf8(extract_doc_xml(&bytes)?).context("utf8")?;
    let owner = r#"<w:tcPr><w:tcW w:w="2400" w:type="dxa"/><w:gridSpan w:val="2"/><w:shd w:val="clear" w:color="auto" w:fill="FFEB78" w:themeFill="accent1"/></w:tcPr>"#;
    let tr_pr = r#"<w:trPr><w:cnfStyle w:val="100000000000"/><w:trHeight w:val="400"/></w:trPr>"#;
    /* Issue #263 — the merged-away cell's content is appended into the owner,
    so `B1` must survive the merge (inside the owner cell). */
    if !got.contains(owner) || !got.contains(tr_pr) || !got.contains(">B1<") {
        bail!(
            "step 30d: merge did not regenerate exactly the owner's tcPr (or lost the merged-away content):\n{got}"
        );
    }
    let reread = read_docx(&bytes).context("re-read merged")?;
    let t = reread.document.blocks[1]
        .as_table()
        .context("merged table")?;
    if t.rows[0].cells.len() != 1 || t.rows[0].cells[0].props.grid_span != 2 {
        bail!("step 30d: merged row re-read with the wrong shape");
    }
    println!(
        "[roundtrip] step 30d OK — a merge regenerates only the owner's tcPr (source spelling adopted) and keeps the merged-away content (#263)"
    );
    Ok(())
}
