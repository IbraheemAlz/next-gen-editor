//! Positioned verbatim spans in `Paragraph::source_markup` (issues #244 /
//! #245 / #246): unmodeled paragraph content a regenerated paragraph must
//! keep whole — PRD Tier 3, "never lose content".

use super::{
    INSERT_TEXT, assert_document_xml_well_formed, build_styled_docx, extract_doc_xml, read_docx,
    write_docx,
};
use anyhow::{Context, Result, bail};
use engine::{BlockPath, LogicalPos};

const STYLES_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"/>"#;

fn document(body: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml" xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006" mc:Ignorable="w14"><w:body>{body}<w:sectPr/></w:body></w:document>"#
    )
}

fn at(block: u32, offset: usize) -> LogicalPos {
    LogicalPos {
        path: BlockPath::top(block),
        offset: offset as u32,
    }
}

/// Every edit in `edits` (`(block, offset)` of an [`INSERT_TEXT`]
/// insertion, and the source substring `from` → `to` it must produce)
/// saves as EXACTLY the source plus the inserted bytes, on both save paths.
fn assert_pure_insertions(
    step: &str,
    source_xml: &str,
    archive: &format_docx::DocxArchive,
    edits: &[(u32, usize, &str, String)],
) -> Result<()> {
    for (block, offset, from, to) in edits {
        let edited = archive
            .document
            .insert_text(at(*block, *offset), INSERT_TEXT);
        let expected = source_xml.replacen(from, to, 1);
        if expected == source_xml {
            bail!("{step}: expectation pattern {from:?} not in the source");
        }
        for (path, bytes) in [
            ("write_docx", write_docx(archive, &edited).context("write")?),
            (
                "save_docx",
                format_docx::save_docx(&edited).context("ui save")?,
            ),
        ] {
            assert_document_xml_well_formed(&bytes).with_context(|| format!("{step} {path}"))?;
            let xml = String::from_utf8(extract_doc_xml(&bytes)?).context("utf8")?;
            if xml != expected {
                bail!(
                    "{step} {path}: edit at {block}:{offset} is not source + insert\n--- expected ---\n{expected}\n--- got ---\n{xml}"
                );
            }
        }
    }
    Ok(())
}

/* ================================== #244 — legacy form fields ==== */

/// `checkboxes.docx`'s shape: a `FORMCHECKBOX` (begin `fldChar` carrying
/// `<w:ffData>`, the name bookmark inside the field, a text-less run, a
/// `separate`) between two text runs, and a separate-less `FORMDROPDOWN`
/// alone in the next paragraph.
const CHECKBOX: &str = concat!(
    r#"<w:r w:rsidR="00414CBC" w:rsidRPr="00045869"><w:rPr><w:lang w:val="de-DE"/></w:rPr><w:fldChar w:fldCharType="begin"><w:ffData><w:name w:val="Check1"/><w:enabled/><w:calcOnExit w:val="0"/><w:checkBox><w:sizeAuto/><w:default w:val="0"/></w:checkBox></w:ffData></w:fldChar></w:r>"#,
    r#"<w:bookmarkStart w:id="0" w:name="Check1"/>"#,
    r#"<w:r w:rsidRPr="00D051C0"><w:instrText xml:space="preserve"> FORMCHECKBOX </w:instrText></w:r>"#,
    r#"<w:r w:rsidR="00414CBC"><w:rPr><w:lang w:val="de-DE"/></w:rPr></w:r>"#,
    r#"<w:r w:rsidR="00414CBC"><w:rPr><w:lang w:val="de-DE"/></w:rPr><w:fldChar w:fldCharType="separate"/></w:r>"#,
    r#"<w:r w:rsidR="00414CBC" w:rsidRPr="00045869"><w:rPr><w:lang w:val="de-DE"/></w:rPr><w:fldChar w:fldCharType="end"/></w:r>"#,
    r#"<w:bookmarkEnd w:id="0"/>"#,
);
const DROPDOWN: &str = concat!(
    r#"<w:r><w:fldChar w:fldCharType="begin"><w:ffData><w:name w:val="Drop1"/><w:enabled/><w:ddList><w:listEntry w:val="one"/><w:listEntry w:val="two"/></w:ddList></w:ffData></w:fldChar></w:r>"#,
    r#"<w:r><w:instrText xml:space="preserve"> FORMDROPDOWN </w:instrText></w:r>"#,
    r#"<w:r><w:fldChar w:fldCharType="end"/></w:r>"#,
);

/// Issue #244 — step 27: legacy form fields.
///
/// a. An untouched save is byte-identical.
/// b. Edits before, at and after the checkbox, and into the paragraph
///    holding only the dropdown, are EXACTLY source + insert on both save
///    paths: the `fldChar` / `ffData` bytes survive whole.
/// c. The edited file re-reads with both fields as content spans.
pub(crate) fn run_form_fields_roundtrip() -> Result<()> {
    let body = format!(
        r#"<w:p w:rsidR="00D8217F"><w:r><w:t xml:space="preserve">unchecked: </w:t></w:r>{CHECKBOX}<w:r w:rsidRPr="00D051C0"><w:t xml:space="preserve"> after</w:t></w:r></w:p><w:p w:rsidR="00C8128C">{DROPDOWN}</w:p>"#
    );
    let xml = document(&body);
    let bytes = build_styled_docx(STYLES_XML, &xml);
    let archive = read_docx(&bytes).context("read form-field fixture")?;
    let untouched = write_docx(&archive, &archive.document).context("untouched save")?;
    if extract_doc_xml(&untouched)? != xml.as_bytes() {
        bail!("step 27: untouched form-field document drifted");
    }
    println!("[roundtrip] step 27a OK — untouched save byte-identical");

    let field_at = "unchecked: ".len();
    assert_pure_insertions(
        "step 27b",
        &xml,
        &archive,
        &[
            (0, 4, "unchecked", format!("unch{INSERT_TEXT}ecked")),
            (
                0,
                field_at,
                "unchecked: <",
                format!("unchecked: {INSERT_TEXT}<"),
            ),
            (
                0,
                field_at + " aft".len(),
                " after",
                format!(" aft{INSERT_TEXT}er"),
            ),
            (
                1,
                0,
                r#"<w:p w:rsidR="00C8128C">"#,
                format!(
                    r#"<w:p w:rsidR="00C8128C"><w:r><w:t xml:space="preserve">{INSERT_TEXT}</w:t></w:r>"#
                ),
            ),
        ],
    )?;
    println!(
        "[roundtrip] step 27b OK — edits around form fields are source + insert (ffData whole)"
    );

    let edited = archive.document.insert_text(at(0, 0), INSERT_TEXT);
    let back = read_docx(&write_docx(&archive, &edited)?).context("re-read")?;
    let spans = (0..2)
        .filter_map(|i| back.document.nth_paragraph(i))
        .filter_map(|p| p.source_markup.as_deref())
        .flat_map(|m| m.markers.iter())
        .filter(|mk| mk.role.must_survive())
        .count();
    if spans != 2 {
        bail!("step 27c: {spans} form-field spans on re-read, expected 2");
    }
    println!("[roundtrip] step 27c OK — both form fields re-read as content spans");
    Ok(())
}

/* ============================ #245 — run-level content controls ==== */

/// `Bug64561.docx`'s shape (Word, tab-indented): nested run-level
/// controls around one run, then a `_GoBack` bookmark.
const SDT_NESTED: &str = "<w:p w:rsidR=\"005828DB\">\n\t\t\t<w:sdt>\n\t\t\t\t<w:sdtPr>\n\t\t\t\t\t<w:alias w:val=\"subject[@list=1]\"/>\n\t\t\t\t\t<w:id w:val=\"1332796321\"/>\n\t\t\t\t</w:sdtPr>\n\t\t\t\t<w:sdtContent>\n\t\t\t\t\t<w:sdt>\n\t\t\t\t\t\t<w:sdtPr>\n\t\t\t\t\t\t\t<w:alias w:val=\"subjectline\"/>\n\t\t\t\t\t\t\t<w:id w:val=\"614486968\"/>\n\t\t\t\t\t\t</w:sdtPr>\n\t\t\t\t\t\t<w:sdtContent>\n\t\t\t\t\t\t\t<w:r>\n\t\t\t\t\t\t\t\t<w:t>Subject</w:t>\n\t\t\t\t\t\t\t</w:r>\n\t\t\t\t\t\t</w:sdtContent>\n\t\t\t\t\t</w:sdt>\n\t\t\t\t</w:sdtContent>\n\t\t\t</w:sdt>\n\t\t\t<w:bookmarkStart w:id=\"0\" w:name=\"_GoBack\"/>\n\t\t\t<w:bookmarkEnd w:id=\"0\"/>\n\t\t</w:p>";

/// `Bug66263-paragraph.docx`'s shape (Apache POI, space-indented): text,
/// a control whose run carries an rPr, text.
const SDT_BETWEEN: &str = r#"<w:p>
            <w:r><w:t xml:space="preserve">Before </w:t></w:r>
            <w:sdt>
                <w:sdtPr><w:id w:val="1001"/></w:sdtPr>
                <w:sdtContent>
                    <w:r>
                        <w:rPr><w:b w:val="on"/></w:rPr>
                        <w:t>SDTRun</w:t>
                    </w:r>
                </w:sdtContent>
            </w:sdt>
            <w:r><w:t xml:space="preserve"> After</w:t></w:r>
        </w:p>"#;

/// Issue #245 — step 28: run-level content controls.
///
/// a. An untouched save is byte-identical.
/// b. Edits inside the nested controls, inside the single control and
///    outside it are EXACTLY source + insert on both save paths (the
///    wrappers, the `sdtPr` bytes and the pretty-print whitespace all
///    survive).
/// c. The edited file re-reads with every opener / closer pair.
/// d. Splitting the paragraph inside a control keeps the part
///    well-formed and the control on the left half.
pub(crate) fn run_content_controls_roundtrip() -> Result<()> {
    let xml = document(&format!("{SDT_NESTED}{SDT_BETWEEN}"));
    let archive = read_docx(&build_styled_docx(STYLES_XML, &xml)).context("read sdt fixture")?;
    let untouched = write_docx(&archive, &archive.document).context("untouched save")?;
    if extract_doc_xml(&untouched)? != xml.as_bytes() {
        bail!("step 28: untouched content-control document drifted");
    }
    println!("[roundtrip] step 28a OK — untouched save byte-identical");

    assert_pure_insertions(
        "step 28b",
        &xml,
        &archive,
        &[
            (0, 3, ">Subject<", format!(">Sub{INSERT_TEXT}ject<")),
            (0, 7, ">Subject<", format!(">Subject{INSERT_TEXT}<")),
            (1, 9, ">SDTRun<", format!(">SD{INSERT_TEXT}TRun<")),
            (1, 3, ">Before <", format!(">Bef{INSERT_TEXT}ore <")),
            (1, 16, "> After<", format!("> Af{INSERT_TEXT}ter<")),
        ],
    )?;
    println!("[roundtrip] step 28b OK — edits inside / outside controls are source + insert");

    let edited = archive.document.insert_text(at(1, 9), INSERT_TEXT);
    let back = read_docx(&write_docx(&archive, &edited)?).context("re-read")?;
    let ends = |i: u32| -> usize {
        back.document
            .nth_paragraph(i)
            .and_then(|p| p.source_markup.as_deref())
            .map_or(0, |m| {
                m.markers
                    .iter()
                    .filter(|mk| {
                        matches!(
                            mk.role,
                            engine::MarkerRole::Open { .. } | engine::MarkerRole::Close { .. }
                        )
                    })
                    .count()
            })
    };
    if (ends(0), ends(1)) != (4, 2) {
        bail!(
            "step 28c: control ends on re-read {:?}, expected (4, 2)",
            (ends(0), ends(1))
        );
    }
    println!("[roundtrip] step 28c OK — every control re-reads as an opener / closer pair");

    let split = archive.document.split_paragraph(at(1, 10));
    for bytes in [
        write_docx(&archive, &split).context("write split")?,
        format_docx::save_docx(&split).context("ui save split")?,
    ] {
        assert_document_xml_well_formed(&bytes).context("step 28d: split")?;
        let out = String::from_utf8(extract_doc_xml(&bytes)?).context("utf8")?;
        if out.matches("<w:sdt>").count() != 3 || out.matches("</w:sdt>").count() != 3 {
            bail!("step 28d: split lost or duplicated a control\n{out}");
        }
    }
    println!("[roundtrip] step 28d OK — a split inside a control stays well-formed");
    Ok(())
}
