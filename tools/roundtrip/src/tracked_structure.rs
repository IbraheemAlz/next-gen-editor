//! Structural edits recorded with review mode on (issue #442) — read →
//! tracked row insert / row delete / table insert / section break → save
//! → re-read → accept / reject.

use super::{assert_document_xml_well_formed, build_styled_docx, extract_doc_xml, read_docx};
use anyhow::{Context, Result, bail};
use engine::{Block, BlockPath, DocumentTree, LogicalPos, RevisionKind, SectionType};

const STYLES_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"/>"#;

const REVIEWER: &str = "Reviewer";
const REVIEW_DATE: &str = "2026-10-16T00:00:00Z";

fn cell(text: &str) -> String {
    format!(
        r#"<w:tc><w:tcPr><w:tcW w:w="3000" w:type="dxa"/></w:tcPr><w:p w:rsidR="00AB12CD"><w:r><w:t>{text}</w:t></w:r></w:p></w:tc>"#
    )
}

fn source_row(rsid: &str, a: &str, b: &str) -> String {
    format!(r#"<w:tr w:rsidR="{rsid}">{}{}</w:tr>"#, cell(a), cell(b))
}

fn fixture() -> (Vec<u8>, [String; 2]) {
    let rows = [
        source_row("00AB12CD", "a1", "b1"),
        source_row("00AB12CE", "a2", "b2"),
    ];
    let body = format!(
        r#"<w:p><w:r><w:t>before</w:t></w:r></w:p><w:tbl><w:tblPr><w:tblW w:w="0" w:type="auto"/></w:tblPr><w:tblGrid><w:gridCol w:w="3000"/><w:gridCol w:w="3000"/></w:tblGrid>{}{}</w:tbl><w:p><w:r><w:t>after</w:t></w:r></w:p>"#,
        rows[0], rows[1]
    );
    let doc = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}<w:sectPr/></w:body></w:document>"#
    );
    (build_styled_docx(STYLES_XML, &doc), rows)
}

/// Per row of the first table: its revision kinds and its cell texts.
type Rows = Vec<(Vec<RevisionKind>, Vec<String>)>;

fn rows(doc: &DocumentTree) -> Rows {
    doc.blocks
        .iter()
        .find_map(Block::as_table)
        .map(|t| {
            t.rows
                .iter()
                .map(|r| {
                    (
                        r.props.revisions.iter().map(|x| x.kind).collect(),
                        r.cells
                            .iter()
                            .map(|c| {
                                c.blocks
                                    .iter()
                                    .filter_map(Block::as_paragraph)
                                    .map(|p| p.text.clone())
                                    .collect::<String>()
                            })
                            .collect(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

fn row(kinds: &[RevisionKind], cells: [&str; 2]) -> (Vec<RevisionKind>, Vec<String>) {
    (
        kinds.to_vec(),
        cells.iter().map(|s| s.to_string()).collect(),
    )
}

fn paragraphs(doc: &DocumentTree) -> Vec<String> {
    doc.blocks
        .iter()
        .filter_map(Block::as_paragraph)
        .map(|p| p.text.clone())
        .collect()
}

/// Save `doc` on both paths; each output well-formed, re-read.
fn save_and_reread(
    step: &str,
    archive: &format_docx::DocxArchive,
    doc: &DocumentTree,
) -> Result<Vec<(String, DocumentTree)>> {
    let mut out = Vec::new();
    for (path, bytes) in [
        (
            "write_docx",
            format_docx::write_docx(archive, doc).context("write")?,
        ),
        ("save_docx", format_docx::save_docx(doc).context("ui save")?),
    ] {
        assert_document_xml_well_formed(&bytes).with_context(|| format!("{step} {path}"))?;
        let xml = String::from_utf8(extract_doc_xml(&bytes)?).context("utf8")?;
        let reread = read_docx(&bytes).with_context(|| format!("{step} {path} re-read"))?;
        out.push((xml, reread.document));
    }
    Ok(out)
}

/// `xml` carries an engine-minted `<w:{tag} w:id="N" …/>` by the reviewer
/// (a non-zero id).
fn has_minted(xml: &str, tag: &str) -> bool {
    let head = format!(r#"<w:{tag} w:id=""#);
    let tail = format!(r#"" w:author="{REVIEWER}" w:date="{REVIEW_DATE}"/>"#);
    xml.match_indices(&head).any(|(at, _)| {
        let rest = &xml[at + head.len()..];
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        digits > 0 && &rest[..digits] != "0" && rest[digits..].starts_with(&tail)
    })
}

/// Issue #442 — step 62 (`before` / a two-row source table / `after`).
///
/// a. A tracked row insertion saves the new row with
///    `<w:trPr><w:ins …/></w:trPr>` (minted id) while both source rows
///    re-emit byte for byte; the re-read lists it; reject-all removes it,
///    accept-all keeps it unmarked.
/// b. A tracked row deletion saves `<w:trPr><w:del …/>` and the row's text
///    as `<w:delText>`, the other source row byte for byte; accept removes
///    the row, reject keeps it.
/// c. A tracked table insertion saves every new row as inserted; reject-all
///    on the re-read restores the original paragraphs and table.
/// d. A tracked section break saves the inserted mark (`<w:pPr><w:rPr><w:ins/>`)
///    beside the new `<w:sectPr>`; reject-all on the re-read merges back
///    into one section, accept-all keeps two.
pub(crate) fn run_tracked_structure_roundtrip() -> Result<()> {
    let (bytes, source_rows) = fixture();
    let archive = read_docx(&bytes).context("step 62: read")?;
    let doc = &archive.document;
    let table = BlockPath::top(1);
    use RevisionKind::{Delete, Insert};

    /* a. Row insertion. */
    let inserted = doc.tracked_insert_row(table.clone(), 1, REVIEWER, REVIEW_DATE);
    let want = vec![
        row(&[], ["a1", "b1"]),
        row(&[Insert], ["", ""]),
        row(&[], ["a2", "b2"]),
    ];
    if rows(&inserted) != want {
        bail!("step 62a: recorded {:?}", rows(&inserted));
    }
    for (xml, reread) in save_and_reread("step 62a", &archive, &inserted)? {
        if !source_rows.iter().all(|r| xml.contains(r.as_str()))
            || !xml.contains("<w:trPr><w:ins ")
            || !has_minted(&xml, "ins")
            || rows(&reread) != want
        {
            bail!("step 62a: saved {:?}\n{xml}", rows(&reread));
        }
        let rejected = reread.resolve_all_revisions(false);
        let accepted = reread.resolve_all_revisions(true);
        if rows(&rejected) != [row(&[], ["a1", "b1"]), row(&[], ["a2", "b2"])]
            || rows(&accepted).len() != 3
            || rejected.has_revisions()
            || accepted.has_revisions()
        {
            bail!(
                "step 62a: resolved to {:?} / {:?}",
                rows(&rejected),
                rows(&accepted)
            );
        }
    }
    println!(
        "[roundtrip] step 62a OK — a tracked row insertion saves trPr/w:ins (minted id), source rows byte-identical; reject removes it"
    );

    /* b. Row deletion. */
    let deleted = doc.tracked_delete_row(table.clone(), 0, REVIEWER, REVIEW_DATE);
    let want = vec![row(&[Delete], ["a1", "b1"]), row(&[], ["a2", "b2"])];
    if rows(&deleted) != want {
        bail!("step 62b: recorded {:?}", rows(&deleted));
    }
    for (xml, reread) in save_and_reread("step 62b", &archive, &deleted)? {
        if !xml.contains(source_rows[1].as_str())
            || !xml.contains("<w:trPr><w:del ")
            || !has_minted(&xml, "del")
            || !xml.contains(">a1</w:delText>")
            || rows(&reread) != want
        {
            bail!("step 62b: saved {:?}\n{xml}", rows(&reread));
        }
        let accepted = reread.resolve_all_revisions(true);
        let rejected = reread.resolve_all_revisions(false);
        if rows(&accepted) != [row(&[], ["a2", "b2"])]
            || rows(&rejected) != [row(&[], ["a1", "b1"]), row(&[], ["a2", "b2"])]
            || accepted.has_revisions()
            || rejected.has_revisions()
        {
            bail!(
                "step 62b: resolved to {:?} / {:?}",
                rows(&accepted),
                rows(&rejected)
            );
        }
    }
    println!(
        "[roundtrip] step 62b OK — a tracked row deletion saves trPr/w:del + delText, the other source row byte-identical; accept removes it"
    );

    /* c. Table insertion (at the end: the escape paragraph comes from a
    tracked split of `after`). */
    let with_table = doc.tracked_insert_table(BlockPath::top(3), 2, 2, REVIEWER, REVIEW_DATE);
    for (xml, reread) in save_and_reread("step 62c", &archive, &with_table)? {
        let tables: Vec<_> = reread.blocks.iter().filter_map(Block::as_table).collect();
        let new_rows_inserted = tables.get(1).is_some_and(|t| {
            t.rows
                .iter()
                .all(|r| r.props.revisions.iter().any(|x| x.kind == Insert))
        });
        if xml.matches("<w:trPr><w:ins ").count() != 2 || !new_rows_inserted {
            bail!("step 62c: saved\n{xml}");
        }
        let rejected = reread.resolve_all_revisions(false);
        if paragraphs(&rejected) != paragraphs(doc)
            || rejected.blocks.iter().filter_map(Block::as_table).count() != 1
            || rejected.has_revisions()
        {
            bail!("step 62c: reject-all left {:?}", paragraphs(&rejected));
        }
    }
    println!(
        "[roundtrip] step 62c OK — a tracked table insertion saves every row as inserted; reject-all restores the document"
    );

    /* d. Section break. */
    let broken = doc.tracked_insert_section_break_at(
        LogicalPos::new(BlockPath::top(0), 3),
        SectionType::NextPage,
        REVIEWER,
        REVIEW_DATE,
    );
    for (xml, reread) in save_and_reread("step 62d", &archive, &broken)? {
        let p0 = reread.blocks.front().and_then(Block::as_paragraph);
        let marked = p0.is_some_and(|p| {
            p.section_end.is_some() && p.mark_revisions.iter().any(|r| r.kind == Insert)
        });
        if !marked || !xml.contains("<w:rPr><w:ins ") || !has_minted(&xml, "ins") {
            bail!("step 62d: saved\n{xml}");
        }
        let rejected = reread.resolve_all_revisions(false);
        let accepted = reread.resolve_all_revisions(true);
        if paragraphs(&rejected) != paragraphs(doc)
            || rejected.effective_sections().len() != 1
            || accepted.effective_sections().len() != 2
            || rejected.has_revisions()
            || accepted.has_revisions()
        {
            bail!(
                "step 62d: resolved to {:?} / {} sections",
                paragraphs(&rejected),
                accepted.effective_sections().len()
            );
        }
    }
    println!(
        "[roundtrip] step 62d OK — a tracked section break saves its inserted mark beside the sectPr; reject-all merges back to one section"
    );
    Ok(())
}
