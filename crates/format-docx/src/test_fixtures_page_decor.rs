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

/// The paragraph `line_numbers.docx` suppresses through its PARAGRAPH
/// STYLE (issue #487: `NoNumbersChild`, `basedOn` `NoNumbers`, which
/// carries `w:suppressLineNumbers`) — no direct `<w:pPr>` switch.
pub const STYLE_SUPPRESSED_TEXT: &str = "This paragraph is suppressed by its style.";

/// The numbered paragraph that follows it: the count continues here.
pub const AFTER_STYLE_TEXT: &str = "A final numbered paragraph after the styled one.";

/// Issue #487 — the styles of `line_numbers.docx`: Word's `LineNumber`
/// character style (10 pt, grey — smaller than the 11 pt docDefaults,
/// `basedOn` `DefaultParagraphFont`), a paragraph style carrying
/// `w:suppressLineNumbers` and a child style `basedOn` it.
pub const LINE_NUMBER_STYLES: &str = "\
<w:style w:type=\"character\" w:default=\"1\" w:styleId=\"DefaultParagraphFont\">\
<w:name w:val=\"Default Paragraph Font\"/><w:uiPriority w:val=\"1\"/><w:semiHidden/>\
<w:unhideWhenUsed/></w:style>\
<w:style w:type=\"character\" w:styleId=\"LineNumber\"><w:name w:val=\"line number\"/>\
<w:basedOn w:val=\"DefaultParagraphFont\"/><w:uiPriority w:val=\"99\"/><w:semiHidden/>\
<w:unhideWhenUsed/><w:rPr><w:color w:val=\"7F7F7F\"/><w:sz w:val=\"20\"/>\
<w:szCs w:val=\"20\"/></w:rPr></w:style>\
<w:style w:type=\"paragraph\" w:styleId=\"NoNumbers\"><w:name w:val=\"No Numbers\"/>\
<w:pPr><w:suppressLineNumbers/></w:pPr></w:style>\
<w:style w:type=\"paragraph\" w:styleId=\"NoNumbersChild\"><w:name w:val=\"No Numbers Child\"/>\
<w:basedOn w:val=\"NoNumbers\"/></w:style>";

fn text_para(text: &str) -> String {
    format!("<w:p><w:r><w:t xml:space=\"preserve\">{text}</w:t></w:r></w:p>")
}

/// A package with `body` and `sect_extra` spliced into the trailing
/// `<w:sectPr>` right after `<w:pgMar>` (where CT_SectPr puts
/// `<w:pgBorders>` / `<w:lnNumType>`).
pub fn package_with_sect_extra(body: &str, sect_extra: &str) -> Vec<u8> {
    package_with_styles(body, sect_extra, "")
}

/// [`package_with_sect_extra`] with `extra_styles` (whole `<w:style>`
/// elements) appended after `<w:docDefaults>` in `styles.xml`.
pub fn package_with_styles(body: &str, sect_extra: &str, extra_styles: &str) -> Vec<u8> {
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
         </w:docDefaults>{extra_styles}</w:styles>"
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
/// paragraph and a one-cell table (neither counted), a numbered
/// paragraph, a paragraph whose STYLE suppresses the numbers (issue
/// #487, through a `basedOn` chain), and a last numbered paragraph.
pub fn line_numbers_docx() -> Vec<u8> {
    let [a, b, c] = PAGE_DECOR_TEXTS;
    let body = format!(
        "{}{}<w:p><w:pPr><w:suppressLineNumbers/></w:pPr><w:r><w:t xml:space=\"preserve\">\
         {SUPPRESSED_TEXT}</w:t></w:r></w:p>\
         <w:tbl><w:tblPr><w:tblW w:w=\"0\" w:type=\"auto\"/></w:tblPr>\
         <w:tblGrid><w:gridCol w:w=\"4000\"/></w:tblGrid>\
         <w:tr><w:tc><w:tcPr><w:tcW w:w=\"4000\" w:type=\"dxa\"/></w:tcPr>\
         <w:p><w:r><w:t>Table cell</w:t></w:r></w:p></w:tc></w:tr></w:tbl>{}\
         <w:p><w:pPr><w:pStyle w:val=\"NoNumbersChild\"/></w:pPr><w:r><w:t xml:space=\"preserve\">\
         {STYLE_SUPPRESSED_TEXT}</w:t></w:r></w:p>{}",
        text_para(a),
        text_para(b),
        text_para(c),
        text_para(AFTER_STYLE_TEXT),
    );
    package_with_styles(
        &body,
        "<w:lnNumType w:countBy=\"1\" w:distance=\"360\"/>",
        LINE_NUMBER_STYLES,
    )
}

/// The anchor paragraphs of `drop_cap.docx` (each follows its cap).
pub const DROP_CAP_TEXTS: [&str; 2] = [
    "rop caps open a chapter or an article: the first letter is set in a \
     frame three lines tall, and the first three lines of the paragraph \
     wrap around it while the following lines return to the full width of \
     the column, exactly as the frame properties of the cap paragraph ask. \
     The cap paragraph itself takes no room in the flow: this paragraph \
     starts at its top, and the cap's baseline meets the third line's.",
    "argin caps hang in the left margin instead: the letter sits outside \
     the text column, two lines tall, and every line of this paragraph \
     keeps the full column width.",
];

/// One Word-shaped drop-cap pair: the frame paragraph holding `letter`
/// (`<w:framePr w:dropCap w:lines>`, an exact line rule `lines` × 13.8
/// pt tall, the letter at `lines` × 13.8 pt) and its anchor paragraph.
fn drop_cap_pair(kind: &str, lines: u32, letter: &str, text: &str) -> String {
    let line_twips = lines * 276;
    let half_points = lines * 25;
    format!(
        "<w:p><w:pPr><w:keepNext/><w:framePr w:dropCap=\"{kind}\" w:lines=\"{lines}\" \
         w:wrap=\"around\" w:vAnchor=\"text\" w:hAnchor=\"text\" w:hSpace=\"72\"/>\
         <w:spacing w:after=\"0\" w:line=\"{line_twips}\" w:lineRule=\"exact\"/>\
         <w:textAlignment w:val=\"baseline\"/></w:pPr><w:r><w:rPr>\
         <w:sz w:val=\"{half_points}\"/><w:szCs w:val=\"{half_points}\"/></w:rPr>\
         <w:t>{letter}</w:t></w:r></w:p>{}",
        text_para(text)
    )
}

/// Issue #328 — a `drop` cap three lines tall ("D" + its paragraph),
/// then a `margin` cap two lines tall ("M" + its paragraph).
pub fn drop_cap_docx() -> Vec<u8> {
    let [a, b] = DROP_CAP_TEXTS;
    let body = format!(
        "{}{}",
        drop_cap_pair("drop", 3, "D", a),
        drop_cap_pair("margin", 2, "M", b)
    );
    package_with_sect_extra(&body, "")
}
