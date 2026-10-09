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
