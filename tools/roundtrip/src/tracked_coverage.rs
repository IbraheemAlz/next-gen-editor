//! Tracked-change coverage (issues #366 / #365 / #367): content that
//! arrives with review mode on through the paste paths, tracked table-row
//! revisions, and resolving a mark revision that carries a section break —
//! each through read → edit → save → re-read → accept / reject.

use super::{
    assert_document_xml_well_formed, build_styled_docx, extract_doc_xml, read_docx,
    rewritten_region, write_docx,
};
use anyhow::{Context, Result, bail};
use engine::{Block, BlockPath, DocumentTree, LogicalPos, RevisionKind};

const STYLES_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"/>"#;

const REVIEWER: &str = "Reviewer";
const REVIEW_DATE: &str = "2026-10-09T00:00:00Z";

fn document(body: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}<w:sectPr/></w:body></w:document>"#
    )
}

fn at(block: u32, offset: usize) -> LogicalPos {
    LogicalPos::new(BlockPath::top(block), offset as u32)
}

fn texts(doc: &DocumentTree) -> Vec<String> {
    doc.blocks
        .iter()
        .filter_map(Block::as_paragraph)
        .map(|p| p.text.clone())
        .collect()
}

/// Per top-level paragraph: its text revisions `(kind, start, end)` and
/// its mark's change kinds.
type Tracked = Vec<(Vec<(RevisionKind, u32, u32)>, Vec<RevisionKind>)>;

fn tracked(doc: &DocumentTree) -> Tracked {
    doc.blocks
        .iter()
        .filter_map(Block::as_paragraph)
        .map(|p| {
            let mut revs: Vec<_> = p
                .revisions
                .iter()
                .map(|r| (r.kind, r.start, r.end))
                .collect();
            revs.sort_by_key(|r| (r.1, r.2));
            (revs, p.mark_revisions.iter().map(|r| r.kind).collect())
        })
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
        ("write_docx", write_docx(archive, doc).context("write")?),
        ("save_docx", format_docx::save_docx(doc).context("ui save")?),
    ] {
        assert_document_xml_well_formed(&bytes).with_context(|| format!("{step} {path}"))?;
        let xml = String::from_utf8(extract_doc_xml(&bytes)?).context("utf8")?;
        let reread = read_docx(&bytes).with_context(|| format!("{step} {path} re-read"))?;
        out.push((xml, reread.document));
    }
    Ok(out)
}

/// `got` carries the reviewer's paragraph-mark insertion
/// (`<w:pPr><w:rPr><w:ins …/>`) under a minted, non-zero id.
fn has_minted_inserted_mark(got: &str) -> bool {
    let head = r#"<w:pPr><w:rPr><w:ins w:id=""#;
    let tail = format!(r#"" w:author="{REVIEWER}" w:date="{REVIEW_DATE}"/></w:rPr></w:pPr>"#);
    got.match_indices(head).any(|(at, _)| {
        let rest = &got[at + head.len()..];
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        digits > 0 && &rest[..digits] != "0" && rest[digits..].starts_with(&tail)
    })
}

/// A plain source paragraph pair the tracked pastes land in.
const PASTE_BODY: &str = concat!(
    r#"<w:p w:rsidR="00C0FFEE"><w:r><w:t>alpha beta</w:t></w:r></w:p>"#,
    r#"<w:p><w:r><w:t>last</w:t></w:r></w:p>"#,
);

/// Issue #366 — step 47: a two-paragraph paste with track changes on,
/// through the plain multi-line path and the rich (HTML) path.
///
/// a. The pasted text saves inside `<w:ins>` (both paragraphs) and the
///    mark the paste created as `<w:pPr><w:rPr><w:ins …/>`, on both save
///    paths; the re-read carries the same insertions.
/// b. Reject-all on the re-read restores the original paragraphs, saved
///    clean; on the in-memory tree the reject gives the source bytes back
///    (`write_docx` rewrites no source byte).
/// c. Accept-all keeps the paste, saved clean.
pub(crate) fn run_tracked_paste_roundtrip() -> Result<()> {
    let xml = document(PASTE_BODY);
    let bytes = build_styled_docx(STYLES_XML, &xml);
    let archive = read_docx(&bytes).context("read tracked-paste fixture")?;
    let doc = &archive.document;
    let html_blocks = engine::html::from_html_blocks("<p>one</p><p>two</p>");
    let pastes = [
        (
            "plain",
            doc.tracked_insert_multiline(at(0, 5), "one\ntwo", REVIEWER, REVIEW_DATE)
                .0,
        ),
        (
            "html",
            doc.tracked_insert_rich_blocks(at(0, 5), &html_blocks, REVIEWER, REVIEW_DATE)
                .0,
        ),
    ];
    let want_texts = vec!["alphaone", "two beta", "last"];
    let want_tracked: Tracked = vec![
        (
            vec![(RevisionKind::Insert, 5, 8)],
            vec![RevisionKind::Insert],
        ),
        (vec![(RevisionKind::Insert, 0, 3)], vec![]),
        (vec![], vec![]),
    ];
    for (how, pasted) in &pastes {
        if texts(pasted) != want_texts || tracked(pasted) != want_tracked {
            bail!(
                "step 47a ({how}): pasted {:?} / {:?}",
                texts(pasted),
                tracked(pasted)
            );
        }
        let mut reread_pasted = None;
        for (got, reread) in save_and_reread(&format!("step 47a ({how})"), &archive, pasted)? {
            let ins_runs = got.matches("<w:ins ").count();
            if !has_minted_inserted_mark(&got) || ins_runs < 3 {
                bail!("step 47a ({how}): the inserted text / mark was not written\n{got}");
            }
            if texts(&reread) != want_texts || tracked(&reread) != want_tracked {
                bail!(
                    "step 47a ({how}): re-read {:?} / {:?}",
                    texts(&reread),
                    tracked(&reread)
                );
            }
            reread_pasted = Some(reread);
        }
        let reread_pasted = reread_pasted.context("no re-read")?;
        for (accept, want) in [
            (false, vec!["alpha beta", "last"]),
            (true, want_texts.clone()),
        ] {
            let step = if accept { "step 47c" } else { "step 47b" };
            let resolved = reread_pasted.resolve_all_revisions(accept);
            if texts(&resolved) != want || resolved.has_revisions() {
                bail!("{step} ({how}): resolved to {:?}", texts(&resolved));
            }
            for (got, reread) in save_and_reread(step, &archive, &resolved)? {
                if got.contains("<w:ins ") || reread.has_revisions() || texts(&reread) != want {
                    bail!("{step} ({how}): saved {:?}\n{got}", texts(&reread));
                }
            }
        }
        let rejected = pasted.resolve_all_revisions(false);
        let out = write_docx(&archive, &rejected).context("write rejected")?;
        let got = extract_doc_xml(&out)?;
        let (_, rewritten, _) = rewritten_region(xml.as_bytes(), &got);
        if rewritten != 0 {
            bail!(
                "step 47b ({how}): the reject rewrote {rewritten} source bytes\n{}",
                String::from_utf8_lossy(&got)
            );
        }
    }
    println!(
        "[roundtrip] step 47 OK — a tracked paste (plain + HTML) saves w:ins text + an inserted mark; reject-all restores the source, accept-all keeps it"
    );
    Ok(())
}

/// The path of cell `(row, col)`'s first paragraph of the table at block 1.
fn cell(row: u32, col: u32, offset: u32) -> LogicalPos {
    LogicalPos::new(
        BlockPath {
            steps: vec![
                engine::PathStep::Block(1),
                engine::PathStep::Cell { row, col },
                engine::PathStep::Block(0),
            ],
        },
        offset,
    )
}

/// Per row of the table at block 1: its revision kinds, and its cells'
/// first-paragraph texts.
type Rows = Vec<(Vec<RevisionKind>, Vec<String>)>;

fn table_rows(doc: &DocumentTree) -> Rows {
    doc.table_at_path(&BlockPath::top(1))
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
                                    .first()
                                    .and_then(Block::as_paragraph)
                                    .map_or(String::new(), |p| p.text.clone())
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

/// Issue #365 — step 48: tracked table rows (`tracked_table_rows.docx`:
/// a deleted row, an inserted row, `<w:tblPrChange>` / `<w:trPrChange>`
/// history).
///
/// a. `<w:trPr><w:del/>` / `<w:ins/>` read as row revisions; the
///    untouched save is byte-identical on both save paths.
/// b. Typing in any row regenerates the table as a pure insertion: the
///    row revisions and the property history re-emit verbatim.
/// c. Accept-all removes the deleted row and keeps the inserted one,
///    reject-all the reverse; both save with no `<w:ins>` / `<w:del>`
///    left (the property history stays) and re-read clean.
/// d. A tracked deletion of the first two rows (start of the first cell
///    to the end of the second row) saves `<w:trPr><w:del …/>` on the
///    source row under a fresh id; on the re-read, accepting that row
///    alone (by its `revision_id`) removes it, accept-all the rest.
pub(crate) fn run_tracked_table_rows_roundtrip() -> Result<()> {
    let bytes = format_docx::test_fixtures::tracked_table_rows_docx();
    let source = extract_doc_xml(&bytes)?;
    let archive = read_docx(&bytes).context("read tracked-rows fixture")?;
    let doc = &archive.document;
    let fixture = vec![
        row(&[], ["kept A", "kept B"]),
        row(&[RevisionKind::Delete], ["gone A", "gone B"]),
        row(&[RevisionKind::Insert], ["new A", "new B"]),
    ];
    if table_rows(doc) != fixture {
        bail!("step 48a: rows read as {:?}", table_rows(doc));
    }
    for (path, out) in [
        ("write_docx", write_docx(&archive, doc).context("write")?),
        ("save_docx", format_docx::save_docx(doc).context("ui save")?),
    ] {
        if extract_doc_xml(&out)? != source {
            bail!("step 48a: the untouched {path} save drifted");
        }
    }
    println!(
        "[roundtrip] step 48a OK — trPr ins / del read as row revisions; zero-edit save byte-identical"
    );

    for (r, c) in [(0, 0), (1, 1), (2, 0)] {
        let edited = doc.insert_text(cell(r, c, 2), "XY");
        for (path, out) in [
            (
                "write_docx",
                write_docx(&archive, &edited).context("write")?,
            ),
            (
                "save_docx",
                format_docx::save_docx(&edited).context("ui save")?,
            ),
        ] {
            assert_document_xml_well_formed(&out).with_context(|| format!("step 48b {path}"))?;
            let got = extract_doc_xml(&out)?;
            let (_, rewritten, _) = rewritten_region(&source, &got);
            if rewritten != 0 {
                bail!(
                    "step 48b {path}: typing in row {r} rewrote {rewritten} source bytes\n{}",
                    String::from_utf8_lossy(&got)
                );
            }
        }
    }
    println!(
        "[roundtrip] step 48b OK — typing in a tracked table is a pure insertion (row revisions + trPrChange / tblPrChange verbatim)"
    );

    for (accept, want) in [
        (
            true,
            vec![row(&[], ["kept A", "kept B"]), row(&[], ["new A", "new B"])],
        ),
        (
            false,
            vec![
                row(&[], ["kept A", "kept B"]),
                row(&[], ["gone A", "gone B"]),
            ],
        ),
    ] {
        let resolved = doc.resolve_all_revisions(accept);
        if table_rows(&resolved) != want || resolved.has_revisions() {
            bail!(
                "step 48c (accept={accept}): resolved to {:?}",
                table_rows(&resolved)
            );
        }
        for (got, reread) in save_and_reread("step 48c", &archive, &resolved)? {
            if got.contains("<w:del ")
                || got.contains("<w:ins ")
                || got.contains("<w:delText")
                || !got.contains("<w:trPrChange ")
                || !got.contains("<w:tblPrChange ")
                || reread.has_revisions()
                || table_rows(&reread) != want
            {
                bail!(
                    "step 48c (accept={accept}): saved {:?}\n{got}",
                    table_rows(&reread)
                );
            }
        }
    }
    println!(
        "[roundtrip] step 48c OK — accept-all / reject-all remove the right row; saves clean, property history kept"
    );

    let deleted = doc
        .try_tracked_delete_range(cell(0, 0, 0), cell(1, 1, 6), REVIEWER, REVIEW_DATE)
        .map_err(|e| anyhow::anyhow!("step 48d: refused: {e}"))?
        .doc;
    let want = vec![
        row(&[RevisionKind::Delete], ["kept A", "kept B"]),
        row(&[RevisionKind::Delete], ["gone A", "gone B"]),
        row(&[RevisionKind::Insert], ["new A", "new B"]),
    ];
    if table_rows(&deleted) != want {
        bail!("step 48d: recorded {:?}", table_rows(&deleted));
    }
    let mut reread_deleted = None;
    for (got, reread) in save_and_reread("step 48d", &archive, &deleted)? {
        let minted = format!(r#"" w:author="{REVIEWER}" w:date="{REVIEW_DATE}"/><w:trPrChange "#);
        if !got.contains(&minted) || table_rows(&reread) != want {
            bail!("step 48d: saved {:?}\n{got}", table_rows(&reread));
        }
        reread_deleted = Some(reread);
    }
    let reread = reread_deleted.context("no re-read")?;
    let first = reread
        .revision_entries()
        .into_iter()
        .find(|e| e.at.slot == (engine::RevisionSlot::Row { row: 0, index: 0 }))
        .context("step 48d: the first row's change is not listed")?;
    let one = reread
        .resolve_revision(&first.at, true)
        .context("step 48d: no such revision")?;
    if table_rows(&one) != want[1..] {
        bail!("step 48d: accepting row 0 left {:?}", table_rows(&one));
    }
    let all = one.resolve_all_revisions(true);
    if table_rows(&all) != vec![row(&[], ["new A", "new B"])] || all.has_revisions() {
        bail!("step 48d: accept-all left {:?}", table_rows(&all));
    }
    println!(
        "[roundtrip] step 48d OK — a tracked deletion of two rows saves trPr/del (fresh id); one row accepts by id, accept-all the rest"
    );
    Ok(())
}

/// The doc's sections: `(page width in twips, default header rid, first
/// header rid)` per effective section, the headers as resolved through
/// link-to-previous.
fn sections(doc: &DocumentTree) -> Vec<(i32, Option<String>, Option<String>)> {
    let secs = doc.effective_sections();
    let resolved = engine::resolve_hf_inheritance(&secs);
    secs.iter()
        .zip(resolved)
        .map(|(s, (h, _))| {
            (
                (s.geometry.width * 20.0).round() as i32,
                h.default.clone(),
                h.first.clone(),
            )
        })
        .collect()
}

/// Issue #367 — step 49: a section break on a tracked paragraph mark
/// (`section_break_revision.docx`: the first section's `<w:sectPr>` on a
/// deleted mark, its header references inherited by the final section).
///
/// a. The mark reads as a deletion carrying the section break; the
///    untouched save is byte-identical on both save paths.
/// b. Accept-all merges paragraph 0 into the FOLLOWING section (Word's
///    rule): one section, the final section's A4 page; the dropped
///    section's header refs fill the final section's empty slots, so the
///    headers resolve to the same parts. Saved on both paths: one
///    `<w:sectPr>` carrying both `<w:headerReference>`s, no `<w:del>`;
///    the re-read agrees.
/// c. Reject-all keeps both sections, the revision gone, saved clean.
/// d. The same break as a tracked INSERTION: reject-all removes it,
///    accept-all keeps it.
pub(crate) fn run_section_break_revision_roundtrip() -> Result<()> {
    use format_docx::test_fixtures::{SECTION_BREAK_TEXTS, section_break_revision_docx};
    let [t0, t1, t2] = SECTION_BREAK_TEXTS;
    let bytes = section_break_revision_docx(false);
    let source = extract_doc_xml(&bytes)?;
    let archive = read_docx(&bytes).context("read section-break fixture")?;
    let doc = &archive.document;
    let h1 = Some("rIdH1".to_string());
    let h2 = Some("rIdH2".to_string());
    let small = (7920, h1.clone(), h2.clone());
    let a4 = |h: Option<String>, f: Option<String>| (11906, h, f);
    let p0 = doc.nth_paragraph(0).context("paragraph 0")?;
    if p0.mark_revisions.iter().map(|r| r.kind).collect::<Vec<_>>() != vec![RevisionKind::Delete]
        || p0.section_end.is_none()
        || sections(doc) != vec![small.clone(), a4(h1.clone(), h2.clone())]
    {
        bail!(
            "step 49a: read {:?} / {:?}",
            p0.mark_revisions,
            sections(doc)
        );
    }
    for (path, out) in [
        ("write_docx", write_docx(&archive, doc).context("write")?),
        ("save_docx", format_docx::save_docx(doc).context("ui save")?),
    ] {
        if extract_doc_xml(&out)? != source {
            bail!("step 49a: the untouched {path} save drifted");
        }
    }
    println!(
        "[roundtrip] step 49a OK — a deleted mark carrying a section break reads; zero-edit save byte-identical"
    );

    let merged_texts = vec![format!("{t0}{t1}"), t2.to_string()];
    let accepted = doc.resolve_all_revisions(true);
    let want = vec![a4(h1.clone(), h2.clone())];
    if texts(&accepted) != merged_texts || sections(&accepted) != want || accepted.has_revisions() {
        bail!(
            "step 49b: accepted {:?} / {:?}",
            texts(&accepted),
            sections(&accepted)
        );
    }
    for (got, reread) in save_and_reread("step 49b", &archive, &accepted)? {
        let refs = [
            r#"<w:headerReference w:type="default" r:id="rIdH1"/>"#,
            r#"<w:headerReference w:type="first" r:id="rIdH2"/>"#,
        ];
        if got.contains("<w:del ")
            || got.matches("<w:sectPr").count() != 1
            || refs.iter().any(|r| !got.contains(r))
            || texts(&reread) != merged_texts
            || sections(&reread) != want
            || !reread.headers.contains_key("rIdH1")
        {
            bail!(
                "step 49b: saved {:?} / {:?}\n{got}",
                texts(&reread),
                sections(&reread)
            );
        }
    }
    println!(
        "[roundtrip] step 49b OK — accept-all joins the following section (A4); the dropped section's headers backfill its empty slots"
    );

    let rejected = doc.resolve_all_revisions(false);
    let want = vec![small.clone(), a4(h1.clone(), h2.clone())];
    if texts(&rejected) != vec![t0, t1, t2]
        || sections(&rejected) != want
        || rejected.has_revisions()
    {
        bail!("step 49c: rejected {:?}", sections(&rejected));
    }
    for (got, reread) in save_and_reread("step 49c", &archive, &rejected)? {
        if got.contains("<w:del ")
            || got.matches("<w:sectPr").count() != 2
            || sections(&reread) != want
            || reread.has_revisions()
        {
            bail!("step 49c: saved {:?}\n{got}", sections(&reread));
        }
    }
    println!("[roundtrip] step 49c OK — reject-all keeps both sections, saved clean");

    let inserted = read_docx(&section_break_revision_docx(true))
        .context("read inserted-break fixture")?
        .document;
    let rejected = inserted.resolve_all_revisions(false);
    let accepted = inserted.resolve_all_revisions(true);
    if texts(&rejected) != merged_texts
        || sections(&rejected) != vec![a4(h1.clone(), h2.clone())]
        || sections(&accepted) != vec![small, a4(h1, h2)]
        || rejected.has_revisions()
        || accepted.has_revisions()
    {
        bail!(
            "step 49d: rejected {:?} / accepted {:?}",
            sections(&rejected),
            sections(&accepted)
        );
    }
    println!(
        "[roundtrip] step 49d OK — a tracked section break (inserted mark): reject removes it, accept keeps it"
    );
    Ok(())
}
