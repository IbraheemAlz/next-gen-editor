//! Issue #89 — the dedicated Arabic / RTL fixture corpus for the
//! differential oracle harness (`tools/differential/`).
//!
//! Every fixture here is **hand-written OOXML**, not a hand-edited binary:
//! each is a small, from-scratch `.docx` zip assembled the same way
//! `tools/roundtrip/src/main.rs`'s `build_table_fixture` family does. The
//! Arabic body text is original filler prose composed for this harness
//! (short generic sentences about testing layout/justification — the same
//! spirit as `tools/perf-fixtures`'s `FILLER` constant), never copied from
//! any external document.
//!
//! CLEAN-ROOM: composed from ECMA-376 (`<w:bidi>`, `<w:jc w:val="both">`,
//! `<w:numPr>`, `<w:bidiVisual>`, …) and this repo's own established XML
//! shape (`tools/roundtrip`), never from `/data/code/reference/`.
//!
//! `cargo run -p differential-native --release -- --gen-fixtures [dir]`
//! (re)generates every file plus `_manifest.json` into
//! `tools/differential/fixtures/arabic/` by default. Idempotent — safe to
//! re-run and commit the diff (there should usually be none).

use anyhow::{Context, Result};
use std::io::Write;
use std::path::Path;
use zip::write::{SimpleFileOptions, ZipWriter};

pub const DEFAULT_FIXTURES_DIR: &str = "tools/differential/fixtures/arabic";

const DOT_RELS_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#;

const CONTENT_TYPES_PLAIN: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#;

const CONTENT_TYPES_NUMBERING: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
<Override PartName="/word/numbering.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml"/>
</Types>"#;

const DOC_RELS_PLAIN: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
</Relationships>"#;

const DOC_RELS_NUMBERING: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering" Target="numbering.xml"/>
</Relationships>"#;

fn docx_zip(entries: &[(&str, &str)]) -> Vec<u8> {
    let mut buf: Vec<u8> = Vec::new();
    {
        let mut zip = ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(0o644);
        for (name, body) in entries {
            zip.start_file(*name, opts).unwrap();
            zip.write_all(body.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    buf
}

/// Explicit `<w:pgSz>` + `<w:pgMar>` (A4, 1in margins, 0.5in header/footer —
/// the exact canonical twips `engine::PageGeometry::a4()` documents, see
/// `crates/engine/src/lib.rs`). Deliberately explicit rather than an empty
/// `<w:sectPr/>`: without a `<w:pgSz>`, our engine defaults to A4 while this
/// harness's LibreOffice defaults to US Letter (its locale default) —
/// confirmed during development (see `tools/differential/README.md`'s
/// findings). Pinning the page size here isolates the fixtures' real
/// purpose (justify/kashida/bidi/list/table line-break agreement) from that
/// unrelated, already-known, separately-reported page-size-default gap.
const SECT_PR_A4: &str = r#"<w:sectPr><w:pgSz w:w="11906" w:h="16838"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440" w:header="720" w:footer="720"/></w:sectPr>"#;

fn document_xml_plain(body_inner: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body_inner}{SECT_PR_A4}</w:body></w:document>"#
    )
}

fn build_plain(body_inner: &str) -> Vec<u8> {
    let document_xml = document_xml_plain(body_inner);
    docx_zip(&[
        ("[Content_Types].xml", CONTENT_TYPES_PLAIN),
        ("_rels/.rels", DOT_RELS_XML),
        ("word/_rels/document.xml.rels", DOC_RELS_PLAIN),
        ("word/document.xml", &document_xml),
    ])
}

fn build_with_numbering(body_inner: &str, numbering_xml: &str) -> Vec<u8> {
    let document_xml = document_xml_plain(body_inner);
    docx_zip(&[
        ("[Content_Types].xml", CONTENT_TYPES_NUMBERING),
        ("_rels/.rels", DOT_RELS_XML),
        ("word/_rels/document.xml.rels", DOC_RELS_NUMBERING),
        ("word/numbering.xml", numbering_xml),
        ("word/document.xml", &document_xml),
    ])
}

/// `<w:spacing w:after="120"/>` = 6pt after every top-level paragraph.
/// Real Word documents almost always carry non-zero paragraph spacing
/// (the stock "Normal" style ships ~8pt after); without it, consecutive
/// justified paragraphs stack with *zero* visual gap and the
/// `tools/differential` runner's vertical-gap paragraph-grouping
/// heuristic has nothing to key off. `<w:bidi/>` selects RTL base
/// direction (confirmed round-tripped by `pPr_bidi_rtl.docx`).
const PARA_SPACING_AFTER: &str = r#"<w:spacing w:after="120"/>"#;

/// One RTL/justified Arabic paragraph, `<w:jc w:val="both">` (OOXML's
/// "justify" value) so the layout justify pass — kashida on our engine,
/// whatever LibreOffice's own Arabic justify does — actually runs.
fn rtl_justified_paragraph(text: &str) -> String {
    format!(
        r#"<w:p><w:pPr><w:bidi/><w:jc w:val="both"/>{PARA_SPACING_AFTER}</w:pPr><w:r><w:t xml:space="preserve">{text}</w:t></w:r></w:p>"#
    )
}

fn rtl_paragraph(text: &str) -> String {
    format!(
        r#"<w:p><w:pPr><w:bidi/>{PARA_SPACING_AFTER}</w:pPr><w:r><w:t xml:space="preserve">{text}</w:t></w:r></w:p>"#
    )
}

fn rtl_list_item(text: &str, num_id: u32) -> String {
    format!(
        r#"<w:p><w:pPr><w:bidi/><w:numPr><w:ilvl w:val="0"/><w:numId w:val="{num_id}"/></w:numPr></w:pPr><w:r><w:t xml:space="preserve">{text}</w:t></w:r></w:p>"#
    )
}

/* ---------------------------------------------------------------- */
/* Filler prose — original, composed for this harness.               */
/* ---------------------------------------------------------------- */

/// Multi-sentence Arabic filler long enough to wrap across several lines
/// at A4 content width (~451pt) — short single-line paragraphs never
/// exercise the justify pass (the last line of a paragraph is never
/// justified).
const FILLER_A: &str = "هذا نص عربي تجريبي لاختبار محاذاة النص وضبط المسافات بين الكلمات. \
يهدف هذا النص إلى قياس جودة التبرير في الفقرات الطويلة عبر عدة أسطر متتالية. \
تتكرر بعض الكلمات لضمان وجود فرص كافية لتمديد الحروف المتصلة عند الحاجة إلى الكشيدة. \
نأمل أن يوفر هذا المحتوى تغطية كافية لاختبار المحرك مقابل ليبر أوفيس.";

/// Denser filler (fewer inter-word spaces relative to length, more
/// cursive-joining runs) meant to push the justify pass toward Kashida
/// elongation rather than pure space-stretching.
const FILLER_KASHIDA: &str = "بسم الله الرحمن الرحيم نبدأ هذا النص التجريبي الطويل الممتد عبر عدة أسطر \
متتالية ومتراصة لاختبار عملية التبرير الكامل للنص العربي المتصل الحروف بشكل \
مستمر دون فواصل كثيرة بين الكلمات المتجاورة في هذه الفقرة الطويلة.";

/* ---------------------------------------------------------------- */
/* Fixture builders                                                  */
/* ---------------------------------------------------------------- */

fn build_justified_simple() -> Vec<u8> {
    let body = format!(
        "{}{}",
        rtl_justified_paragraph(FILLER_A),
        rtl_justified_paragraph(FILLER_A)
    );
    build_plain(&body)
}

fn build_kashida_justified() -> Vec<u8> {
    let body = format!(
        "{}{}{}",
        rtl_justified_paragraph(FILLER_KASHIDA),
        rtl_justified_paragraph(FILLER_KASHIDA),
        rtl_justified_paragraph(FILLER_KASHIDA)
    );
    build_plain(&body)
}

fn build_mixed_direction() -> Vec<u8> {
    let text = "يستخدم هذا التقرير Microsoft Word و LibreOffice لمقارنة النتائج. \
تم تسجيل 128 صفحة و 4096 كلمة خلال الاختبار. \
This paragraph mixes English words like Engine and Layout directly inside Arabic text \
لضمان اختبار الاتجاه المختلط داخل السطر الواحد. \
Numbers like 2026 and identifiers like v0.6.0-beta.2 also appear inline.";
    let body = rtl_justified_paragraph(text);
    build_plain(&body)
}

fn build_rtl_bullet_list() -> Vec<u8> {
    let numbering_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
<w:abstractNum w:abstractNumId="0">
<w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="bullet"/><w:lvlText w:val="*"/></w:lvl>
</w:abstractNum>
<w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>
</w:numbering>"#;
    let body = format!(
        "{}{}{}{}",
        rtl_paragraph("مقدمة القائمة النقطية"),
        rtl_list_item("العنصر الأول في القائمة النقطية", 1),
        rtl_list_item(
            "العنصر الثاني يحتوي على نص أطول قليلاً لاختبار التفاف الأسطر داخل عنصر القائمة نفسه",
            1,
        ),
        rtl_list_item("العنصر الثالث والأخير في هذه القائمة", 1),
    );
    build_with_numbering(&body, numbering_xml)
}

fn build_rtl_numbered_list() -> Vec<u8> {
    let numbering_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
<w:abstractNum w:abstractNumId="0">
<w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/></w:lvl>
<w:lvl w:ilvl="1"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1.%2."/></w:lvl>
</w:abstractNum>
<w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>
</w:numbering>"#;
    let nested = r#"<w:p><w:pPr><w:bidi/><w:numPr><w:ilvl w:val="1"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t xml:space="preserve">بند فرعي أول</w:t></w:r></w:p>"#;
    let body = format!(
        "{}{}{}{}{}",
        rtl_paragraph("مقدمة القائمة المرقمة"),
        rtl_list_item("البند الأول من القائمة المرقمة", 1),
        rtl_list_item("البند الثاني من القائمة المرقمة", 1),
        nested,
        rtl_list_item("البند الثالث بعد العودة إلى المستوى الأول", 1),
    );
    build_with_numbering(&body, numbering_xml)
}

fn build_rtl_table() -> Vec<u8> {
    let header_cell = |text: &str| -> String {
        format!(
            r#"<w:tc><w:p><w:pPr><w:bidi/><w:jc w:val="center"/></w:pPr><w:r><w:t xml:space="preserve">{text}</w:t></w:r></w:p></w:tc>"#
        )
    };
    let data_cell = |text: &str| -> String {
        format!(
            r#"<w:tc><w:p><w:pPr><w:bidi/></w:pPr><w:r><w:t xml:space="preserve">{text}</w:t></w:r></w:p></w:tc>"#
        )
    };
    let tbl = format!(
        r#"<w:tbl><w:tblPr><w:bidiVisual/></w:tblPr><w:tblGrid><w:gridCol w:w="1800"/><w:gridCol w:w="1800"/><w:gridCol w:w="1800"/></w:tblGrid>
<w:tr>{}{}{}</w:tr>
<w:tr>{}{}{}</w:tr>
<w:tr>{}{}{}</w:tr>
</w:tbl>"#,
        header_cell("الاسم"),
        header_cell("المدينة"),
        header_cell("الملاحظات"),
        data_cell("أحمد"),
        data_cell("القاهرة"),
        data_cell("حضر الاجتماع الأول وشارك في المناقشة"),
        data_cell("سارة"),
        data_cell("دبي"),
        data_cell("قدمت تقريراً مفصلاً عن سير العمل خلال الأسبوع"),
    );
    let body = format!(
        "{}{}{}",
        rtl_paragraph("مقدمة الجدول"),
        tbl,
        rtl_paragraph("خاتمة الجدول")
    );
    build_plain(&body)
}

/// A longer, multi-page document — the strongest page-count + line-break
/// oracle test (a single-page fixture trivially agrees on page count with
/// any oracle). ~24 justified Arabic paragraphs, each a few lines, should
/// span multiple A4 pages at default 1in margins / 12pt body text.
fn build_long_document() -> Vec<u8> {
    let mut body = String::new();
    for i in 1..=24u32 {
        let para_text = format!("الفقرة رقم {i}: {FILLER_A}");
        body.push_str(&rtl_justified_paragraph(&para_text));
    }
    build_plain(&body)
}

/* ---------------------------------------------------------------- */
/* Table pagination corpus (issue #155)                              */
/* ---------------------------------------------------------------- */

/// Where `--gen-table-fixtures` writes by default.
pub const TABLE_FIXTURES_DIR: &str = "tools/differential/fixtures/tables";

/// One single-line paragraph at an exact 14pt line pitch with zero
/// before/after spacing. Every fixture line is short enough to stay one
/// line in any sans/serif face, and the exact pitch makes the page
/// distribution independent of the font LibreOffice substitutes — so the
/// oracle compares *where rows break*, not font metrics.
fn exact_line(text: &str) -> String {
    format!(
        r#"<w:p><w:pPr><w:spacing w:before="0" w:after="0" w:line="280" w:lineRule="exact"/></w:pPr><w:r><w:t xml:space="preserve">{text}</w:t></w:r></w:p>"#
    )
}

/// A two-column table: a one-line row, a tall row (cell A = `tall_lines`
/// exact lines, cell B = one line), a one-line row. `cant_split` stamps
/// `<w:cantSplit/>` on the tall row.
fn long_cell_table(tag: &str, tall_lines: u32, cant_split: bool) -> String {
    let cell = |inner: String| {
        format!(r#"<w:tc><w:tcPr><w:tcW w:w="4500" w:type="dxa"/></w:tcPr>{inner}</w:tc>"#)
    };
    let tall_a: String = (1..=tall_lines)
        .map(|i| exact_line(&format!("{tag} cell line {i}")))
        .collect();
    let tr_pr = if cant_split {
        "<w:trPr><w:cantSplit/></w:trPr>"
    } else {
        ""
    };
    format!(
        r#"<w:tbl><w:tblPr><w:tblW w:w="9000" w:type="dxa"/><w:tblLayout w:type="fixed"/></w:tblPr><w:tblGrid><w:gridCol w:w="4500"/><w:gridCol w:w="4500"/></w:tblGrid><w:tr>{}{}</w:tr><w:tr>{tr_pr}{}{}</w:tr><w:tr>{}{}</w:tr></w:tbl>"#,
        cell(exact_line(&format!("{tag} first row A"))),
        cell(exact_line(&format!("{tag} first row B"))),
        cell(tall_a),
        cell(exact_line(&format!("{tag} short cell"))),
        cell(exact_line(&format!("{tag} last row A"))),
        cell(exact_line(&format!("{tag} last row B"))),
    )
}

/// Issue #155 — Word's default "allow row to break across pages". On A4
/// with 1in margins the body is ~697.9pt: 44 exact 14pt lines (616pt) +
/// the first row (14pt) leave ~67.9pt, so table T1's 20-line tall row
/// keeps 4 lines on page 1 and continues (16 lines + the last row) on
/// page 2. 30 more body lines then leave ~25.9pt under T2's first row:
/// T2's tall row is `<w:cantSplit/>`, so it moves whole to page 3 (a
/// splittable row would keep one line). Expected: 3 pages; page 1 ends
/// at "T1 cell line 4", page 3 opens with "T2 cell line 1".
fn build_long_cell_table() -> Vec<u8> {
    let mut body = String::new();
    for i in 1..=44u32 {
        body.push_str(&exact_line(&format!("Body line {i}")));
    }
    body.push_str(&long_cell_table("T1", 20, false));
    for i in 1..=30u32 {
        body.push_str(&exact_line(&format!("Between line {i}")));
    }
    body.push_str(&long_cell_table("T2", 20, true));
    body.push_str(&exact_line("End of document"));
    build_plain(&body)
}

/// Issue #173 — one fixed-width (2 × 2000 twips = 200pt) bordered 2 × 1
/// table. `placement` is the `<w:tblPr>` children between `<w:tblW>` and
/// `<w:tblBorders>` (`<w:bidiVisual/>` goes before `<w:tblW>` per
/// CT_TblPrBase, so it is a separate flag, which also makes the cell
/// paragraphs bidi).
fn placed_table(tag: &str, placement: &str, bidi_visual: bool) -> String {
    let edge =
        |e: &str| format!(r#"<w:{e} w:val="single" w:sz="4" w:space="0" w:color="000000"/>"#);
    let borders = format!(
        "<w:tblBorders>{}{}{}{}</w:tblBorders>",
        edge("top"),
        edge("left"),
        edge("bottom"),
        edge("right")
    );
    let bidi = if bidi_visual { "<w:bidiVisual/>" } else { "" };
    let ppr = if bidi_visual {
        r#"<w:pPr><w:bidi/><w:spacing w:before="0" w:after="0" w:line="280" w:lineRule="exact"/></w:pPr>"#
    } else {
        r#"<w:pPr><w:spacing w:before="0" w:after="0" w:line="280" w:lineRule="exact"/></w:pPr>"#
    };
    let cell = |text: String| {
        format!(
            r#"<w:tc><w:tcPr><w:tcW w:w="2000" w:type="dxa"/></w:tcPr><w:p>{ppr}<w:r><w:t xml:space="preserve">{text}</w:t></w:r></w:p></w:tc>"#
        )
    };
    format!(
        r#"<w:tbl><w:tblPr>{bidi}<w:tblW w:w="4000" w:type="dxa"/>{placement}{borders}<w:tblLayout w:type="fixed"/></w:tblPr><w:tblGrid><w:gridCol w:w="2000"/><w:gridCol w:w="2000"/></w:tblGrid><w:tr>{}{}</w:tr></w:tbl>"#,
        cell(format!("{tag} A")),
        cell(format!("{tag} B")),
    )
}

/// Issue #173 — table horizontal placement oracle. Four 200pt tables on
/// an A4 page (1in margins → a ~451.3pt column): `<w:jc w:val="center">`
/// (left edge ~125.6pt into the column), `<w:jc w:val="right">` (flush
/// right, ~251.3pt), `<w:tblInd w:w="720">` (36pt), and a `<w:bidiVisual>`
/// table with no `<w:jc>` — its default start edge is the RIGHT margin
/// (~251.3pt), cell A rightmost. Separated by exact-pitch paragraphs.
fn build_table_placement() -> Vec<u8> {
    let mut body = String::new();
    body.push_str(&exact_line("Centered table"));
    body.push_str(&placed_table("Center", r#"<w:jc w:val="center"/>"#, false));
    body.push_str(&exact_line("Right-aligned table"));
    body.push_str(&placed_table("Right", r#"<w:jc w:val="right"/>"#, false));
    body.push_str(&exact_line("Indented table"));
    body.push_str(&placed_table(
        "Indent",
        r#"<w:tblInd w:w="720" w:type="dxa"/>"#,
        false,
    ));
    body.push_str(&exact_line("RTL table"));
    body.push_str(&placed_table("RTL", "", true));
    body.push_str(&exact_line("End of document"));
    build_plain(&body)
}

/// Issue #169 — `<w:trHeight w:hRule>` oracle. One fixed-width 2-column
/// bordered table: a one-line row; an `exact` 840-twip (42pt) row whose
/// cell A holds 6 exact 14pt lines (84pt of content — overflows; cell B
/// one line); the same content under `atLeast` 840 twips (grows to 84pt);
/// a one-line row. Zero cell top/bottom margins (Word's stock), so the
/// exact row shows lines 1-3 of cell A and clips lines 4-6.
fn build_exact_row_table() -> Vec<u8> {
    let edge =
        |e: &str| format!(r#"<w:{e} w:val="single" w:sz="4" w:space="0" w:color="000000"/>"#);
    let borders = format!(
        "<w:tblBorders>{}{}{}{}{}{}</w:tblBorders>",
        edge("top"),
        edge("left"),
        edge("bottom"),
        edge("right"),
        edge("insideH"),
        edge("insideV")
    );
    let cell = |inner: String| {
        format!(r#"<w:tc><w:tcPr><w:tcW w:w="4500" w:type="dxa"/></w:tcPr>{inner}</w:tc>"#)
    };
    let tall = |tag: &str| -> String {
        (1..=6u32)
            .map(|i| exact_line(&format!("{tag} line {i}")))
            .collect()
    };
    let row =
        |tr_pr: &str, a: String, b: String| format!("<w:tr>{tr_pr}{}{}</w:tr>", cell(a), cell(b));
    let table = format!(
        r#"<w:tbl><w:tblPr><w:tblW w:w="9000" w:type="dxa"/>{borders}<w:tblLayout w:type="fixed"/></w:tblPr><w:tblGrid><w:gridCol w:w="4500"/><w:gridCol w:w="4500"/></w:tblGrid>{}{}{}{}</w:tbl>"#,
        row("", exact_line("First row A"), exact_line("First row B")),
        row(
            r#"<w:trPr><w:trHeight w:val="840" w:hRule="exact"/></w:trPr>"#,
            tall("Exact"),
            exact_line("Exact row B"),
        ),
        row(
            r#"<w:trPr><w:trHeight w:val="840" w:hRule="atLeast"/></w:trPr>"#,
            tall("AtLeast"),
            exact_line("AtLeast row B"),
        ),
        row("", exact_line("Last row A"), exact_line("Last row B")),
    );
    let mut body = String::new();
    body.push_str(&exact_line("Exact-height row table"));
    body.push_str(&table);
    body.push_str(&exact_line("End of document"));
    build_plain(&body)
}

fn table_fixtures() -> Vec<Fixture> {
    vec![
        Fixture {
            name: "long_cell_table.docx",
            description: "Issue #155 row-split oracle: a 20-line cell row at a page bottom breaks at a line boundary (4 lines stay on page 1); the same row with <w:cantSplit/> later moves whole. Exact 14pt line pitch, explicit A4 pgSz. Expected 3 pages.",
            bytes: build_long_cell_table(),
        },
        Fixture {
            name: "table_placement.docx",
            description: "Issue #173 horizontal-placement oracle: four fixed-width 200pt bordered tables on A4 (1in margins, ~451.3pt column) — jc=center (left edge ~125.6pt into the column), jc=right (~251.3pt, flush right), tblInd=720 twips (36pt), and a bidiVisual table with no jc (default start = right margin, ~251.3pt; cell A rightmost). Explicit A4 pgSz. Expected 1 page.",
            bytes: build_table_placement(),
        },
        Fixture {
            name: "exact_row_table.docx",
            description: "Issue #169 <w:trHeight w:hRule> oracle: a 2-column bordered table (exact 14pt line pitch, zero cell top/bottom margins) whose second row is hRule=exact 840 twips (42pt) holding 6 lines in cell A — the row stays 42pt and lines 4-6 are clipped (not painted; our PDF drops them from the content stream too); the third row carries the same 6 lines under hRule=atLeast 840 and grows to 84pt. Explicit A4 pgSz. Expected 1 page, rows 14/42/84/14pt.",
            bytes: build_exact_row_table(),
        },
    ]
}

/// `--gen-table-fixtures [dir]` — the table pagination corpus.
pub fn generate_tables(dir: &Path) -> Result<()> {
    write_set(
        dir,
        &table_fixtures(),
        "Issue #155 table pagination differential-oracle fixture corpus. word_pdf is PENDING for every entry: no Word 365 available in this environment (see tools/differential/README.md).",
    )
}

/* ---------------------------------------------------------------- */
/* Manifest + entry point                                            */
/* ---------------------------------------------------------------- */

struct Fixture {
    name: &'static str,
    description: &'static str,
    bytes: Vec<u8>,
}

fn fixtures() -> Vec<Fixture> {
    vec![
        Fixture {
            name: "justified_simple.docx",
            description: "Two justified (jc=both), RTL-bidi Arabic paragraphs, each wrapping across several lines.",
            bytes: build_justified_simple(),
        },
        Fixture {
            name: "kashida_justified.docx",
            description: "Dense, low-space-ratio justified Arabic paragraphs meant to push the justify pass toward Kashida elongation rather than pure space-stretch.",
            bytes: build_kashida_justified(),
        },
        Fixture {
            name: "mixed_direction.docx",
            description: "One RTL-bidi paragraph mixing Arabic prose with embedded Latin words/identifiers and digits (UAX #9 mixed-direction reordering).",
            bytes: build_mixed_direction(),
        },
        Fixture {
            name: "rtl_bullet_list.docx",
            description: "RTL (bidi) bulleted list — one abstractNum bullet definition, three list-item paragraphs with numPr.",
            bytes: build_rtl_bullet_list(),
        },
        Fixture {
            name: "rtl_numbered_list.docx",
            description: "RTL (bidi) decimal-numbered list with one nested level-1 item, mirroring format-docx's list_bullet_numbered.docx fixture but RTL.",
            bytes: build_rtl_numbered_list(),
        },
        Fixture {
            name: "rtl_table.docx",
            description: "A <w:tblPr><w:bidiVisual/> RTL table: 3 columns x 3 rows (header + 2 data rows), every cell paragraph bidi.",
            bytes: build_rtl_table(),
        },
        Fixture {
            name: "long_document.docx",
            description: "24 justified RTL Arabic paragraphs — a multi-page stress fixture for the page-count and per-paragraph line-break oracle.",
            bytes: build_long_document(),
        },
    ]
}

pub fn generate(dir: &Path) -> Result<()> {
    write_set(
        dir,
        &fixtures(),
        "Issue #89 Arabic/RTL differential-oracle fixture corpus. word_pdf is PENDING for every entry: no Word 365 available in this environment (see tools/differential/README.md).",
    )
}

/// Write `items` plus a `_manifest.json` (with `comment`) into `dir`.
fn write_set(dir: &Path, items: &[Fixture], comment: &str) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("mkdir {}", dir.display()))?;
    let mut manifest = String::new();
    manifest.push_str(&format!(
        "{{\n  \"_comment\": \"{}\",\n  \"fixtures\": {{\n",
        comment.replace('"', "'")
    ));
    let last = items.len() - 1;
    for (i, fx) in items.iter().enumerate() {
        let path = dir.join(fx.name);
        std::fs::write(&path, &fx.bytes).with_context(|| format!("write {}", path.display()))?;
        println!(
            "[gen-fixtures] wrote {} ({} bytes)",
            path.display(),
            fx.bytes.len()
        );
        manifest.push_str(&format!(
            "    \"{}\": {{\n      \"description\": \"{}\",\n      \"generator\": \"differential-native --gen-fixtures (hand-written OOXML)\",\n      \"word_pdf\": null,\n      \"word_pdf_status\": \"PENDING\"\n    }}{}\n",
            fx.name,
            fx.description.replace('"', "'"),
            if i == last { "" } else { "," }
        ));
    }
    manifest.push_str("  }\n}\n");
    let manifest_path = dir.join("_manifest.json");
    std::fs::write(&manifest_path, &manifest)
        .with_context(|| format!("write {}", manifest_path.display()))?;
    println!("[gen-fixtures] wrote {}", manifest_path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use format_docx::read_docx;

    /// Every generated fixture must be a well-formed `.docx` — `read_docx`
    /// parses it back without error and finds at least one paragraph.
    /// Idempotency (re-running `generate` produces byte-identical output)
    /// is exercised by the `--gen-fixtures` CLI path being deterministic
    /// (no timestamps, no random ids in any builder above).
    #[test]
    fn every_fixture_round_trips_through_read_docx() {
        for fx in fixtures().into_iter().chain(table_fixtures()) {
            let archive = read_docx(&fx.bytes)
                .unwrap_or_else(|e| panic!("{} failed to parse: {e:?}", fx.name));
            assert!(
                archive.document.paragraph_count() > 0,
                "{} has no paragraphs",
                fx.name
            );
        }
    }

    #[test]
    fn generate_writes_every_fixture_plus_manifest() {
        let dir = std::env::temp_dir().join(format!(
            "differential-native-fixtures-test-{}",
            std::process::id()
        ));
        generate(&dir).expect("generate");
        for fx in fixtures() {
            assert!(dir.join(fx.name).exists(), "{} missing", fx.name);
        }
        assert!(dir.join("_manifest.json").exists());
        let _ = std::fs::remove_dir_all(&dir);

        let dir = dir.with_extension("tables");
        generate_tables(&dir).expect("generate tables");
        for fx in table_fixtures() {
            assert!(dir.join(fx.name).exists(), "{} missing", fx.name);
        }
        assert!(dir.join("_manifest.json").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Issue #173 — our side of the `table_placement.docx` oracle: each
    /// table's x-origin in the column (the manifest's promised edges),
    /// and the bidiVisual table's cell A rightmost.
    #[test]
    fn table_placement_fixture_positions_every_table() {
        use layout::LayoutBlock;
        let bytes = include_bytes!("../../../ts/fonts/LiberationSans-Regular.ttf").to_vec();
        let face =
            text_pipeline::LoadedFont::parse("liberation".into(), bytes).expect("parse font");
        let mut faces = std::collections::HashMap::new();
        faces.insert("liberation".to_string(), std::sync::Arc::new(face));
        let fonts = text_pipeline::FontStack::from_faces(faces, "liberation");
        let mut doc = read_docx(&build_table_placement()).expect("parse").document;
        let built = crate::pipeline::build_pages(&mut doc, &fonts);
        assert_eq!(built.pages.len(), 1);
        let page = &built.pages[0];
        let cw = page.size.width - page.margins.left - page.margins.right;
        let tables: Vec<&layout::TableBox> = page
            .blocks
            .iter()
            .filter_map(LayoutBlock::as_table)
            .collect();
        assert_eq!(tables.len(), 4);
        let near = |got: f32, want: f32| assert!((got - want).abs() < 0.01, "{got} vs {want}");
        for t in &tables {
            near(t.size.width, 200.0);
        }
        near(tables[0].origin.x, (cw - 200.0) / 2.0);
        near(tables[1].origin.x, cw - 200.0);
        near(tables[2].origin.x, 36.0);
        near(tables[3].origin.x, cw - 200.0);
        let rtl = &tables[3].rows[0].cells;
        assert!(rtl[0].origin.x > rtl[1].origin.x, "cell A rightmost");
        /* The paragraphs between them stay at the column edge. */
        for b in &page.blocks {
            if let LayoutBlock::Paragraph(p) = b {
                assert_eq!(p.origin.x, 0.0);
            }
        }
    }

    /// Issue #169 — our side of the `exact_row_table.docx` oracle: the
    /// exact row keeps its declared 42pt (content clipped at paint time),
    /// the atLeast row grows to its 84pt of content.
    #[test]
    fn exact_row_table_fixes_the_exact_row_height() {
        use layout::LayoutBlock;
        let bytes = include_bytes!("../../../ts/fonts/LiberationSans-Regular.ttf").to_vec();
        let face =
            text_pipeline::LoadedFont::parse("liberation".into(), bytes).expect("parse font");
        let mut faces = std::collections::HashMap::new();
        faces.insert("liberation".to_string(), std::sync::Arc::new(face));
        let fonts = text_pipeline::FontStack::from_faces(faces, "liberation");
        let mut doc = read_docx(&build_exact_row_table()).expect("parse").document;
        let built = crate::pipeline::build_pages(&mut doc, &fonts);
        assert_eq!(built.pages.len(), 1);
        let tables: Vec<&layout::TableBox> = built.pages[0]
            .blocks
            .iter()
            .filter_map(LayoutBlock::as_table)
            .collect();
        assert_eq!(tables.len(), 1);
        let heights: Vec<f32> = tables[0].rows.iter().map(|r| r.size.height).collect();
        let near = |got: f32, want: f32| assert!((got - want).abs() < 0.01, "{got} vs {want}");
        near(heights[1], 42.0);
        near(heights[2], 84.0);
        let exact: Vec<bool> = tables[0].rows.iter().map(|r| r.exact_height).collect();
        assert_eq!(exact, vec![false, true, false, false]);
        assert!(
            tables[0].rows[1].cant_split,
            "exact rows never split (#155)"
        );
    }

    /// Issue #155 — our side of the `long_cell_table.docx` oracle: the
    /// page distribution the fixture's docs promise (LibreOffice is
    /// expected to agree; `tools/differential` checks it in CI).
    #[test]
    fn long_cell_table_breaks_the_row_at_a_line_boundary() {
        use layout::LayoutBlock;
        let bytes = include_bytes!("../../../ts/fonts/LiberationSans-Regular.ttf").to_vec();
        let face =
            text_pipeline::LoadedFont::parse("liberation".into(), bytes).expect("parse font");
        let mut faces = std::collections::HashMap::new();
        faces.insert("liberation".to_string(), std::sync::Arc::new(face));
        let fonts = text_pipeline::FontStack::from_faces(faces, "liberation");
        let mut doc = read_docx(&build_long_cell_table()).expect("parse").document;
        let built = crate::pipeline::build_pages(&mut doc, &fonts);
        assert_eq!(built.pages.len(), 3);
        let lines = |row: &layout::TableRowBox, cell: usize| -> usize {
            row.cells[cell]
                .content
                .iter()
                .filter_map(LayoutBlock::as_paragraph)
                .count()
        };
        let tables = |p: usize| -> Vec<&layout::TableBox> {
            built.pages[p]
                .blocks
                .iter()
                .filter_map(LayoutBlock::as_table)
                .collect()
        };
        /* Page 1: T1's first row + 4 lines of the tall row. */
        let t1 = tables(0);
        assert_eq!(t1.len(), 1);
        assert_eq!(t1[0].rows.len(), 2);
        assert_eq!(lines(&t1[0].rows[1], 0), 4);
        assert_eq!(lines(&t1[0].rows[1], 1), 1);
        /* Page 2: the 16 remaining lines + the last row; T2's first row
        (its cantSplit row does not fit the ~25.9pt left). */
        let p2 = tables(1);
        assert_eq!(p2.len(), 2);
        assert_eq!(p2[0].rows[0].source_row, 1);
        assert_eq!(lines(&p2[0].rows[0], 0), 16);
        assert_eq!(p2[1].rows.len(), 1);
        /* Page 3: T2's cantSplit row, whole. */
        let p3 = tables(2);
        assert_eq!(p3[0].rows[0].source_row, 1);
        assert_eq!(lines(&p3[0].rows[0], 0), 20);
    }
}
