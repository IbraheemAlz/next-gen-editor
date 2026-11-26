//! Issue #558 — a document whose DEFAULT CHARACTER style
//! (`DefaultParagraphFont`, `w:type="character" w:default="1"`) carries
//! formatting: Liberation Serif, bold, 16 pt. ECMA-376 §17.7.4.17 gives it
//! to every run that names no character style, between the paragraph
//! style's `<w:rPr>` and the run's own formatting; a run naming another
//! character style takes that style's chain instead (with
//! `DefaultParagraphFont` only when that chain is based on it). Word's UI
//! never formats Default Paragraph Font, but converters and hand-written
//! packages do. Hand-written OOXML in Word's own shape (A4, 1-inch
//! margins, Word's `<w:docDefaults>` spacing under an EMPTY `Normal` —
//! also the issue #560 case of inherited properties `Normal` does not
//! override); the source of `tools/roundtrip`'s `default_char_style.docx`
//! and the format-docx / engine-wasm / e2e pins.

use super::{W_NS, rels, zip_entries};

/// The six paragraphs, in order (the manifest asserts them):
///
/// 0. a run with no `<w:rPr>` — the default character style: serif, bold,
///    16 pt;
/// 1. `<w:rStyle w:val="Quiet"/>` (a character style NOT based on it:
///    italic) — not bold, the docDefaults' 11 pt sans;
/// 2. `<w:rStyle w:val="Loud"/>` (based on it, red) — serif, bold, 16 pt,
///    red;
/// 3. a direct `<w:b w:val="0"/>` — the direct value wins, serif 16 pt;
/// 4. an EMPTY paragraph (`<w:p/>`) — its mark is a run too: 16 pt line;
/// 5. in paragraph style `Aside` (italic, indented) — the default
///    character style above the paragraph style's run formatting: bold
///    italic serif 16 pt.
pub const DEFAULT_CHAR_TEXTS: [&str; 6] = [
    "Plain run in the default character style",
    "Quiet names its own style",
    "Loud is based on it",
    "Direct formatting wins",
    "",
    "An aside over it",
];

/// `word/styles.xml`: Word's docDefaults (Liberation Sans 11 pt, 8 pt
/// after, 1.08 lines), an empty `Normal`, the formatted
/// `DefaultParagraphFont`, `Quiet` (character, based on nothing:
/// italic), `Loud` (character, based on `DefaultParagraphFont`: red) and
/// `Aside` (paragraph, based on `Normal`: indented, italic runs).
pub const DEFAULT_CHAR_STYLES_XML: &str = concat!(
    "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n",
    "<w:styles xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">",
    "<w:docDefaults><w:rPrDefault><w:rPr>",
    "<w:rFonts w:ascii=\"Liberation Sans\" w:hAnsi=\"Liberation Sans\" w:cs=\"Amiri\"/>",
    "<w:sz w:val=\"22\"/><w:szCs w:val=\"22\"/></w:rPr></w:rPrDefault>",
    "<w:pPrDefault><w:pPr><w:spacing w:after=\"160\" w:line=\"259\" w:lineRule=\"auto\"/></w:pPr></w:pPrDefault>",
    "</w:docDefaults>",
    "<w:style w:type=\"paragraph\" w:default=\"1\" w:styleId=\"Normal\"><w:name w:val=\"Normal\"/><w:qFormat/></w:style>",
    "<w:style w:type=\"character\" w:default=\"1\" w:styleId=\"DefaultParagraphFont\">",
    "<w:name w:val=\"Default Paragraph Font\"/><w:uiPriority w:val=\"1\"/><w:semiHidden/><w:unhideWhenUsed/>",
    "<w:rPr><w:rFonts w:ascii=\"Liberation Serif\" w:hAnsi=\"Liberation Serif\"/><w:b/><w:sz w:val=\"32\"/></w:rPr></w:style>",
    "<w:style w:type=\"character\" w:customStyle=\"1\" w:styleId=\"Quiet\"><w:name w:val=\"Quiet\"/>",
    "<w:rPr><w:i/></w:rPr></w:style>",
    "<w:style w:type=\"character\" w:customStyle=\"1\" w:styleId=\"Loud\"><w:name w:val=\"Loud\"/>",
    "<w:basedOn w:val=\"DefaultParagraphFont\"/><w:rPr><w:color w:val=\"C00000\"/></w:rPr></w:style>",
    "<w:style w:type=\"paragraph\" w:customStyle=\"1\" w:styleId=\"Aside\"><w:name w:val=\"Aside\"/>",
    "<w:basedOn w:val=\"Normal\"/><w:pPr><w:ind w:left=\"720\"/></w:pPr><w:rPr><w:i/></w:rPr></w:style>",
    "</w:styles>",
);

/// The `<w:body>` blocks of [`default_char_style_docx`], in the writer's
/// own regeneration shape (an edited paragraph regenerates to exactly
/// source + insert).
pub const DEFAULT_CHAR_BODY: &str = concat!(
    "<w:p><w:r><w:t xml:space=\"preserve\">Plain run in the default character style</w:t></w:r></w:p>",
    "<w:p><w:r><w:rPr><w:rStyle w:val=\"Quiet\"/></w:rPr><w:t xml:space=\"preserve\">Quiet names its own style</w:t></w:r></w:p>",
    "<w:p><w:r><w:rPr><w:rStyle w:val=\"Loud\"/></w:rPr><w:t xml:space=\"preserve\">Loud is based on it</w:t></w:r></w:p>",
    "<w:p><w:r><w:rPr><w:b w:val=\"0\"/></w:rPr><w:t xml:space=\"preserve\">Direct formatting wins</w:t></w:r></w:p>",
    "<w:p/>",
    "<w:p><w:pPr><w:pStyle w:val=\"Aside\"/></w:pPr><w:r><w:t xml:space=\"preserve\">An aside over it</w:t></w:r></w:p>",
);

/// Issue #558 — the package: [`DEFAULT_CHAR_BODY`] under an A4 section,
/// [`DEFAULT_CHAR_STYLES_XML`] as `word/styles.xml`.
pub fn default_char_style_docx() -> Vec<u8> {
    let document = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n\
         <w:document xmlns:w=\"{W_NS}\"><w:body>{DEFAULT_CHAR_BODY}\
         <w:sectPr><w:pgSz w:w=\"11906\" w:h=\"16838\"/>\
         <w:pgMar w:top=\"1440\" w:right=\"1440\" w:bottom=\"1440\" w:left=\"1440\" \
         w:header=\"720\" w:footer=\"720\" w:gutter=\"0\"/></w:sectPr></w:body></w:document>"
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
        ("word/_rels/document.xml.rels", doc_rels.as_bytes()),
        ("word/styles.xml", DEFAULT_CHAR_STYLES_XML.as_bytes()),
    ])
}
