//! Issue #556 — a document whose DEFAULT paragraph style (`Normal`,
//! `w:default="1"`) carries the formatting: `<w:bidi/>`, spacing (over a
//! docDefaults spacing it overrides) and a run font + size. Word applies
//! `Normal` to every paragraph that names no style (and to one naming a
//! style the part does not define), never underneath a style that is not
//! based on it. Hand-written OOXML in Word's own shape (A4, 1-inch
//! margins); the source of `tools/roundtrip`'s `default_style_rtl.docx`,
//! the format-docx / engine-wasm direction + layout pins and the
//! `default-paragraph-style.spec.ts` e2e.

use super::{W_NS, rels, zip_entries};

/// The six paragraphs, in order (the manifest asserts them):
///
/// 0. unstyled Arabic text — RTL either way, `Normal`'s spacing and font;
/// 1. unstyled EMPTY paragraph (`<w:p/>`) — RTL only through `Normal`
///    (before #556 it fell to the LTR document base);
/// 2. unstyled Latin-first text — RTL through `Normal`, not first-strong;
/// 3. `<w:pStyle w:val="Missing"/>` (no such style) — formatted with
///    `Normal`, RTL;
/// 4. `Plain` (a style NOT based on `Normal`) — no `Normal` underneath,
///    LTR by its Latin text;
/// 5. unstyled with a direct `<w:bidi w:val="0"/>` — the direct LTR beats
///    `Normal`'s RTL.
pub const DEFAULT_STYLE_TEXTS: [&str; 6] = [
    "مرحبا بالعالم",
    "",
    "Latin first, then العربية",
    "Dangling style id",
    "Own style, not based on Normal",
    "Direct LTR override",
];

/// `word/styles.xml`: docDefaults (Liberation Sans 11 pt, 8 pt after,
/// 1.08 lines), `Normal` (default: RTL, 6 pt before / 12 pt after, 1.5
/// lines, Liberation Serif 14 pt / complex-script 16 pt), `Plain` (not
/// based on anything: left-aligned), and Word's `DefaultParagraphFont`
/// default character style.
pub const DEFAULT_STYLE_STYLES_XML: &str = concat!(
    "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n",
    "<w:styles xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">",
    "<w:docDefaults><w:rPrDefault><w:rPr>",
    "<w:rFonts w:ascii=\"Liberation Sans\" w:hAnsi=\"Liberation Sans\" w:cs=\"Amiri\"/>",
    "<w:sz w:val=\"22\"/><w:szCs w:val=\"22\"/></w:rPr></w:rPrDefault>",
    "<w:pPrDefault><w:pPr><w:spacing w:after=\"160\" w:line=\"259\" w:lineRule=\"auto\"/></w:pPr></w:pPrDefault>",
    "</w:docDefaults>",
    "<w:style w:type=\"paragraph\" w:default=\"1\" w:styleId=\"Normal\"><w:name w:val=\"Normal\"/><w:qFormat/>",
    "<w:pPr><w:bidi/><w:spacing w:before=\"120\" w:after=\"240\" w:line=\"360\" w:lineRule=\"auto\"/></w:pPr>",
    "<w:rPr><w:rFonts w:ascii=\"Liberation Serif\" w:hAnsi=\"Liberation Serif\" w:cs=\"Noto Naskh Arabic\"/>",
    "<w:sz w:val=\"28\"/><w:szCs w:val=\"32\"/></w:rPr></w:style>",
    "<w:style w:type=\"character\" w:default=\"1\" w:styleId=\"DefaultParagraphFont\">",
    "<w:name w:val=\"Default Paragraph Font\"/><w:uiPriority w:val=\"1\"/><w:semiHidden/><w:unhideWhenUsed/></w:style>",
    "<w:style w:type=\"paragraph\" w:customStyle=\"1\" w:styleId=\"Plain\"><w:name w:val=\"Plain\"/>",
    "<w:pPr><w:jc w:val=\"left\"/></w:pPr></w:style>",
    "</w:styles>",
);

/// The `<w:body>` blocks of [`default_style_rtl_docx`], in the writer's
/// own regeneration shape (an edited paragraph regenerates to exactly
/// source + insert).
pub const DEFAULT_STYLE_BODY: &str = concat!(
    "<w:p><w:r><w:t xml:space=\"preserve\">مرحبا بالعالم</w:t></w:r></w:p>",
    "<w:p/>",
    "<w:p><w:r><w:t xml:space=\"preserve\">Latin first, then العربية</w:t></w:r></w:p>",
    "<w:p><w:pPr><w:pStyle w:val=\"Missing\"/></w:pPr><w:r><w:t xml:space=\"preserve\">Dangling style id</w:t></w:r></w:p>",
    "<w:p><w:pPr><w:pStyle w:val=\"Plain\"/></w:pPr><w:r><w:t xml:space=\"preserve\">Own style, not based on Normal</w:t></w:r></w:p>",
    "<w:p><w:pPr><w:bidi w:val=\"0\"/></w:pPr><w:r><w:t xml:space=\"preserve\">Direct LTR override</w:t></w:r></w:p>",
);

/// Issue #556 — the package: [`DEFAULT_STYLE_BODY`] under an A4 section,
/// [`DEFAULT_STYLE_STYLES_XML`] as `word/styles.xml`.
pub fn default_style_rtl_docx() -> Vec<u8> {
    let document = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n\
         <w:document xmlns:w=\"{W_NS}\"><w:body>{DEFAULT_STYLE_BODY}\
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
        ("word/styles.xml", DEFAULT_STYLE_STYLES_XML.as_bytes()),
    ])
}
