//! Issue #328 — page borders, line numbers and drop caps: hand-written
//! OOXML in Word's own shape (A4, 1-inch margins, 11 pt docDefaults),
//! the sources of `tools/roundtrip`'s `page_borders.docx` /
//! `line_numbers.docx` / `drop_cap.docx` and the visual-diff goldens.

use super::{W_NS, rels, zip_entries};

/// The body paragraphs every decoration fixture shares (long enough to
/// wrap, so line numbers count wrapped lines).
pub const PAGE_DECOR_TEXTS: [&str; 3] = [
    "Line numbering, page borders and drop caps are section and paragraph \
     decorations that Word paints outside the text flow: the numbers sit in \
     the left margin, the border box frames the page.",
    "This second paragraph keeps the count going across a paragraph break, \
     and it wraps onto a second line so the count is visibly per line.",
    "A short closing paragraph.",
];

/// The paragraph `line_numbers.docx` suppresses (`w:suppressLineNumbers`).
pub const SUPPRESSED_TEXT: &str = "This paragraph suppresses its line numbers.";

fn text_para(text: &str) -> String {
    format!("<w:p><w:r><w:t xml:space=\"preserve\">{text}</w:t></w:r></w:p>")
}

/// A package with `body` and `sect_extra` spliced into the trailing
/// `<w:sectPr>` right after `<w:pgMar>` (where CT_SectPr puts
/// `<w:pgBorders>` / `<w:lnNumType>`).
pub fn package_with_sect_extra(body: &str, sect_extra: &str) -> Vec<u8> {
    let document = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
         <w:document xmlns:w=\"{W_NS}\"><w:body>{body}\
         <w:sectPr><w:pgSz w:w=\"11906\" w:h=\"16838\"/>\
         <w:pgMar w:top=\"1440\" w:right=\"1440\" w:bottom=\"1440\" w:left=\"1440\" \
         w:header=\"720\" w:footer=\"720\" w:gutter=\"0\"/>{sect_extra}</w:sectPr>\
         </w:body></w:document>"
    );
    let styles = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
         <w:styles xmlns:w=\"{W_NS}\"><w:docDefaults><w:rPrDefault><w:rPr>\
         <w:sz w:val=\"22\"/><w:szCs w:val=\"22\"/></w:rPr></w:rPrDefault>\
         </w:docDefaults></w:styles>"
    );
    let content_types = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
<Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
<Default Extension=\"xml\" ContentType=\"application/xml\"/>\
<Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/>\
<Override PartName=\"/word/styles.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml\"/>\
</Types>";
    let dot_rels = rels(&[(
        "rId1",
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument",
        "word/document.xml",
    )]);
    let doc_rels = rels(&[(
        "rId1",
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles",
        "styles.xml",
    )]);
    zip_entries(vec![
        ("[Content_Types].xml", content_types.as_bytes()),
        ("_rels/.rels", dot_rels.as_bytes()),
        ("word/document.xml", document.as_bytes()),
        ("word/styles.xml", styles.as_bytes()),
        ("word/_rels/document.xml.rels", doc_rels.as_bytes()),
    ])
}

/// Issue #328 — `<w:pgBorders w:offsetFrom="page">`: a 3 pt dark-blue
/// single box 24 pt inside the page edge on all four sides.
pub fn page_borders_docx() -> Vec<u8> {
    let body: String = PAGE_DECOR_TEXTS.iter().map(|t| text_para(t)).collect();
    package_with_sect_extra(
        &body,
        "<w:pgBorders w:offsetFrom=\"page\">\
         <w:top w:val=\"single\" w:sz=\"24\" w:space=\"24\" w:color=\"1F3864\"/>\
         <w:left w:val=\"single\" w:sz=\"24\" w:space=\"24\" w:color=\"1F3864\"/>\
         <w:bottom w:val=\"single\" w:sz=\"24\" w:space=\"24\" w:color=\"1F3864\"/>\
         <w:right w:val=\"single\" w:sz=\"24\" w:space=\"24\" w:color=\"1F3864\"/>\
         </w:pgBorders>",
    )
}

/// Issue #328 — `<w:lnNumType w:countBy="1" w:distance="360">` (restart
/// per page): the shared paragraphs, then a `w:suppressLineNumbers`
/// paragraph and a one-cell table (neither counted), then a last
/// numbered paragraph.
pub fn line_numbers_docx() -> Vec<u8> {
    let [a, b, c] = PAGE_DECOR_TEXTS;
    let body = format!(
        "{}{}<w:p><w:pPr><w:suppressLineNumbers/></w:pPr><w:r><w:t xml:space=\"preserve\">\
         {SUPPRESSED_TEXT}</w:t></w:r></w:p>\
         <w:tbl><w:tblPr><w:tblW w:w=\"0\" w:type=\"auto\"/></w:tblPr>\
         <w:tblGrid><w:gridCol w:w=\"4000\"/></w:tblGrid>\
         <w:tr><w:tc><w:tcPr><w:tcW w:w=\"4000\" w:type=\"dxa\"/></w:tcPr>\
         <w:p><w:r><w:t>Table cell</w:t></w:r></w:p></w:tc></w:tr></w:tbl>{}",
        text_para(a),
        text_para(b),
        text_para(c),
    );
    package_with_sect_extra(&body, "<w:lnNumType w:countBy=\"1\" w:distance=\"360\"/>")
}
