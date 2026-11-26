//! Issue #354 — `word/settings.xml` toggles the editor reads: a document
//! Word saved with Track Changes on (`<w:trackRevisions/>`) and one laid
//! out with mirrored margins (`<w:mirrorMargins/>`, Multiple pages:
//! "Mirror margins" in Word's Page Setup). Hand-written OOXML in Word's own
//! shape (A4, the settings children Word writes, in its order); the
//! sources of `tools/roundtrip`'s `track_revisions.docx` /
//! `mirror_margins.docx`, the engine-wasm pins and the
//! `settings-toggles.spec.ts` e2e.

use super::{SETTINGS_REL, W_NS, rels, zip_entries};

/// The paragraphs of [`track_revisions_docx`].
pub const TRACK_REVISIONS_TEXTS: [&str; 2] = [
    "Track Changes was on when this document was saved.",
    "Typing here is recorded as an insertion.",
];

/// The paragraphs of [`mirror_margins_docx`], one per page (the second
/// and third open with `<w:pageBreakBefore/>`).
pub const MIRROR_MARGINS_TEXTS: [&str; 3] = [
    "Page one is odd: its inside margin is on the left.",
    "Page two is even: its inside margin is on the right.",
    "Page three is odd again.",
];

/// [`mirror_margins_docx`]'s `<w:pgMar>`: a 2-inch inside margin (`w:left`)
/// and a half-inch outside one (`w:right`), so the swap is unmistakable.
pub const MIRROR_LEFT_TWIPS: i32 = 2880;
pub const MIRROR_RIGHT_TWIPS: i32 = 720;

/// The settings part Word 2016 writes for a fresh document, with `extra`
/// (whole elements) spliced in at their CT_Settings rank: `before_proof`
/// right after `<w:zoom>` (where `w:mirrorMargins` belongs), `after_proof`
/// right after `<w:proofState>` (where `w:trackRevisions` belongs).
fn word_settings(before_proof: &str, after_proof: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n\
         <w:settings xmlns:w=\"{W_NS}\" \
         xmlns:w14=\"http://schemas.microsoft.com/office/word/2010/wordml\" \
         xmlns:mc=\"http://schemas.openxmlformats.org/markup-compatibility/2006\" \
         mc:Ignorable=\"w14\">\
         <w:zoom w:percent=\"100\"/>{before_proof}\
         <w:proofState w:spelling=\"clean\" w:grammar=\"clean\"/>{after_proof}\
         <w:defaultTabStop w:val=\"720\"/>\
         <w:characterSpacingControl w:val=\"doNotCompress\"/>\
         <w:compat><w:compatSetting w:name=\"compatibilityMode\" \
         w:uri=\"http://schemas.microsoft.com/office/word\" w:val=\"15\"/></w:compat>\
         <w:rsids><w:rsidRoot w:val=\"00354A0B\"/><w:rsid w:val=\"00354A0B\"/></w:rsids>\
         <w:themeFontLang w:val=\"en-US\"/>\
         <w:decimalSymbol w:val=\".\"/><w:listSeparator w:val=\",\"/>\
         <w14:docId w14:val=\"3540354A\"/></w:settings>"
    )
}

/// A package around `body` (block content) and `sect_pr` (the trailing
/// `<w:sectPr>`), carrying `settings` as `word/settings.xml` — related and
/// declared the way Word writes it.
fn package_with_settings(body: &str, sect_pr: &str, settings: &str) -> Vec<u8> {
    let document = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
         <w:document xmlns:w=\"{W_NS}\"><w:body>{body}{sect_pr}</w:body></w:document>"
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
<Override PartName=\"/word/settings.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.settings+xml\"/>\
</Types>";
    let dot_rels = rels(&[(
        "rId1",
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument",
        "word/document.xml",
    )]);
    let doc_rels = rels(&[
        (
            "rId1",
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles",
            "styles.xml",
        ),
        ("rId2", SETTINGS_REL, "settings.xml"),
    ]);
    zip_entries(vec![
        ("[Content_Types].xml", content_types.as_bytes()),
        ("_rels/.rels", dot_rels.as_bytes()),
        ("word/document.xml", document.as_bytes()),
        ("word/styles.xml", styles.as_bytes()),
        ("word/_rels/document.xml.rels", doc_rels.as_bytes()),
        ("word/settings.xml", settings.as_bytes()),
    ])
}

fn text_para(ppr: &str, text: &str) -> String {
    format!("<w:p>{ppr}<w:r><w:t xml:space=\"preserve\">{text}</w:t></w:r></w:p>")
}

/// The stock A4, 1-inch-margin `<w:sectPr>` Word writes.
const A4_SECT_PR: &str = "<w:sectPr><w:pgSz w:w=\"11906\" w:h=\"16838\"/>\
<w:pgMar w:top=\"1440\" w:right=\"1440\" w:bottom=\"1440\" w:left=\"1440\" \
w:header=\"720\" w:footer=\"720\" w:gutter=\"0\"/></w:sectPr>";

/// The settings part of [`track_revisions_docx`] (the e2e and the
/// roundtrip step compare saved parts against it).
pub fn track_revisions_settings_xml() -> String {
    word_settings("", "<w:trackRevisions/>")
}

/// Issue #354 — a document saved by Word with Track Changes on: opens
/// with review mode on; typing records `<w:ins>`.
pub fn track_revisions_docx() -> Vec<u8> {
    let body: String = TRACK_REVISIONS_TEXTS
        .iter()
        .map(|t| text_para("", t))
        .collect();
    package_with_settings(&body, A4_SECT_PR, &track_revisions_settings_xml())
}

/// Issue #354 — three pages (A4) with a 2-inch `w:left` and a half-inch
/// `w:right` margin under `<w:mirrorMargins/>`: page 2's text starts half
/// an inch from the left edge, pages 1 and 3 two inches.
pub fn mirror_margins_docx() -> Vec<u8> {
    let [a, b, c] = MIRROR_MARGINS_TEXTS;
    let next_page = "<w:pPr><w:pageBreakBefore/></w:pPr>";
    let body = format!(
        "{}{}{}",
        text_para("", a),
        text_para(next_page, b),
        text_para(next_page, c)
    );
    let sect_pr = format!(
        "<w:sectPr><w:pgSz w:w=\"11906\" w:h=\"16838\"/>\
         <w:pgMar w:top=\"1440\" w:right=\"{MIRROR_RIGHT_TWIPS}\" w:bottom=\"1440\" \
         w:left=\"{MIRROR_LEFT_TWIPS}\" w:header=\"720\" w:footer=\"720\" w:gutter=\"0\"/>\
         </w:sectPr>"
    );
    package_with_settings(&body, &sect_pr, &word_settings("<w:mirrorMargins/>", ""))
}
