//! Issue #282 — step 38: comments on paragraphs the writer replays from
//! their source bytes.

use super::{
    BARE_SECT_PR, assert_document_xml_well_formed, entry_bytes, extract_doc_xml,
    package_document_xml_with_parts, read_docx, write_docx,
};
use anyhow::{Context, Result, bail};
use engine::{BlockPath, DocumentTree, LogicalPos, PathStep};

/// Paragraph 0 carries comment 0 (on "comment "), paragraph 1 is plain;
/// the table's second row holds "three four".
const BODY: &str = concat!(
    r#"<w:p w:rsidR="00B561CA"><w:r><w:t xml:space="preserve">this is a </w:t></w:r>"#,
    r#"<w:commentRangeStart w:id="0"/><w:r><w:t xml:space="preserve">comment </w:t></w:r>"#,
    r#"<w:commentRangeEnd w:id="0"/><w:r w:rsidR="002903BF"><w:rPr><w:rStyle w:val="CommentReference"/></w:rPr><w:commentReference w:id="0"/></w:r>"#,
    r#"<w:r><w:t>paragraph!</w:t></w:r></w:p>"#,
    r#"<w:p w:rsidR="00C0FFEE"><w:r w:rsidRPr="00AB12CD"><w:rPr><w:i/></w:rPr><w:t>alpha beta gamma</w:t></w:r></w:p>"#,
    r#"<w:tbl><w:tblPr><w:tblW w:w="0" w:type="auto"/></w:tblPr><w:tblGrid><w:gridCol w:w="4000"/></w:tblGrid>"#,
    r#"<w:tr><w:tc><w:p><w:r><w:t>one</w:t></w:r></w:p></w:tc></w:tr>"#,
    r#"<w:tr><w:tc><w:p><w:r><w:t>three four</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#,
);

const COMMENTS: &str = concat!(
    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
    r#"<w:comments xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">"#,
    r#"<w:comment w:id="0" w:author="A" w:date="2026-01-01T00:00:00Z" w:initials="A"><w:p><w:r><w:t>first</w:t></w:r></w:p></w:comment>"#,
    r#"</w:comments>"#,
);

fn build() -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{BODY}{BARE_SECT_PR}</w:body></w:document>"#
    );
    let rels = concat!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
        r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#,
        r#"<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments" Target="comments.xml"/>"#,
        r#"</Relationships>"#,
    );
    package_document_xml_with_parts(&document_xml, rels, &[("word/comments.xml", COMMENTS)])
}

/// `edited` is `orig` plus insertions only (a byte-level minimal diff).
fn pure_insertion(orig: &[u8], edited: &[u8]) -> bool {
    use format_docx::schema::anchor_patch::{Op, diff_by};
    let Some(budget) = edited.len().checked_sub(orig.len()) else {
        return false;
    };
    diff_by(
        orig.len(),
        edited.len(),
        |i, j| orig[i] == edited[j],
        budget,
    )
    .is_some_and(|ops| !ops.iter().any(|o| matches!(o, Op::Delete(_))))
}

/// The text comment `id`'s (single-paragraph) range covers.
fn covered(doc: &DocumentTree, id: u32) -> Option<String> {
    let r = doc.comment_ranges.iter().find(|r| r.id == id)?;
    let p = doc.paragraph_at_path(&r.start.path)?;
    (r.start.path == r.end.path)
        .then(|| p.text.get(r.start.offset as usize..r.end.offset as usize))
        .flatten()
        .map(str::to_string)
}

/// Both save paths for `doc` (they must agree).
fn save_both(archive: &format_docx::DocxArchive, doc: &DocumentTree) -> Result<Vec<u8>> {
    let bytes = write_docx(archive, doc).context("write_docx")?;
    let ui = format_docx::save_docx(doc).context("save_docx")?;
    if ui != bytes {
        bail!("save_docx and write_docx disagree");
    }
    assert_document_xml_well_formed(&bytes)?;
    Ok(bytes)
}

/// Issue #282 — step 38.
///
/// a. A comment added to an untouched paragraph, and one added inside an
///    untouched table, are spliced into the replayed source bytes: the
///    saved `document.xml` and `comments.xml` are the source plus
///    insertions only (both save paths), and the re-read comments cover
///    the same text with their bodies.
/// b. Deleting the source comment of an untouched paragraph removes its
///    anchors (reference run included) and its `comments.xml` entry as a
///    pure deletion; the re-read document has no trace of it.
pub fn run_comment_patch_roundtrip() -> Result<()> {
    let fixture = build();
    let archive = read_docx(&fixture).context("read comment patch fixture")?;
    let doc_xml = extract_doc_xml(&fixture)?;
    if extract_doc_xml(&write_docx(&archive, &archive.document)?)? != doc_xml {
        bail!("step 38: untouched save drifted");
    }

    let top = |block: u32, offset: u32| LogicalPos::new(BlockPath::top(block), offset);
    let cell = BlockPath {
        steps: vec![
            PathStep::Block(2),
            PathStep::Cell { row: 1, col: 0 },
            PathStep::Block(0),
        ],
    };
    let (doc, para_id) = archive.document.insert_comment(
        top(1, 6),
        top(1, 10),
        "on beta".into(),
        "Reviewer".into(),
        "2026-10-09T00:00:00Z".into(),
    );
    let (doc, cell_id) = doc.insert_comment(
        LogicalPos::new(cell.clone(), 6),
        LogicalPos::new(cell, 10),
        "on four".into(),
        "Reviewer".into(),
        "2026-10-09T00:00:00Z".into(),
    );
    let bytes = save_both(&archive, &doc)?;
    let xml = extract_doc_xml(&bytes)?;
    if !pure_insertion(&doc_xml, &xml) {
        bail!(
            "step 38a: document.xml is not source + insertions:\n{}",
            String::from_utf8_lossy(&xml)
        );
    }
    let back = read_docx(&bytes).context("re-read")?;
    let comments = entry_bytes(&back, "word/comments.xml").context("comments.xml")?;
    if !pure_insertion(COMMENTS.as_bytes(), comments) {
        bail!("step 38a: comments.xml is not source + insertions");
    }
    if covered(&back.document, para_id).as_deref() != Some("beta") {
        bail!(
            "step 38a: paragraph comment re-reads on {:?}",
            covered(&back.document, para_id)
        );
    }
    for (id, text) in [(para_id, "on beta"), (cell_id, "on four")] {
        if back
            .document
            .comment_defs
            .get(&id)
            .map(|d| d.paragraphs.join("\n"))
            .as_deref()
            != Some(text)
        {
            bail!("step 38a: comment {id} lost its body");
        }
    }
    println!(
        "[roundtrip] step 38a OK — comments added to an untouched paragraph and table cell are spliced in (source + insertions, both save paths)"
    );

    let deleted = archive.document.delete_comment(0);
    let bytes = save_both(&archive, &deleted)?;
    let xml = extract_doc_xml(&bytes)?;
    if !pure_insertion(&xml, &doc_xml) || String::from_utf8_lossy(&xml).contains("w:comment") {
        bail!(
            "step 38b: delete is not a pure deletion of every anchor:\n{}",
            String::from_utf8_lossy(&xml)
        );
    }
    let back = read_docx(&bytes).context("re-read deleted")?;
    if !back.document.comment_defs.is_empty() || !back.document.comment_ranges.is_empty() {
        bail!("step 38b: the deleted comment re-reads");
    }
    println!(
        "[roundtrip] step 38b OK — a deleted comment leaves the untouched paragraph and comments.xml (pure deletion)"
    );
    Ok(())
}
