//! Tracked-change structure a regenerated paragraph must keep (issues
//! #247 / #262): move wrappers and their range markers, paragraph-mark
//! revisions, and the engine-side accept-all / reject-all.

use super::{
    INSERT_TEXT, assert_document_xml_well_formed, build_styled_docx, extract_doc_xml, read_docx,
    rewritten_region, write_docx,
};
use anyhow::{Context, Result, bail};
use engine::{Alignment, BlockPath, DocumentTree, LogicalPos, RevisionKind};

const STYLES_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"/>"#;

fn document(body: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}<w:sectPr/></w:body></w:document>"#
    )
}

fn at(block: u32, offset: usize) -> LogicalPos {
    LogicalPos {
        path: BlockPath::top(block),
        offset: offset as u32,
    }
}

/// `Tika-792.docx`'s shape (Apache POI corpus): the moved text's
/// destination (`<w:moveTo>` holding a `<w:del>`) in the first paragraph,
/// its source (`<w:moveFrom>` holding an `<w:ins>` whose run carries a
/// `<w:rPrChange>`) in the second, both inside named move ranges whose
/// markers interleave across the paragraph boundary.
pub(crate) const TIKA_792_BODY: &str = concat!(
    r#"<w:p w:rsidR="00910EBC" w:rsidRDefault="004F232C" w:rsidP="00910EBC">"#,
    r#"<w:bookmarkStart w:id="0" w:name="_GoBack"/><w:bookmarkEnd w:id="0"/>"#,
    r#"<w:del w:id="1" w:author="Author"><w:r><w:delText>s</w:delText></w:r></w:del>"#,
    r#"<w:moveToRangeStart w:id="2" w:author="Author" w:name="move256509658"/>"#,
    r#"<w:moveTo w:id="3" w:author="Author"><w:del w:id="4" w:author="Author"><w:r><w:delText>.</w:delText></w:r></w:del></w:moveTo>"#,
    r#"</w:p>"#,
    r#"<w:p w:rsidR="00BE45BB" w:rsidRDefault="004F232C" w:rsidP="00910EBC">"#,
    r#"<w:moveFromRangeStart w:id="5" w:author="Author" w:name="move256509658"/><w:moveToRangeEnd w:id="2"/>"#,
    r#"<w:moveFrom w:id="6" w:author="Author"><w:ins w:id="7" w:author="Author"><w:r w:rsidRPr="00227EAB"><w:rPr><w:rPrChange w:id="8" w:author="Author"><w:rPr><w:color w:val="FF0000"/></w:rPr></w:rPrChange></w:rPr><w:t>b</w:t></w:r></w:ins></w:moveFrom>"#,
    r#"<w:moveFromRangeEnd w:id="5"/>"#,
    r#"</w:p>"#,
);

/// Issue #247 — step 31: tracked moves.
///
/// a. The move wrappers read as `MoveTo` / `MoveFrom` revisions named by
///    their range; an untouched save is byte-identical.
/// b. An unrelated edit in either paragraph is a pure insertion on both
///    save paths: the `<w:moveTo>` / `<w:moveFrom>` wrappers regenerate
///    (the same-range `<w:moveTo><w:del>` nesting in source order) and
///    the range markers ride as positioned verbatim markup.
/// c. Accept keeps the destination text and drops the source; reject the
///    reverse.
pub(crate) fn run_tracked_moves_roundtrip() -> Result<()> {
    let xml = document(TIKA_792_BODY);
    let bytes = build_styled_docx(STYLES_XML, &xml);
    let archive = read_docx(&bytes).context("read tracked-move fixture")?;
    let doc = &archive.document;
    let kinds = |block: u32| -> Vec<(RevisionKind, u32, u32, Option<String>)> {
        doc.nth_paragraph(block)
            .map(|p| {
                p.revisions
                    .iter()
                    .map(|r| (r.kind, r.start, r.end, r.move_name.clone()))
                    .collect()
            })
            .unwrap_or_default()
    };
    let name = Some("move256509658".to_string());
    if !kinds(0).contains(&(RevisionKind::MoveTo, 1, 2, name.clone()))
        || !kinds(1).contains(&(RevisionKind::MoveFrom, 0, 1, name.clone()))
    {
        bail!(
            "step 31a: moves not modeled: {:?} / {:?}",
            kinds(0),
            kinds(1)
        );
    }
    let untouched = write_docx(&archive, &archive.document).context("untouched save")?;
    if extract_doc_xml(&untouched)? != xml.as_bytes() {
        bail!("step 31a: untouched tracked-move document drifted");
    }
    println!(
        "[roundtrip] step 31a OK — moves read as MoveTo / MoveFrom, untouched save byte-identical"
    );

    for (block, offset) in [(0u32, 1usize), (1, 1), (1, 0)] {
        let edited = archive.document.insert_text(at(block, offset), INSERT_TEXT);
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
            assert_document_xml_well_formed(&out)
                .with_context(|| format!("step 31b {path} {block}:{offset}"))?;
            let got = extract_doc_xml(&out)?;
            let (_, rewritten, _) = rewritten_region(xml.as_bytes(), &got);
            if rewritten != 0 {
                bail!(
                    "step 31b {path}: edit at {block}:{offset} rewrote {rewritten} source bytes\n{}",
                    String::from_utf8_lossy(&got)
                );
            }
            let reread = read_docx(&out).context("re-read")?;
            let p = reread.document.nth_paragraph(block).context("para")?;
            let want = if block == 0 {
                RevisionKind::MoveTo
            } else {
                RevisionKind::MoveFrom
            };
            if !p
                .revisions
                .iter()
                .any(|r| r.kind == want && r.move_name == name)
            {
                bail!("step 31b {path}: the move did not survive the edit at {block}:{offset}");
            }
        }
    }
    println!(
        "[roundtrip] step 31b OK — edits beside a move are pure insertions, the move re-reads"
    );

    /* c. A one-paragraph move: the source half first, the destination
    after the unmoved text. */
    let simple = document(concat!(
        r#"<w:p><w:moveFromRangeStart w:id="1" w:name="m"/>"#,
        r#"<w:moveFrom w:id="2" w:author="A"><w:r><w:t>moved</w:t></w:r></w:moveFrom>"#,
        r#"<w:moveFromRangeEnd w:id="1"/><w:r><w:t xml:space="preserve"> stay </w:t></w:r>"#,
        r#"<w:moveToRangeStart w:id="3" w:name="m"/>"#,
        r#"<w:moveTo w:id="4" w:author="A"><w:r><w:t>moved</w:t></w:r></w:moveTo>"#,
        r#"<w:moveToRangeEnd w:id="3"/></w:p>"#,
    ));
    let simple = read_docx(&build_styled_docx(STYLES_XML, &simple)).context("read simple move")?;
    let d = &simple.document;
    let accepted = d.accept_revision_at(0, 11, 16).accept_revision_at(0, 0, 5);
    let rejected = d.reject_revision_at(0, 11, 16).reject_revision_at(0, 0, 5);
    if accepted.paragraph_text(0) != Some(" stay moved")
        || rejected.paragraph_text(0) != Some("moved stay ")
    {
        bail!(
            "step 31c: accept kept {:?}, reject kept {:?}",
            accepted.paragraph_text(0),
            rejected.paragraph_text(0)
        );
    }
    for (what, doc) in [("accepted", &accepted), ("rejected", &rejected)] {
        let out = format_docx::save_docx(doc).context("save resolved move")?;
        assert_document_xml_well_formed(&out).with_context(|| format!("step 31c {what}"))?;
        let xml = String::from_utf8(extract_doc_xml(&out)?).context("utf8")?;
        if xml.contains("<w:moveFrom ") || xml.contains("<w:moveTo ") {
            bail!("step 31c: the {what} move still carries a wrapper:\n{xml}");
        }
    }
    println!("[roundtrip] step 31c OK — accept keeps the destination, reject the source");
    Ok(())
}

/// Paragraph-mark revisions (issue #262): the first paragraph's mark is a
/// tracked deletion (a merge with the next) next to other mark formatting,
/// the second's a tracked insertion (a split).
const MARKS_BODY: &str = concat!(
    r#"<w:p w:rsidR="00A1B2C3"><w:pPr><w:jc w:val="center"/><w:rPr><w:del w:id="10" w:author="A" w:date="2026-01-01T00:00:00Z"/><w:b/></w:rPr></w:pPr>"#,
    r#"<w:del w:id="11" w:author="A" w:date="2026-01-01T00:00:00Z"><w:r><w:delText xml:space="preserve">gone </w:delText></w:r></w:del>"#,
    r#"<w:r><w:t xml:space="preserve">head </w:t></w:r></w:p>"#,
    r#"<w:p w:rsidR="00D4E5F6"><w:pPr><w:rPr><w:ins w:id="12" w:author="B" w:date="2026-01-02T00:00:00Z"/></w:rPr></w:pPr><w:r><w:t>tail</w:t></w:r></w:p>"#,
    r#"<w:p><w:r><w:t>last</w:t></w:r></w:p>"#,
);

fn texts(doc: &DocumentTree) -> Vec<String> {
    doc.blocks
        .iter()
        .filter_map(engine::Block::as_paragraph)
        .map(|p| p.text.clone())
        .collect()
}

fn marks(doc: &DocumentTree) -> Vec<Option<(RevisionKind, Option<u32>)>> {
    doc.blocks
        .iter()
        .filter_map(engine::Block::as_paragraph)
        .map(|p| p.mark_revision.as_ref().map(|r| (r.kind, r.id)))
        .collect()
}

/// Issue #262 — step 32: paragraph-mark revisions + engine accept-all.
///
/// a. `<w:pPr><w:rPr><w:del/>` / `<w:ins/>` read as
///    `Paragraph::mark_revision`; an untouched save is byte-identical.
/// b. An edit in a paragraph with a tracked mark is a pure insertion
///    (the verified source `<w:pPr>` carries the mark).
/// c. A regenerated `<w:pPr>` (alignment change) re-injects the mark
///    revision into the mark's `<w:rPr>`; it re-reads.
/// d. Accept-all merges the deleted mark's paragraph, keeps the inserted
///    break; reject-all the reverse. Both save with no revision left.
pub(crate) fn run_paragraph_mark_revisions_roundtrip() -> Result<()> {
    let xml = document(MARKS_BODY);
    let bytes = build_styled_docx(STYLES_XML, &xml);
    let archive = read_docx(&bytes).context("read mark-revision fixture")?;
    let doc = &archive.document;
    let want = vec![
        Some((RevisionKind::Delete, Some(10))),
        Some((RevisionKind::Insert, Some(12))),
        None,
    ];
    if marks(doc) != want {
        bail!("step 32a: marks read as {:?}", marks(doc));
    }
    let untouched = write_docx(&archive, doc).context("untouched save")?;
    if extract_doc_xml(&untouched)? != xml.as_bytes() {
        bail!("step 32a: untouched mark-revision document drifted");
    }
    println!(
        "[roundtrip] step 32a OK — paragraph-mark ins / del modeled, untouched save byte-identical"
    );

    for (block, offset) in [(0u32, "gone head".len()), (1, 2)] {
        let edited = doc.insert_text(at(block, offset), INSERT_TEXT);
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
            assert_document_xml_well_formed(&out)
                .with_context(|| format!("step 32b {path} {block}:{offset}"))?;
            let got = extract_doc_xml(&out)?;
            let (_, rewritten, _) = rewritten_region(xml.as_bytes(), &got);
            if rewritten != 0 {
                bail!(
                    "step 32b {path}: edit at {block}:{offset} rewrote {rewritten} source bytes\n{}",
                    String::from_utf8_lossy(&got)
                );
            }
        }
    }
    println!("[roundtrip] step 32b OK — edits in tracked-mark paragraphs are pure insertions");

    let realigned = doc
        .set_alignment(at(0, 0), at(1, 0), Alignment::End)
        .insert_text(at(2, 0), INSERT_TEXT);
    let out = write_docx(&archive, &realigned).context("realigned save")?;
    assert_document_xml_well_formed(&out).context("step 32c")?;
    let got = String::from_utf8(extract_doc_xml(&out)?).context("utf8")?;
    for needle in [
        r#"<w:rPr><w:del w:id="10" w:author="A" w:date="2026-01-01T00:00:00Z"/><w:b/></w:rPr>"#,
        r#"<w:rPr><w:ins w:id="12" w:author="B" w:date="2026-01-02T00:00:00Z"/></w:rPr>"#,
    ] {
        if !got.contains(needle) {
            bail!("step 32c: regenerated pPr lost {needle}\n{got}");
        }
    }
    let reread = read_docx(&out).context("re-read realigned")?;
    if marks(&reread.document) != want {
        bail!("step 32c: marks re-read as {:?}", marks(&reread.document));
    }
    println!("[roundtrip] step 32c OK — a regenerated pPr re-injects the mark revision");

    for (accept, expect) in [
        (true, vec!["head tail", "last"]),
        (false, vec!["gone head ", "taillast"]),
    ] {
        let resolved = doc.resolve_all_revisions(accept);
        if texts(&resolved) != expect {
            bail!("step 32d (accept={accept}): {:?}", texts(&resolved));
        }
        let out = format_docx::save_docx(&resolved).context("save resolved")?;
        assert_document_xml_well_formed(&out).with_context(|| format!("step 32d {accept}"))?;
        let got = String::from_utf8(extract_doc_xml(&out)?).context("utf8")?;
        if got.contains("<w:del ") || got.contains("<w:ins ") || got.contains("<w:delText") {
            bail!("step 32d (accept={accept}): a revision survived\n{got}");
        }
        let reread = read_docx(&out).context("re-read resolved")?;
        if reread.document.has_revisions() || texts(&reread.document) != expect {
            bail!(
                "step 32d (accept={accept}): re-read {:?}",
                texts(&reread.document)
            );
        }
    }
    println!("[roundtrip] step 32d OK — accept-all / reject-all resolve marks, save clean");
    Ok(())
}
