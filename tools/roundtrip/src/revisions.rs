//! Tracked-change structure a regenerated paragraph must keep (issues
//! #247 / #262): move wrappers and their range markers, paragraph-mark
//! revisions, and the engine-side accept-all / reject-all.

use super::{
    INSERT_TEXT, assert_document_xml_well_formed, build_styled_docx, extract_doc_xml, read_docx,
    rewritten_region, write_docx,
};
use anyhow::{Context, Result, bail};
use engine::{BlockPath, LogicalPos, RevisionKind};

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

/// Issue #247 — step 30: tracked moves.
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
            "step 30a: moves not modeled: {:?} / {:?}",
            kinds(0),
            kinds(1)
        );
    }
    let untouched = write_docx(&archive, &archive.document).context("untouched save")?;
    if extract_doc_xml(&untouched)? != xml.as_bytes() {
        bail!("step 30a: untouched tracked-move document drifted");
    }
    println!(
        "[roundtrip] step 30a OK — moves read as MoveTo / MoveFrom, untouched save byte-identical"
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
                .with_context(|| format!("step 30b {path} {block}:{offset}"))?;
            let got = extract_doc_xml(&out)?;
            let (_, rewritten, _) = rewritten_region(xml.as_bytes(), &got);
            if rewritten != 0 {
                bail!(
                    "step 30b {path}: edit at {block}:{offset} rewrote {rewritten} source bytes\n{}",
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
                bail!("step 30b {path}: the move did not survive the edit at {block}:{offset}");
            }
        }
    }
    println!(
        "[roundtrip] step 30b OK — edits beside a move are pure insertions, the move re-reads"
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
            "step 30c: accept kept {:?}, reject kept {:?}",
            accepted.paragraph_text(0),
            rejected.paragraph_text(0)
        );
    }
    for (what, doc) in [("accepted", &accepted), ("rejected", &rejected)] {
        let out = format_docx::save_docx(doc).context("save resolved move")?;
        assert_document_xml_well_formed(&out).with_context(|| format!("step 30c {what}"))?;
        let xml = String::from_utf8(extract_doc_xml(&out)?).context("utf8")?;
        if xml.contains("<w:moveFrom ") || xml.contains("<w:moveTo ") {
            bail!("step 30c: the {what} move still carries a wrapper:\n{xml}");
        }
    }
    println!("[roundtrip] step 30c OK — accept keeps the destination, reject the source");
    Ok(())
}
