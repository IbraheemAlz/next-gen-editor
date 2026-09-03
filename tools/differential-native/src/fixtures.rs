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
    std::fs::create_dir_all(dir).with_context(|| format!("mkdir {}", dir.display()))?;
    let mut manifest = String::new();
    manifest.push_str("{\n  \"_comment\": \"Issue #89 Arabic/RTL differential-oracle fixture corpus. word_pdf is PENDING for every entry: no Word 365 available in this environment (see tools/differential/README.md).\",\n  \"fixtures\": {\n");
    let items = fixtures();
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
        for fx in fixtures() {
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
    }
}
