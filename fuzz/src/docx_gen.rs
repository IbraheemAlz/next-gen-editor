//! Structure-aware `.docx` generator for `docx_reader` / `docx_roundtrip`
//! (D5.5, issue #90; widened by issue #358, bounded by issue #422).
//!
//! Builds schema-shaped WML — random `<w:pPr>` / `<w:rPr>` / `<w:tbl>` /
//! `<w:sectPr>` trees with a mix of valid and deliberately-invalid
//! attribute values — wrapped in a minimal real OPC package (`zip` crate:
//! `[Content_Types].xml`, `_rels/.rels`, `word/document.xml`, and
//! optionally `word/_rels/document.xml.rels`), rather than handing the
//! fuzzer's raw input bytes straight to `read_docx`. The OPC skeleton
//! mirrors `format_docx::writer`'s own `zip_minimal_docx` test helper
//! (`crates/format-docx/src/writer.rs`) — reimplemented here rather than
//! imported since that helper is `#[cfg(test)]`-private to its crate.
//!
//! ## Issue #358 — the hostile shapes
//!
//! Re-created from the spec's intents (no foreign XML copied):
//! - complex fields: `fldChar` begin / separate / end sequences, balanced
//!   and not (a missing marker, a surplus one, a bogus `fldCharType`),
//!   nested to [`MAX_FIELD_NESTING`] — as a chain, inside the instruction
//!   or the result — plus `fldSimple` with hostile instructions;
//! - content controls: `w:sdt` at block and run level, nested to one of
//!   [`SDT_DEPTHS`] (5000 is far past the reader's 256-level XML cap — the
//!   typed refusal is the expected outcome);
//! - `mc:AlternateContent` at block (paragraph) and run level with random
//!   `Requires` (known, unknown, empty, undeclared prefixes), several or no
//!   `Choice`s, with or without a `Fallback`;
//! - `w:drawing` inline / anchor whose `r:embed` names a missing
//!   relationship, a relationship whose part is absent, or the wrong
//!   relationship type; an empty `wp:inline`; hostile extents / offsets;
//! - numbers from `NaN`, `inf`, `-1e30`, `1in`, `50%`, `0x20`, …;
//! - tables nested to [`MAX_TABLE_NESTING`];
//! - a splice mode ([`build_docx`]): a valid generated `document.xml`
//!   gets spec snippets spliced in (`&#0;`, U+FFFC, an empty `wp:inline`,
//!   stray `fldChar`s, …), a short slice of the raw fuzz input spliced in
//!   (where the nightly's `fuzz/dictionaries/docx.dict` tokens land),
//!   bytes flipped, or a truncation, before zipping.
//!
//! ## Issue #422 — bounded by construction
//!
//! Nested shapes are chains (one nested child per level), never trees, so
//! their size is linear in the depth; tables nested past depth 1 are 1×1;
//! and [`gen_body`] stops adding blocks once `document.xml` passes
//! [`MAX_DOCUMENT_XML_BYTES`] — the largest single block (a 5000-deep
//! `sdt`) is ~250 KB, so a package never exceeds a few MB.

use crate::util::pick;
use arbitrary::Unstructured;
use std::io::{Cursor, Write};
use zip::{ZipWriter, write::SimpleFileOptions};

/// Issue #422 — [`gen_body`] stops adding blocks past this many bytes of
/// `document.xml`.
pub const MAX_DOCUMENT_XML_BYTES: usize = 2 * 1024 * 1024;
/// Issue #358 — the deepest table nesting generated (Word's own limit is
/// far lower; the reader's layout caps at 32 and flattens the rest).
pub const MAX_TABLE_NESTING: u32 = 60;
/// Issue #358 — the deepest field nesting generated.
pub const MAX_FIELD_NESTING: u32 = 40;
/// Issue #358 — content-control nesting depths: shallow, at / around the
/// reader's 256-level XML cap, and far past it.
pub const SDT_DEPTHS: &[u32] = &[1, 1, 2, 3, 8, 40, 120, 255, 256, 257, 5000];

const CONTENT_TYPES_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Default Extension="png" ContentType="image/png"/>
<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#;

const DOT_RELS_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#;

/// `rId2` — the external hyperlink every `<w:hyperlink>` names.
const HYPERLINK_REL: &str = r#"<Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="https://example.invalid" TargetMode="External"/>"#;
/// `rId3` — an image relationship whose target part is never written
/// (issue #358 — "drawings with missing rels").
const MISSING_IMAGE_REL: &str = r#"<Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/missing.png"/>"#;

/// Which relationships the generated body referenced, so the package
/// carries (only) the matching `word/_rels/document.xml.rels` entries.
#[derive(Default)]
struct Rels {
    hyperlink: bool,
    missing_image: bool,
}

impl Rels {
    fn xml(&self) -> Option<String> {
        if !self.hyperlink && !self.missing_image {
            return None;
        }
        let mut s = String::from(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#,
        );
        if self.hyperlink {
            s.push_str(HYPERLINK_REL);
        }
        if self.missing_image {
            s.push_str(MISSING_IMAGE_REL);
        }
        s.push_str("</Relationships>");
        Some(s)
    }
}

/// `&` / `<` / `>` only — matches the writer's own escape set
/// (`.claude/rules/docx.md`).
fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Attribute values also need `"` escaped.
fn escape_attr(s: &str) -> String {
    escape_xml(s).replace('"', "&quot;")
}

/// Issue #349 / #358 — what `f32::from_str` used to accept into geometry,
/// the universal-measure units ECMA-376 allows on twips measures, and
/// their malformed cousins.
const HOSTILE_NUMBERS: &[&str] = &[
    "not_a_number",
    "-99999999999999",
    "",
    "3.14.15",
    "0x1F",
    "0x20",
    "999999999999999999999999",
    "NaN",
    "inf",
    "-inf",
    "infinity",
    "1e30",
    "-1e30",
    "1e400",
    "-720",
    "1in",
    "2.5in",
    "-0.5in",
    "12pt",
    "2.54cm",
    "25.4mm",
    "6pc",
    "3pi",
    "12px",
    "720.5",
    "+720",
    "50%",
];

/// A numeric OOXML attribute value — usually a plausible integer,
/// occasionally deliberately-invalid garbage (issue #90: "valid and
/// invalid attributes"). The reader must reject or ignore the garbage
/// case, never panic on it.
fn attr_num(u: &mut Unstructured, max: i64) -> String {
    if u.ratio(1, 6).unwrap_or(false) {
        pick(u, HOSTILE_NUMBERS).to_string()
    } else {
        u.int_in_range(0..=max).unwrap_or(0).to_string()
    }
}

fn maybe_bool_element(u: &mut Unstructured, tag: &str) -> String {
    if u.ratio(1, 2).unwrap_or(false) {
        format!("<{tag}/>")
    } else {
        String::new()
    }
}

fn gen_text_run_content(u: &mut Unstructured) -> String {
    const POOL: &[&str] = &[
        "hello",
        "world",
        "السلام عليكم",
        "<injected>",
        "a & b",
        "",
        "\u{0301}\u{0301}",
        "line\nbreak",
        "tab\ttab",
    ];
    pick(u, POOL).to_string()
}

fn gen_rpr(u: &mut Unstructured) -> String {
    if u.ratio(1, 4).unwrap_or(false) {
        return String::new();
    }
    let mut s = String::from("<w:rPr>");
    s.push_str(&maybe_bool_element(u, "w:b"));
    s.push_str(&maybe_bool_element(u, "w:i"));
    if u.ratio(1, 2).unwrap_or(false) {
        let val = pick(
            u,
            &[
                "single",
                "double",
                "wave",
                "dotted",
                "dash",
                "garbage-style",
                "",
            ],
        );
        s.push_str(&format!(r#"<w:u w:val="{val}"/>"#));
    }
    if u.ratio(1, 2).unwrap_or(false) {
        s.push_str(&format!(r#"<w:sz w:val="{}"/>"#, attr_num(u, 200)));
    }
    if u.ratio(1, 2).unwrap_or(false) {
        let color = pick(u, &["FF0000", "not-a-color", "000000", ""]);
        s.push_str(&format!(r#"<w:color w:val="{color}"/>"#));
    }
    s.push_str("</w:rPr>");
    s
}

fn gen_run(u: &mut Unstructured) -> String {
    let rpr = gen_rpr(u);
    let text = gen_text_run_content(u);
    // Deliberately skip escaping some of the time to feed the parser
    // malformed XML — `read_docx` must return `Err`, never panic.
    let body = if u.ratio(1, 10).unwrap_or(false) {
        text
    } else {
        escape_xml(&text)
    };
    format!(r#"<w:r>{rpr}<w:t xml:space="preserve">{body}</w:t></w:r>"#)
}

/* ------------------------------------------------------------------ */
/* Issue #358 — fields                                                  */
/* ------------------------------------------------------------------ */

const FIELD_INSTRUCTIONS: &[&str] = &[
    " PAGE ",
    " NUMPAGES ",
    " PAGE \\* MERGEFORMAT ",
    r#" DATE \@ "yyyy-MM-dd" "#,
    r#" TOC \o "1-3" \h \z \u "#,
    r#" HYPERLINK "https://example.invalid" "#,
    " MERGEFIELD Name ",
    r#" IF 1 = 1 "yes" "no" "#,
    " SEQ Figure \\* ARABIC ",
    " REF _Ref1 \\h ",
    " FILENAME ",
    " = 2 + 2 ",
    "",
    " ",
    "BOGUS",
    "\"",
];

fn fld_char(kind: &str) -> String {
    format!(r#"<w:r><w:fldChar w:fldCharType="{kind}"/></w:r>"#)
}

fn instr_run(u: &mut Unstructured) -> String {
    let code = escape_xml(pick::<&str>(u, FIELD_INSTRUCTIONS));
    format!(r#"<w:r><w:instrText xml:space="preserve">{code}</w:instrText></w:r>"#)
}

/// A complex field, nested as a CHAIN (at most one nested field per level,
/// inside the instruction or inside the result — linear size, issue #422)
/// down to `depth_left` more levels. Unbalanced some of the time: a marker
/// dropped, a bogus `fldCharType`, a surplus `separate` / `end`.
fn gen_complex_field(u: &mut Unstructured, depth_left: u32) -> String {
    let balanced = u.ratio(3, 4).unwrap_or(true);
    let keep = |u: &mut Unstructured| balanced || u.ratio(2, 3).unwrap_or(true);
    let marker = |u: &mut Unstructured, kind: &'static str| {
        if balanced || u.ratio(5, 6).unwrap_or(true) {
            kind
        } else {
            *pick(u, &["bogus", "", "BEGIN", "end", "begin"])
        }
    };
    let nest = depth_left > 0 && u.ratio(1, 3).unwrap_or(false);
    let nest_in_instruction = u.ratio(1, 2).unwrap_or(true);
    let mut s = String::new();
    if keep(u) {
        let m = marker(u, "begin");
        s.push_str(&fld_char(m));
    }
    s.push_str(&instr_run(u));
    if nest && nest_in_instruction {
        s.push_str(&gen_complex_field(u, depth_left - 1));
        s.push_str(&instr_run(u));
    }
    if keep(u) {
        let m = marker(u, "separate");
        s.push_str(&fld_char(m));
    }
    s.push_str(&gen_run(u));
    if nest && !nest_in_instruction {
        s.push_str(&gen_complex_field(u, depth_left - 1));
    }
    if keep(u) {
        let m = marker(u, "end");
        s.push_str(&fld_char(m));
    }
    if !balanced && u.ratio(1, 2).unwrap_or(false) {
        let stray = *pick(u, &["end", "separate", "begin"]);
        s.push_str(&fld_char(stray));
    }
    s
}

/// A field run group: a complex field (shallow most of the time, the full
/// [`MAX_FIELD_NESTING`] chain occasionally) or a `fldSimple`.
fn gen_field(u: &mut Unstructured) -> String {
    if u.ratio(1, 3).unwrap_or(false) {
        let instr = escape_attr(pick::<&str>(u, FIELD_INSTRUCTIONS));
        let result = if u.ratio(4, 5).unwrap_or(true) {
            gen_run(u)
        } else {
            String::new()
        };
        return format!(r#"<w:fldSimple w:instr="{instr}">{result}</w:fldSimple>"#);
    }
    let depth = if u.ratio(1, 8).unwrap_or(false) {
        MAX_FIELD_NESTING
    } else {
        u.int_in_range(0..=3).unwrap_or(0)
    };
    gen_complex_field(u, depth)
}

/* ------------------------------------------------------------------ */
/* Issue #358 — content controls, AlternateContent, drawings             */
/* ------------------------------------------------------------------ */

/// `inner` wrapped in `depth` nested `w:sdt`s (the innermost carries an
/// `sdtPr` some of the time). Linear in `depth`.
fn wrap_sdt(u: &mut Unstructured, inner: &str, depth: u32) -> String {
    let props = match u.int_in_range(0u8..=3).unwrap_or(0) {
        0 => String::new(),
        1 => "<w:sdtPr/>".to_string(),
        2 => r#"<w:sdtPr><w:alias w:val="fuzz"/><w:tag w:val="t"/><w:id w:val="-1"/></w:sdtPr>"#
            .to_string(),
        _ => format!(
            r#"<w:sdtPr><w:id w:val="{}"/><w:showingPlcHdr/></w:sdtPr>"#,
            attr_num(u, i64::from(i32::MAX))
        ),
    };
    let depth = depth as usize;
    let mut s = String::with_capacity(inner.len() + depth * 48);
    for _ in 0..depth.saturating_sub(1) {
        s.push_str("<w:sdt><w:sdtContent>");
    }
    s.push_str("<w:sdt>");
    s.push_str(&props);
    s.push_str("<w:sdtContent>");
    s.push_str(inner);
    s.push_str("</w:sdtContent></w:sdt>");
    for _ in 0..depth.saturating_sub(1) {
        s.push_str("</w:sdtContent></w:sdt>");
    }
    s
}

const REQUIRES: &[&str] = &[
    "wps", "w14", "wpg", "wp14", "v", "", "  ", "bogus", "wps w14", "w99", "a:b", "w",
];

/// `mc:AlternateContent` around branches built by `branch`: zero to two
/// `Choice`s with random `Requires`, an optional `Fallback`, sometimes an
/// empty element.
fn gen_alternate_content(
    u: &mut Unstructured,
    branch: &mut dyn FnMut(&mut Unstructured) -> String,
) -> String {
    if u.ratio(1, 12).unwrap_or(false) {
        return "<mc:AlternateContent/>".to_string();
    }
    let mut s = String::from("<mc:AlternateContent>");
    let choices = u.int_in_range(0u8..=2).unwrap_or(1);
    for _ in 0..choices {
        let req = *pick(u, REQUIRES);
        s.push_str(&format!(r#"<mc:Choice Requires="{req}">"#));
        s.push_str(&branch(u));
        s.push_str("</mc:Choice>");
    }
    if u.ratio(3, 4).unwrap_or(true) {
        s.push_str("<mc:Fallback>");
        s.push_str(&branch(u));
        s.push_str("</mc:Fallback>");
    }
    s.push_str("</mc:AlternateContent>");
    s
}

/// Issue #358 — a drawing whose `r:embed` resolves to nothing usable: a
/// missing relationship, a relationship to an absent part (`rId3`), the
/// hyperlink relationship (wrong type), or empty — inline or anchored,
/// with hostile extents and offsets; sometimes an empty `wp:inline`.
fn gen_drawing(u: &mut Unstructured, rels: &mut Rels) -> String {
    if u.ratio(1, 8).unwrap_or(false) {
        return "<w:r><w:drawing><wp:inline/></w:drawing></w:r>".to_string();
    }
    let rid = match u.int_in_range(0u8..=3).unwrap_or(0) {
        0 => "rIdMissing",
        1 => {
            rels.missing_image = true;
            "rId3"
        }
        2 => {
            rels.hyperlink = true;
            "rId2"
        }
        _ => "",
    };
    let cx = attr_num(u, 20_000_000);
    let cy = attr_num(u, 20_000_000);
    let id = attr_num(u, i64::from(i32::MAX));
    let graphic = format!(
        r#"<a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:pic><pic:nvPicPr><pic:cNvPr id="{id}" name="p"/><pic:cNvPicPr/></pic:nvPicPr><pic:blipFill><a:blip r:embed="{rid}"/></pic:blipFill><pic:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="{cx}" cy="{cy}"/></a:xfrm></pic:spPr></pic:pic></a:graphicData></a:graphic>"#
    );
    let doc_pr = format!(r#"<wp:docPr id="{id}" name="Picture"/>"#);
    let body = if u.ratio(1, 2).unwrap_or(true) {
        format!(r#"<wp:inline><wp:extent cx="{cx}" cy="{cy}"/>{doc_pr}{graphic}</wp:inline>"#)
    } else {
        let behind = *pick(u, &["0", "1", "true", "bogus"]);
        let height = attr_num(u, i64::from(u32::MAX));
        let x = attr_num(u, 10_000_000);
        let y = attr_num(u, 10_000_000);
        let wrap = *pick(
            u,
            &[
                r#"<wp:wrapSquare wrapText="bothSides"/>"#,
                "<wp:wrapNone/>",
                "<wp:wrapTopAndBottom/>",
                r#"<wp:wrapTight wrapText="left"/>"#,
                "",
            ],
        );
        format!(
            r#"<wp:anchor behindDoc="{behind}" relativeHeight="{height}" simplePos="0" locked="0" layoutInCell="1" allowOverlap="1"><wp:simplePos x="0" y="0"/><wp:positionH relativeFrom="column"><wp:posOffset>{x}</wp:posOffset></wp:positionH><wp:positionV relativeFrom="paragraph"><wp:posOffset>{y}</wp:posOffset></wp:positionV><wp:extent cx="{cx}" cy="{cy}"/>{wrap}{doc_pr}{graphic}</wp:anchor>"#
        )
    };
    format!("<w:r><w:drawing>{body}</w:drawing></w:r>")
}

/// One inline item: a plain run most of the time, else a hyperlink, a
/// field, a run-level content control, run-level `AlternateContent` or a
/// drawing.
fn gen_inline(u: &mut Unstructured, rels: &mut Rels) -> String {
    match u.int_in_range(0u8..=15).unwrap_or(0) {
        0..=8 => gen_run(u),
        9 | 10 => {
            rels.hyperlink = true;
            let run = gen_run(u);
            format!(r#"<w:hyperlink r:id="rId2">{run}</w:hyperlink>"#)
        }
        11 => gen_field(u),
        12 => {
            let depth = *pick(u, SDT_DEPTHS);
            let inner = gen_run(u);
            wrap_sdt(u, &inner, depth)
        }
        13 => gen_alternate_content(u, &mut gen_run),
        14 => gen_drawing(u, rels),
        _ => {
            // A field inside a content control inside AlternateContent.
            let field = gen_field(u);
            let sdt = wrap_sdt(u, &field, 1);
            gen_alternate_content(u, &mut |_| sdt.clone())
        }
    }
}

fn gen_ppr(u: &mut Unstructured, allow_sect_pr: bool) -> String {
    if u.ratio(1, 5).unwrap_or(false) {
        return String::new();
    }
    let mut s = String::from("<w:pPr>");
    if u.ratio(1, 2).unwrap_or(false) {
        let jc = pick(u, &["start", "end", "center", "both", "not-a-jc", ""]);
        s.push_str(&format!(r#"<w:jc w:val="{jc}"/>"#));
    }
    if u.ratio(1, 2).unwrap_or(false) {
        s.push_str(&format!(
            r#"<w:ind w:start="{}" w:end="{}" w:firstLine="{}" w:hanging="{}"/>"#,
            attr_num(u, 5000),
            attr_num(u, 5000),
            attr_num(u, 2000),
            attr_num(u, 2000)
        ));
    }
    if u.ratio(1, 2).unwrap_or(false) {
        s.push_str(&format!(
            r#"<w:spacing w:before="{}" w:after="{}" w:line="{}"/>"#,
            attr_num(u, 2000),
            attr_num(u, 2000),
            attr_num(u, 720)
        ));
    }
    s.push_str(&maybe_bool_element(u, "w:bidi"));
    s.push_str(&maybe_bool_element(u, "w:pageBreakBefore"));
    if u.ratio(1, 3).unwrap_or(false) {
        s.push_str(&format!(
            r#"<w:numPr><w:ilvl w:val="{}"/><w:numId w:val="{}"/></w:numPr>"#,
            attr_num(u, 8),
            attr_num(u, 20)
        ));
    }
    // A mid-document section break: `<w:sectPr>` nested inside a
    // paragraph's `<w:pPr>` closes out the PREVIOUS section.
    if allow_sect_pr && u.ratio(1, 6).unwrap_or(false) {
        s.push_str(&gen_sect_pr(u));
    }
    s.push_str("</w:pPr>");
    s
}

fn gen_sect_pr(u: &mut Unstructured) -> String {
    format!(
        r#"<w:sectPr><w:pgSz w:w="{}" w:h="{}"/><w:pgMar w:top="{}" w:right="{}" w:bottom="{}" w:left="{}" w:header="{}" w:footer="{}"/></w:sectPr>"#,
        attr_num(u, 30000),
        attr_num(u, 30000),
        attr_num(u, 5000),
        attr_num(u, 5000),
        attr_num(u, 5000),
        attr_num(u, 5000),
        attr_num(u, 1440),
        attr_num(u, 1440),
    )
}

fn gen_paragraph(u: &mut Unstructured, allow_sect_pr: bool, rels: &mut Rels) -> String {
    let ppr = gen_ppr(u, allow_sect_pr);
    let run_count = u.int_in_range(0u32..=4).unwrap_or(0);
    let mut runs = String::new();
    for _ in 0..run_count {
        runs.push_str(&gen_inline(u, rels));
    }
    format!("<w:p>{ppr}{runs}</w:p>")
}

/// A cell; past nesting depth 0 its content may be a nested table
/// (`depth` levels deep already, at most [`MAX_TABLE_NESTING`]).
fn gen_cell(u: &mut Unstructured, rels: &mut Rels, depth: u32) -> String {
    let mut tc_pr = String::from("<w:tcPr>");
    if u.ratio(1, 2).unwrap_or(false) {
        let wtype = pick(u, &["dxa", "pct", "auto", "nil", "bogus"]);
        tc_pr.push_str(&format!(
            r#"<w:tcW w:w="{}" w:type="{wtype}"/>"#,
            attr_num(u, 10000)
        ));
    }
    if u.ratio(1, 3).unwrap_or(false) {
        tc_pr.push_str(&format!(r#"<w:gridSpan w:val="{}"/>"#, attr_num(u, 8)));
    }
    if u.ratio(1, 3).unwrap_or(false) {
        let v = pick(u, &["restart", "continue", "bogus"]);
        tc_pr.push_str(&format!(r#"<w:vMerge w:val="{v}"/>"#));
    }
    tc_pr.push_str("</w:tcPr>");
    let mut content = String::new();
    if depth + 1 < MAX_TABLE_NESTING && u.ratio(1, 8).unwrap_or(false) {
        content.push_str(&gen_table(u, rels, depth + 1));
    }
    let para_count = u.int_in_range(1u32..=2).unwrap_or(1);
    for _ in 0..para_count {
        content.push_str(&gen_paragraph(u, false, rels));
    }
    format!("<w:tc>{tc_pr}{content}</w:tc>")
}

/// A table at nesting `depth` (0 = top level). Issue #422 — nested tables
/// are 1×1, so a nesting chain stays linear in its depth.
fn gen_table(u: &mut Unstructured, rels: &mut Rels, depth: u32) -> String {
    let (cols, rows) = if depth == 0 {
        (
            u.int_in_range(1u32..=5).unwrap_or(1),
            u.int_in_range(0u32..=5).unwrap_or(0),
        )
    } else {
        (1, 1)
    };
    let mut grid = String::from("<w:tblGrid>");
    for _ in 0..cols {
        grid.push_str(&format!(r#"<w:gridCol w:w="{}"/>"#, attr_num(u, 5000)));
    }
    grid.push_str("</w:tblGrid>");
    let mut rows_xml = String::new();
    for _ in 0..rows {
        // Deliberately allow the per-row cell count to diverge from `cols`
        // — a mismatched grid/row cell count is exactly the kind of
        // "valid but hostile" shape the reader must tolerate.
        let cells_in_row = if depth == 0 {
            u.int_in_range(0u32..=cols.max(1) + 1).unwrap_or(0)
        } else {
            1
        };
        let mut cells = String::new();
        for _ in 0..cells_in_row {
            cells.push_str(&gen_cell(u, rels, depth));
        }
        rows_xml.push_str(&format!("<w:tr>{cells}</w:tr>"));
    }
    /* Issue #349 — table width / indent measures, valid or hostile. */
    let tbl_pr = if u.ratio(1, 2).unwrap_or(false) {
        let wtype = pick(u, &["dxa", "pct", "auto", "bogus"]);
        format!(
            r#"<w:tblPr><w:tblW w:w="{}" w:type="{wtype}"/><w:tblInd w:w="{}" w:type="dxa"/></w:tblPr>"#,
            attr_num(u, 10000),
            attr_num(u, 1440)
        )
    } else {
        "<w:tblPr/>".to_string()
    };
    format!("<w:tbl>{tbl_pr}{grid}{rows_xml}</w:tbl>")
}

/// Issue #358 — a 1×1 table chain exactly `depth` tables deep around one
/// paragraph (the 60-deep shape, linear in size).
fn gen_table_chain(u: &mut Unstructured, rels: &mut Rels, depth: u32) -> String {
    let mut inner = gen_paragraph(u, false, rels);
    for _ in 0..depth {
        inner = format!(
            r#"<w:tbl><w:tblPr/><w:tblGrid><w:gridCol w:w="{}"/></w:tblGrid><w:tr><w:tc>{inner}<w:p/></w:tc></w:tr></w:tbl>"#,
            attr_num(u, 5000)
        );
    }
    inner
}

/// One body-level block.
fn gen_block(u: &mut Unstructured, rels: &mut Rels) -> String {
    match u.int_in_range(0u8..=15).unwrap_or(0) {
        0..=8 => gen_paragraph(u, true, rels),
        9..=11 => gen_table(u, rels, 0),
        12 => {
            let depth = *pick(u, SDT_DEPTHS);
            let inner = gen_paragraph(u, false, rels);
            wrap_sdt(u, &inner, depth)
        }
        13 => gen_alternate_content(u, &mut |u: &mut Unstructured| {
            gen_paragraph(u, false, &mut Rels::default())
        }),
        14 => {
            let depth = *pick(u, &[2u32, 8, 31, 32, 33, MAX_TABLE_NESTING]);
            gen_table_chain(u, rels, depth)
        }
        _ => {
            // AlternateContent INSIDE a paragraph, around its runs.
            let ppr = gen_ppr(u, false);
            let ac = gen_alternate_content(u, &mut gen_run);
            format!("<w:p>{ppr}{ac}</w:p>")
        }
    }
}

fn gen_body(u: &mut Unstructured, rels: &mut Rels) -> String {
    let block_count = u.int_in_range(0u32..=8).unwrap_or(0);
    let mut body = String::new();
    for _ in 0..block_count {
        /* Issue #422 — the package budget. */
        if body.len() > MAX_DOCUMENT_XML_BYTES {
            break;
        }
        body.push_str(&gen_block(u, rels));
    }
    // Body-level trailing `<w:sectPr>` — required-ish per `.claude/rules/docx.md`.
    body.push_str(&gen_sect_pr(u));
    body
}

/// The root element's namespace declarations: `w` / `r` always, the
/// drawing / markup-compatibility ones most of the time (an undeclared
/// prefix is a hostile shape too), and a random `mc:Ignorable`.
fn gen_root_open(u: &mut Unstructured) -> String {
    let mut s = String::from(
        r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships""#,
    );
    if u.ratio(7, 8).unwrap_or(true) {
        s.push_str(concat!(
            r#" xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006""#,
            r#" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing""#,
            r#" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main""#,
            r#" xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture""#,
            r#" xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape""#,
            r#" xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml""#,
            r#" xmlns:v="urn:schemas-microsoft-com:vml""#,
        ));
        let ignorable = *pick(u, &["", "w14", "w14 wps", "bogus", "w14 w99"]);
        if !ignorable.is_empty() {
            s.push_str(&format!(r#" mc:Ignorable="{ignorable}""#));
        }
    }
    s.push('>');
    s
}

/// Assemble the OPC zip exactly like `format_docx::writer`'s private
/// `zip_minimal_docx` test helper.
fn zip_opc(document_xml: &[u8], doc_rels: Option<&str>) -> Vec<u8> {
    let mut buf: Vec<u8> = Vec::new();
    {
        let mut zip = ZipWriter::new(Cursor::new(&mut buf));
        let opts = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(0o644);
        let _ = zip.start_file("[Content_Types].xml", opts);
        let _ = zip.write_all(CONTENT_TYPES_XML.as_bytes());
        let _ = zip.start_file("_rels/.rels", opts);
        let _ = zip.write_all(DOT_RELS_XML.as_bytes());
        if let Some(rels) = doc_rels {
            let _ = zip.start_file("word/_rels/document.xml.rels", opts);
            let _ = zip.write_all(rels.as_bytes());
        }
        let _ = zip.start_file("word/document.xml", opts);
        let _ = zip.write_all(document_xml);
        let _ = zip.finish();
    }
    buf
}

/* ------------------------------------------------------------------ */
/* Issue #358 — splice mode                                             */
/* ------------------------------------------------------------------ */

/// Spec snippets spliced into a valid `document.xml`: a NUL character
/// reference, U+FFFC (the engine's own inline-object placeholder), an
/// empty `wp:inline`, stray field characters, unbalanced structure,
/// non-characters, markup the reader does not expect where it lands.
pub const SPLICE_SNIPPETS: &[&str] = &[
    "&#0;",
    "&#x0;",
    "\u{FFFC}",
    "&#xFFFC;",
    "&#xFFFE;",
    "&#xD800;",
    "<wp:inline/>",
    "<w:drawing><wp:inline/></w:drawing>",
    r#"<w:r><w:fldChar w:fldCharType="begin"/></w:r>"#,
    r#"<w:r><w:fldChar w:fldCharType="separate"/></w:r>"#,
    r#"<w:r><w:fldChar w:fldCharType="end"/></w:r>"#,
    r#"<w:fldChar w:fldCharType="end"/>"#,
    r#"<w:instrText> PAGE </w:instrText>"#,
    "<mc:AlternateContent/>",
    r#"<mc:Choice Requires="w14">"#,
    "</mc:Fallback>",
    "<w:sdt/>",
    "<w:sdtContent>",
    "</w:p>",
    "<w:p>",
    "<w:t>",
    "<w:tab/>",
    "<w:br/>",
    "<![CDATA[x]]>",
    "<?pi x?>",
    "<!-- c -->",
    "&amp;amp;",
    "&bogus;",
    "\u{0301}",
    "\u{202E}",
];

/// Issue #358 — the most fuzz-input bytes one raw splice copies.
pub const MAX_RAW_SPLICE: usize = 48;

/// Issue #358 — mutate a valid `document.xml`: splice snippets at tag
/// boundaries (or anywhere), splice a short slice of the fuzz input itself
/// (how the nightly's `-dict=dictionaries/docx.dict` tokens reach the
/// XML), flip bytes, or truncate. Bounded: at most eight edits of at most
/// [`MAX_RAW_SPLICE`] bytes each.
fn mutate_document_xml(u: &mut Unstructured, xml: String) -> Vec<u8> {
    let mut bytes = xml.into_bytes();
    let edits = u.int_in_range(1u8..=8).unwrap_or(1);
    for _ in 0..edits {
        if bytes.is_empty() {
            break;
        }
        match u.int_in_range(0u8..=10).unwrap_or(0) {
            // Raw input bytes, verbatim, at a tag boundary.
            10 => {
                let n = u.int_in_range(1usize..=MAX_RAW_SPLICE).unwrap_or(1);
                let raw = u.bytes(n.min(u.len())).unwrap_or(&[]).to_vec();
                let start = u.choose_index(bytes.len()).unwrap_or(0);
                let at = bytes[start..]
                    .iter()
                    .position(|b| *b == b'>')
                    .map_or(bytes.len(), |i| start + i + 1);
                bytes.splice(at..at, raw);
            }
            // Splice a snippet right after a `>` (a tag boundary)…
            0..=5 => {
                let snippet = pick(u, SPLICE_SNIPPETS).as_bytes();
                let start = u.choose_index(bytes.len()).unwrap_or(0);
                let at = bytes[start..]
                    .iter()
                    .position(|b| *b == b'>')
                    .map_or(bytes.len(), |i| start + i + 1);
                bytes.splice(at..at, snippet.iter().copied());
            }
            // …or anywhere.
            6 => {
                let snippet = pick(u, SPLICE_SNIPPETS).as_bytes();
                let at = u.choose_index(bytes.len() + 1).unwrap_or(0);
                bytes.splice(at..at, snippet.iter().copied());
            }
            // Flip a byte.
            7 | 8 => {
                let at = u.choose_index(bytes.len()).unwrap_or(0);
                let mask = *pick(u, &[0x01u8, 0x20, 0x80, 0xff]);
                bytes[at] ^= mask;
            }
            // Truncate.
            _ => {
                let at = u.choose_index(bytes.len() + 1).unwrap_or(0);
                bytes.truncate(at);
            }
        }
    }
    bytes
}

/// Build one fuzz input's `.docx` bytes: a schema-shaped `word/document.xml`
/// wrapped in a minimal OPC package. Returns `None` only when `data` is
/// too small to make any decision at all (the libFuzzer minimal-input
/// case), so the target can bail out cheaply.
///
/// Issue #358 — one input in five takes the splice mode: the generated
/// `document.xml` is mutated ([`mutate_document_xml`]) before zipping.
pub fn build_docx(u: &mut Unstructured) -> Option<Vec<u8>> {
    if u.is_empty() {
        return None;
    }
    let mut rels = Rels::default();
    let root = gen_root_open(u);
    let body = gen_body(u, &mut rels);
    let document_xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n{root}<w:body>{body}</w:body></w:document>"
    );
    let document_xml = if u.ratio(1, 5).unwrap_or(false) {
        mutate_document_xml(u, document_xml)
    } else {
        document_xml.into_bytes()
    };
    let mut bytes = zip_opc(&document_xml, rels.xml().as_deref());
    // Occasionally truncate the finished archive — a corrupt/incomplete
    // zip is a real thing a hostile or crashed upload can produce, and
    // `read_docx` must return `Err`, not panic, on it.
    if !bytes.is_empty() && u.ratio(1, 20).unwrap_or(false) {
        let cut = u.int_in_range(0..=bytes.len() as u64).unwrap_or(0) as usize;
        bytes.truncate(cut);
    }
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn document_xml_len(docx: &[u8]) -> usize {
        let Ok(mut z) = zip::ZipArchive::new(Cursor::new(docx)) else {
            return 0;
        };
        let Ok(mut f) = z.by_name("word/document.xml") else {
            return 0;
        };
        let mut v = Vec::new();
        let _ = f.read_to_end(&mut v);
        v.len()
    }

    /// Issue #422 — whatever the bytes, a generated package stays within
    /// the budget (+ one block of overshoot), and the deep shapes are
    /// actually produced.
    #[test]
    fn generated_packages_are_bounded_and_reach_the_deep_shapes() {
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        let mut max = 0usize;
        let (mut deep_sdt, mut fields, mut drawings, mut ac, mut chains) = (0, 0, 0, 0, 0);
        for _ in 0..3000 {
            let mut noise = Vec::new();
            for _ in 0..512 {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                noise.push(state as u8);
            }
            let Some(docx) = build_docx(&mut Unstructured::new(&noise)) else {
                continue;
            };
            max = max.max(document_xml_len(&docx));
            let mut z = match zip::ZipArchive::new(Cursor::new(&docx)) {
                Ok(z) => z,
                Err(_) => continue,
            };
            let mut xml = String::new();
            if let Ok(mut f) = z.by_name("word/document.xml") {
                let _ = f.read_to_string(&mut xml);
            }
            deep_sdt += usize::from(xml.matches("<w:sdt>").count() >= 255);
            fields += usize::from(xml.contains("w:fldChar"));
            drawings += usize::from(xml.contains("<w:drawing>"));
            ac += usize::from(xml.contains("<mc:AlternateContent"));
            chains += usize::from(xml.matches("<w:tbl>").count() >= 32);
        }
        assert!(
            max <= MAX_DOCUMENT_XML_BYTES + 512 * 1024,
            "largest document.xml was {max} bytes"
        );
        for (name, n) in [
            ("deep sdt", deep_sdt),
            ("fields", fields),
            ("drawings", drawings),
            ("AlternateContent", ac),
            ("deep table chains", chains),
        ] {
            assert!(n > 0, "the sweep never produced {name}");
        }
    }

    /// The worst case by construction: a 5000-deep sdt is ~250 KB, a
    /// 60-deep table chain and a 40-deep field chain a few KB.
    #[test]
    fn the_deepest_shapes_are_linear_in_size() {
        let mut u = Unstructured::new(&[0u8; 64]);
        assert!(wrap_sdt(&mut u, "<w:p/>", 5000).len() < 300 * 1024);
        let mut u = Unstructured::new(&[0u8; 4096]);
        let chain = gen_table_chain(&mut u, &mut Rels::default(), MAX_TABLE_NESTING);
        assert_eq!(chain.matches("<w:tbl>").count(), MAX_TABLE_NESTING as usize);
        assert!(chain.len() < 16 * 1024);
        let mut u = Unstructured::new(&[0xffu8; 8192]);
        assert!(gen_complex_field(&mut u, MAX_FIELD_NESTING).len() < 64 * 1024);
    }
}
