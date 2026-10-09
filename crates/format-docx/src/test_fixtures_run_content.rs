//! Issues #335 / #357 / #326 — fixtures for run-content elements and
//! hyphenation. Hand-written OOXML in Word's own shape (our own XML, no
//! part copied from a Word package).

use super::{W_NS, rels, zip_entries};

/// Issue #335 — paragraph 0 of [`soft_hyphen_docx`]: justified English
/// whose long words carry author-placed optional hyphens (U+00AD here,
/// `<w:softHyphen/>` in the package), long enough to wrap several times
/// so lines break at soft hyphens.
pub const SOFT_HYPHEN_TEXT: &str = "Typesetters mark optional breaks in long words: \
extra\u{AD}ordinary, in\u{AD}com\u{AD}pre\u{AD}hen\u{AD}si\u{AD}bil\u{AD}i\u{AD}ties, \
coun\u{AD}ter\u{AD}rev\u{AD}o\u{AD}lu\u{AD}tion\u{AD}ary and elec\u{AD}tro\u{AD}en\u{AD}\
ceph\u{AD}a\u{AD}lo\u{AD}graph\u{AD}ic all wrap at a marked point, while the drawn hyphen \
never enters the text, the caret or the copied char\u{AD}ac\u{AD}ters of the \
para\u{AD}graph that holds them.";

/// Issue #335 — paragraph 1: non-breaking hyphens (U+2011,
/// `<w:noBreakHyphen/>`) that must never split their word.
pub const NB_HYPHEN_TEXT: &str = "An e\u{2011}mail address, the X\u{2011}ray of a \
well\u{2011}known case and the twenty\u{2011}first\u{2011}century reader never split at a \
non\u{2011}breaking hyphen, however the line falls.";

/// One `<w:r>` spelling `text` the way Word does: U+00AD as
/// `<w:softHyphen/>` inside the run, U+2011 as `<w:noBreakHyphen/>` in a
/// run of its own.
fn word_runs(text: &str) -> String {
    let mut out = String::from("<w:r><w:t xml:space=\"preserve\">");
    for ch in text.chars() {
        match ch {
            '\u{AD}' => out.push_str("</w:t><w:softHyphen/><w:t xml:space=\"preserve\">"),
            '\u{2011}' => out.push_str(
                "</w:t></w:r><w:r><w:noBreakHyphen/></w:r><w:r><w:t xml:space=\"preserve\">",
            ),
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c => out.push(c),
        }
    }
    out.push_str("</w:t></w:r>");
    out
}

/// A package with `body` (block content) and an A4 1-inch-margin
/// `<w:sectPr>`, plus a styles part whose docDefaults set `w:sz="22"`
/// (11 pt) and `settings_xml` when given.
pub fn package_with_body(body: &str, settings_xml: Option<&str>) -> Vec<u8> {
    let document = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
         <w:document xmlns:w=\"{W_NS}\"><w:body>{body}\
         <w:sectPr><w:pgSz w:w=\"11906\" w:h=\"16838\"/>\
         <w:pgMar w:top=\"1440\" w:right=\"1440\" w:bottom=\"1440\" w:left=\"1440\" \
         w:header=\"720\" w:footer=\"720\" w:gutter=\"0\"/></w:sectPr>\
         </w:body></w:document>"
    );
    let styles = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
         <w:styles xmlns:w=\"{W_NS}\"><w:docDefaults><w:rPrDefault><w:rPr>\
         <w:sz w:val=\"22\"/><w:szCs w:val=\"22\"/></w:rPr></w:rPrDefault>\
         </w:docDefaults></w:styles>"
    );
    let settings_override = if settings_xml.is_some() {
        "<Override PartName=\"/word/settings.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.settings+xml\"/>"
    } else {
        ""
    };
    let content_types = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
<Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
<Default Extension=\"xml\" ContentType=\"application/xml\"/>\
<Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/>\
<Override PartName=\"/word/styles.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml\"/>\
{settings_override}</Types>"
    );
    let dot_rels = rels(&[(
        "rId1",
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument",
        "word/document.xml",
    )]);
    let mut doc_rels_rows = vec![(
        "rId1",
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles",
        "styles.xml",
    )];
    if settings_xml.is_some() {
        doc_rels_rows.push(("rId2", super::SETTINGS_REL, "settings.xml"));
    }
    let doc_rels = rels(&doc_rels_rows);
    let mut entries: Vec<(&str, &[u8])> = vec![
        ("[Content_Types].xml", content_types.as_bytes()),
        ("_rels/.rels", dot_rels.as_bytes()),
        ("word/document.xml", document.as_bytes()),
        ("word/styles.xml", styles.as_bytes()),
        ("word/_rels/document.xml.rels", doc_rels.as_bytes()),
    ];
    if let Some(s) = settings_xml {
        entries.push(("word/settings.xml", s.as_bytes()));
    }
    zip_entries(entries)
}

/// Issue #335 — soft and non-breaking hyphens: paragraph 0
/// ([`SOFT_HYPHEN_TEXT`], justified) wraps at author-placed soft hyphens,
/// paragraph 1 ([`NB_HYPHEN_TEXT`]) keeps every non-breaking hyphen's word
/// whole. A4, 1-inch margins, 11 pt. The source of `tools/roundtrip`'s
/// `soft_hyphen.docx`, the engine-wasm layout pin and the visual-diff
/// `soft-hyphen` golden.
pub fn soft_hyphen_docx() -> Vec<u8> {
    let body = format!(
        "<w:p><w:pPr><w:jc w:val=\"both\"/></w:pPr>{}</w:p><w:p>{}</w:p>",
        word_runs(SOFT_HYPHEN_TEXT),
        word_runs(NB_HYPHEN_TEXT),
    );
    package_with_body(&body, None)
}

/// One `<w:r>` holding `<w:sym w:font w:char/>`, in Word's shape (the
/// run's own fonts name the symbol face).
fn sym_run(font: &str, code: &str) -> String {
    format!(
        "<w:r><w:rPr><w:rFonts w:ascii=\"{font}\" w:hAnsi=\"{font}\"/></w:rPr>\
         <w:sym w:font=\"{font}\" w:char=\"{code}\"/></w:r>"
    )
}

fn text_run(text: &str) -> String {
    format!("<w:r><w:t xml:space=\"preserve\">{text}</w:t></w:r>")
}

/// Issue #357 — the text of each [`run_content_docx`] paragraph as the
/// reader models it: U+FFFC for each `<w:sym>` / `<w:ptab>`, U+000D for
/// `<w:cr/>`, the UAX #9 controls for `<w:bdo>` / `<w:dir>`.
pub const RUN_CONTENT_TEXTS: [&str; 4] = [
    "Symbol font: \u{FFFC} \u{FFFC} \u{FFFC} \u{FFFC} \u{FFFC}; Wingdings: \u{FFFC} \u{FFFC} \
     \u{FFFC} \u{FFFC} (a check).",
    "First line of the paragraph\rand a second line after a carriage return.",
    "Left\u{FFFC}Centre\u{FFFC}Right",
    "Override: \u{202E}ABC def\u{202C}; embedding: \u{202B}abc 123\u{202C} done.",
];

/// Issue #357 — one paragraph per run-content element: `<w:sym>` from
/// Symbol (α β π ∑ ∞) and Wingdings (● ■ □ and a ✓ the shipped faces do
/// not cover), a `<w:cr/>` line break, two `<w:ptab>`s (centre, right
/// with a dot leader) laying out a header-style line, and a `<w:bdo
/// w:val="rtl">` override next to a `<w:dir w:val="rtl">` embedding. A4,
/// 1-inch margins, 11 pt. The source of `tools/roundtrip`'s
/// `run_content.docx`, the engine-wasm layout pin and the visual-diff
/// `run-content` golden.
pub fn run_content_docx() -> Vec<u8> {
    let p0 = [
        text_run("Symbol font: "),
        sym_run("Symbol", "F061"),
        text_run(" "),
        sym_run("Symbol", "F062"),
        text_run(" "),
        sym_run("Symbol", "F070"),
        text_run(" "),
        sym_run("Symbol", "F0E5"),
        text_run(" "),
        sym_run("Symbol", "F0A5"),
        text_run("; Wingdings: "),
        sym_run("Wingdings", "F06C"),
        text_run(" "),
        sym_run("Wingdings", "F06E"),
        text_run(" "),
        sym_run("Wingdings", "F06F"),
        text_run(" "),
        sym_run("Wingdings", "F0FC"),
        text_run(" (a check)."),
    ]
    .concat();
    let p1 = "<w:r><w:t>First line of the paragraph</w:t><w:cr/>\
              <w:t>and a second line after a carriage return.</w:t></w:r>";
    let p2 = [
        text_run("Left"),
        "<w:r><w:ptab w:relativeTo=\"margin\" w:alignment=\"center\" w:leader=\"none\"/></w:r>"
            .to_string(),
        text_run("Centre"),
        "<w:r><w:ptab w:relativeTo=\"margin\" w:alignment=\"right\" w:leader=\"dot\"/></w:r>"
            .to_string(),
        text_run("Right"),
    ]
    .concat();
    let p3 = format!(
        "{}<w:bdo w:val=\"rtl\">{}</w:bdo>{}<w:dir w:val=\"rtl\">{}</w:dir>{}",
        text_run("Override: "),
        text_run("ABC def"),
        text_run("; embedding: "),
        text_run("abc 123"),
        text_run(" done."),
    );
    let body = format!("<w:p>{p0}</w:p><w:p>{p1}</w:p><w:p>{p2}</w:p><w:p>{p3}</w:p>");
    package_with_body(&body, None)
}
