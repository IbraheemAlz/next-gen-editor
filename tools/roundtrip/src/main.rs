//! Round-trip harness.
//!
//! Three modes:
//!
//! - **default** (no args): the classic Phase 1 Arabic-seed exit-gate test.
//!   Builds a minimal `.docx` from a seed, edits, saves, asserts the writer
//!   preserved sibling entries verbatim and the `document.xml` drift is
//!   bounded by `2 × |inserted_text_bytes|`. Kept verbatim — this is what
//!   the CI gate has run since Phase 1 weeks 19-24.
//!
//! - **`--fixtures [dir]`**: walks `crates/format-docx/tests/fixtures/`
//!   (or the supplied dir), looks up each `.docx` in `_manifest.json`, and:
//!   1. parses it via `read_docx`,
//!   2. validates the manifest's `asserts` (paragraph count + texts),
//!   3. re-emits via `write_docx`,
//!   4. asserts sibling entries are byte-identical,
//!   5. asserts `document.xml` drift ≤ `roundtrip.document_xml_drift_bytes`
//!      (default 0 — Phase 1 fixtures are self-built so the writer is
//!      byte-stable; Phase 3's passthrough optimisation will preserve
//!      this bound for Word-generated fixtures too).
//!
//! - **`--gen-seed [dir]`**: materialises the seed corpus + `_manifest.json`
//!   into the target dir. Idempotent; commit the output.
//!   - Phase 1: plain-text + Arabic + XML-escape fixtures.
//!   - Phase 2: `pPr_jc_center.docx`, `pPr_ind_firstline.docx`,
//!     `pPr_spacing.docx`, `pPr_bidi_rtl.docx` — handcrafted via our own
//!     writer (the Word-authored ground-truth fixtures aren't in the tree
//!     at Phase 2 cut; Phase 3 will replace them with real Word output).
//!
//! Exit 0 on PASS, non-zero on FAIL.

mod inline_spans;
mod revisions;
mod table_markup;

use anyhow::{Context, Result, bail};
use engine::{Alignment, DocumentTree, Indent, ParaProperties, Paragraph, Spacing, TextDirection};
use format_docx::writer::build_minimal_docx;
use format_docx::{DocxArchive, read_docx, write_docx};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const DEFAULT_FIXTURES_DIR: &str = "crates/format-docx/tests/fixtures";
const MANIFEST_NAME: &str = "_manifest.json";

const SEED_TEXT: &str = "السلام عليكم ورحمة الله وبركاته";
const INSERT_TEXT: &str = " تم التعديل";

/* ==================================================== pgSz pinning (#109) ==== */

/// Issue #109 — `Word.exe` always stamps `<w:pgSz>`/`<w:pgMar>` explicitly
/// on every `<w:sectPr>` it writes. Every handcrafted / seed fixture here
/// used to rely on the bare `<w:sectPr/>` short-hand instead, which meant
/// `--fixtures` could never prove the reader's `<w:sectPr>`-omits-`<w:pgSz>`
/// fallback (`format_docx::parts::document::SectPrAccum::into_geometry`)
/// wasn't silently carrying every single committed fixture. Every fixture
/// this module can safely touch now pins this exact A4 sectPr into its
/// SOURCE bytes — byte-for-byte what `format_docx::writer::emit_sect_pr`
/// itself would write for stock A4 geometry, so it is not a semantic
/// change. See `ppr_fixtures` for the one exception (`pPr_bidi_rtl.docx`)
/// and `run_gen_seed` / `prebuilt_fixtures` for the fixtures this module
/// deliberately leaves alone because their generator functions are SHARED
/// with a `run_default()` exact-byte-equality assertion that compares a
/// resave against the pinned SOURCE text (`grab_bag_exotic.docx`,
/// `floating_image_anchor.docx`, `footnotes_endnotes.docx`,
/// `table_cell_runs.docx`, `image_wrap_modes.docx`, `toc_word_shape.docx`,
/// `text_boxes_wrap.docx`)
/// — the writer's
/// trailing-sectPr compaction (`sect_pr_compaction_delta`) would desync
/// those comparisons. `w14_paraid_word.docx` is the one exception THAT
/// IS pinned despite sharing a generator with a `run_default()` step:
/// its assertion compares two fresh resaves of the same edited tree
/// against EACH OTHER, not against the pinned source, so both sides
/// compact identically and the comparison still holds.
const A4_SECT_PR_EXPLICIT: &str = concat!(
    "<w:sectPr>",
    r#"<w:pgSz w:w="11906" w:h="16838"/>"#,
    r#"<w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440" w:header="720" w:footer="720"/>"#,
    "</w:sectPr>",
);
const BARE_SECT_PR: &str = "<w:sectPr/>";

/// `write_docx`'s trailing-sectPr emission (`geometry_is_stock_a4`) is a
/// deliberate byte-stability optimisation: ANY untouched stock-A4 document
/// re-saves with the bare `<w:sectPr/>` footer, regardless of what its
/// SOURCE bytes said. So a no-op `--fixtures` resave of a pinned fixture
/// compacts `A4_SECT_PR_EXPLICIT` straight back to `BARE_SECT_PR` — this is
/// that known, bounded, one-time delta, not corruption. Every pinned
/// fixture's manifest entry carries this as `document_xml_drift_bytes`
/// (plus any other fixture-specific drift already accounted for, e.g.
/// `bom_utf8_passthrough.docx`'s BOM-removal bytes).
fn sect_pr_compaction_delta() -> usize {
    A4_SECT_PR_EXPLICIT.len() - BARE_SECT_PR.len()
}

/// Splice [`A4_SECT_PR_EXPLICIT`] into a `build_minimal_docx` zip's
/// `word/document.xml`, in place of the bare `<w:sectPr/>` footer the
/// writer emits for a freshly-built stock-A4 `DocumentTree`. Re-zips the
/// archive with the same entries, in the same order, at the same
/// compression — only `word/document.xml`'s bytes change.
fn pin_explicit_a4_sect_pr(bytes: &[u8]) -> Vec<u8> {
    use std::io::{Read, Write};
    use zip::write::{SimpleFileOptions, ZipWriter};

    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).expect("read seed zip");
    let mut entries: Vec<(String, Vec<u8>)> = Vec::with_capacity(archive.len());
    for i in 0..archive.len() {
        let mut file = archive.by_index(i).expect("zip entry");
        let name = file.name().to_owned();
        let mut buf = Vec::new();
        file.read_to_end(&mut buf).expect("read entry");
        entries.push((name, buf));
    }
    let mut patched = false;
    for (name, buf) in &mut entries {
        if name == "word/document.xml" {
            let xml = std::str::from_utf8(buf).expect("utf8 document.xml");
            assert!(
                xml.contains(BARE_SECT_PR),
                "expected build_minimal_docx to emit the bare <w:sectPr/> footer, got:\n{xml}"
            );
            *buf = xml
                .replacen(BARE_SECT_PR, A4_SECT_PR_EXPLICIT, 1)
                .into_bytes();
            patched = true;
        }
    }
    assert!(patched, "seed zip has no word/document.xml to patch");

    let mut out: Vec<u8> = Vec::new();
    {
        let mut zip = ZipWriter::new(std::io::Cursor::new(&mut out));
        let opts = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(0o644);
        for (name, buf) in &entries {
            zip.start_file(name.as_str(), opts).expect("start entry");
            zip.write_all(buf).expect("write entry");
        }
        zip.finish().expect("finish zip");
    }
    out
}

/* ============================================================ default ==== */

fn run_default() -> Result<()> {
    let seed_doc = DocumentTree::from_text(SEED_TEXT);
    let fixture_bytes = build_minimal_docx(&seed_doc).context("build fixture")?;
    println!("[roundtrip] fixture .docx: {} bytes", fixture_bytes.len());

    let archive_a = read_docx(&fixture_bytes).context("read fixture")?;
    if archive_a.document.paragraph_count() != 1 {
        bail!(
            "fixture has wrong paragraph count: {}",
            archive_a.document.paragraph_count()
        );
    }
    if archive_a.document.paragraph_text(0) != Some(SEED_TEXT) {
        bail!(
            "fixture text mismatch — expected `{}`, got `{:?}`",
            SEED_TEXT,
            archive_a.document.paragraph_text(0)
        );
    }
    println!("[roundtrip] step 2 OK — fixture parses back to seed");

    let end = archive_a.document.end_of_document();
    let edited = archive_a.document.insert_text(end, INSERT_TEXT);
    let expected_combined = format!("{SEED_TEXT}{INSERT_TEXT}");
    if edited.paragraph_text(0) != Some(expected_combined.as_str()) {
        bail!(
            "in-memory insert wrong — expected `{}`, got `{:?}`",
            expected_combined,
            edited.paragraph_text(0)
        );
    }
    println!("[roundtrip] step 3 OK — in-memory edit reflected");

    let edited_bytes = write_docx(&archive_a, &edited).context("write edited")?;
    assert_document_xml_well_formed(&edited_bytes).context("edited .docx")?;
    println!(
        "[roundtrip] saved edited .docx: {} bytes",
        edited_bytes.len()
    );

    let archive_b = read_docx(&edited_bytes).context("re-read edited")?;
    if archive_b.document.paragraph_text(0) != Some(expected_combined.as_str()) {
        bail!(
            "saved .docx didn't preserve edit — expected `{}`, got `{:?}`",
            expected_combined,
            archive_b.document.paragraph_text(0)
        );
    }
    println!("[roundtrip] step 5 OK — saved .docx parses back to edited tree");

    let mut sibling_drift = 0_usize;
    for (name, bytes) in &archive_a.other_entries {
        let b = archive_b
            .other_entries
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, b)| b);
        match b {
            Some(b) if b == bytes => {
                println!("  [SAME] {name} ({} B)", bytes.len());
            }
            Some(b) => {
                sibling_drift += b.len().abs_diff(bytes.len());
                println!("  [DRIFT] {name}: {} -> {} bytes", bytes.len(), b.len());
            }
            None => {
                bail!("entry `{name}` missing from saved archive");
            }
        }
    }
    if sibling_drift != 0 {
        bail!(
            "{sibling_drift} bytes of sibling-entry drift — writer should preserve them verbatim"
        );
    }
    println!("[roundtrip] step 6a OK — all sibling entries byte-identical");

    let doc_a = extract_doc_xml(&fixture_bytes)?;
    let doc_b = extract_doc_xml(&edited_bytes)?;
    let doc_diff = (doc_b.len() as isize - doc_a.len() as isize).unsigned_abs();
    let insert_len_utf8 = INSERT_TEXT.len();
    println!(
        "[roundtrip] document.xml: {} -> {} bytes (Δ {} B, insert text {} B)",
        doc_a.len(),
        doc_b.len(),
        doc_diff,
        insert_len_utf8
    );
    /* Issue #251 — the PRIMARY bound: the edited save must not rewrite any
    ORIGINAL byte. The old size-only `≤ 2×N` check (kept below as an
    informational bound) cannot tell a faithful insertion that needed its
    own new `<w:r>` from a lossy regeneration that happens to balance out
    to the same size — see issue #199's 82 -> 92 false regression. */
    let (rewrite_start, source_bytes_rewritten, _edited_region_bytes) =
        rewritten_region(doc_a.as_slice(), doc_b.as_slice());
    if source_bytes_rewritten > 0 {
        bail!(
            "document.xml rewrote {source_bytes_rewritten} B of the ORIGINAL part at offset \
             {rewrite_start} — a faithful edit must not touch source bytes (issue #251)"
        );
    }
    println!("[roundtrip] step 6b OK — no original document.xml bytes rewritten (issue #251)");

    /* Secondary, informational-turned-advisory size bound: 2×N plus an
    allowance for any run(s) the faithful insertion had to create (see
    `tools/corpus-native/src/pipeline.rs`'s `NEW_RUN_ALLOWANCE_BYTES` for
    why 48 B/run). The bare `2×N` number is still asserted here since this
    fixture's insert always lands in a plain single-run paragraph — a
    violation here would mean the writer grew the save for no run-creation
    reason, which is exactly the size-based smell the old bound was meant
    to catch. */
    let new_runs =
        count_run_open_tags(doc_b.as_slice()).saturating_sub(count_run_open_tags(doc_a.as_slice()));
    let new_run_allowance = new_runs * NEW_RUN_ALLOWANCE_BYTES;
    let bound = insert_len_utf8 * 2 + new_run_allowance;
    if doc_diff > bound {
        bail!(
            "document.xml diff {doc_diff} B exceeds secondary bound {bound} B (insert \
             {insert_len_utf8} B × 2 + {new_runs} new run(s) × {NEW_RUN_ALLOWANCE_BYTES} B)"
        );
    }
    println!("[roundtrip] step 6c OK — document.xml diff within the secondary size bound");

    /* Sprint 9 — exercise the non-OOXML emitters too. The `DocumentTree`
    that came back from the edited round-trip is the freshest view of
    the model — feed it to `format_html::to_html` + `to_plain_text`
    and assert each emits a non-empty payload containing the seed +
    inserted-text bytes. Guards against regressions in the
    `Command::SaveDocument { Html | PlainText }` engine surface. */
    let html = format_html::to_html(&edited);
    if !html.starts_with("<!DOCTYPE html>") {
        bail!(
            "HTML emit missing doctype prefix: {}",
            &html[..80.min(html.len())]
        );
    }
    if !html.contains(SEED_TEXT) {
        bail!("HTML emit dropped seed text");
    }
    if !html.contains(INSERT_TEXT.trim_start()) {
        bail!("HTML emit dropped inserted text");
    }
    println!(
        "[roundtrip] step 7 OK — format_html::to_html emitted {} bytes",
        html.len()
    );

    let plain = edited.to_plain_text();
    let expected_plain = format!("{SEED_TEXT}{INSERT_TEXT}");
    if plain != expected_plain {
        bail!(
            "to_plain_text mismatch — expected `{}`, got `{}`",
            expected_plain,
            plain
        );
    }
    println!(
        "[roundtrip] step 8 OK — to_plain_text emitted {} bytes",
        plain.len()
    );

    run_grab_bag_survival()?;
    run_floating_anchor_survival()?;
    run_notes_roundtrip()?;
    run_ui_save_root_bindings()?;
    run_table_cell_runs_survival()?;
    run_wrap_modes_roundtrip()?;
    run_toc_roundtrip()?;
    run_text_boxes_roundtrip()?;
    run_nested_text_boxes_roundtrip()?;
    run_text_box_pictures_roundtrip()?;
    run_rtl_table_roundtrip()?;
    run_table_jc_tblind_roundtrip()?;
    run_body_passthrough_roundtrip()?;
    run_package_media_insert()?;
    run_package_ui_save()?;
    run_style_bidi_roundtrip()?;
    run_part_scoped_media_roundtrip()?;
    run_source_markup_roundtrip()?;
    inline_spans::run_form_fields_roundtrip()?;
    inline_spans::run_content_controls_roundtrip()?;
    inline_spans::run_field_source_form_roundtrip()?;
    table_markup::run_table_markup_roundtrip()?;
    revisions::run_tracked_moves_roundtrip()?;
    revisions::run_paragraph_mark_revisions_roundtrip()?;
    run_hyperlink_identity_roundtrip()?;
    run_comment_anchor_roundtrip()?;

    println!("\nPASS");
    Ok(())
}

/* ===================================================== grab bags (#84) ==== */

/// Issue #84 — the `w14` (Word 2010 extensions) namespace the exotic
/// fixture declares on its root and uses for one `<w:rPr>` child.
const W14_NS: &str = "http://schemas.microsoft.com/office/word/2010/wordml";

/// Exotic `<w:rPr>` child in a foreign namespace, exactly as it sits in
/// the fixture (prefix bound on the ROOT element, like Word writes it).
/// The writer re-declares the source root's bindings on its synthesized
/// root (`DocxArchive::document_root_attrs`), so the fragment itself is
/// preserved byte-for-byte like every `w:` one.
const GLOW_SRC: &str = r#"<w14:glow w14:rad="63500"><w14:srgbClr w14:val="FFC000"/></w14:glow>"#;

/// Every unmodeled child the fixture plants, byte-for-byte as the writer
/// must re-emit it inside a REGENERATED paragraph / table.
fn exotic_fragments() -> Vec<String> {
    vec![
        /* `<w:pPr>` children (+ the whole paragraph-mark `<w:rPr>`). */
        r#"<w:framePr w:w="2880" w:hAnchor="margin" w:xAlign="right"/>"#.into(),
        r#"<w:widowControl w:val="false"/>"#.into(),
        "<w:suppressAutoHyphens/>".into(),
        r#"<w:outlineLvl w:val="2"/>"#.into(),
        r#"<w:cnfStyle w:val="000000100000"/><w:rPr><w:lang w:val="ar-SA" w:bidi="ar-SA"/></w:rPr></w:pPr>"#.into(),
        /* `<w:rPr>` children. */
        "<w:noProof/>".into(),
        r#"<w:kern w:val="28"/>"#.into(),
        r#"<w:fitText w:val="1440" w:id="7"/>"#.into(),
        r#"<w:lang w:val="en-GB"/>"#.into(),
        r#"<w:eastAsianLayout w:id="1" w:combine="1"/>"#.into(),
        GLOW_SRC.into(),
        /* `<w:tblPr>` / `<w:trPr>` / `<w:tcPr>` children. */
        "<w:bidiVisual/>".into(),
        r#"<w:tblLook w:val="04A0" w:firstRow="1" w:lastRow="0" w:firstColumn="1" w:lastColumn="0" w:noHBand="0" w:noVBand="1"/>"#.into(),
        r#"<w:cnfStyle w:val="100000000000"/>"#.into(),
        r#"<w:jc w:val="center"/></w:trPr>"#.into(),
        "<w:noWrap/>".into(),
        r#"<w:textDirection w:val="btLr"/>"#.into(),
        "<w:hideMark/>".into(),
    ]
}

/// `word/document.xml` of the exotic fixture. Authored in the writer's
/// own canonical shape (schema-ordered children, `xml:space="preserve"`,
/// no pretty-printing) so a regenerated paragraph / table is
/// byte-identical to its source except for the edit itself — which is
/// what lets the step assert an EXACT expected output rather than just
/// containment.
fn grab_bag_exotic_document_xml() -> String {
    format!(
        concat!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
            "\n",
            r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:w14="{w14}">"#,
            "<w:body>",
            "<w:p><w:pPr><w:keepNext/>",
            r#"<w:framePr w:w="2880" w:hAnchor="margin" w:xAlign="right"/>"#,
            r#"<w:widowControl w:val="false"/>"#,
            "<w:suppressAutoHyphens/>",
            r#"<w:jc w:val="center"/>"#,
            r#"<w:outlineLvl w:val="2"/>"#,
            r#"<w:cnfStyle w:val="000000100000"/>"#,
            r#"<w:rPr><w:lang w:val="ar-SA" w:bidi="ar-SA"/></w:rPr>"#,
            "</w:pPr>",
            "<w:r><w:rPr><w:b/><w:noProof/>",
            r#"<w:kern w:val="28"/>"#,
            r#"<w:fitText w:val="1440" w:id="7"/>"#,
            r#"<w:lang w:val="en-GB"/>"#,
            r#"<w:eastAsianLayout w:id="1" w:combine="1"/>"#,
            "{glow}",
            "</w:rPr>",
            r#"<w:t xml:space="preserve">exotic run</w:t></w:r></w:p>"#,
            "<w:tbl><w:tblPr>",
            r#"<w:tblStyle w:val="TableGrid"/>"#,
            "<w:bidiVisual/>",
            r#"<w:tblW w:w="0" w:type="auto"/>"#,
            r#"<w:jc w:val="center"/>"#,
            r#"<w:tblLook w:val="04A0" w:firstRow="1" w:lastRow="0" w:firstColumn="1" w:lastColumn="0" w:noHBand="0" w:noVBand="1"/>"#,
            "</w:tblPr>",
            r#"<w:tblGrid><w:gridCol w:w="2880"/><w:gridCol w:w="2880"/></w:tblGrid>"#,
            "<w:tr><w:trPr>",
            r#"<w:cnfStyle w:val="100000000000"/>"#,
            "<w:cantSplit/>",
            r#"<w:trHeight w:val="400" w:hRule="atLeast"/>"#,
            r#"<w:jc w:val="center"/>"#,
            "</w:trPr>",
            "<w:tc><w:tcPr>",
            r#"<w:tcW w:w="2880" w:type="dxa"/>"#,
            "<w:noWrap/>",
            r#"<w:textDirection w:val="btLr"/>"#,
            r#"<w:vAlign w:val="center"/>"#,
            "<w:hideMark/>",
            "</w:tcPr>",
            r#"<w:p><w:r><w:t xml:space="preserve">cell</w:t></w:r></w:p></w:tc>"#,
            r#"<w:tc><w:p><w:r><w:t xml:space="preserve">other</w:t></w:r></w:p></w:tc>"#,
            "</w:tr></w:tbl>",
            r#"<w:p><w:r><w:t xml:space="preserve">after</w:t></w:r></w:p>"#,
            "<w:sectPr/></w:body></w:document>",
        ),
        w14 = W14_NS,
        glow = GLOW_SRC,
    )
}

/// Issue #84 fixture: one paragraph, one 1×2 table and one trailing
/// paragraph, every property container seeded with children the model
/// does not express. Rides the `--fixtures` passthrough at drift 0 and
/// the default harness's dirty-regeneration step.
fn build_grab_bag_exotic_docx() -> Vec<u8> {
    use std::io::Write;
    use zip::write::{SimpleFileOptions, ZipWriter};
    let document_xml = grab_bag_exotic_document_xml();
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#;
    let dot_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
</Relationships>"#;
    let mut buf: Vec<u8> = Vec::new();
    {
        let mut zip = ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(0o644);
        for (name, body) in [
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", dot_rels),
            ("word/_rels/document.xml.rels", doc_rels),
            ("word/document.xml", document_xml.as_str()),
        ] {
            zip.start_file(name, opts).unwrap();
            zip.write_all(body.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    buf
}

/// Issue #84 — step 9: edit a paragraph AND a table cell whose property
/// containers carry unmodeled children, save, and require that the
/// regenerated `document.xml` is EXACTLY the source with the two edits
/// applied. Every exotic child must therefore survive byte-for-byte and
/// in schema order, the root keeps its `xmlns:w14` binding, and the
/// drift is exactly the inserted text — the bags add zero bytes.
fn run_grab_bag_survival() -> Result<()> {
    use engine::{BlockPath, LogicalPos, PathStep};

    let fixture_bytes = build_grab_bag_exotic_docx();
    let archive_a = read_docx(&fixture_bytes).context("read exotic fixture")?;
    if archive_a.document.paragraph_text(0) != Some("exotic run") {
        bail!(
            "exotic fixture text mismatch: {:?}",
            archive_a.document.paragraph_text(0)
        );
    }

    /* Paragraph edit strictly INSIDE the styled span so the span grows
    and the paragraph still serializes as a single `<w:r>`. */
    let para_pos = LogicalPos {
        path: BlockPath::top(0),
        offset: 3,
    };
    /* Cell edit: appends to the first cell's only paragraph, which
    dirties the containing table (full `<w:tbl>` regeneration). */
    let cell_pos = LogicalPos {
        path: BlockPath::top(1)
            .push(PathStep::Cell { row: 0, col: 0 })
            .push(PathStep::Block(0)),
        offset: "cell".len() as u32,
    };
    let edited = archive_a
        .document
        .insert_text(para_pos, INSERT_TEXT)
        .insert_text(cell_pos, INSERT_TEXT);
    let expected_para = format!("exo{INSERT_TEXT}tic run");
    if edited.paragraph_text(0) != Some(expected_para.as_str()) {
        bail!(
            "in-memory paragraph edit wrong: {:?}",
            edited.paragraph_text(0)
        );
    }
    let edited_bytes = write_docx(&archive_a, &edited).context("write edited exotic")?;
    assert_document_xml_well_formed(&edited_bytes).context("edited exotic .docx")?;

    /* Siblings verbatim. */
    let archive_b = read_docx(&edited_bytes).context("re-read edited exotic")?;
    for (name, bytes) in &archive_a.other_entries {
        let same = archive_b
            .other_entries
            .iter()
            .any(|(n, b)| n == name && b == bytes);
        if !same {
            bail!("exotic fixture: sibling `{name}` drifted");
        }
    }

    /* Exact expected output: the source with the two inserts, nothing
    else — root bindings, every fragment and every modeled child byte-
    identical and in place. */
    let doc_a = String::from_utf8(extract_doc_xml(&fixture_bytes)?).context("utf8 source")?;
    let doc_b = String::from_utf8(extract_doc_xml(&edited_bytes)?).context("utf8 output")?;
    let expected = doc_a
        .replacen(
            r#"<w:t xml:space="preserve">exotic run</w:t>"#,
            &format!(r#"<w:t xml:space="preserve">{expected_para}</w:t>"#),
            1,
        )
        .replacen(
            r#"<w:t xml:space="preserve">cell</w:t>"#,
            &format!(r#"<w:t xml:space="preserve">cell{INSERT_TEXT}</w:t>"#),
            1,
        );
    for frag in exotic_fragments() {
        if !doc_b.contains(&frag) {
            bail!("exotic fragment lost or altered on regenerate: `{frag}`\n--- got ---\n{doc_b}");
        }
    }
    if doc_b != expected {
        bail!(
            "regenerated document.xml is not source + edits\n--- expected ---\n{expected}\n--- got ---\n{doc_b}"
        );
    }
    let drift = (doc_b.len() as isize - doc_a.len() as isize).unsigned_abs();
    let inserted = 2 * INSERT_TEXT.len();
    println!(
        "[roundtrip] exotic document.xml: {} -> {} bytes (Δ {} B, inserted {} B)",
        doc_a.len(),
        doc_b.len(),
        drift,
        inserted
    );
    if drift > 2 * inserted {
        bail!(
            "exotic document.xml drift {drift} B exceeds bound {} B",
            2 * inserted
        );
    }

    /* The bags themselves round-trip: a second read captures the same
    fragments the first did (the writer emitted them verbatim). */
    let bag_a = archive_a
        .document
        .blocks
        .iter()
        .filter_map(engine::Block::as_paragraph)
        .next()
        .and_then(|p| p.spans.first())
        .and_then(|s| s.style.grab_bag.clone());
    let bag_b = archive_b
        .document
        .blocks
        .iter()
        .filter_map(engine::Block::as_paragraph)
        .next()
        .and_then(|p| p.spans.first())
        .and_then(|s| s.style.grab_bag.clone());
    if bag_a.is_none() || bag_a != bag_b {
        bail!("run grab bag did not survive the round-trip: {bag_a:?} vs {bag_b:?}");
    }
    let ppr_a = archive_a
        .document
        .blocks
        .iter()
        .filter_map(engine::Block::as_paragraph)
        .next()
        .and_then(|p| p.props.grab_bag.clone());
    let ppr_b = archive_b
        .document
        .blocks
        .iter()
        .filter_map(engine::Block::as_paragraph)
        .next()
        .and_then(|p| p.props.grab_bag.clone());
    if ppr_a.is_none() || ppr_a != ppr_b {
        bail!("paragraph grab bag did not survive the round-trip: {ppr_a:?} vs {ppr_b:?}");
    }
    println!("[roundtrip] step 9 OK — grab bags survive dirty regeneration byte-for-byte");
    Ok(())
}

/* ============================================ floating anchors (#69) ==== */

/// Issue #69 — the `word/document.xml` of the floating-picture fixture,
/// authored in the writer's own canonical shape (Word's `CT_Anchor` child
/// order, compact, `xml:space="preserve"`) so a regenerated paragraph is
/// byte-identical to its source except for the edit itself. The anchor
/// exercises a fixed EMU offset (`<wp:posOffset>`), a Word-2010 percentage
/// offset (`<wp14:pctPosVOffset>`, root-bound `wp14`), an empty wrap
/// element and a `<wp:docPr>` with a `descr` — the two children that ride
/// the model verbatim.
fn floating_anchor_document_xml() -> String {
    concat!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
        "\n",
        r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" "#,
        r#"xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" "#,
        r#"xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" "#,
        r#"xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" "#,
        r#"xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture" "#,
        r#"xmlns:wp14="http://schemas.microsoft.com/office/word/2010/wordprocessingDrawing">"#,
        "<w:body>",
        r#"<w:p><w:r><w:t xml:space="preserve">before</w:t></w:r></w:p>"#,
        r#"<w:p><w:r><w:t xml:space="preserve">float </w:t></w:r>"#,
        "<w:r><w:drawing>",
        r#"<wp:anchor distT="0" distB="0" distL="114300" distR="114300" simplePos="0" "#,
        r#"relativeHeight="251659264" behindDoc="0" locked="0" layoutInCell="1" allowOverlap="1">"#,
        r#"<wp:simplePos x="0" y="0"/>"#,
        r#"<wp:positionH relativeFrom="column"><wp:posOffset>914400</wp:posOffset></wp:positionH>"#,
        r#"<wp:positionV relativeFrom="paragraph"><wp14:pctPosVOffset>25000</wp14:pctPosVOffset></wp:positionV>"#,
        r#"<wp:extent cx="914400" cy="457200"/>"#,
        r#"<wp:effectExtent l="0" t="0" r="0" b="0"/>"#,
        r#"<wp:wrapSquare wrapText="bothSides"/>"#,
        r#"<wp:docPr id="7" name="Picture 7" descr="floating fixture"/>"#,
        "<wp:cNvGraphicFramePr/>",
        "<a:graphic>",
        r#"<a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture">"#,
        "<pic:pic>",
        r#"<pic:nvPicPr><pic:cNvPr id="0" name="Image"/><pic:cNvPicPr/></pic:nvPicPr>"#,
        "<pic:blipFill>",
        r#"<a:blip r:embed="rId5"/>"#,
        "<a:stretch><a:fillRect/></a:stretch>",
        "</pic:blipFill>",
        "<pic:spPr>",
        r#"<a:xfrm><a:off x="0" y="0"/><a:ext cx="914400" cy="457200"/></a:xfrm>"#,
        r#"<a:prstGeom prst="rect"><a:avLst/></a:prstGeom>"#,
        "</pic:spPr>",
        "</pic:pic>",
        "</a:graphicData>",
        "</a:graphic>",
        "</wp:anchor></w:drawing></w:r>",
        r#"<w:r><w:t xml:space="preserve">here</w:t></w:r></w:p>"#,
        r#"<w:p><w:r><w:t xml:space="preserve">after</w:t></w:r></w:p>"#,
        "<w:sectPr/></w:body></w:document>",
    )
    .to_string()
}

/// Issue #69 fixture: three paragraphs, the middle one anchoring a
/// floating picture (`<wp:anchor>`) whose blob lives at
/// `word/media/image1.png` (the 8-byte PNG signature — the reader stores
/// bytes, decoding happens in the browser). Rides the `--fixtures`
/// passthrough at drift 0 and the default harness's step 10.
fn build_floating_image_anchor_docx() -> Vec<u8> {
    pack_docx_with_png(&floating_anchor_document_xml())
}

/// A minimal OPC package around `document_xml` with one image part
/// (`rId5` → `word/media/image1.png`, the 8-byte PNG signature).
fn pack_docx_with_png(document_xml: &str) -> Vec<u8> {
    pack_docx_with_png_bytes(
        document_xml,
        &[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a],
    )
}

/// [`pack_docx_with_png`] with the image part's bytes supplied.
fn pack_docx_with_png_bytes(document_xml: &str, png: &[u8]) -> Vec<u8> {
    use std::io::Write;
    use zip::write::{SimpleFileOptions, ZipWriter};
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Default Extension="png" ContentType="image/png"/>
<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#;
    let dot_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId5" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/image1.png"/>
</Relationships>"#;
    let mut buf: Vec<u8> = Vec::new();
    {
        let mut zip = ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(0o644);
        for (name, body) in [
            ("[Content_Types].xml", content_types.as_bytes()),
            ("_rels/.rels", dot_rels.as_bytes()),
            ("word/_rels/document.xml.rels", doc_rels.as_bytes()),
            ("word/document.xml", document_xml.as_bytes()),
            ("word/media/image1.png", png),
        ] {
            zip.start_file(name, opts).unwrap();
            zip.write_all(body).unwrap();
        }
        zip.finish().unwrap();
    }
    buf
}

/// Issue #69 — step 10: open the floating-picture fixture, check the
/// anchor lowered into the typed model, edit the anchor paragraph BEFORE
/// the sentinel (so the anchor byte shifts), save, and require that the
/// regenerated `document.xml` is EXACTLY the source with the insert
/// applied — the `<wp:anchor>` (attributes, both positioning axes, the
/// wp14 percentage element, the verbatim wrap + docPr) survives byte-for-
/// byte, and a second read still sees a floating picture at the shifted
/// offset. Then move the float and check the offsets land in the file.
fn run_floating_anchor_survival() -> Result<()> {
    use engine::{BlockPath, FloatOffset, HRelativeFrom, LogicalPos, VRelativeFrom, WrapKind};

    let fixture_bytes = build_floating_image_anchor_docx();
    let archive_a = read_docx(&fixture_bytes).context("read floating fixture")?;
    let doc_a_tree = &archive_a.document;
    if doc_a_tree.paragraph_text(1) != Some("float \u{FFFC}here") {
        bail!(
            "floating fixture text mismatch: {:?}",
            doc_a_tree.paragraph_text(1)
        );
    }
    let para = doc_a_tree
        .blocks
        .iter()
        .filter_map(engine::Block::as_paragraph)
        .nth(1)
        .context("anchor paragraph")?;
    let obj = para.inline_objects.first().context("one inline object")?;
    let anchor = obj
        .anchor
        .as_deref()
        .context("the picture must be floating")?;
    if obj.at != 6
        || anchor.position_h.relative_from != HRelativeFrom::Column
        || anchor.position_h.offset != FloatOffset::Emu(914_400)
        || anchor.position_v.relative_from != VRelativeFrom::Paragraph
        || anchor.position_v.offset != FloatOffset::PercentMilli(25_000)
        || anchor.wrap != WrapKind::Square
        || anchor.dist_left_emu != 114_300
        || anchor.relative_height != 251_659_264
    {
        bail!(
            "floating fixture anchor lowered wrongly: at={} {anchor:?}",
            obj.at
        );
    }
    if doc_a_tree.count_floating_images() != 1 {
        bail!("expected exactly one floating image");
    }

    /* Edit BEFORE the sentinel: "flo|at " — the anchor byte must shift. */
    let pos = LogicalPos {
        path: BlockPath::top(1),
        offset: 3,
    };
    let edited = doc_a_tree.insert_text(pos, INSERT_TEXT);
    let expected_para = format!("flo{INSERT_TEXT}at \u{FFFC}here");
    if edited.paragraph_text(1) != Some(expected_para.as_str()) {
        bail!("in-memory edit wrong: {:?}", edited.paragraph_text(1));
    }
    let edited_bytes = write_docx(&archive_a, &edited).context("write edited floating")?;
    assert_document_xml_well_formed(&edited_bytes).context("edited floating .docx")?;

    /* Siblings (rels, content types, the media blob) verbatim. */
    let archive_b = read_docx(&edited_bytes).context("re-read edited floating")?;
    for (name, bytes) in &archive_a.other_entries {
        let same = archive_b
            .other_entries
            .iter()
            .any(|(n, b)| n == name && b == bytes);
        if !same {
            bail!("floating fixture: sibling `{name}` drifted");
        }
    }

    /* Exact expected output: source + the insert, nothing else. */
    let doc_a = String::from_utf8(extract_doc_xml(&fixture_bytes)?).context("utf8 source")?;
    let doc_b = String::from_utf8(extract_doc_xml(&edited_bytes)?).context("utf8 output")?;
    let expected = doc_a.replacen(
        r#"<w:t xml:space="preserve">float </w:t>"#,
        &format!(r#"<w:t xml:space="preserve">flo{INSERT_TEXT}at </w:t>"#),
        1,
    );
    if doc_b != expected {
        bail!(
            "regenerated document.xml is not source + edit\n--- expected ---\n{expected}\n--- got ---\n{doc_b}"
        );
    }
    let drift = (doc_b.len() as isize - doc_a.len() as isize).unsigned_abs();
    println!(
        "[roundtrip] floating document.xml: {} -> {} bytes (Δ {} B, inserted {} B)",
        doc_a.len(),
        doc_b.len(),
        drift,
        INSERT_TEXT.len()
    );
    if drift > 2 * INSERT_TEXT.len() {
        bail!(
            "floating document.xml drift {drift} B exceeds bound {} B",
            2 * INSERT_TEXT.len()
        );
    }

    /* The re-read picture is still floating, at the shifted sentinel. */
    let para_b = archive_b
        .document
        .blocks
        .iter()
        .filter_map(engine::Block::as_paragraph)
        .nth(1)
        .context("anchor paragraph after edit")?;
    let obj_b = para_b.inline_objects.first().context("object survives")?;
    if obj_b.at != 6 + INSERT_TEXT.len() as u32 || obj_b.anchor.as_deref() != Some(anchor) {
        bail!(
            "anchor did not survive the edit: at={} {:?}",
            obj_b.at,
            obj_b.anchor
        );
    }

    /* A move writes fixed EMU offsets on both axes (the percentage
    placement is replaced) inside the SAME frames; simplePos stays off. */
    let moved = edited.move_floating_image_at(&BlockPath::top(1), obj_b.at, 1_828_800, 91_440);
    let moved_bytes = write_docx(&archive_a, &moved).context("write moved floating")?;
    let doc_c = String::from_utf8(extract_doc_xml(&moved_bytes)?).context("utf8 moved")?;
    let needle_h = r#"<wp:positionH relativeFrom="column"><wp:posOffset>1828800</wp:posOffset></wp:positionH>"#;
    let needle_v = r#"<wp:positionV relativeFrom="paragraph"><wp:posOffset>91440</wp:posOffset></wp:positionV>"#;
    if !doc_c.contains(needle_h) || !doc_c.contains(needle_v) || doc_c.contains("pctPosVOffset") {
        bail!("moved anchor offsets did not land in document.xml:\n{doc_c}");
    }
    let archive_c = read_docx(&moved_bytes).context("re-read moved floating")?;
    let anchor_c = archive_c
        .document
        .blocks
        .iter()
        .filter_map(engine::Block::as_paragraph)
        .nth(1)
        .and_then(|p| p.inline_objects.first())
        .and_then(|o| o.anchor.as_deref())
        .cloned()
        .context("moved picture still floating")?;
    if anchor_c.position_h.offset != FloatOffset::Emu(1_828_800)
        || anchor_c.position_v.offset != FloatOffset::Emu(91_440)
        || anchor_c.wrap_xml != anchor.wrap_xml
        || anchor_c.doc_pr_xml != anchor.doc_pr_xml
    {
        bail!("moved anchor re-read wrongly: {anchor_c:?}");
    }
    println!("[roundtrip] step 10 OK — floating anchor survives edit + move byte-for-byte");
    Ok(())
}

/* ============================================== text wrap (#82) ==== */

/// Issue #82 — one `<wp:anchor>` picture paragraph of the wrap fixture.
fn wrap_anchor_paragraph(label: &str, attrs: &str, wrap: &str, id: u32) -> String {
    format!(
        concat!(
            r#"<w:p><w:r><w:t xml:space="preserve">{label} </w:t></w:r><w:r><w:drawing>"#,
            r#"<wp:anchor {attrs}simplePos="0" relativeHeight="251659264" behindDoc="{behind}" locked="0" layoutInCell="1" allowOverlap="1">"#,
            r#"<wp:simplePos x="0" y="0"/>"#,
            r#"<wp:positionH relativeFrom="column"><wp:posOffset>914400</wp:posOffset></wp:positionH>"#,
            r#"<wp:positionV relativeFrom="paragraph"><wp:posOffset>0</wp:posOffset></wp:positionV>"#,
            r#"<wp:extent cx="914400" cy="457200"/><wp:effectExtent l="0" t="0" r="0" b="0"/>"#,
            "{wrap}",
            r#"<wp:docPr id="{id}" name="Picture {id}"/><wp:cNvGraphicFramePr/>"#,
            r#"<a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture">"#,
            r#"<pic:pic><pic:nvPicPr><pic:cNvPr id="0" name="Image"/><pic:cNvPicPr/></pic:nvPicPr>"#,
            r#"<pic:blipFill><a:blip r:embed="rId5"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill>"#,
            r#"<pic:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="914400" cy="457200"/></a:xfrm>"#,
            r#"<a:prstGeom prst="rect"><a:avLst/></a:prstGeom></pic:spPr></pic:pic>"#,
            "</a:graphicData></a:graphic></wp:anchor></w:drawing></w:r>",
            r#"<w:r><w:t xml:space="preserve">text</w:t></w:r></w:p>"#,
        ),
        label = label,
        attrs = attrs,
        behind = u8::from(label == "behind"),
        wrap = wrap,
        id = id,
    )
}

/// The five wrap children the fixture plants, one per paragraph, with
/// their anchor-level distances and layering flags. Tight carries an
/// EDITED polygon (a triangle) that the model must keep.
const WRAP_CASES: &[(&str, &str, &str)] = &[
    (
        "square",
        r#"distT="0" distB="0" distL="114300" distR="228600" "#,
        r#"<wp:wrapSquare wrapText="largest"/>"#,
    ),
    (
        "tight",
        r#"distT="0" distB="0" distL="91440" distR="91440" "#,
        concat!(
            r#"<wp:wrapTight wrapText="bothSides"><wp:wrapPolygon edited="1">"#,
            r#"<wp:start x="0" y="0"/><wp:lineTo x="21600" y="0"/><wp:lineTo x="10800" y="21600"/>"#,
            r#"<wp:lineTo x="0" y="0"/></wp:wrapPolygon></wp:wrapTight>"#
        ),
    ),
    (
        "through",
        r#"distT="0" distB="0" distL="0" distR="0" "#,
        concat!(
            r#"<wp:wrapThrough wrapText="right"><wp:wrapPolygon edited="0">"#,
            r#"<wp:start x="0" y="0"/><wp:lineTo x="0" y="21600"/><wp:lineTo x="21600" y="21600"/>"#,
            r#"<wp:lineTo x="21600" y="0"/><wp:lineTo x="0" y="0"/></wp:wrapPolygon></wp:wrapThrough>"#
        ),
    ),
    (
        "topbottom",
        r#"distT="45720" distB="91440" distL="0" distR="0" "#,
        r#"<wp:wrapTopAndBottom/>"#,
    ),
    (
        "behind",
        r#"distT="0" distB="0" distL="0" distR="0" "#,
        r#"<wp:wrapNone/>"#,
    ),
];

fn wrap_modes_document_xml() -> String {
    let mut body = String::new();
    for (i, (label, attrs, wrap)) in WRAP_CASES.iter().enumerate() {
        body.push_str(&wrap_anchor_paragraph(label, attrs, wrap, 10 + i as u32));
    }
    format!(
        concat!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
            "\n",
            r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" "#,
            r#"xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" "#,
            r#"xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" "#,
            r#"xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" "#,
            r#"xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture" "#,
            r#"xmlns:wp14="http://schemas.microsoft.com/office/word/2010/wordprocessingDrawing">"#,
            "<w:body>{body}<w:sectPr/></w:body></w:document>",
        ),
        body = body
    )
}

/// Issue #82 fixture: one floating picture per wrap mode. Rides the
/// `--fixtures` passthrough at drift 0 and the default harness's step 14.
fn build_image_wrap_modes_docx() -> Vec<u8> {
    pack_docx_with_png(&wrap_modes_document_xml())
}

/// Issue #82 — step 14: wrap mode + distances round-trip.
/// (a) every wrap child lowers into typed fields (kind, side rule,
/// polygon, the four distances, `behindDoc`); (b) a dirty-paragraph
/// save of every anchor paragraph regenerates `document.xml` as EXACTLY
/// the source plus the edits — the verbatim wrap elements survive
/// byte-for-byte; (c) a wrap-mode change regenerates that one element
/// from the model (Square → Tight gets Word's default polygon; the
/// distances and side rule persist) and re-reads as the new mode.
fn run_wrap_modes_roundtrip() -> Result<()> {
    use engine::{BlockPath, LogicalPos, WrapKind, WrapText};

    let fixture = build_image_wrap_modes_docx();
    let archive = read_docx(&fixture).context("read wrap fixture")?;
    let anchors = |d: &DocumentTree| -> Vec<engine::FloatAnchor> {
        d.blocks
            .iter()
            .filter_map(engine::Block::as_paragraph)
            .filter_map(|p| p.inline_objects.first())
            .filter_map(|o| o.anchor.as_deref().cloned())
            .collect()
    };
    let a = anchors(&archive.document);
    if a.len() != WRAP_CASES.len() {
        bail!(
            "wrap fixture: expected {} floats, got {}",
            WRAP_CASES.len(),
            a.len()
        );
    }
    let expect: [(WrapKind, WrapText, usize, [i64; 4], bool); 5] = [
        (
            WrapKind::Square,
            WrapText::Largest,
            0,
            [0, 0, 114_300, 228_600],
            false,
        ),
        (
            WrapKind::Tight,
            WrapText::BothSides,
            4,
            [0, 0, 91_440, 91_440],
            false,
        ),
        (WrapKind::Through, WrapText::Right, 5, [0, 0, 0, 0], false),
        (
            WrapKind::TopAndBottom,
            WrapText::BothSides,
            0,
            [45_720, 91_440, 0, 0],
            false,
        ),
        (WrapKind::None, WrapText::BothSides, 0, [0, 0, 0, 0], true),
    ];
    for (i, (got, (kind, text, poly, dist, behind))) in a.iter().zip(expect).enumerate() {
        let d = [
            got.dist_top_emu,
            got.dist_bottom_emu,
            got.dist_left_emu,
            got.dist_right_emu,
        ];
        if got.wrap != kind
            || got.wrap_text != text
            || got.wrap_polygon.as_ref().map_or(0, Vec::len) != poly
            || d != dist
            || got.behind_doc != behind
        {
            bail!("wrap fixture anchor {i} lowered wrongly: {got:?}");
        }
    }
    println!("[roundtrip] step 14a OK — five wrap modes lower into typed fields");

    /* (b) Dirty every anchor paragraph (insert before its sentinel). */
    let mut edited = archive.document.clone();
    for i in 0..WRAP_CASES.len() {
        edited = edited.insert_text(
            LogicalPos {
                path: BlockPath::top(i as u32),
                offset: 0,
            },
            INSERT_TEXT,
        );
    }
    let bytes = write_docx(&archive, &edited).context("write edited wrap fixture")?;
    assert_document_xml_well_formed(&bytes).context("edited wrap .docx")?;
    let src = String::from_utf8(extract_doc_xml(&fixture)?).context("utf8 source")?;
    let out = String::from_utf8(extract_doc_xml(&bytes)?).context("utf8 output")?;
    let mut want = src.clone();
    for (label, _, _) in WRAP_CASES {
        want = want.replacen(
            &format!(r#"<w:t xml:space="preserve">{label} </w:t>"#),
            &format!(r#"<w:t xml:space="preserve">{INSERT_TEXT}{label} </w:t>"#),
            1,
        );
    }
    if out != want {
        bail!(
            "wrap fixture: regenerated document.xml is not source + edits\n--- expected ---\n{want}\n--- got ---\n{out}"
        );
    }
    let reread = read_docx(&bytes).context("re-read edited wrap fixture")?;
    if anchors(&reread.document) != a {
        bail!("wrap fixture: anchors drifted through a dirty save");
    }
    println!("[roundtrip] step 14b OK — dirty save keeps all five wrap elements byte-for-byte");

    /* (c) Square → Tight on the first picture. */
    let at = edited
        .paragraph_at_path(&BlockPath::top(0))
        .and_then(|p| p.inline_objects.first())
        .map(|o| o.at)
        .context("square picture")?;
    let switched =
        edited.set_floating_image_wrap_at(&BlockPath::top(0), at, WrapKind::Tight, false);
    let bytes_c = write_docx(&archive, &switched).context("write switched wrap")?;
    assert_document_xml_well_formed(&bytes_c).context("switched wrap .docx")?;
    let doc_c = String::from_utf8(extract_doc_xml(&bytes_c)?).context("utf8 switched")?;
    if doc_c.contains(r#"<wp:wrapSquare wrapText="largest"/>"#)
        || !doc_c.contains(r#"<wp:wrapTight wrapText="largest"><wp:wrapPolygon edited="0">"#)
    {
        bail!("wrap switch did not regenerate the element:\n{doc_c}");
    }
    let c = anchors(&read_docx(&bytes_c).context("re-read switched")?.document);
    if c[0].wrap != WrapKind::Tight
        || c[0].wrap_text != WrapText::Largest
        || c[0].wrap_polygon.as_ref().map(Vec::len) != Some(5)
        || (c[0].dist_left_emu, c[0].dist_right_emu) != (114_300, 228_600)
        || c[1..] != a[1..]
    {
        bail!("wrap switch re-read wrongly: {:?}", c[0]);
    }
    println!(
        "[roundtrip] step 14c OK — a wrap-mode change regenerates one element, distances kept"
    );
    Ok(())
}

/* ============================================== text boxes (#83) ==== */

const TB_PROSE: &str = "Body text wraps around the framed story on the left while the paragraph keeps going for several more lines of ordinary prose that fill the column.";
const TB_ARABIC: &str =
    "هذا نص عربي يلتف حول صندوق النص على اليمين ويستمر لعدة أسطر أخرى من النثر العادي.";
const TB_STORY_A: &str = "Box one frames a short story.";
const TB_STORY_B: &str = "صندوق نص من اليمين";

/// Issue #83 fixture: two floating text boxes with square wrap. Box A
/// (LTR) is Word's `mc:AlternateContent` shape — the DrawingML choice
/// plus its VML `<v:textbox>` fallback — anchored at the left of the
/// first paragraph's column; box B (RTL story, `<w:bidi/>`) is a bare
/// `<w:drawing>` aligned right in the second, RTL paragraph. Every
/// paragraph is in the writer's canonical shape so an edited story
/// regenerates byte-identical modulo the edit.
fn text_boxes_document_xml() -> String {
    let anchor = |align_h: &str, id: u32| {
        format!(
            concat!(
                r#"<wp:anchor distT="0" distB="0" distL="114300" distR="114300" simplePos="0" relativeHeight="{id}" "#,
                r#"behindDoc="0" locked="0" layoutInCell="1" allowOverlap="1"><wp:simplePos x="0" y="0"/>"#,
                r#"<wp:positionH relativeFrom="column">{align_h}</wp:positionH>"#,
                r#"<wp:positionV relativeFrom="paragraph"><wp:posOffset>0</wp:posOffset></wp:positionV>"#,
                r#"<wp:extent cx="1371600" cy="685800"/><wp:effectExtent l="0" t="0" r="0" b="0"/>"#,
                r#"<wp:wrapSquare wrapText="bothSides"/><wp:docPr id="{id}" name="Text Box {id}"/>"#,
                r#"<wp:cNvGraphicFramePr/>"#,
            ),
            align_h = align_h,
            id = id
        )
    };
    let wsp = |story: &str| {
        format!(
            concat!(
                r#"<a:graphic><a:graphicData uri="http://schemas.microsoft.com/office/word/2010/wordprocessingShape">"#,
                r#"<wps:wsp><wps:cNvSpPr txBox="1"/><wps:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="1371600" cy="685800"/></a:xfrm>"#,
                r#"<a:prstGeom prst="rect"><a:avLst/></a:prstGeom><a:solidFill><a:srgbClr val="FFFFFF"/></a:solidFill>"#,
                r#"<a:ln w="9525"><a:solidFill><a:srgbClr val="000000"/></a:solidFill></a:ln></wps:spPr>"#,
                r#"<wps:txbx><w:txbxContent>{story}</w:txbxContent></wps:txbx>"#,
                r#"<wps:bodyPr rot="0" vert="horz" wrap="square" lIns="91440" tIns="45720" rIns="91440" bIns="45720" anchor="t" anchorCtr="0"><a:noAutofit/></wps:bodyPr>"#,
                r#"</wps:wsp></a:graphicData></a:graphic>"#,
            ),
            story = story
        )
    };
    let story_a = format!(r#"<w:p><w:r><w:t xml:space="preserve">{TB_STORY_A}</w:t></w:r></w:p>"#);
    let story_b = format!(
        r#"<w:p><w:pPr><w:bidi/></w:pPr><w:r><w:t xml:space="preserve">{TB_STORY_B}</w:t></w:r></w:p>"#
    );
    let box_a = format!(
        concat!(
            r#"<mc:AlternateContent><mc:Choice Requires="wps"><w:drawing>{anchor}{wsp}</wp:anchor></w:drawing></mc:Choice>"#,
            r##"<mc:Fallback><w:pict><v:shape id="Text Box 1" o:spid="_x0000_s1026" type="#_x0000_t202" "##,
            r#"style="position:absolute;margin-left:0;margin-top:0;width:108pt;height:54pt;z-index:1" strokeweight=".5pt">"#,
            r#"<v:textbox><w:txbxContent>{story}</w:txbxContent></v:textbox>"#,
            r#"<w10:wrap type="square"/></v:shape></w:pict></mc:Fallback></mc:AlternateContent>"#,
        ),
        anchor = anchor("<wp:posOffset>0</wp:posOffset>", 1),
        wsp = wsp(&story_a),
        story = story_a
    );
    let box_b = format!(
        "<w:drawing>{}{}</wp:anchor></w:drawing>",
        anchor("<wp:align>right</wp:align>", 2),
        wsp(&story_b)
    );
    format!(
        concat!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
            "\n",
            r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" "#,
            r#"xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" "#,
            r#"xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" "#,
            r#"xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" "#,
            r#"xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture" "#,
            r#"xmlns:wp14="http://schemas.microsoft.com/office/word/2010/wordprocessingDrawing" "#,
            r#"xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape" "#,
            r#"xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006" "#,
            r#"xmlns:v="urn:schemas-microsoft-com:vml" xmlns:o="urn:schemas-microsoft-com:office:office" "#,
            r#"xmlns:w10="urn:schemas-microsoft-com:office:word" mc:Ignorable="wp14">"#,
            "<w:body>",
            r#"<w:p><w:r><w:t xml:space="preserve">Intro </w:t></w:r><w:r>{box_a}</w:r>"#,
            r#"<w:r><w:t xml:space="preserve">{prose}</w:t></w:r></w:p>"#,
            r#"<w:p><w:pPr><w:bidi/></w:pPr><w:r>{box_b}</w:r>"#,
            r#"<w:r><w:t xml:space="preserve">{arabic}</w:t></w:r></w:p>"#,
            "<w:sectPr/></w:body></w:document>",
        ),
        box_a = box_a,
        box_b = box_b,
        prose = TB_PROSE,
        arabic = TB_ARABIC
    )
}

/* ============================================ nested text boxes (#196) ==== */

const TBN_OUTER: &str = "Outer story text.";
const TBN_INNER: &str = "Inner story text.";

/// Issue #196 fixture: a page-anchored 3" × 2" text box (at 1", 3" on
/// the page) whose story hosts a second, nested 1.5" × 0.6" box placed
/// 1" right / 0.5" down inside the outer box's content rect. Both are
/// bare `<w:drawing>` shapes; every paragraph is in the writer's
/// canonical shape so an edited nested story regenerates byte-identical
/// modulo the edit. The e2e spec `ts/e2e/nested-text-box.spec.ts`
/// clicks into the nested box by this geometry.
fn nested_text_boxes_document_xml() -> String {
    let drawing = |x: i64, y: i64, cx: i64, cy: i64, id: u32, story: &str| {
        format!(
            concat!(
                r#"<w:drawing><wp:anchor distT="0" distB="0" distL="114300" distR="114300" simplePos="0" relativeHeight="{id}" "#,
                r#"behindDoc="0" locked="0" layoutInCell="1" allowOverlap="1"><wp:simplePos x="0" y="0"/>"#,
                r#"<wp:positionH relativeFrom="page"><wp:posOffset>{x}</wp:posOffset></wp:positionH>"#,
                r#"<wp:positionV relativeFrom="page"><wp:posOffset>{y}</wp:posOffset></wp:positionV>"#,
                r#"<wp:extent cx="{cx}" cy="{cy}"/><wp:effectExtent l="0" t="0" r="0" b="0"/>"#,
                r#"<wp:wrapSquare wrapText="bothSides"/><wp:docPr id="{id}" name="Text Box {id}"/>"#,
                r#"<wp:cNvGraphicFramePr/>"#,
                r#"<a:graphic><a:graphicData uri="http://schemas.microsoft.com/office/word/2010/wordprocessingShape">"#,
                r#"<wps:wsp><wps:cNvSpPr txBox="1"/><wps:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="{cx}" cy="{cy}"/></a:xfrm>"#,
                r#"<a:prstGeom prst="rect"><a:avLst/></a:prstGeom><a:solidFill><a:srgbClr val="FFFFFF"/></a:solidFill>"#,
                r#"<a:ln w="9525"><a:solidFill><a:srgbClr val="000000"/></a:solidFill></a:ln></wps:spPr>"#,
                r#"<wps:txbx><w:txbxContent>{story}</w:txbxContent></wps:txbx>"#,
                r#"<wps:bodyPr rot="0" vert="horz" wrap="square" lIns="91440" tIns="45720" rIns="91440" bIns="45720" anchor="t" anchorCtr="0"><a:noAutofit/></wps:bodyPr>"#,
                r#"</wps:wsp></a:graphicData></a:graphic></wp:anchor></w:drawing>"#,
            ),
            x = x,
            y = y,
            cx = cx,
            cy = cy,
            id = id,
            story = story
        )
    };
    let inner_story =
        format!(r#"<w:p><w:r><w:t xml:space="preserve">{TBN_INNER}</w:t></w:r></w:p>"#);
    let inner = drawing(914_400, 457_200, 1_371_600, 548_640, 2, &inner_story);
    let outer_story = format!(
        concat!(
            r#"<w:p><w:r><w:t xml:space="preserve">{outer}</w:t></w:r></w:p>"#,
            r#"<w:p><w:r>{inner}</w:r><w:r><w:t xml:space="preserve">Nested host.</w:t></w:r></w:p>"#,
        ),
        outer = TBN_OUTER,
        inner = inner
    );
    let outer = drawing(914_400, 2_743_200, 2_743_200, 1_828_800, 1, &outer_story);
    format!(
        concat!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
            "\n",
            r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" "#,
            r#"xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" "#,
            r#"xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" "#,
            r#"xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" "#,
            r#"xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape">"#,
            "<w:body>",
            r#"<w:p><w:r><w:t xml:space="preserve">Intro paragraph.</w:t></w:r></w:p>"#,
            r#"<w:p><w:r>{outer}</w:r><w:r><w:t xml:space="preserve">Host paragraph.</w:t></w:r></w:p>"#,
            "<w:sectPr/></w:body></w:document>",
        ),
        outer = outer
    )
}

/// Issue #196 fixture builder. Rides the `--fixtures` passthrough at
/// drift 0, the default harness's nested step and the e2e spec.
fn build_nested_text_boxes_docx() -> Vec<u8> {
    package_document_xml(&nested_text_boxes_document_xml())
}

/// Issue #196 — the nested text-box round-trip contract: (a) the outer
/// box and the box nested in its story both parse (two levels); (b) a
/// zero-edit save is byte-identical; (c) typing into the NESTED story
/// through the engine's nested write path (outer story tree →
/// `with_updated_text_box(inner)` → `with_updated_text_box(outer)`)
/// regenerates ONLY the nested story paragraph — the whole part equals
/// the source plus the inserted text; (d) the edit re-reads.
fn run_nested_text_boxes_roundtrip() -> Result<()> {
    use engine::{BlockPath, LogicalPos};

    let fixture = build_nested_text_boxes_docx();
    let archive = read_docx(&fixture).context("read nested text-box fixture")?;
    let doc = &archive.document;
    let outer_addr = (BlockPath::top(1), 0u32);
    let inner_addr = (BlockPath::top(1), 0u32);
    let nested_text = |d: &DocumentTree| -> Option<String> {
        let outer = d.text_box_at(&outer_addr.0, outer_addr.1)?;
        let tree = DocumentTree::from_blocks(outer.body.clone());
        let inner = tree.text_box_at(&inner_addr.0, inner_addr.1)?;
        inner
            .body
            .first()
            .and_then(engine::Block::as_paragraph)
            .map(|p| p.text.clone())
    };
    if doc.text_box_addresses() != vec![outer_addr.clone()] {
        bail!(
            "nested text-box fixture: expected one body box at {outer_addr:?}, got {:?}",
            doc.text_box_addresses()
        );
    }
    if nested_text(doc).as_deref() != Some(TBN_INNER) {
        bail!("nested text-box fixture: the nested story parsed wrongly");
    }
    println!("[roundtrip] step 20a OK — a box nested in a box's story parses (two levels)");

    let src = String::from_utf8(extract_doc_xml(&fixture)?).context("utf8 source")?;
    let zero = write_docx(&archive, doc).context("zero-edit write")?;
    if String::from_utf8(extract_doc_xml(&zero)?).context("utf8 zero")? != src {
        bail!("nested text-box fixture: zero-edit save drifted");
    }
    println!("[roundtrip] step 20b OK — zero-edit save is byte-identical");

    /* (c) Type into the nested story exactly like the engine's nested
    story adapter (`write_text_box_story`) does. */
    let outer = doc
        .text_box_at(&outer_addr.0, outer_addr.1)
        .context("outer box")?;
    let outer_tree = DocumentTree::from_blocks(outer.body.clone());
    let inner = outer_tree
        .text_box_at(&inner_addr.0, inner_addr.1)
        .context("nested box")?;
    let typed = DocumentTree::from_blocks(inner.body.clone()).insert_text(
        LogicalPos {
            path: BlockPath::top(0),
            offset: 0,
        },
        INSERT_TEXT,
    );
    let outer_edited = outer_tree.with_updated_text_box(
        &inner_addr.0,
        inner_addr.1,
        typed.blocks.iter().cloned().collect(),
    );
    let edited = doc.with_updated_text_box(
        &outer_addr.0,
        outer_addr.1,
        outer_edited.blocks.iter().cloned().collect(),
    );
    let bytes = write_docx(&archive, &edited).context("write edited nested text box")?;
    assert_document_xml_well_formed(&bytes).context("edited nested text-box .docx")?;
    let out = String::from_utf8(extract_doc_xml(&bytes)?).context("utf8 edited")?;
    let needle = format!(r#"<w:t xml:space="preserve">{TBN_INNER}</w:t>"#);
    let want = src.replace(
        &needle,
        &format!(r#"<w:t xml:space="preserve">{INSERT_TEXT}{TBN_INNER}</w:t>"#),
    );
    if src.matches(&needle).count() != 1 || out != want {
        bail!(
            "nested text-box fixture: edited save is not source + edit\n--- expected ---\n{want}\n--- got ---\n{out}"
        );
    }
    let drift = out.len() - src.len();
    if drift > 2 * INSERT_TEXT.len() {
        bail!("nested text-box fixture: drift {drift} B exceeds the bound");
    }
    println!(
        "[roundtrip] step 20c OK — a nested story edit splices only the nested story (Δ {drift} B)"
    );

    let reread = read_docx(&bytes).context("re-read edited nested text box")?;
    if nested_text(&reread.document) != Some(format!("{INSERT_TEXT}{TBN_INNER}")) {
        bail!("nested text-box fixture: the nested edit did not re-read");
    }
    let outer_text = reread
        .document
        .text_box_at(&outer_addr.0, outer_addr.1)
        .and_then(|s| {
            s.body
                .first()
                .and_then(engine::Block::as_paragraph)
                .map(|p| p.text.clone())
        });
    if outer_text.as_deref() != Some(TBN_OUTER) {
        bail!("nested text-box fixture: the outer story changed ({outer_text:?})");
    }
    println!("[roundtrip] step 20d OK — the nested edit re-reads, the outer story untouched");
    Ok(())
}

/* ======================================= pictures in text boxes (#206) ==== */

/// Issue #206 — an 8 × 8 solid-blue PNG (a real image, so the picture
/// paints in a browser; the reader stores bytes, decoding is the shell's).
const TBP_PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, 0x08, 0x08, 0x02, 0x00, 0x00, 0x00, 0x4b, 0x6d, 0x29,
    0xdc, 0x00, 0x00, 0x00, 0x11, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0x90, 0x8b, 0x3a, 0x81,
    0x15, 0x31, 0x0c, 0x2d, 0x09, 0x00, 0x18, 0x19, 0x50, 0x01, 0x47, 0xb6, 0x9a, 0xb3, 0x00, 0x00,
    0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];
const TBP_OUTER: &str = "Outer story text flows beside the picture.";
const TBP_INNER: &str = "Inner text.";

/// Issue #206 — a floating `<wp:anchor>` picture (`rId5`) at fixed EMU
/// offsets from the column / paragraph frames, square wrap, in the
/// writer's canonical `CT_Anchor` child order.
fn tbp_picture(x: i64, y: i64, cx: i64, cy: i64, id: u32) -> String {
    format!(
        concat!(
            r#"<w:drawing><wp:anchor distT="0" distB="0" distL="114300" distR="114300" simplePos="0" relativeHeight="{id}" "#,
            r#"behindDoc="0" locked="0" layoutInCell="1" allowOverlap="1"><wp:simplePos x="0" y="0"/>"#,
            r#"<wp:positionH relativeFrom="column"><wp:posOffset>{x}</wp:posOffset></wp:positionH>"#,
            r#"<wp:positionV relativeFrom="paragraph"><wp:posOffset>{y}</wp:posOffset></wp:positionV>"#,
            r#"<wp:extent cx="{cx}" cy="{cy}"/><wp:effectExtent l="0" t="0" r="0" b="0"/>"#,
            r#"<wp:wrapSquare wrapText="bothSides"/><wp:docPr id="{id}" name="Picture {id}"/>"#,
            r#"<wp:cNvGraphicFramePr/><a:graphic>"#,
            r#"<a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:pic>"#,
            r#"<pic:nvPicPr><pic:cNvPr id="0" name="Image"/><pic:cNvPicPr/></pic:nvPicPr>"#,
            r#"<pic:blipFill><a:blip r:embed="rId5"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill>"#,
            r#"<pic:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="{cx}" cy="{cy}"/></a:xfrm>"#,
            r#"<a:prstGeom prst="rect"><a:avLst/></a:prstGeom></pic:spPr>"#,
            r#"</pic:pic></a:graphicData></a:graphic></wp:anchor></w:drawing>"#,
        ),
        x = x,
        y = y,
        cx = cx,
        cy = cy,
        id = id
    )
}

/// Issue #206 fixture: the #196 geometry — a page-anchored 3" × 2" box at
/// (1", 3") — whose story opens with a 0.75" × 0.5" floating picture at
/// the column / paragraph corner, then hosts a nested 1.5" × 0.8" box
/// (1.2" right / 0.9" down in the outer content rect) whose own story
/// opens with a 0.5" × 0.3" floating picture. The e2e spec
/// `ts/e2e/text-box-pictures.spec.ts` targets both pictures by this
/// geometry.
fn text_box_pictures_document_xml() -> String {
    let text_box = |x: i64, y: i64, cx: i64, cy: i64, id: u32, story: &str| {
        format!(
            concat!(
                r#"<w:drawing><wp:anchor distT="0" distB="0" distL="114300" distR="114300" simplePos="0" relativeHeight="{id}" "#,
                r#"behindDoc="0" locked="0" layoutInCell="1" allowOverlap="1"><wp:simplePos x="0" y="0"/>"#,
                r#"<wp:positionH relativeFrom="page"><wp:posOffset>{x}</wp:posOffset></wp:positionH>"#,
                r#"<wp:positionV relativeFrom="page"><wp:posOffset>{y}</wp:posOffset></wp:positionV>"#,
                r#"<wp:extent cx="{cx}" cy="{cy}"/><wp:effectExtent l="0" t="0" r="0" b="0"/>"#,
                r#"<wp:wrapSquare wrapText="bothSides"/><wp:docPr id="{id}" name="Text Box {id}"/>"#,
                r#"<wp:cNvGraphicFramePr/>"#,
                r#"<a:graphic><a:graphicData uri="http://schemas.microsoft.com/office/word/2010/wordprocessingShape">"#,
                r#"<wps:wsp><wps:cNvSpPr txBox="1"/><wps:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="{cx}" cy="{cy}"/></a:xfrm>"#,
                r#"<a:prstGeom prst="rect"><a:avLst/></a:prstGeom><a:solidFill><a:srgbClr val="FFFFFF"/></a:solidFill>"#,
                r#"<a:ln w="9525"><a:solidFill><a:srgbClr val="000000"/></a:solidFill></a:ln></wps:spPr>"#,
                r#"<wps:txbx><w:txbxContent>{story}</w:txbxContent></wps:txbx>"#,
                r#"<wps:bodyPr rot="0" vert="horz" wrap="square" lIns="91440" tIns="45720" rIns="91440" bIns="45720" anchor="t" anchorCtr="0"><a:noAutofit/></wps:bodyPr>"#,
                r#"</wps:wsp></a:graphicData></a:graphic></wp:anchor></w:drawing>"#,
            ),
            x = x,
            y = y,
            cx = cx,
            cy = cy,
            id = id,
            story = story
        )
    };
    let inner_story = format!(
        r#"<w:p><w:r>{pic}</w:r><w:r><w:t xml:space="preserve">{TBP_INNER}</w:t></w:r></w:p>"#,
        pic = tbp_picture(0, 0, 457_200, 274_320, 4)
    );
    let inner = text_box(1_097_280, 822_960, 1_371_600, 731_520, 2, &inner_story);
    let outer_story = format!(
        concat!(
            r#"<w:p><w:r>{pic}</w:r><w:r><w:t xml:space="preserve">{outer}</w:t></w:r></w:p>"#,
            r#"<w:p><w:r>{inner}</w:r><w:r><w:t xml:space="preserve">Nested host.</w:t></w:r></w:p>"#,
        ),
        pic = tbp_picture(0, 0, 685_800, 457_200, 3),
        outer = TBP_OUTER,
        inner = inner
    );
    let outer = text_box(914_400, 2_743_200, 2_743_200, 1_828_800, 1, &outer_story);
    format!(
        concat!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
            "\n",
            r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" "#,
            r#"xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" "#,
            r#"xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" "#,
            r#"xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" "#,
            r#"xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture" "#,
            r#"xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape">"#,
            "<w:body>",
            r#"<w:p><w:r><w:t xml:space="preserve">Intro paragraph.</w:t></w:r></w:p>"#,
            r#"<w:p><w:r>{outer}</w:r><w:r><w:t xml:space="preserve">Host paragraph.</w:t></w:r></w:p>"#,
            "<w:sectPr/></w:body></w:document>",
        ),
        outer = outer
    )
}

/// Issue #206 fixture builder. Rides the `--fixtures` passthrough at
/// drift 0, the default harness's step 24 and the e2e spec.
fn build_text_box_pictures_docx() -> Vec<u8> {
    pack_docx_with_png_bytes(&text_box_pictures_document_xml(), TBP_PNG)
}

/// Issue #206 — pictures inside text-box stories round-trip their edits:
/// (a) both pictures parse as floating pictures of their stories (the
/// outer box's, and the nested box's); (b) a zero-edit save is
/// byte-identical; (c) moving the outer-story picture and re-wrapping +
/// resizing the nested-story picture through the engine's story-chain
/// edit path (`with_text_box_story_edit`, what `MoveImage` / `ResizeImage`
/// / `SetImageWrap` run with a `story`) regenerates exactly those two
/// drawings — the verified passthrough (#112/#119) refuses their stale
/// bytes — while every untouched paragraph keeps its bytes; (d) the
/// edits re-read.
fn run_text_box_pictures_roundtrip() -> Result<()> {
    use engine::{BlockPath, FloatOffset, WrapKind};

    let fixture = build_text_box_pictures_docx();
    let archive = read_docx(&fixture).context("read text-box pictures fixture")?;
    let doc = &archive.document;
    let outer_chain = vec![(BlockPath::top(1), 0u32)];
    let inner_chain = vec![(BlockPath::top(1), 0u32), (BlockPath::top(1), 0u32)];
    let picture = |d: &DocumentTree, chain: &[(BlockPath, u32)]| -> Option<engine::InlineObject> {
        d.text_box_story_tree(chain)?
            .paragraph_at_path(&BlockPath::top(0))?
            .inline_objects
            .iter()
            .find(|io| io.at == 0 && matches!(io.kind, engine::InlineKind::Image { .. }))
            .cloned()
    };
    let (Some(op), Some(ip)) = (picture(doc, &outer_chain), picture(doc, &inner_chain)) else {
        bail!("text-box pictures fixture: a story picture did not parse");
    };
    if !op.is_floating() || !ip.is_floating() || op.source_xml.is_none() {
        bail!("text-box pictures fixture: the story pictures must be floating with bytes");
    }
    println!("[roundtrip] step 24a OK — pictures parse inside a box story and a nested box story");

    let src = String::from_utf8(extract_doc_xml(&fixture)?).context("utf8 source")?;
    let zero = write_docx(&archive, doc).context("zero-edit write")?;
    if String::from_utf8(extract_doc_xml(&zero)?).context("utf8 zero")? != src {
        bail!("text-box pictures fixture: zero-edit save drifted");
    }
    println!("[roundtrip] step 24b OK — zero-edit save is byte-identical");

    let at0 = BlockPath::top(0);
    let edited = doc
        .with_text_box_story_edit(&outer_chain, |t| {
            t.move_floating_image_at(&at0, 0, 228_600, 91_440)
        })
        .context("move the outer-story picture")?;
    let edited = edited
        .with_text_box_story_edit(&inner_chain, |t| {
            t.set_floating_image_wrap_at(&at0, 0, WrapKind::TopAndBottom, false)
                .resize_inline_image_at(&at0, 0, 548_640, 329_184)
        })
        .context("re-wrap + resize the nested-story picture")?;
    let bytes = write_docx(&archive, &edited).context("write edited pictures")?;
    assert_document_xml_well_formed(&bytes).context("edited text-box pictures .docx")?;
    let out = String::from_utf8(extract_doc_xml(&bytes)?).context("utf8 edited")?;
    let unchanged = [
        r#"<w:p><w:r><w:t xml:space="preserve">Intro paragraph.</w:t></w:r></w:p>"#.to_string(),
        format!(r#"<w:t xml:space="preserve">{TBP_OUTER}</w:t>"#),
        format!(r#"<w:t xml:space="preserve">{TBP_INNER}</w:t>"#),
        r#"<w:t xml:space="preserve">Nested host.</w:t>"#.to_string(),
        r#"<w:t xml:space="preserve">Host paragraph.</w:t>"#.to_string(),
    ];
    for needle in &unchanged {
        if !out.contains(needle.as_str()) {
            bail!("text-box pictures fixture: lost `{needle}`\n{out}");
        }
    }
    for needle in [
        "<wp:posOffset>228600</wp:posOffset>",
        "<wp:posOffset>91440</wp:posOffset>",
        "<wp:wrapTopAndBottom/>",
        r#"cx="548640" cy="329184""#,
    ] {
        if !out.contains(needle) {
            bail!("text-box pictures fixture: the edit `{needle}` was not written\n{out}");
        }
    }
    if out.contains("<wp:wrapSquare wrapText=\"bothSides\"/><wp:docPr id=\"4\"") {
        bail!("text-box pictures fixture: the nested picture kept its stale wrap bytes");
    }
    println!("[roundtrip] step 24c OK — box-story picture edits regenerate their drawings only");

    let reread = read_docx(&bytes).context("re-read edited pictures")?;
    let (Some(op), Some(ip)) = (
        picture(&reread.document, &outer_chain),
        picture(&reread.document, &inner_chain),
    ) else {
        bail!("text-box pictures fixture: an edited picture did not re-read");
    };
    let oa = op.anchor.as_deref().context("outer still floating")?;
    let ia = ip.anchor.as_deref().context("nested still floating")?;
    if oa.position_h.offset != FloatOffset::Emu(228_600)
        || oa.position_v.offset != FloatOffset::Emu(91_440)
        || ia.wrap != WrapKind::TopAndBottom
        || !matches!(
            ip.kind,
            engine::InlineKind::Image {
                width_emu: 548_640,
                height_emu: 329_184,
                ..
            }
        )
    {
        bail!(
            "text-box pictures fixture: the re-read edits differ: {oa:?} / {ia:?} / {:?}",
            ip.kind
        );
    }
    println!("[roundtrip] step 24d OK — the moved / re-wrapped / resized pictures re-read");
    Ok(())
}

/// Issue #83 fixture builder. Rides the `--fixtures` passthrough at
/// drift 0 and the default harness's text-box step.
fn build_text_boxes_docx() -> Vec<u8> {
    package_document_xml(&text_boxes_document_xml())
}

/// Issue #83 — the text-box round-trip contract:
/// (a) both shapes parse into text boxes (story text, the RTL story's
/// direction, square wrap, right alignment); (b) a zero-edit save is
/// byte-identical; (c) editing box A's story regenerates ONLY the edited
/// story paragraph — in the DrawingML choice AND the VML fallback — and
/// leaves the host paragraphs and box B verbatim (the whole part equals
/// the source plus the inserted text, twice); (d) the edit re-reads;
/// (e) an engine-authored box synthesizes a well-formed `<wps:wsp>` on
/// both save paths and re-reads as a third box.
fn run_text_boxes_roundtrip() -> Result<()> {
    use engine::{BlockPath, InlineKind, LogicalPos};

    let fixture = build_text_boxes_docx();
    let archive = read_docx(&fixture).context("read text-box fixture")?;
    let doc = &archive.document;
    let boxes = doc.text_box_addresses();
    if boxes.len() != 2 {
        bail!("text-box fixture: expected 2 boxes, got {}", boxes.len());
    }
    let story_text = |d: &DocumentTree, i: usize| -> Option<String> {
        let (h, a) = d.text_box_addresses().get(i)?.clone();
        let st = d.text_box_at(&h, a)?;
        st.body
            .first()
            .and_then(engine::Block::as_paragraph)
            .map(|p| p.text.clone())
    };
    if story_text(doc, 0).as_deref() != Some(TB_STORY_A)
        || story_text(doc, 1).as_deref() != Some(TB_STORY_B)
    {
        bail!("text-box fixture: stories parsed wrongly");
    }
    let (h1, a1) = boxes[1].clone();
    let rtl = doc
        .text_box_at(&h1, a1)
        .and_then(|s| s.body.first())
        .and_then(engine::Block::as_paragraph)
        .and_then(|p| p.props.direction);
    if rtl != Some(engine::TextDirection::Rtl) {
        bail!("text-box fixture: box B's story is not RTL ({rtl:?})");
    }
    for (h, a) in &boxes {
        let io = doc
            .paragraph_at_path(h)
            .and_then(|p| p.inline_objects.iter().find(|o| o.at == *a))
            .context("box object")?;
        let anchor = io.anchor.as_deref().context("box floats")?;
        if anchor.wrap != engine::WrapKind::Square || !matches!(io.kind, InlineKind::TextBox { .. })
        {
            bail!("text-box fixture: box at {h:?}/{a} lowered wrongly: {anchor:?}");
        }
    }
    println!("[roundtrip] step 16a OK — two text boxes (one RTL) parse with square wrap");

    let src = String::from_utf8(extract_doc_xml(&fixture)?).context("utf8 source")?;
    let zero = write_docx(&archive, doc).context("zero-edit write")?;
    if String::from_utf8(extract_doc_xml(&zero)?).context("utf8 zero")? != src {
        bail!("text-box fixture: zero-edit save drifted");
    }
    println!("[roundtrip] step 16b OK — zero-edit save is byte-identical");

    /* (c) Type into box A's story through a story tree, exactly like the
    engine's story adapter does. */
    let (h0, a0) = boxes[0].clone();
    let story = doc.text_box_at(&h0, a0).context("box A")?;
    let story_tree = DocumentTree::from_blocks(story.body.clone());
    let typed = story_tree.insert_text(
        LogicalPos {
            path: BlockPath::top(0),
            offset: 0,
        },
        INSERT_TEXT,
    );
    let edited = doc.with_updated_text_box(&h0, a0, typed.blocks.iter().cloned().collect());
    let bytes = write_docx(&archive, &edited).context("write edited text box")?;
    assert_document_xml_well_formed(&bytes).context("edited text-box .docx")?;
    let out = String::from_utf8(extract_doc_xml(&bytes)?).context("utf8 edited")?;
    let needle = format!(r#"<w:t xml:space="preserve">{TB_STORY_A}</w:t>"#);
    let want = src.replace(
        &needle,
        &format!(r#"<w:t xml:space="preserve">{INSERT_TEXT}{TB_STORY_A}</w:t>"#),
    );
    if src.matches(&needle).count() != 2 || out != want {
        bail!(
            "text-box fixture: edited save is not source + edit in both copies\n--- expected ---\n{want}\n--- got ---\n{out}"
        );
    }
    let drift = out.len() - src.len();
    if drift > 2 * 2 * INSERT_TEXT.len() {
        bail!("text-box fixture: drift {drift} B exceeds the bound");
    }
    println!("[roundtrip] step 16c OK — story edit splices choice + fallback only (Δ {drift} B)");

    let reread = read_docx(&bytes).context("re-read edited text box")?;
    if story_text(&reread.document, 0) != Some(format!("{INSERT_TEXT}{TB_STORY_A}"))
        || story_text(&reread.document, 1).as_deref() != Some(TB_STORY_B)
    {
        bail!("text-box fixture: edit did not re-read");
    }
    println!("[roundtrip] step 16d OK — the edited story re-reads, box B untouched");

    let (with_new, _, _) = edited.insert_text_box_at(
        LogicalPos {
            path: BlockPath::top(1),
            offset: 0,
        },
        1_828_800,
        914_400,
    );
    for (label, bytes) in [
        (
            "write_docx",
            write_docx(&archive, &with_new).context("write new box")?,
        ),
        (
            "build_minimal_docx",
            build_minimal_docx(&with_new).context("ui save")?,
        ),
    ] {
        assert_document_xml_well_formed(&bytes).with_context(|| format!("{label} new box"))?;
        let back = read_docx(&bytes).with_context(|| format!("{label} re-read"))?;
        if back.document.text_box_addresses().len() != 3 {
            bail!("{label}: the engine-authored box did not re-read");
        }
    }
    println!("[roundtrip] step 16e OK — an engine-authored box synthesizes and re-reads");
    Ok(())
}

/* ================================================== notes (#80) ==== */

/// Issue #80 fixture: three body paragraphs referencing three footnotes
/// and two endnotes, with Word's stock separator stories in both note
/// parts and `w14:paraId` markup on one note so passthrough fidelity is
/// observable. The body sits in the writer's canonical shape so a dirty
/// paragraph regenerates byte-identical modulo the edit.
const NOTES_W14_NS: &str = "http://schemas.microsoft.com/office/word/2010/wordml";

fn notes_fixture_document_xml() -> String {
    concat!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
        "\n",
        r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">"#,
        "<w:body>",
        r#"<w:p><w:r><w:t xml:space="preserve">Alpha body</w:t></w:r>"#,
        r#"<w:r><w:rPr><w:vertAlign w:val="superscript"/></w:rPr><w:footnoteReference w:id="1"/></w:r>"#,
        r#"<w:r><w:t xml:space="preserve"> continues</w:t></w:r></w:p>"#,
        r#"<w:p><w:r><w:t xml:space="preserve">Beta body</w:t></w:r>"#,
        r#"<w:r><w:rPr><w:vertAlign w:val="superscript"/></w:rPr><w:footnoteReference w:id="2"/></w:r>"#,
        r#"<w:r><w:rPr><w:vertAlign w:val="superscript"/></w:rPr><w:endnoteReference w:id="1"/></w:r></w:p>"#,
        r#"<w:p><w:r><w:t xml:space="preserve">Gamma body</w:t></w:r>"#,
        r#"<w:r><w:rPr><w:vertAlign w:val="superscript"/></w:rPr><w:footnoteReference w:id="3"/></w:r>"#,
        r#"<w:r><w:rPr><w:vertAlign w:val="superscript"/></w:rPr><w:endnoteReference w:id="2"/></w:r></w:p>"#,
        "<w:sectPr/></w:body></w:document>",
    )
    .to_string()
}

fn notes_fixture_footnotes_xml() -> String {
    format!(
        concat!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
            "\n",
            r#"<w:footnotes xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:w14="{w14}">"#,
            r#"<w:footnote w:type="separator" w:id="-1"><w:p><w:pPr><w:spacing w:after="0" w:line="240" w:lineRule="auto"/></w:pPr><w:r><w:separator/></w:r></w:p></w:footnote>"#,
            r#"<w:footnote w:type="continuationSeparator" w:id="0"><w:p><w:pPr><w:spacing w:after="0" w:line="240" w:lineRule="auto"/></w:pPr><w:r><w:continuationSeparator/></w:r></w:p></w:footnote>"#,
            r#"<w:footnote w:id="1"><w:p w14:paraId="0A1B2C3D"><w:pPr><w:pStyle w:val="FootnoteText"/></w:pPr><w:r><w:rPr><w:rStyle w:val="FootnoteReference"/></w:rPr><w:footnoteRef/></w:r><w:r><w:t xml:space="preserve"> First footnote.</w:t></w:r></w:p></w:footnote>"#,
            r#"<w:footnote w:id="2"><w:p><w:pPr><w:pStyle w:val="FootnoteText"/></w:pPr><w:r><w:rPr><w:rStyle w:val="FootnoteReference"/></w:rPr><w:footnoteRef/></w:r><w:r><w:t xml:space="preserve"> Second footnote.</w:t></w:r></w:p></w:footnote>"#,
            r#"<w:footnote w:id="3"><w:p><w:pPr><w:pStyle w:val="FootnoteText"/></w:pPr><w:r><w:rPr><w:rStyle w:val="FootnoteReference"/></w:rPr><w:footnoteRef/></w:r><w:r><w:t xml:space="preserve"> Third footnote, </w:t></w:r><w:r><w:rPr><w:i/></w:rPr><w:t xml:space="preserve">italic</w:t></w:r><w:r><w:t xml:space="preserve">.</w:t></w:r></w:p><w:p><w:pPr><w:pStyle w:val="FootnoteText"/></w:pPr><w:r><w:t xml:space="preserve">Second paragraph of the third.</w:t></w:r></w:p></w:footnote>"#,
            "</w:footnotes>",
        ),
        w14 = NOTES_W14_NS,
    )
}

fn notes_fixture_endnotes_xml() -> &'static str {
    concat!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
        "\n",
        r#"<w:endnotes xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">"#,
        r#"<w:endnote w:type="separator" w:id="-1"><w:p><w:r><w:separator/></w:r></w:p></w:endnote>"#,
        r#"<w:endnote w:type="continuationSeparator" w:id="0"><w:p><w:r><w:continuationSeparator/></w:r></w:p></w:endnote>"#,
        r#"<w:endnote w:id="1"><w:p><w:pPr><w:pStyle w:val="EndnoteText"/></w:pPr><w:r><w:rPr><w:rStyle w:val="EndnoteReference"/></w:rPr><w:endnoteRef/></w:r><w:r><w:t xml:space="preserve"> First endnote.</w:t></w:r></w:p></w:endnote>"#,
        r#"<w:endnote w:id="2"><w:p><w:pPr><w:pStyle w:val="EndnoteText"/></w:pPr><w:r><w:rPr><w:rStyle w:val="EndnoteReference"/></w:rPr><w:endnoteRef/></w:r><w:r><w:t xml:space="preserve"> Second endnote.</w:t></w:r></w:p></w:endnote>"#,
        "</w:endnotes>",
    )
}

fn build_footnotes_endnotes_docx() -> Vec<u8> {
    use std::io::Write;
    use zip::write::{SimpleFileOptions, ZipWriter};
    let document_xml = notes_fixture_document_xml();
    let footnotes_xml = notes_fixture_footnotes_xml();
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
<Override PartName="/word/footnotes.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footnotes+xml"/>
<Override PartName="/word/endnotes.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.endnotes+xml"/>
<Override PartName="/word/settings.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.settings+xml"/>
</Types>"#;
    let dot_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footnotes" Target="footnotes.xml"/>
<Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/endnotes" Target="endnotes.xml"/>
<Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/settings" Target="settings.xml"/>
</Relationships>"#;
    /* Document-level note properties: lower-roman endnotes so the
    marker derivation is observable ("i", "ii"). */
    let settings = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:settings xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:footnotePr><w:footnote w:id="-1"/><w:footnote w:id="0"/></w:footnotePr><w:endnotePr><w:numFmt w:val="lowerRoman"/><w:endnote w:id="-1"/><w:endnote w:id="0"/></w:endnotePr></w:settings>"#;
    let mut buf: Vec<u8> = Vec::new();
    {
        let mut zip = ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(0o644);
        for (name, body) in [
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", dot_rels),
            ("word/_rels/document.xml.rels", doc_rels),
            ("word/document.xml", document_xml.as_str()),
            ("word/footnotes.xml", footnotes_xml.as_str()),
            ("word/endnotes.xml", notes_fixture_endnotes_xml()),
            ("word/settings.xml", settings),
        ] {
            zip.start_file(name, opts).unwrap();
            zip.write_all(body.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    buf
}

fn entry_bytes<'a>(archive: &'a DocxArchive, name: &str) -> Option<&'a [u8]> {
    archive
        .other_entries
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, b)| b.as_slice())
}

/// Issue #80 — step 11: the note story round-trip contract.
///
/// 1. The parts parse into stories (3 footnotes + 2 endnotes + the four
///    separator sentinels), references land as inline anchors and the
///    document-order markers derive (endnotes lower-roman per
///    `settings.xml`).
/// 2. A DIRTY body paragraph carrying a reference saves the reference
///    back (`<w:footnoteReference w:id>`), and both note parts pass
///    through byte-identical.
/// 3. Editing ONE footnote regenerates `footnotes.xml` only: the
///    untouched entries ride their captured bytes verbatim, the edited
///    one carries the edit, `endnotes.xml` is untouched.
/// 4. Inserting a footnote at the caret mints the next id, splices the
///    anchor, renumbers every later marker, and round-trips.
/// 5. A document that never had a note part gets one synthesized
///    (part + content type + relationship) on save.
fn run_notes_roundtrip() -> Result<()> {
    use engine::{BlockPath, InlineKind, LogicalPos, NoteAnchor, NoteKind, NoteType};

    let fixture_bytes = build_footnotes_endnotes_docx();
    let a = read_docx(&fixture_bytes).context("read notes fixture")?;
    let doc_a = &a.document;
    let normal = |m: &std::collections::HashMap<i32, engine::NoteStory>| {
        m.values()
            .filter(|s| s.note_type == NoteType::Normal)
            .count()
    };
    if normal(&doc_a.footnote_stories) != 3 || doc_a.footnote_stories.len() != 5 {
        bail!(
            "footnote stories: {} normal / {} total",
            normal(&doc_a.footnote_stories),
            doc_a.footnote_stories.len()
        );
    }
    if normal(&doc_a.endnote_stories) != 2 || doc_a.endnote_stories.len() != 4 {
        bail!("endnote stories: {}", doc_a.endnote_stories.len());
    }
    let refs = doc_a.note_references();
    let ids: Vec<(NoteKind, u32)> = refs.iter().map(|r| (r.anchor.kind, r.anchor.id)).collect();
    let want = vec![
        (NoteKind::Footnote, 1),
        (NoteKind::Footnote, 2),
        (NoteKind::Endnote, 1),
        (NoteKind::Footnote, 3),
        (NoteKind::Endnote, 2),
    ];
    if ids != want {
        bail!("note references in document order: {ids:?}");
    }
    let markers = doc_a.note_markers();
    let mark = |kind: NoteKind, id: u32| {
        markers
            .get(&NoteAnchor { kind, id })
            .cloned()
            .unwrap_or_default()
    };
    if mark(NoteKind::Footnote, 3) != "3" || mark(NoteKind::Endnote, 2) != "ii" {
        bail!(
            "markers: fn3=`{}` en2=`{}`",
            mark(NoteKind::Footnote, 3),
            mark(NoteKind::Endnote, 2)
        );
    }
    let third = &doc_a.footnote_stories[&3];
    if third.body.len() != 2 || third.source_xml.is_none() || third.dirty {
        bail!("third footnote story shape: {} blocks", third.body.len());
    }
    println!("[roundtrip] step 11a OK — note parts parse into stories, markers derive");

    /* 2. Dirty body paragraph → reference survives, parts pass through. */
    let edited = doc_a.insert_text(
        LogicalPos {
            path: BlockPath::top(0),
            offset: "Alpha".len() as u32,
        },
        INSERT_TEXT,
    );
    let bytes_b = write_docx(&a, &edited).context("write dirty-paragraph doc")?;
    assert_document_xml_well_formed(&bytes_b)?;
    let b = read_docx(&bytes_b).context("re-read dirty-paragraph doc")?;
    let p0 = b.document.blocks[0].as_paragraph().context("paragraph 0")?;
    let has_ref = p0
        .inline_objects
        .iter()
        .any(|o| matches!(o.kind, InlineKind::FootnoteRef { id: 1, .. }));
    if !has_ref {
        bail!(
            "footnote reference dropped on dirty-paragraph save: {:?}",
            p0.inline_objects
        );
    }
    if !p0.text.contains(INSERT_TEXT) {
        bail!("edit lost: {:?}", p0.text);
    }
    for part in [
        "word/footnotes.xml",
        "word/endnotes.xml",
        "word/settings.xml",
    ] {
        if entry_bytes(&a, part) != entry_bytes(&b, part) {
            bail!("`{part}` drifted on a body-only edit");
        }
    }
    if b.document.note_references().len() != 5 {
        bail!(
            "reference count after save: {}",
            b.document.note_references().len()
        );
    }
    println!(
        "[roundtrip] step 11b OK — reference survives a dirty-paragraph save, note parts passthrough"
    );

    /* 3. Edit footnote 2 → only footnotes.xml regenerates; untouched
    entries verbatim. */
    let story2 = &doc_a.footnote_stories[&2];
    let story_doc = DocumentTree::from_blocks(story2.body.iter().cloned());
    let story_doc = story_doc.insert_text(
        LogicalPos {
            path: BlockPath::top(0),
            offset: story_doc.paragraph_text(0).map_or(0, |t| t.len() as u32),
        },
        " (edited)",
    );
    let new_body: Vec<engine::Block> = story_doc.blocks.iter().cloned().collect();
    let edited2 = doc_a.with_updated_note_story(NoteKind::Footnote, 2, new_body);
    if !edited2.notes_dirty.footnotes || edited2.notes_dirty.endnotes {
        bail!("notes_dirty flags: {:?}", edited2.notes_dirty);
    }
    let bytes_c = write_docx(&a, &edited2).context("write edited-note doc")?;
    assert_document_xml_well_formed(&bytes_c)?;
    let c = read_docx(&bytes_c).context("re-read edited-note doc")?;
    let fn_a = entry_bytes(&a, "word/footnotes.xml").context("fixture footnotes.xml")?;
    let fn_c = entry_bytes(&c, "word/footnotes.xml").context("saved footnotes.xml")?;
    if fn_a == fn_c {
        bail!("footnotes.xml was not regenerated after a note edit");
    }
    let fn_c_str = std::str::from_utf8(fn_c).context("utf8 footnotes.xml")?;
    for id in [-1, 0, 1, 3] {
        let raw = doc_a.footnote_stories[&id]
            .source_xml
            .as_deref()
            .context("source bytes")?;
        let raw = std::str::from_utf8(raw)?;
        if !fn_c_str.contains(raw) {
            bail!("untouched footnote {id} not verbatim in the regenerated part:\n{fn_c_str}");
        }
    }
    if !fn_c_str.contains("Second footnote. (edited)")
        || fn_c_str.contains("Second footnote.</w:t>")
    {
        bail!("edited footnote text not regenerated:\n{fn_c_str}");
    }
    if !fn_c_str.contains(r#"xmlns:w14=""#) {
        bail!("regenerated footnotes.xml lost the root's w14 binding");
    }
    if entry_bytes(&a, "word/endnotes.xml") != entry_bytes(&c, "word/endnotes.xml") {
        bail!("endnotes.xml drifted on a footnote edit");
    }
    if extract_doc_xml(&fixture_bytes)? != extract_doc_xml(&bytes_c)? {
        bail!("document.xml drifted on a note-only edit");
    }
    let c2 = c.document.footnote_stories[&2]
        .body
        .first()
        .and_then(engine::Block::as_paragraph)
        .map(|p| p.text.clone())
        .unwrap_or_default();
    if !c2.ends_with("Second footnote. (edited)") {
        bail!("re-read edited footnote: {c2:?}");
    }
    if !c.document.footnote_stories[&2].body[0]
        .as_paragraph()
        .is_some_and(|p| {
            p.inline_objects
                .iter()
                .any(|o| matches!(o.kind, InlineKind::NoteSelfRef { .. }))
        })
    {
        bail!("regenerated footnote lost its <w:footnoteRef/> self-mark");
    }
    println!(
        "[roundtrip] step 11c OK — one edited note regenerates its part only, siblings verbatim"
    );

    /* 4. Insert a footnote at the caret → id 4, renumbered markers. */
    let (with_new, new_id) = c.document.insert_note_at(
        LogicalPos {
            path: BlockPath::top(1),
            offset: "Beta".len() as u32,
        },
        NoteKind::Footnote,
    );
    if new_id != 4 {
        bail!("fresh footnote id: {new_id}");
    }
    let markers = with_new.note_markers();
    let m = |id: u32| {
        markers
            .get(&NoteAnchor {
                kind: NoteKind::Footnote,
                id,
            })
            .cloned()
            .unwrap_or_default()
    };
    if (m(1), m(4), m(2), m(3)) != ("1".into(), "2".into(), "3".into(), "4".into()) {
        bail!(
            "renumbered markers: 1=`{}` 4=`{}` 2=`{}` 3=`{}`",
            m(1),
            m(4),
            m(2),
            m(3)
        );
    }
    let bytes_d = write_docx(&c, &with_new).context("write inserted-note doc")?;
    assert_document_xml_well_formed(&bytes_d)?;
    let d = read_docx(&bytes_d).context("re-read inserted-note doc")?;
    if normal(&d.document.footnote_stories) != 4 || d.document.note_references().len() != 6 {
        bail!(
            "after insert: {} footnotes / {} references",
            normal(&d.document.footnote_stories),
            d.document.note_references().len()
        );
    }
    let fresh_story = &d.document.footnote_stories[&4];
    if !fresh_story.body[0].as_paragraph().is_some_and(|p| {
        p.inline_objects
            .iter()
            .any(|o| matches!(o.kind, InlineKind::NoteSelfRef { .. }))
    }) {
        bail!("inserted footnote body lacks its self-mark");
    }
    println!(
        "[roundtrip] step 11d OK — insert footnote at caret mints id 4, renumbers, round-trips"
    );

    /* 5. A note-less document synthesizes the part on save. */
    let fresh = DocumentTree::from_text("fresh document");
    let (fresh, id) = fresh.insert_note_at(
        LogicalPos {
            path: BlockPath::top(0),
            offset: "fresh".len() as u32,
        },
        NoteKind::Footnote,
    );
    if id != 1 {
        bail!("first footnote id in a fresh document: {id}");
    }
    let fresh_bytes = build_minimal_docx(&fresh).context("build fresh docx")?;
    assert_document_xml_well_formed(&fresh_bytes)?;
    let e = read_docx(&fresh_bytes).context("re-read fresh docx")?;
    let ct = std::str::from_utf8(entry_bytes(&e, "[Content_Types].xml").context("ct")?)?;
    if !ct.contains("/word/footnotes.xml") {
        bail!("[Content_Types].xml lacks the synthesized footnotes override");
    }
    let rels =
        std::str::from_utf8(entry_bytes(&e, "word/_rels/document.xml.rels").context("rels")?)?;
    if !rels.contains("relationships/footnotes") {
        bail!("document.xml.rels lacks the footnotes relationship");
    }
    if normal(&e.document.footnote_stories) != 1 || e.document.note_references().len() != 1 {
        bail!(
            "fresh document after save: {} footnotes / {} references",
            normal(&e.document.footnote_stories),
            e.document.note_references().len()
        );
    }
    let fn_e = std::str::from_utf8(entry_bytes(&e, "word/footnotes.xml").context("footnotes")?)?;
    if !fn_e.contains(r#"w:type="separator""#)
        || !fn_e.contains(r#"w:type="continuationSeparator""#)
    {
        bail!("synthesized footnotes.xml lacks Word's stock separators:\n{fn_e}");
    }
    println!("[roundtrip] step 11e OK — a fresh document synthesizes footnotes.xml + OPC plumbing");
    Ok(())
}

/* ============================== UI save path root bindings (#100) ==== */

/// Issue #100 — the root Word writes on every `word/document.xml`: `w`
/// first (so the writer's synthesized root re-emits it byte-identically),
/// then the foreign bindings passthrough paragraphs lean on and the
/// `mc:Ignorable` list naming them.
const WORD_ROOT_OPEN: &str = concat!(
    r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" "#,
    r#"xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006" "#,
    r#"xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml" "#,
    r#"xmlns:w15="http://schemas.microsoft.com/office/word/2012/wordml" "#,
    r#"mc:Ignorable="w14 w15">"#,
);

/// Issue #100 fixture: a Word-authored-shaped body — every `<w:p>` (body
/// AND table cell) carries `w14:paraId` / `w14:textId` exactly like Word
/// writes them, bound only on the root.
fn w14_paraid_document_xml() -> String {
    format!(
        concat!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
            "\n",
            "{root}",
            "<w:body>",
            r#"<w:p w14:paraId="1A2B3C4D" w14:textId="0E0F1A2B" w:rsidR="00AB12CD" w:rsidRDefault="00AB12CD">"#,
            r#"<w:r><w:t xml:space="preserve">first paragraph</w:t></w:r></w:p>"#,
            r#"<w:p w14:paraId="2B3C4D5E" w14:textId="1F2A3B4C" w:rsidR="00AB12CD" w:rsidRDefault="00AB12CD">"#,
            r#"<w:r><w:rPr><w:b/></w:rPr><w:t xml:space="preserve">second paragraph</w:t></w:r></w:p>"#,
            r#"<w:tbl><w:tblGrid><w:gridCol w:w="2880"/><w:gridCol w:w="2880"/></w:tblGrid><w:tr>"#,
            r#"<w:tc><w:p w14:paraId="3C4D5E6F" w14:textId="2A3B4C5D"><w:r><w:t xml:space="preserve">cell a</w:t></w:r></w:p></w:tc>"#,
            r#"<w:tc><w:p w14:paraId="4D5E6F70" w14:textId="3B4C5D6E"><w:r><w:t xml:space="preserve">cell b</w:t></w:r></w:p></w:tc>"#,
            "</w:tr></w:tbl>",
            r#"<w:p w14:paraId="5E6F7081" w14:textId="4C5D6E7F"><w:r><w:t xml:space="preserve">after</w:t></w:r></w:p>"#,
            "{sect_pr}</w:body></w:document>",
        ),
        root = WORD_ROOT_OPEN,
        sect_pr = A4_SECT_PR_EXPLICIT,
    )
}

fn build_w14_paraid_docx() -> Vec<u8> {
    package_document_xml(&w14_paraid_document_xml())
}

/// Issue #100 — step 12: the live editor saves through
/// `build_minimal_docx` (engine-wasm `SaveDocx` / `SaveDocument`), NOT
/// `write_docx` with the source archive. Open the Word-shaped fixture,
/// edit one body paragraph AND one table cell (a dirty table regenerates
/// around its clean `w14:paraId` cell paragraphs), save through the UI
/// path and require: the part is namespace-well-formed, the root still
/// binds `w14` + `mc:Ignorable`, untouched paragraphs keep their
/// `w14:paraId` bytes, and the UI path's `document.xml` equals the
/// archive path's byte for byte.
fn run_ui_save_root_bindings() -> Result<()> {
    use engine::{BlockPath, LogicalPos, PathStep};

    let fixture_bytes = build_w14_paraid_docx();
    let archive_a = read_docx(&fixture_bytes).context("read w14 fixture")?;
    if archive_a.document.document_root_attrs != archive_a.document_root_attrs {
        bail!(
            "DocumentTree must carry the source root attrs: tree {:?} vs archive {:?}",
            archive_a.document.document_root_attrs,
            archive_a.document_root_attrs
        );
    }
    let edited = archive_a
        .document
        .insert_text(
            LogicalPos {
                path: BlockPath::top(0),
                offset: "first".len() as u32,
            },
            INSERT_TEXT,
        )
        .insert_text(
            LogicalPos {
                path: BlockPath::top(2)
                    .push(PathStep::Cell { row: 0, col: 0 })
                    .push(PathStep::Block(0)),
                offset: "cell a".len() as u32,
            },
            INSERT_TEXT,
        );
    let ui_bytes = build_minimal_docx(&edited).context("UI-path save")?;
    assert_document_xml_well_formed(&ui_bytes).context("UI-path save of a w14 document")?;
    let doc_ui = String::from_utf8(extract_doc_xml(&ui_bytes)?).context("utf8 UI output")?;
    for needle in [
        r#"xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml""#,
        r#"mc:Ignorable="w14 w15""#,
        r#"<w:p w14:paraId="2B3C4D5E" w14:textId="1F2A3B4C" w:rsidR="00AB12CD" w:rsidRDefault="00AB12CD">"#,
        r#"<w:p w14:paraId="4D5E6F70" w14:textId="3B4C5D6E">"#,
        r#"<w:p w14:paraId="5E6F7081" w14:textId="4C5D6E7F">"#,
    ] {
        if !doc_ui.contains(needle) {
            bail!("UI-path save lost `{needle}`\n--- got ---\n{doc_ui}");
        }
    }
    let archive_bytes = write_docx(&archive_a, &edited).context("archive-path save")?;
    let doc_archive = String::from_utf8(extract_doc_xml(&archive_bytes)?).context("utf8")?;
    if doc_ui != doc_archive {
        bail!(
            "UI-path document.xml differs from the archive path\n--- archive ---\n{doc_archive}\n--- ui ---\n{doc_ui}"
        );
    }
    let reread = read_docx(&ui_bytes).context("re-read UI save")?;
    let expected = format!("first{INSERT_TEXT} paragraph");
    if reread.document.paragraph_text(0) != Some(expected.as_str()) {
        bail!(
            "UI save lost the edit: {:?}",
            reread.document.paragraph_text(0)
        );
    }
    if reread.document.document_root_attrs != archive_a.document_root_attrs {
        bail!("root attrs did not survive the UI save");
    }
    println!("[roundtrip] step 12 OK — UI save path re-declares the source root bindings");
    Ok(())
}

/* ================================== table cell runs (#101) ============ */

/// Issue #101 — a 1×2 table whose cells carry mixed run formatting (bold,
/// italic + colour + size, a raw font + an unmodeled `<w:lang>` grab-bag
/// child, underline) and an inline picture. Authored in the writer's own
/// canonical shape (the drawing root, schema-ordered `<w:rPr>` children,
/// the writer's `<wp:inline>` form) so a regenerated cell paragraph is
/// byte-identical to its source except for the edit itself.
fn table_cell_runs_document_xml() -> String {
    concat!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
        "\n",
        r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" "#,
        r#"xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" "#,
        r#"xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" "#,
        r#"xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" "#,
        r#"xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture" "#,
        r#"xmlns:wp14="http://schemas.microsoft.com/office/word/2010/wordprocessingDrawing">"#,
        "<w:body>",
        r#"<w:p><w:r><w:t xml:space="preserve">intro</w:t></w:r></w:p>"#,
        r#"<w:tbl><w:tblGrid><w:gridCol w:w="2880"/><w:gridCol w:w="2880"/></w:tblGrid><w:tr>"#,
        "<w:tc><w:p>",
        r#"<w:r><w:rPr><w:b/></w:rPr><w:t xml:space="preserve">Bold</w:t></w:r>"#,
        r#"<w:r><w:t xml:space="preserve"> plain </w:t></w:r>"#,
        r#"<w:r><w:rPr><w:i/><w:color w:val="FF0000"/><w:sz w:val="28"/><w:szCs w:val="28"/></w:rPr>"#,
        r#"<w:t xml:space="preserve">red italic</w:t></w:r>"#,
        r#"<w:r><w:rPr><w:rFonts w:ascii="Georgia" w:hAnsi="Georgia" w:cs="Georgia"/><w:lang w:val="en-GB"/></w:rPr>"#,
        r#"<w:t xml:space="preserve">serif</w:t></w:r>"#,
        "</w:p></w:tc>",
        "<w:tc><w:p>",
        r#"<w:r><w:rPr><w:u w:val="single"/></w:rPr><w:t xml:space="preserve">under</w:t></w:r>"#,
        "<w:r><w:drawing>",
        r#"<wp:inline distT="0" distB="0" distL="0" distR="0">"#,
        r#"<wp:extent cx="914400" cy="457200"/>"#,
        r#"<wp:effectExtent l="0" t="0" r="0" b="0"/>"#,
        r#"<wp:docPr id="1" name="Picture"/>"#,
        "<wp:cNvGraphicFramePr/>",
        "<a:graphic>",
        r#"<a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture">"#,
        "<pic:pic>",
        r#"<pic:nvPicPr><pic:cNvPr id="0" name="Image"/><pic:cNvPicPr/></pic:nvPicPr>"#,
        "<pic:blipFill>",
        r#"<a:blip r:embed="rId5"/>"#,
        "<a:stretch><a:fillRect/></a:stretch>",
        "</pic:blipFill>",
        "<pic:spPr>",
        r#"<a:xfrm><a:off x="0" y="0"/><a:ext cx="914400" cy="457200"/></a:xfrm>"#,
        r#"<a:prstGeom prst="rect"><a:avLst/></a:prstGeom>"#,
        "</pic:spPr>",
        "</pic:pic>",
        "</a:graphicData>",
        "</a:graphic>",
        "</wp:inline></w:drawing></w:r>",
        r#"<w:r><w:t xml:space="preserve">pic</w:t></w:r>"#,
        "</w:p></w:tc>",
        "</w:tr></w:tbl>",
        r#"<w:p><w:r><w:t xml:space="preserve">after</w:t></w:r></w:p>"#,
        "<w:sectPr/></w:body></w:document>",
    )
    .to_string()
}

/// Issue #101 fixture: `table_cell_runs_document_xml` + the picture blob
/// (`word/media/image1.png`, the 8-byte PNG signature) behind `rId5`.
fn build_table_cell_runs_docx() -> Vec<u8> {
    use std::io::Write;
    use zip::write::{SimpleFileOptions, ZipWriter};
    let document_xml = table_cell_runs_document_xml();
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Default Extension="png" ContentType="image/png"/>
<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#;
    let dot_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId5" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/image1.png"/>
</Relationships>"#;
    let png_signature: &[u8] = &[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
    let mut buf: Vec<u8> = Vec::new();
    {
        let mut zip = ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(0o644);
        for (name, body) in [
            ("[Content_Types].xml", content_types.as_bytes()),
            ("_rels/.rels", dot_rels.as_bytes()),
            ("word/_rels/document.xml.rels", doc_rels.as_bytes()),
            ("word/document.xml", document_xml.as_bytes()),
            ("word/media/image1.png", png_signature),
        ] {
            zip.start_file(name, opts).unwrap();
            zip.write_all(body).unwrap();
        }
        zip.finish().unwrap();
    }
    buf
}

/// Issue #101 — step 13: edit BOTH cells (inside the plain run of the
/// mixed-format cell, and after the picture in the other), save, and
/// require the regenerated `document.xml` to be EXACTLY the source plus
/// the two inserts — every other run's `<w:rPr>` (grab-bag `<w:lang>`
/// included) and the cell picture survive byte-for-byte — then re-read
/// and check the typed model: styled spans + the picture's inline object.
fn run_table_cell_runs_survival() -> Result<()> {
    use engine::{BlockPath, InlineKind, LogicalPos, PathStep};

    let fixture_bytes = build_table_cell_runs_docx();
    let archive_a = read_docx(&fixture_bytes).context("read cell-runs fixture")?;
    let cell_path = |col: u32| {
        BlockPath::top(1)
            .push(PathStep::Cell { row: 0, col })
            .push(PathStep::Block(0))
    };
    let cell_para = |doc: &DocumentTree, col: usize| -> Result<Paragraph> {
        let t = doc.blocks[1].as_table().context("block 1 is the table")?;
        t.rows[0].cells[col].blocks[0]
            .as_paragraph()
            .cloned()
            .context("cell paragraph")
    };
    let c0 = cell_para(&archive_a.document, 0)?;
    if c0.text != "Bold plain red italicserif" {
        bail!("cell 0 text: {:?}", c0.text);
    }
    let styled = c0
        .spans
        .iter()
        .filter(|s| s.style != engine::SpanStyle::default())
        .count();
    if styled != 3 {
        bail!("cell 0 must carry 3 styled spans, got {:?}", c0.spans);
    }
    let c1 = cell_para(&archive_a.document, 1)?;
    let has_pic = c1.inline_objects.iter().any(|o| {
        matches!(
            &o.kind,
            InlineKind::Image { rel_id, width_emu: 914400, height_emu: 457200, .. } if rel_id == "rId5"
        )
    });
    if !has_pic {
        bail!("cell 1 picture not read: {:?}", c1.inline_objects);
    }

    let pic_offset = ("under".len() + '\u{FFFC}'.len_utf8() + 1) as u32;
    let edited = archive_a
        .document
        .insert_text(
            LogicalPos {
                path: cell_path(0),
                offset: "Bold pl".len() as u32,
            },
            INSERT_TEXT,
        )
        .insert_text(
            LogicalPos {
                path: cell_path(1),
                offset: pic_offset,
            },
            INSERT_TEXT,
        );
    let edited_bytes = write_docx(&archive_a, &edited).context("write edited cell runs")?;
    assert_document_xml_well_formed(&edited_bytes).context("edited cell-runs .docx")?;
    let ui_bytes = build_minimal_docx(&edited).context("UI-path save of cell runs")?;
    assert_document_xml_well_formed(&ui_bytes).context("UI-path cell-runs .docx")?;

    let doc_a = String::from_utf8(extract_doc_xml(&fixture_bytes)?).context("utf8 source")?;
    let doc_b = String::from_utf8(extract_doc_xml(&edited_bytes)?).context("utf8 output")?;
    let expected = doc_a
        .replacen(
            r#"<w:t xml:space="preserve"> plain </w:t>"#,
            &format!(r#"<w:t xml:space="preserve"> pl{INSERT_TEXT}ain </w:t>"#),
            1,
        )
        .replacen(
            r#"<w:t xml:space="preserve">pic</w:t>"#,
            &format!(r#"<w:t xml:space="preserve">p{INSERT_TEXT}ic</w:t>"#),
            1,
        );
    if doc_b != expected {
        bail!(
            "regenerated cells are not source + edits\n--- expected ---\n{expected}\n--- got ---\n{doc_b}"
        );
    }
    let drift = (doc_b.len() as isize - doc_a.len() as isize).unsigned_abs();
    let inserted = 2 * INSERT_TEXT.len();
    if drift > 2 * inserted {
        bail!(
            "cell-runs document.xml drift {drift} B exceeds bound {} B",
            2 * inserted
        );
    }

    let archive_b = read_docx(&edited_bytes).context("re-read edited cell runs")?;
    let c0b = cell_para(&archive_b.document, 0)?;
    let styles_a: Vec<_> = c0.spans.iter().map(|s| s.style.clone()).collect();
    let styles_b: Vec<_> = c0b.spans.iter().map(|s| s.style.clone()).collect();
    if styles_a != styles_b {
        bail!("cell 0 run styles drifted: {styles_a:?} vs {styles_b:?}");
    }
    let c1b = cell_para(&archive_b.document, 1)?;
    /* Issue #188 — media is keyed by the part-resolved target path. */
    let pic_key = c1b
        .inline_objects
        .first()
        .and_then(|o| o.kind.image_media_key());
    if c1b.inline_objects.len() != 1
        || !pic_key.is_some_and(|k| archive_b.document.media.contains_key(k))
    {
        bail!("cell picture lost on save: {:?}", c1b.inline_objects);
    }
    println!(
        "[roundtrip] step 13 OK — table cell runs, grab bags and pictures survive a cell edit (Δ {drift} B)"
    );
    Ok(())
}

/* ====================================================== RTL tables (#79) ==== */

/// The `<w:tbl>…</w:tbl>` region of a `document.xml` string.
fn table_region(xml: &str) -> Result<&str> {
    let start = xml.find("<w:tbl>").context("no <w:tbl>")?;
    let end = xml.rfind("</w:tbl>").context("no </w:tbl>")? + "</w:tbl>".len();
    Ok(&xml[start..end])
}

/// Issue #79 — step 17: the `<w:bidiVisual>` round-trip contract on the
/// `table_in_rtl_doc.docx` fixture (a 2-column bidiVisual table).
///
/// a. The flag is MODELED on read (`TableProperties::bidi_visual`), not
///    carried in the tblPr grab bag.
/// b. An untouched save keeps the table region byte-identical
///    (passthrough).
/// c. Typing into the visually RIGHTMOST cell (logical grid cell 1)
///    dirties the table: the regenerated table is exactly the source
///    table plus the insert — `<w:bidiVisual/>` emitted once, at its
///    schema rank — on both save paths, and the text lands in grid
///    cell 1 of the saved file.
/// d. Toggling the flag through the engine (`set_table_bidi_visual`)
///    removes / adds exactly the element.
fn run_rtl_table_roundtrip() -> Result<()> {
    use engine::{BlockPath, LogicalPos, PathStep};

    let fixture_bytes = build_table_in_rtl_doc_docx();
    let archive_a = read_docx(&fixture_bytes).context("read RTL table fixture")?;
    let table = archive_a.document.blocks[1]
        .as_table()
        .context("block 1 is the table")?;
    if !table.props.bidi_visual {
        bail!("<w:bidiVisual/> not modeled on read");
    }
    if table.props.grab_bag.is_some() {
        bail!(
            "bidiVisual must not ride the tblPr grab bag: {:?}",
            table.props.grab_bag
        );
    }
    let doc_a = String::from_utf8(extract_doc_xml(&fixture_bytes)?).context("utf8 source")?;
    let src_tbl = table_region(&doc_a)?.to_string();

    /* b. untouched save — the table passes through verbatim. */
    let untouched = write_docx(&archive_a, &archive_a.document).context("untouched save")?;
    assert_document_xml_well_formed(&untouched).context("untouched RTL table .docx")?;
    let doc_u = String::from_utf8(extract_doc_xml(&untouched)?).context("utf8 untouched")?;
    if table_region(&doc_u)? != src_tbl {
        bail!("untouched RTL table drifted:\n{}", table_region(&doc_u)?);
    }
    println!(
        "[roundtrip] step 17a OK — bidiVisual modeled on read, untouched table byte-identical"
    );

    /* c. edit grid cell 1 (the visually rightmost cell). */
    let cell0 = BlockPath::top(1)
        .push(PathStep::Cell { row: 0, col: 0 })
        .push(PathStep::Block(0));
    let edited = archive_a.document.insert_text(
        LogicalPos {
            path: cell0,
            offset: "يمين".len() as u32,
        },
        INSERT_TEXT,
    );
    let expected_tbl = src_tbl.replacen("يمين", &format!("يمين{INSERT_TEXT}"), 1);
    for (label, bytes) in [
        (
            "write_docx",
            write_docx(&archive_a, &edited).context("write edited RTL table")?,
        ),
        (
            "build_minimal_docx",
            build_minimal_docx(&edited).context("UI-path save of RTL table")?,
        ),
    ] {
        assert_document_xml_well_formed(&bytes).with_context(|| format!("{label} RTL table"))?;
        let xml = String::from_utf8(extract_doc_xml(&bytes)?).context("utf8 edited")?;
        let tbl = table_region(&xml)?;
        if tbl != expected_tbl {
            bail!(
                "{label}: regenerated RTL table is not source + edit\n--- expected ---\n{expected_tbl}\n--- got ---\n{tbl}"
            );
        }
        let back = read_docx(&bytes).with_context(|| format!("{label}: re-read"))?;
        let t = back.document.blocks[1].as_table().context("table")?;
        let first = t.rows[0].cells[0].blocks[0]
            .as_paragraph()
            .context("cell paragraph")?;
        if !t.props.bidi_visual || first.text != format!("يمين{INSERT_TEXT}") {
            bail!("{label}: flag / grid-cell-1 text lost: {:?}", first.text);
        }
    }
    let drift = expected_tbl.len() - src_tbl.len();
    println!(
        "[roundtrip] step 17b OK — dirty RTL table regenerates source + edit, flag emitted once (Δ {drift} B)"
    );

    /* d. the flag itself is authorable. */
    let off = edited.set_table_bidi_visual(BlockPath::top(1), false);
    let off_bytes = write_docx(&archive_a, &off).context("write flag-off table")?;
    let off_xml = String::from_utf8(extract_doc_xml(&off_bytes)?).context("utf8 off")?;
    if table_region(&off_xml)? != expected_tbl.replacen("<w:tblPr><w:bidiVisual/></w:tblPr>", "", 1)
    {
        bail!(
            "flag off must drop exactly the element:\n{}",
            table_region(&off_xml)?
        );
    }
    let on = off.set_table_bidi_visual(BlockPath::top(1), true);
    let on_bytes = write_docx(&archive_a, &on).context("write flag-on table")?;
    let on_xml = String::from_utf8(extract_doc_xml(&on_bytes)?).context("utf8 on")?;
    if table_region(&on_xml)? != expected_tbl {
        bail!(
            "flag on must restore the element:\n{}",
            table_region(&on_xml)?
        );
    }
    println!(
        "[roundtrip] step 17c OK — toggling bidiVisual adds / removes exactly <w:bidiVisual/>"
    );
    Ok(())
}

/* ================================== style-inherited direction (#202) ==== */

/// Issue #202 — `RtlBase` sets `<w:bidi/>`; `RtlBody` inherits it via
/// `basedOn` (the Arabic-template "RTL Body" shape).
const STYLE_BIDI_STYLES_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
<w:style w:type="paragraph" w:styleId="RtlBase"><w:name w:val="RTL Base"/><w:pPr><w:bidi/></w:pPr></w:style>
<w:style w:type="paragraph" w:styleId="RtlBody"><w:name w:val="RTL Body"/><w:basedOn w:val="RtlBase"/></w:style>
</w:styles>"#;

/// The three paragraphs of `pPr_bidi_style.docx`, byte-for-byte: (0) a
/// style-RTL paragraph that starts with a Latin word, (1) the same style
/// under a direct `<w:bidi w:val="false"/>`, (2) an unstyled paragraph.
/// Written in the writer's own regeneration shape, so an edited
/// paragraph regenerates to exactly source + insert.
const STYLE_BIDI_PARAGRAPHS: [&str; 3] = [
    r#"<w:p><w:pPr><w:pStyle w:val="RtlBody"/></w:pPr><w:r><w:t xml:space="preserve">Word مرحبا</w:t></w:r></w:p>"#,
    r#"<w:p><w:pPr><w:pStyle w:val="RtlBody"/><w:bidi w:val="false"/></w:pPr><w:r><w:t xml:space="preserve">Word مرحبا</w:t></w:r></w:p>"#,
    r#"<w:p><w:r><w:t xml:space="preserve">plain</w:t></w:r></w:p>"#,
];

fn build_style_bidi_docx() -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{}{BARE_SECT_PR}</w:body></w:document>"#,
        STYLE_BIDI_PARAGRAPHS.concat()
    );
    build_styled_docx(STYLE_BIDI_STYLES_XML, &document_xml)
}

/// Issue #202 — step 23: paragraph direction inherited from a style.
///
/// a. `bidi` resolves through the `basedOn` chain on read (RTL although
///    the text starts with a Latin word) without becoming a direct
///    override; a direct `w:val="false"` beats the style.
/// b. An untouched save is byte-identical.
/// c. Editing the style-RTL paragraph regenerates exactly source +
///    insert — it does NOT gain a direct `<w:bidi/>` — the other two
///    paragraphs pass through byte-identical, and the re-read still
///    resolves RTL from styles.xml.
fn run_style_bidi_roundtrip() -> Result<()> {
    use engine::{BlockPath, LogicalPos, TextDirection};

    type Dirs = Vec<(Option<TextDirection>, Option<TextDirection>)>;
    let dirs = |doc: &DocumentTree| -> Dirs {
        (0..3)
            .filter_map(|i| doc.nth_paragraph(i))
            .map(|p| (p.props.direction, p.direct_overrides.direction))
            .collect()
    };
    let expected: Dirs = vec![
        (Some(TextDirection::Rtl), None),
        (Some(TextDirection::Ltr), Some(TextDirection::Ltr)),
        (None, None),
    ];

    let fixture_bytes = build_style_bidi_docx();
    let archive_a = read_docx(&fixture_bytes).context("read style-bidi fixture")?;
    if dirs(&archive_a.document) != expected {
        bail!(
            "style bidi not resolved on read: {:?}",
            dirs(&archive_a.document)
        );
    }
    println!("[roundtrip] step 23a OK — bidi resolves through basedOn, direct off wins");

    let doc_a = String::from_utf8(extract_doc_xml(&fixture_bytes)?).context("utf8 source")?;
    let untouched = write_docx(&archive_a, &archive_a.document).context("untouched save")?;
    if extract_doc_xml(&untouched)? != doc_a.as_bytes() {
        bail!("untouched style-bidi document drifted");
    }
    println!("[roundtrip] step 23b OK — untouched save byte-identical");

    let edited = archive_a.document.insert_text(
        LogicalPos {
            path: BlockPath::top(0),
            offset: "Word".len() as u32,
        },
        INSERT_TEXT,
    );
    let expected_xml = doc_a.replacen("Word مرحبا", &format!("Word{INSERT_TEXT} مرحبا"), 1);
    let bytes = write_docx(&archive_a, &edited).context("write edited style-bidi")?;
    assert_document_xml_well_formed(&bytes).context("edited style-bidi .docx")?;
    let xml = String::from_utf8(extract_doc_xml(&bytes)?).context("utf8 edited")?;
    if xml != expected_xml {
        bail!(
            "edited style-RTL paragraph is not source + edit (inherited <w:bidi/> leaked?)\n--- expected ---\n{expected_xml}\n--- got ---\n{xml}"
        );
    }
    let back = read_docx(&bytes).context("re-read edited style-bidi")?;
    if dirs(&back.document) != expected {
        bail!("direction lost on re-read: {:?}", dirs(&back.document));
    }
    let drift = expected_xml.len() - doc_a.len();
    println!(
        "[roundtrip] step 23c OK — edited style-RTL paragraph gains no direct <w:bidi/>, re-reads RTL (Δ {drift} B)"
    );
    Ok(())
}

/* ================================= source markup (#199 / #106) ==== */

/// docDefaults with spacing: a writer that bakes resolved properties into
/// a regenerated paragraph would add a direct `<w:spacing>`.
const SOURCE_MARKUP_STYLES_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:docDefaults><w:pPrDefault><w:pPr><w:spacing w:after="200" w:line="276" w:lineRule="auto"/></w:pPr></w:pPrDefault></w:docDefaults></w:styles>"#;

/// Word-authored paragraph markup the typed model does not represent:
/// `w14:paraId` / rsids on `<w:p>`, run rsids on equally formatted runs,
/// bare `<w:t>`, `<w:proofErr>`, an underline with an unread `w:color`,
/// a tab inside a text run, a trailing `_GoBack` bookmark, a self-closing
/// paragraph and a run led by `<w:lastRenderedPageBreak/>`.
const SOURCE_MARKUP_BODY: &str = concat!(
    r#"<w:p w14:paraId="1A2B3C4D" w14:textId="77777777" w:rsidR="00A1B2C3" w:rsidRDefault="00A1B2C3" w:rsidP="00D4E5F6"><w:pPr><w:ind w:left="720"/></w:pPr><w:r w:rsidRPr="00112233"><w:t xml:space="preserve">Hello </w:t></w:r><w:proofErr w:type="spellStart"/><w:r w:rsidR="00445566"><w:t>wrold</w:t></w:r><w:proofErr w:type="spellEnd"/><w:r w:rsidR="00445566"><w:rPr><w:u w:val="single" w:color="FF0000"/></w:rPr><w:t xml:space="preserve"> underlined</w:t></w:r><w:r w:rsidR="00778899"><w:tab/><w:t>tabbed</w:t></w:r><w:bookmarkStart w:id="0" w:name="_GoBack"/><w:bookmarkEnd w:id="0"/></w:p>"#,
    r#"<w:p w:rsidR="00A1B2C3" w:rsidRDefault="00A1B2C3"/>"#,
    r#"<w:p w:rsidR="00A1B2C3" w:rsidRDefault="00A1B2C3"><w:r><w:lastRenderedPageBreak/><w:t>Second page</w:t></w:r></w:p>"#,
);

fn build_source_markup_docx() -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml" xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006" mc:Ignorable="w14"><w:body>{SOURCE_MARKUP_BODY}{BARE_SECT_PR}</w:body></w:document>"#
    );
    build_styled_docx(SOURCE_MARKUP_STYLES_XML, &document_xml)
}

/// Issues #199 / #106 — step 26: the attribute-level grab bag.
///
/// a. An untouched save is byte-identical.
/// b. Typing inside a word of the Word-shaped paragraph regenerates it as
///    EXACTLY source + the inserted bytes (drift = N, not ≤ 2×N): `<w:p>`
///    / `<w:r>` / `<w:t>` attributes, `<w:proofErr>`, the source run
///    boundaries, the tab inside its run, the trailing bookmark and the
///    verified `<w:pPr>` (no docDefaults spacing baked in) — on both save
///    paths.
/// c. Bolding the underlined run regenerates its `<w:rPr>` but the
///    unchanged `<w:u>` keeps its unread `w:color` (#106), and the run
///    keeps its rsid.
fn run_source_markup_roundtrip() -> Result<()> {
    use engine::{BlockPath, LogicalPos, SpanStyle};

    let at = |offset: usize| LogicalPos {
        path: BlockPath::top(0),
        offset: offset as u32,
    };
    let fixture_bytes = build_source_markup_docx();
    let archive_a = read_docx(&fixture_bytes).context("read source-markup fixture")?;
    let doc_a = String::from_utf8(extract_doc_xml(&fixture_bytes)?).context("utf8 source")?;
    let untouched = write_docx(&archive_a, &archive_a.document).context("untouched save")?;
    if extract_doc_xml(&untouched)? != doc_a.as_bytes() {
        bail!("untouched source-markup document drifted");
    }
    println!("[roundtrip] step 26a OK — untouched save byte-identical");

    let edited = archive_a
        .document
        .insert_text(at("Hello wr".len()), INSERT_TEXT);
    let expected_xml = doc_a.replacen("wrold", &format!("wr{INSERT_TEXT}old"), 1);
    for (path, bytes) in [
        (
            "write_docx",
            write_docx(&archive_a, &edited).context("write edited")?,
        ),
        (
            "save_docx",
            format_docx::save_docx(&edited).context("ui save edited")?,
        ),
    ] {
        assert_document_xml_well_formed(&bytes).context("edited source-markup .docx")?;
        let xml = String::from_utf8(extract_doc_xml(&bytes)?).context("utf8 edited")?;
        if xml != expected_xml {
            bail!(
                "{path}: edited Word paragraph is not source + edit\n--- expected ---\n{expected_xml}\n--- got ---\n{xml}"
            );
        }
    }
    let drift = expected_xml.len() - doc_a.len();
    println!(
        "[roundtrip] step 26b OK — edited Word paragraph is source + insert on both save paths (Δ {drift} B = N)"
    );

    let start = "Hello wrold".len();
    let bolded = archive_a.document.apply_style(
        at(start),
        at(start + " underlined".len()),
        SpanStyle {
            bold: Some(true),
            ..SpanStyle::default()
        },
    );
    let bytes = write_docx(&archive_a, &bolded).context("write bolded")?;
    assert_document_xml_well_formed(&bytes).context("bolded source-markup .docx")?;
    let xml = String::from_utf8(extract_doc_xml(&bytes)?).context("utf8 bolded")?;
    let run = xml
        .split("<w:r ")
        .find(|r| r.contains(" underlined"))
        .context("bolded run")?;
    if !(run.starts_with(r#"w:rsidR="00445566">"#)
        && run.contains("<w:b/>")
        && run.contains(r#"<w:u w:val="single" w:color="FF0000"/>"#))
    {
        bail!("bolded run lost its rsid or the underline's unread w:color: {run}");
    }
    let back = read_docx(&bytes).context("re-read bolded")?;
    let p0 = back.document.nth_paragraph(0).context("p0")?;
    if p0.style_at(start as u32).bold != Some(true) {
        bail!("bold lost on re-read");
    }
    println!("[roundtrip] step 26c OK — restyled run keeps its rsid and <w:u w:color> (#106)");
    Ok(())
}

/* ============================================ hyperlink identity (#242) ==== */

/// Issue #242 — the rels part of the hyperlink fixture: two rows share one
/// URL (Word writes one row per inserted link, `58618.docx`).
const HYPERLINK_RELS: &str = concat!(
    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
    r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#,
    r#"<Relationship Id="rId6" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="http://b.example/" TargetMode="External"/>"#,
    r#"<Relationship Id="rId5" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="http://a.example/" TargetMode="External"/>"#,
    r#"<Relationship Id="rId4" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="http://a.example/" TargetMode="External"/>"#,
    r#"</Relationships>"#,
);

/// Issue #242 — three external links (two to one URL) and an internal
/// anchor, carrying `w:history` / `w:tooltip` / `w:anchor` /
/// `w:tgtFrame` / `w:docLocation`.
const HYPERLINK_BODY: &str = concat!(
    r#"<w:p><w:r><w:t xml:space="preserve">See </w:t></w:r>"#,
    r#"<w:hyperlink r:id="rId4" w:tooltip="First &amp; best" w:history="1"><w:r><w:t>one</w:t></w:r></w:hyperlink>"#,
    r#"<w:r><w:t xml:space="preserve"> </w:t></w:r>"#,
    r#"<w:hyperlink r:id="rId5" w:history="1"><w:r><w:t>two</w:t></w:r></w:hyperlink>"#,
    r#"<w:r><w:t xml:space="preserve"> </w:t></w:r>"#,
    r#"<w:hyperlink r:id="rId6" w:anchor="part2" w:tgtFrame="_blank" w:history="1"><w:r><w:t>three</w:t></w:r></w:hyperlink>"#,
    r#"<w:r><w:t xml:space="preserve"> and </w:t></w:r>"#,
    r#"<w:hyperlink w:anchor="_Toc1" w:docLocation="x" w:history="1"><w:r><w:t>four</w:t></w:r></w:hyperlink>"#,
    r#"</w:p>"#,
);

fn build_hyperlink_identity_docx() -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:body>{HYPERLINK_BODY}{BARE_SECT_PR}</w:body></w:document>"#
    );
    package_document_xml_with_rels(&document_xml, HYPERLINK_RELS)
}

/// Issue #242 — step 33: typing before, between and after the links of a
/// regenerated paragraph keeps every link's own `r:id` (two links to one
/// URL no longer collapse onto one row), every source attribute and the
/// rels part byte-identical: the saved `document.xml` is exactly source +
/// the inserted bytes, on both save paths; the re-read resolves the same
/// four targets.
fn run_hyperlink_identity_roundtrip() -> Result<()> {
    use engine::{BlockPath, LogicalPos};

    let at = |offset: usize| LogicalPos {
        path: BlockPath::top(0),
        offset: offset as u32,
    };
    let fixture_bytes = build_hyperlink_identity_docx();
    let archive_a = read_docx(&fixture_bytes).context("read hyperlink fixture")?;
    let doc_a = String::from_utf8(extract_doc_xml(&fixture_bytes)?).context("utf8 source")?;
    let untouched = write_docx(&archive_a, &archive_a.document).context("untouched save")?;
    if extract_doc_xml(&untouched)? != doc_a.as_bytes() {
        bail!("untouched hyperlink document drifted");
    }
    println!("[roundtrip] step 33a OK — untouched save byte-identical");

    /* Before the first link, right after a middle link, after the last
    one. Typing at a link's end stays outside the link (the engine's
    travel rule); it continues the link's source run, which the link
    boundary then cuts into a run of its own. */
    let edited = archive_a
        .document
        .insert_text(at("See one two three and four".len()), INSERT_TEXT)
        .insert_text(at("See one two".len()), INSERT_TEXT)
        .insert_text(at("Se".len()), INSERT_TEXT);
    let expected_xml = doc_a
        .replacen("See ", &format!("Se{INSERT_TEXT}e "), 1)
        .replacen(
            "two</w:t></w:r></w:hyperlink>",
            &format!(
                r#"two</w:t></w:r></w:hyperlink><w:r><w:t xml:space="preserve">{INSERT_TEXT}</w:t></w:r>"#
            ),
            1,
        )
        .replacen(
            "four</w:t></w:r></w:hyperlink>",
            &format!(
                r#"four</w:t></w:r></w:hyperlink><w:r><w:t xml:space="preserve">{INSERT_TEXT}</w:t></w:r>"#
            ),
            1,
        );
    for (path, bytes) in [
        (
            "write_docx",
            write_docx(&archive_a, &edited).context("write edited")?,
        ),
        (
            "save_docx",
            format_docx::save_docx(&edited).context("ui save edited")?,
        ),
    ] {
        assert_document_xml_well_formed(&bytes).context("edited hyperlink .docx")?;
        let xml = String::from_utf8(extract_doc_xml(&bytes)?).context("utf8 edited")?;
        if xml != expected_xml {
            bail!(
                "{path}: edited hyperlink paragraph is not source + edit\n--- expected ---\n{expected_xml}\n--- got ---\n{xml}"
            );
        }
        let rels = zip_entries(&bytes)?
            .into_iter()
            .find(|(n, _)| n == "word/_rels/document.xml.rels")
            .context("rels part")?
            .1;
        if rels != HYPERLINK_RELS.as_bytes() {
            bail!("{path}: rels part rewritten");
        }
        let back = read_docx(&bytes).context("re-read edited")?;
        let targets: Vec<String> = back
            .document
            .nth_paragraph(0)
            .context("p0")?
            .hyperlinks
            .iter()
            .map(|h| h.target.clone())
            .collect();
        if targets
            != [
                "http://a.example/",
                "http://a.example/",
                "http://b.example/",
                "#_Toc1",
            ]
        {
            bail!("{path}: re-read targets {targets:?}");
        }
    }
    println!(
        "[roundtrip] step 33b OK — edited link paragraph keeps each r:id + attribute; rels untouched on both save paths"
    );
    Ok(())
}

/* ============================================== comment anchors (#243) ==== */

/// Issue #243 — a commented range with a Word-shaped reference run
/// (own rsid + rPr) behind it, and a paragraph holding only the reference
/// of a second comment (`comment.docx` shape).
const COMMENT_ANCHOR_BODY: &str = concat!(
    r#"<w:p w:rsidR="00B561CA"><w:r><w:t xml:space="preserve">this is a </w:t></w:r>"#,
    r#"<w:commentRangeStart w:id="0"/><w:r><w:t xml:space="preserve">comment </w:t></w:r>"#,
    r#"<w:commentRangeEnd w:id="0"/><w:r w:rsidR="002903BF"><w:rPr><w:rStyle w:val="CommentReference"/></w:rPr><w:commentReference w:id="0"/></w:r>"#,
    r#"<w:r><w:t>paragraph!</w:t></w:r></w:p>"#,
    r#"<w:p><w:r><w:rPr></w:rPr><w:commentReference w:id="1"/></w:r></w:p>"#,
);

const COMMENT_ANCHOR_COMMENTS: &str = concat!(
    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
    r#"<w:comments xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">"#,
    r#"<w:comment w:id="0" w:author="A" w:date="2026-01-01T00:00:00Z"><w:p><w:r><w:t>first</w:t></w:r></w:p></w:comment>"#,
    r#"<w:comment w:id="1" w:author="B" w:date="2026-01-01T00:00:00Z"><w:p><w:r><w:t>second</w:t></w:r></w:p></w:comment>"#,
    r#"</w:comments>"#,
);

fn build_comment_anchor_docx() -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{COMMENT_ANCHOR_BODY}{BARE_SECT_PR}</w:body></w:document>"#
    );
    let rels = concat!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
        r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#,
        r#"<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments" Target="comments.xml"/>"#,
        r#"</Relationships>"#,
    );
    package_document_xml_with_parts(
        &document_xml,
        rels,
        &[("word/comments.xml", COMMENT_ANCHOR_COMMENTS)],
    )
}

/// Issue #243 — step 34: comment anchors of a regenerated paragraph.
///
/// a. An untouched save is byte-identical.
/// b. Typing before, inside and after the commented range keeps
///    `<w:commentRangeStart/>`, `<w:commentRangeEnd/>` and the reference
///    run at the right offsets — the saved part is exactly source + the
///    inserted bytes on both save paths, and the re-read range covers the
///    same (grown) text.
/// c. The reference-only paragraph keeps its reference when it
///    regenerates (it used to save as `<w:p/>`); deleting the comment
///    drops its anchors instead of resurrecting them.
fn run_comment_anchor_roundtrip() -> Result<()> {
    use engine::{BlockPath, LogicalPos};

    let at = |block: u32, offset: usize| LogicalPos {
        path: BlockPath::top(block),
        offset: offset as u32,
    };
    let fixture_bytes = build_comment_anchor_docx();
    let archive_a = read_docx(&fixture_bytes).context("read comment fixture")?;
    let doc_a = String::from_utf8(extract_doc_xml(&fixture_bytes)?).context("utf8 source")?;
    let untouched = write_docx(&archive_a, &archive_a.document).context("untouched save")?;
    if extract_doc_xml(&untouched)? != doc_a.as_bytes() {
        bail!("untouched comment document drifted");
    }
    println!("[roundtrip] step 34a OK — untouched save byte-identical");

    for (offset, from, to, covered) in [
        (
            "th".len(),
            "this is",
            format!("th{INSERT_TEXT}is is"),
            "comment ".to_string(),
        ),
        (
            "this is a co".len(),
            "comment ",
            format!("co{INSERT_TEXT}mment "),
            format!("co{INSERT_TEXT}mment "),
        ),
        (
            "this is a comment paragraph!".len(),
            "paragraph!",
            format!("paragraph!{INSERT_TEXT}"),
            "comment ".to_string(),
        ),
    ] {
        let edited = archive_a.document.insert_text(at(0, offset), INSERT_TEXT);
        let expected_xml = doc_a.replacen(from, &to, 1);
        for (path, bytes) in [
            (
                "write_docx",
                write_docx(&archive_a, &edited).context("write edited")?,
            ),
            (
                "save_docx",
                format_docx::save_docx(&edited).context("ui save edited")?,
            ),
        ] {
            assert_document_xml_well_formed(&bytes).context("edited comment .docx")?;
            let xml = String::from_utf8(extract_doc_xml(&bytes)?).context("utf8 edited")?;
            if xml != expected_xml {
                bail!(
                    "{path}: insert at {offset} is not source + edit\n--- expected ---\n{expected_xml}\n--- got ---\n{xml}"
                );
            }
            let back = read_docx(&bytes).context("re-read edited")?;
            let r = back
                .document
                .comment_ranges
                .first()
                .context("comment range lost")?;
            let p = back.document.nth_paragraph(0).context("p0")?;
            let got = p
                .text
                .get(r.start.offset as usize..r.end.offset as usize)
                .unwrap_or_default();
            if got != covered {
                bail!("{path}: insert at {offset}: range covers {got:?}, expected {covered:?}");
            }
        }
    }
    println!(
        "[roundtrip] step 34b OK — edits before / inside / after a commented range keep its anchors (source + insert, both save paths)"
    );

    let edited = archive_a.document.insert_text(at(1, 0), INSERT_TEXT);
    let bytes = write_docx(&archive_a, &edited).context("write reference-only")?;
    let xml = String::from_utf8(extract_doc_xml(&bytes)?).context("utf8")?;
    if !xml.contains(r#"<w:r><w:rPr></w:rPr><w:commentReference w:id="1"/></w:r></w:p>"#) {
        bail!("reference-only paragraph lost its reference: {xml}");
    }
    let deleted = archive_a
        .document
        .delete_comment(0)
        .insert_text(at(0, 0), INSERT_TEXT);
    let bytes = write_docx(&archive_a, &deleted).context("write deleted")?;
    assert_document_xml_well_formed(&bytes).context("deleted comment .docx")?;
    let xml = String::from_utf8(extract_doc_xml(&bytes)?).context("utf8")?;
    if xml.contains(r#"w:id="0""#) {
        bail!("deleted comment's anchors resurrected: {xml}");
    }
    println!(
        "[roundtrip] step 34c OK — reference-only paragraph keeps its reference; a deleted comment's anchors are dropped"
    );
    Ok(())
}

/* ================================================ table placement (#173) ==== */

/// Issue #173 — step 18: the `<w:jc>` / `<w:tblInd>` round-trip contract
/// on `table_jc_tblind.docx` (centred, end-aligned and indented tables).
///
/// a. Both are MODELED on read (`TableProperties::alignment` /
///    `indent_twips`), never carried in the tblPr grab bag.
/// b. An untouched save is byte-identical (passthrough, drift 0).
/// c. Typing into each table dirties it: every regenerated table is
///    exactly its source plus the insert — `<w:jc>` and `<w:tblInd>`
///    emitted once each, in schema order — on both save paths, and the
///    re-read model carries the same placement.
fn run_table_jc_tblind_roundtrip() -> Result<()> {
    use engine::{Alignment, BlockPath, LogicalPos, PathStep};

    let fixture_bytes = build_table_jc_tblind_docx();
    let archive_a = read_docx(&fixture_bytes).context("read jc/tblInd fixture")?;
    /* Block indices of the three tables and their expected placement. */
    let tables: [(u32, Option<Alignment>, i32, &str, &str); 3] = [
        (1, Some(Alignment::Center), 0, JC_TBLIND_CENTER, "centre a"),
        (3, Some(Alignment::End), 0, JC_TBLIND_END, "end a"),
        (5, Some(Alignment::Start), 720, JC_TBLIND_INDENT, "indent a"),
    ];
    for (idx, alignment, indent, _, _) in tables {
        let t = archive_a.document.blocks[idx as usize]
            .as_table()
            .with_context(|| format!("block {idx} is a table"))?;
        if t.props.alignment != alignment || t.props.indent_twips != indent {
            bail!(
                "table {idx}: jc/tblInd not modeled: {:?} / {}",
                t.props.alignment,
                t.props.indent_twips
            );
        }
        if t.props.grab_bag.is_some() {
            bail!(
                "table {idx}: jc/tblInd must not ride the tblPr grab bag: {:?}",
                t.props.grab_bag
            );
        }
    }
    let src = String::from_utf8(extract_doc_xml(&fixture_bytes)?).context("utf8 source")?;

    /* b. untouched save — byte-identical. */
    let untouched = write_docx(&archive_a, &archive_a.document).context("untouched save")?;
    assert_document_xml_well_formed(&untouched).context("untouched jc/tblInd .docx")?;
    if extract_doc_xml(&untouched)? != src.as_bytes() {
        bail!("untouched jc/tblInd document.xml drifted");
    }
    println!(
        "[roundtrip] step 18a OK — jc / tblInd modeled on read, untouched save byte-identical"
    );

    /* c. edit the first cell of every table. */
    let mut edited = archive_a.document.clone();
    let mut expected = src.clone();
    for (idx, _, _, tbl_pr, first) in tables {
        let path = BlockPath::top(idx)
            .push(PathStep::Cell { row: 0, col: 0 })
            .push(PathStep::Block(0));
        edited = edited.insert_text(
            LogicalPos {
                path,
                offset: first.len() as u32,
            },
            INSERT_TEXT,
        );
        let src_tbl = jc_tblind_table(tbl_pr, first, &first.replace(" a", " b"));
        let want_tbl = src_tbl.replacen(first, &format!("{first}{INSERT_TEXT}"), 1);
        if !expected.contains(&src_tbl) {
            bail!("fixture does not contain table {idx} verbatim");
        }
        expected = expected.replacen(&src_tbl, &want_tbl, 1);
    }
    for (label, bytes) in [
        (
            "write_docx",
            write_docx(&archive_a, &edited).context("write edited jc/tblInd tables")?,
        ),
        (
            "build_minimal_docx",
            build_minimal_docx(&edited).context("UI-path save of jc/tblInd tables")?,
        ),
    ] {
        assert_document_xml_well_formed(&bytes)
            .with_context(|| format!("{label} jc/tblInd tables"))?;
        let xml = String::from_utf8(extract_doc_xml(&bytes)?).context("utf8 edited")?;
        let body = |s: &str| -> Result<String> {
            let a = s.find("<w:body>").context("no <w:body>")?;
            let b = s.rfind("</w:body>").context("no </w:body>")?;
            Ok(s[a..b].to_string())
        };
        if body(&xml)? != body(&expected)? {
            bail!(
                "{label}: regenerated tables are not source + edit\n--- expected ---\n{expected}\n--- got ---\n{xml}"
            );
        }
        let back = read_docx(&bytes).with_context(|| format!("{label}: re-read"))?;
        for (idx, alignment, indent, _, _) in tables {
            let t = back.document.blocks[idx as usize]
                .as_table()
                .context("table")?;
            if t.props.alignment != alignment || t.props.indent_twips != indent {
                bail!("{label}: table {idx} placement lost on re-read");
            }
        }
    }
    let drift = expected.len() - src.len();
    println!(
        "[roundtrip] step 18b OK — dirty tables regenerate source + edit, jc / tblInd emitted once in schema order (Δ {drift} B)"
    );
    Ok(())
}

/* ========================================================== manifest ==== */

#[derive(Debug, Serialize, Deserialize)]
struct ManifestFile {
    /// Map from fixture filename (e.g. `"simple_text.docx"`) to its entry.
    fixtures: BTreeMap<String, FixtureEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
struct FixtureEntry {
    /// Where this fixture came from: `"build_minimal_docx"` (seed),
    /// `"word365"`, `"libreoffice"`, or `"handcrafted"`.
    generator: String,
    /// Roadmap phase at which the fixture was added.
    phase_introduced: u8,
    asserts: FixtureAsserts,
    #[serde(default)]
    roundtrip: RoundtripBounds,
}

#[derive(Debug, Serialize, Deserialize)]
struct FixtureAsserts {
    paragraph_count: u32,
    /// Expected `paragraph_text(i)` for each paragraph, in order.
    paragraph_texts: Vec<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct RoundtripBounds {
    /// Max allowed |new − old| byte delta on `word/document.xml` between the
    /// loaded archive and a fresh `write_docx(&archive, &archive.document)`.
    /// Phase 1 seeds emit byte-identical bytes ⇒ default `0`. Phase 3's
    /// passthrough optimisation will keep Word-generated fixtures at `0`
    /// too; Phase 2 / 4 / 5 fixtures may set a small positive bound.
    #[serde(default)]
    document_xml_drift_bytes: usize,
}

/* ==================================================== TOC (#81) ==== */

/// Issue #81 fixture: Word's TOC shape — `begin` + instruction +
/// `separate` in the first entry paragraph, each entry a
/// `<w:hyperlink w:anchor>` with a nested `PAGEREF` over the number and
/// a right dot-leader tab, the `end` alone in a trailing paragraph —
/// followed by the two `_Toc*`-bookmarked headings it lists.
const TOC_DOCUMENT_XML: &str = concat!(
    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
    "\n",
    r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>"#,
    r#"<w:p><w:pPr><w:pStyle w:val="TOC1"/><w:tabs><w:tab w:val="right" w:leader="dot" w:pos="9350"/></w:tabs></w:pPr><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve"> TOC \o "1-3" \h \z \u </w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:hyperlink w:anchor="_Toc111" w:history="1"><w:r><w:t xml:space="preserve">Alpha</w:t></w:r><w:r><w:tab/></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve"> PAGEREF _Toc111 \h </w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve">1</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:hyperlink></w:p>"#,
    r#"<w:p><w:pPr><w:pStyle w:val="TOC2"/><w:tabs><w:tab w:val="right" w:leader="dot" w:pos="9350"/></w:tabs></w:pPr><w:hyperlink w:anchor="_Toc222" w:history="1"><w:r><w:t xml:space="preserve">Beta</w:t></w:r><w:r><w:tab/></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve"> PAGEREF _Toc222 \h </w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve">1</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:hyperlink></w:p>"#,
    r#"<w:p><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p>"#,
    r#"<w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:bookmarkStart w:id="0" w:name="_Toc111"/><w:r><w:t xml:space="preserve">Alpha</w:t></w:r><w:bookmarkEnd w:id="0"/></w:p>"#,
    r#"<w:p><w:pPr><w:pStyle w:val="Heading2"/></w:pPr><w:bookmarkStart w:id="1" w:name="_Toc222"/><w:r><w:t xml:space="preserve">Beta</w:t></w:r><w:bookmarkEnd w:id="1"/></w:p>"#,
    "<w:sectPr/></w:body></w:document>"
);

fn build_toc_word_shape_docx() -> Vec<u8> {
    package_document_xml(TOC_DOCUMENT_XML)
}

/// Issue #81 — step 15: the TOC round-trip contract.
/// (a) Word's shape reads as ONE multi-paragraph TOC region and an
/// untouched save is byte-identical; (b) editing a heading WITHOUT an
/// update keeps the stale TOC byte-identical (Word never rebuilds a TOC
/// on save) at ≤ 2×N drift; (c) a regenerated TOC (the F9 path) writes
/// Word's shape — begin before the first entry link, end after the last,
/// `w:anchor` links, `PAGEREF`s, dot leaders — keeps the headings'
/// bookmark ids, and reads back as the same live, updatable TOC.
fn run_toc_roundtrip() -> Result<()> {
    use engine::{BlockPath, LogicalPos};

    let fixture = build_toc_word_shape_docx();
    let archive = read_docx(&fixture).context("read TOC fixture")?;
    let regions = archive.document.toc_regions();
    if regions.len() != 1 || (regions[0].first, regions[0].last) != (0, 2) {
        bail!("TOC fixture: expected one region over blocks 0..=2, got {regions:?}");
    }
    let bytes = write_docx(&archive, &archive.document).context("write untouched TOC")?;
    if extract_doc_xml(&bytes)? != extract_doc_xml(&fixture)? {
        bail!("TOC fixture: an untouched save is not byte-identical");
    }
    println!(
        "[roundtrip] step 15a OK — Word's TOC reads as one region, untouched save byte-identical"
    );

    /* (b) Edit the "Beta" heading, save without updating the TOC. */
    let edited = archive
        .document
        .insert_text(LogicalPos::new(BlockPath::top(4), 4), INSERT_TEXT);
    let bytes_b = write_docx(&archive, &edited).context("write heading edit")?;
    assert_document_xml_well_formed(&bytes_b).context("heading-edit .docx")?;
    let src = String::from_utf8(extract_doc_xml(&fixture)?).context("utf8 source")?;
    let out_b = String::from_utf8(extract_doc_xml(&bytes_b)?).context("utf8 output")?;
    let drift = (out_b.len() as isize - src.len() as isize).unsigned_abs();
    if drift > 2 * INSERT_TEXT.len() {
        bail!(
            "TOC heading edit: document.xml drift {drift} B exceeds {} B",
            2 * INSERT_TEXT.len()
        );
    }
    let toc_src = &src[src.find("<w:p>").context("first paragraph")?
        ..src
            .find(r#"<w:p><w:pPr><w:pStyle w:val="Heading1"/>"#)
            .context("heading")?];
    if !out_b.contains(toc_src) {
        bail!("TOC heading edit: the stale TOC did not survive byte-for-byte");
    }
    println!("[roundtrip] step 15b OK — a heading edit keeps the stale TOC verbatim (Δ {drift} B)");

    /* (c) Regenerate (F9), save, re-read. */
    let (updated, changed) = edited.regenerate_tocs(&|ord| Some((ord + 1).to_string()));
    if !changed {
        bail!("TOC regeneration after a heading edit reported no change");
    }
    let bytes_c = write_docx(&archive, &updated).context("write regenerated TOC")?;
    assert_document_xml_well_formed(&bytes_c).context("regenerated .docx")?;
    let out_c = String::from_utf8(extract_doc_xml(&bytes_c)?).context("utf8 regenerated")?;
    let begin = out_c
        .find(r#"w:fldCharType="begin""#)
        .context("TOC begin")?;
    let first_link = out_c
        .find(r#"<w:hyperlink w:anchor="_Toc111""#)
        .context("first entry link")?;
    let last_link_close = out_c.rfind("</w:hyperlink>").context("last link close")?;
    let toc_end = out_c
        .rfind(r#"w:fldCharType="end""#)
        .context("TOC end fldChar")?;
    if begin > first_link || toc_end < last_link_close {
        bail!("regenerated TOC is not Word-shaped:\n{out_c}");
    }
    for needle in [
        r#"TOC \o "1-3" \h \z \u"#,
        r#"PAGEREF _Toc222 \h"#,
        r#"<w:tab w:val="right" w:leader="dot""#,
        r#"<w:bookmarkStart w:id="1" w:name="_Toc222"/>"#,
    ] {
        if !out_c.contains(needle) {
            bail!("regenerated TOC lacks `{needle}`:\n{out_c}");
        }
    }
    let reread = read_docx(&bytes_c).context("re-read regenerated TOC")?;
    let d = &reread.document;
    let regions = d.toc_regions();
    if regions.len() != 1 || (regions[0].first, regions[0].last) != (0, 1) {
        bail!("regenerated TOC re-read as {regions:?}");
    }
    let texts: Vec<&str> = (0..2)
        .filter_map(|i| d.paragraph_at_path(&BlockPath::top(i)))
        .map(|p| p.text.as_str())
        .collect();
    let want_beta = format!("Beta{INSERT_TEXT}\t2");
    if texts != ["Alpha\t1", want_beta.as_str()] {
        bail!("regenerated TOC entries re-read as {texts:?}");
    }
    let (_, again) = d.regenerate_tocs(&|ord| Some((ord + 1).to_string()));
    if again {
        bail!("a re-read regenerated TOC is not current (update is not idempotent)");
    }
    println!("[roundtrip] step 15c OK — regenerated TOC writes Word's shape and reads back live");
    Ok(())
}

/* ============================ Word package parts (#135 / #134) ==== */

/// Issues #134 / #135 — a Word-shaped PACKAGE, not just a Word-shaped
/// body: every sibling part a real Word file carries and the model does
/// not regenerate on a text edit — styles, numbering, settings, fontTable,
/// theme, a header and a footer (with its own rels), comments, core / app
/// properties, a custom XML item — plus two body pictures behind `rId9` /
/// `rId10`. The live editor's save path (`format_docx::save_docx`) must
/// re-emit every one of them byte-identical (#134), and a third picture
/// inserted through the model must land beside them without disturbing
/// either (#135).
const PKG_W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const PKG_R_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const PKG_REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const PKG_CT_WML: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml";

/// One inline picture run behind `rid`.
fn package_picture_run(rid: &str, id: u32) -> String {
    format!(
        concat!(
            "<w:r><w:drawing>",
            r#"<wp:inline distT="0" distB="0" distL="0" distR="0">"#,
            r#"<wp:extent cx="914400" cy="457200"/>"#,
            r#"<wp:effectExtent l="0" t="0" r="0" b="0"/>"#,
            r#"<wp:docPr id="{id}" name="Picture {id}"/>"#,
            "<wp:cNvGraphicFramePr/>",
            "<a:graphic>",
            r#"<a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture">"#,
            "<pic:pic>",
            r#"<pic:nvPicPr><pic:cNvPr id="0" name="Image"/><pic:cNvPicPr/></pic:nvPicPr>"#,
            r#"<pic:blipFill><a:blip r:embed="{rid}"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill>"#,
            r#"<pic:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="914400" cy="457200"/></a:xfrm>"#,
            r#"<a:prstGeom prst="rect"><a:avLst/></a:prstGeom></pic:spPr>"#,
            "</pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r>",
        ),
        rid = rid,
        id = id,
    )
}

fn word_package_document_xml() -> String {
    format!(
        concat!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
            "\r\n",
            r#"<w:document xmlns:wpc="http://schemas.microsoft.com/office/word/2010/wordprocessingCanvas" "#,
            r#"xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006" "#,
            r#"xmlns:r="{r}" "#,
            r#"xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" "#,
            r#"xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" "#,
            r#"xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture" "#,
            r#"xmlns:w="{w}" "#,
            r#"xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml" "#,
            r#"mc:Ignorable="w14">"#,
            "<w:body>",
            r#"<w:p w14:paraId="10000001" w14:textId="20000001" w:rsidR="00A1B2C3" w:rsidRDefault="00A1B2C3">"#,
            r#"<w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Package title</w:t></w:r></w:p>"#,
            r#"<w:p w14:paraId="10000002" w14:textId="20000002" w:rsidR="00A1B2C3" w:rsidRDefault="00A1B2C3">"#,
            r#"<w:pPr><w:pStyle w:val="ListParagraph"/><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr>"#,
            r#"<w:r><w:t>first item</w:t></w:r></w:p>"#,
            r#"<w:p w14:paraId="10000003" w14:textId="20000003" w:rsidR="00A1B2C3" w:rsidRDefault="00A1B2C3">"#,
            r#"<w:commentRangeStart w:id="0"/><w:r><w:t xml:space="preserve">commented text</w:t></w:r>"#,
            r#"<w:commentRangeEnd w:id="0"/><w:r><w:rPr><w:rStyle w:val="CommentReference"/></w:rPr>"#,
            r#"<w:commentReference w:id="0"/></w:r></w:p>"#,
            r#"<w:p w14:paraId="10000004" w14:textId="20000004" w:rsidR="00A1B2C3" w:rsidRDefault="00A1B2C3">"#,
            r#"<w:r><w:t xml:space="preserve">pictures </w:t></w:r>{pic1}{pic2}</w:p>"#,
            r#"<w:p w14:paraId="10000005" w14:textId="20000005" w:rsidR="00A1B2C3" w:rsidRDefault="00A1B2C3">"#,
            r#"<w:r><w:t>last paragraph</w:t></w:r></w:p>"#,
            r#"<w:sectPr w:rsidR="00A1B2C3"><w:headerReference w:type="default" r:id="rId6"/>"#,
            r#"<w:footerReference w:type="default" r:id="rId7"/>"#,
            r#"<w:pgSz w:w="11906" w:h="16838"/>"#,
            r#"<w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440" w:header="708" w:footer="708" w:gutter="0"/>"#,
            r#"<w:cols w:space="708"/><w:docGrid w:linePitch="360"/></w:sectPr>"#,
            "</w:body></w:document>",
        ),
        r = PKG_R_NS,
        w = PKG_W_NS,
        pic1 = package_picture_run("rId9", 1),
        pic2 = package_picture_run("rId10", 2),
    )
}

/// The sibling parts, in the order Word writes them.
fn word_package_parts() -> Vec<(&'static str, Vec<u8>)> {
    let xml = |s: String| s.into_bytes();
    let decl = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n";
    let content_types = format!(
        concat!(
            "{decl}",
            r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">"#,
            r#"<Default Extension="png" ContentType="image/png"/>"#,
            r#"<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>"#,
            r#"<Default Extension="xml" ContentType="application/xml"/>"#,
            r#"<Override PartName="/word/document.xml" ContentType="{ct}.document.main+xml"/>"#,
            r#"<Override PartName="/customXml/itemProps1.xml" ContentType="application/vnd.openxmlformats-officedocument.customXmlProperties+xml"/>"#,
            r#"<Override PartName="/word/numbering.xml" ContentType="{ct}.numbering+xml"/>"#,
            r#"<Override PartName="/word/styles.xml" ContentType="{ct}.styles+xml"/>"#,
            r#"<Override PartName="/word/settings.xml" ContentType="{ct}.settings+xml"/>"#,
            r#"<Override PartName="/word/comments.xml" ContentType="{ct}.comments+xml"/>"#,
            r#"<Override PartName="/word/header1.xml" ContentType="{ct}.header+xml"/>"#,
            r#"<Override PartName="/word/footer1.xml" ContentType="{ct}.footer+xml"/>"#,
            r#"<Override PartName="/word/fontTable.xml" ContentType="{ct}.fontTable+xml"/>"#,
            r#"<Override PartName="/word/theme/theme1.xml" ContentType="application/vnd.openxmlformats-officedocument.theme+xml"/>"#,
            r#"<Override PartName="/docProps/core.xml" ContentType="application/vnd.openxmlformats-package.core-properties+xml"/>"#,
            r#"<Override PartName="/docProps/app.xml" ContentType="application/vnd.openxmlformats-officedocument.extended-properties+xml"/>"#,
            "</Types>",
        ),
        decl = decl,
        ct = PKG_CT_WML,
    );
    let dot_rels = format!(
        concat!(
            "{decl}",
            r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#,
            r#"<Relationship Id="rId3" Type="{rel}/extended-properties" Target="docProps/app.xml"/>"#,
            r#"<Relationship Id="rId2" Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties" Target="docProps/core.xml"/>"#,
            r#"<Relationship Id="rId1" Type="{rel}/officeDocument" Target="word/document.xml"/>"#,
            "</Relationships>",
        ),
        decl = decl,
        rel = PKG_REL,
    );
    let doc_rels = format!(
        concat!(
            "{decl}",
            r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#,
            r#"<Relationship Id="rId8" Type="{rel}/comments" Target="comments.xml"/>"#,
            r#"<Relationship Id="rId3" Type="{rel}/settings" Target="settings.xml"/>"#,
            r#"<Relationship Id="rId7" Type="{rel}/footer" Target="footer1.xml"/>"#,
            r#"<Relationship Id="rId2" Type="{rel}/styles" Target="styles.xml"/>"#,
            r#"<Relationship Id="rId1" Type="{rel}/customXml" Target="../customXml/item1.xml"/>"#,
            r#"<Relationship Id="rId6" Type="{rel}/header" Target="header1.xml"/>"#,
            r#"<Relationship Id="rId11" Type="{rel}/theme" Target="theme/theme1.xml"/>"#,
            r#"<Relationship Id="rId5" Type="{rel}/numbering" Target="numbering.xml"/>"#,
            r#"<Relationship Id="rId10" Type="{rel}/image" Target="media/image2.png"/>"#,
            r#"<Relationship Id="rId4" Type="{rel}/fontTable" Target="fontTable.xml"/>"#,
            r#"<Relationship Id="rId9" Type="{rel}/image" Target="media/image1.png"/>"#,
            "</Relationships>",
        ),
        decl = decl,
        rel = PKG_REL,
    );
    let header_rels = format!(
        concat!(
            "{decl}",
            r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#,
            r#"<Relationship Id="rId1" Type="{rel}/hyperlink" Target="https://example.com/header" TargetMode="External"/>"#,
            "</Relationships>",
        ),
        decl = decl,
        rel = PKG_REL,
    );
    let item_rels = format!(
        concat!(
            "{decl}",
            r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#,
            r#"<Relationship Id="rId1" Type="{rel}/customXmlProps" Target="itemProps1.xml"/>"#,
            "</Relationships>",
        ),
        decl = decl,
        rel = PKG_REL,
    );
    let styles = format!(
        concat!(
            "{decl}",
            r#"<w:styles xmlns:w="{w}"><w:docDefaults><w:rPrDefault><w:rPr>"#,
            r#"<w:rFonts w:ascii="Calibri" w:hAnsi="Calibri"/><w:sz w:val="22"/></w:rPr></w:rPrDefault>"#,
            r#"<w:pPrDefault><w:pPr><w:spacing w:after="160" w:line="259" w:lineRule="auto"/></w:pPr></w:pPrDefault></w:docDefaults>"#,
            r#"<w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/><w:qFormat/></w:style>"#,
            r#"<w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/><w:basedOn w:val="Normal"/>"#,
            r#"<w:next w:val="Normal"/><w:qFormat/><w:pPr><w:keepNext/><w:spacing w:before="240"/><w:outlineLvl w:val="0"/></w:pPr>"#,
            r#"<w:rPr><w:b/><w:sz w:val="32"/></w:rPr></w:style>"#,
            r#"<w:style w:type="paragraph" w:styleId="ListParagraph"><w:name w:val="List Paragraph"/><w:basedOn w:val="Normal"/>"#,
            r#"<w:pPr><w:ind w:left="720"/></w:pPr></w:style>"#,
            r#"<w:style w:type="character" w:styleId="CommentReference"><w:name w:val="annotation reference"/><w:rPr><w:sz w:val="16"/></w:rPr></w:style>"#,
            "</w:styles>",
        ),
        decl = decl,
        w = PKG_W_NS,
    );
    let numbering = format!(
        concat!(
            "{decl}",
            r#"<w:numbering xmlns:w="{w}"><w:abstractNum w:abstractNumId="0">"#,
            r#"<w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/>"#,
            r#"<w:lvlJc w:val="left"/><w:pPr><w:ind w:left="720" w:hanging="360"/></w:pPr></w:lvl></w:abstractNum>"#,
            r#"<w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num></w:numbering>"#,
        ),
        decl = decl,
        w = PKG_W_NS,
    );
    let settings = format!(
        concat!(
            "{decl}",
            r#"<w:settings xmlns:w="{w}"><w:zoom w:percent="100"/><w:proofState w:spelling="clean" w:grammar="clean"/>"#,
            r#"<w:defaultTabStop w:val="720"/><w:characterSpacingControl w:val="doNotCompress"/>"#,
            r#"<w:compat><w:compatSetting w:name="compatibilityMode" w:uri="http://schemas.microsoft.com/office/word" w:val="15"/></w:compat>"#,
            "</w:settings>",
        ),
        decl = decl,
        w = PKG_W_NS,
    );
    let font_table = format!(
        concat!(
            "{decl}",
            r#"<w:fonts xmlns:w="{w}"><w:font w:name="Calibri"><w:panose1 w:val="020F0502020204030204"/>"#,
            r#"<w:charset w:val="00"/><w:family w:val="swiss"/><w:pitch w:val="variable"/></w:font></w:fonts>"#,
        ),
        decl = decl,
        w = PKG_W_NS,
    );
    let theme = format!(
        concat!(
            "{decl}",
            r#"<a:theme xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" name="Office Theme">"#,
            r#"<a:themeElements><a:clrScheme name="Office"><a:dk1><a:sysClr val="windowText" lastClr="000000"/></a:dk1>"#,
            r#"<a:lt1><a:sysClr val="window" lastClr="FFFFFF"/></a:lt1></a:clrScheme>"#,
            r#"<a:fontScheme name="Office"><a:majorFont><a:latin typeface="Calibri Light"/></a:majorFont>"#,
            r#"<a:minorFont><a:latin typeface="Calibri"/></a:minorFont></a:fontScheme></a:themeElements></a:theme>"#,
        ),
        decl = decl,
    );
    let comments = format!(
        concat!(
            "{decl}",
            r#"<w:comments xmlns:w="{w}"><w:comment w:id="0" w:author="Reviewer" w:date="2026-01-02T03:04:05Z" w:initials="R">"#,
            r#"<w:p><w:r><w:t>Please check this.</w:t></w:r></w:p></w:comment></w:comments>"#,
        ),
        decl = decl,
        w = PKG_W_NS,
    );
    let header = format!(
        concat!(
            "{decl}",
            r#"<w:hdr xmlns:w="{w}" xmlns:r="{r}"><w:p><w:pPr><w:pStyle w:val="Header"/></w:pPr>"#,
            r#"<w:r><w:t>Header text</w:t></w:r></w:p></w:hdr>"#,
        ),
        decl = decl,
        w = PKG_W_NS,
        r = PKG_R_NS,
    );
    let footer = format!(
        concat!(
            "{decl}",
            r#"<w:ftr xmlns:w="{w}"><w:p><w:r><w:t xml:space="preserve">Page </w:t></w:r>"#,
            r#"<w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve"> PAGE </w:instrText></w:r>"#,
            r#"<w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>1</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r>"#,
            "</w:p></w:ftr>",
        ),
        decl = decl,
        w = PKG_W_NS,
    );
    let core = format!(
        concat!(
            "{decl}",
            r#"<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" "#,
            r#"xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:dcterms="http://purl.org/dc/terms/" "#,
            r#"xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">"#,
            r#"<dc:creator>Package Author</dc:creator><cp:revision>3</cp:revision>"#,
            r#"<dcterms:created xsi:type="dcterms:W3CDTF">2026-01-01T00:00:00Z</dcterms:created></cp:coreProperties>"#,
        ),
        decl = decl,
    );
    let app = format!(
        concat!(
            "{decl}",
            r#"<Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/extended-properties">"#,
            r#"<Application>Microsoft Office Word</Application><Pages>1</Pages></Properties>"#,
        ),
        decl = decl,
    );
    let item = r#"<?xml version="1.0" encoding="UTF-8" standalone="no"?><b:Sources xmlns:b="http://schemas.openxmlformats.org/officeDocument/2006/bibliography" SelectedStyle="\APA.XSL"/>"#.to_string();
    let item_props = format!(
        concat!(
            "{decl}",
            r#"<ds:datastoreItem ds:itemID="{{11111111-2222-3333-4444-555555555555}}" "#,
            r#"xmlns:ds="http://schemas.openxmlformats.org/officeDocument/2006/customXml">"#,
            r#"<ds:schemaRefs><ds:schemaRef ds:uri="http://schemas.openxmlformats.org/officeDocument/2006/bibliography"/></ds:schemaRefs>"#,
            "</ds:datastoreItem>",
        ),
        decl = decl,
    );
    /* Two distinct (tiny) "PNG" blobs — the signature plus a tag byte. */
    let png = |tag: u8| -> Vec<u8> { vec![0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, tag] };
    vec![
        ("[Content_Types].xml", xml(content_types)),
        ("_rels/.rels", xml(dot_rels)),
        ("word/_rels/document.xml.rels", xml(doc_rels)),
        ("word/footer1.xml", xml(footer)),
        ("word/header1.xml", xml(header)),
        ("word/_rels/header1.xml.rels", xml(header_rels)),
        ("word/comments.xml", xml(comments)),
        ("word/media/image1.png", png(1)),
        ("word/media/image2.png", png(2)),
        ("word/theme/theme1.xml", xml(theme)),
        ("word/settings.xml", xml(settings)),
        ("customXml/item1.xml", xml(item)),
        ("customXml/_rels/item1.xml.rels", xml(item_rels)),
        ("customXml/itemProps1.xml", xml(item_props)),
        ("word/numbering.xml", xml(numbering)),
        ("word/styles.xml", xml(styles)),
        ("word/fontTable.xml", xml(font_table)),
        ("docProps/core.xml", xml(core)),
        ("docProps/app.xml", xml(app)),
    ]
}

/// Issues #134 / #135 fixture: `word/document.xml` sits where Word puts it
/// (after the rels, before the parts it references).
fn build_word_package_parts_docx() -> Vec<u8> {
    use std::io::Write;
    use zip::write::{SimpleFileOptions, ZipWriter};
    let parts = word_package_parts();
    let document_xml = word_package_document_xml();
    let mut buf: Vec<u8> = Vec::new();
    {
        let mut zip = ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(0o644);
        for (i, (name, body)) in parts.iter().enumerate() {
            if i == 3 {
                zip.start_file("word/document.xml", opts).unwrap();
                zip.write_all(document_xml.as_bytes()).unwrap();
            }
            zip.start_file(*name, opts).unwrap();
            zip.write_all(body).unwrap();
        }
        zip.finish().unwrap();
    }
    buf
}

/// Issue #188 — step 25: relationship ids are scoped per OPC part. The
/// body, header and footer of the fixture each declare `rId5`: the body's
/// and the footer's name `image1.jpeg`, the header's `image2.jpeg`.
/// (a) media is keyed by the part-resolved target (two blobs, the footer
/// deduped onto the body's) and each picture carries its media key;
/// (b) an edit of the body text saves through the UI path (`save_docx`,
/// package present) with every sibling byte-identical and the pictures'
/// part-local `r:embed="rId5"` untouched, and the re-read resolves each
/// part's picture to its own blob again.
fn run_part_scoped_media_roundtrip() -> Result<()> {
    use format_docx::test_fixtures::part_scoped_media_docx;
    const BODY: &[u8] = b"\xFF\xD8body-picture";
    const HEADER: &[u8] = b"\xFF\xD8header-picture";
    let fixture = part_scoped_media_docx(BODY, HEADER);
    let archive = read_docx(&fixture).context("read part-scoped media fixture")?;
    let key_of = |blocks: &[engine::Block]| -> Option<String> {
        blocks.iter().find_map(|b| {
            b.as_paragraph()?
                .inline_objects
                .iter()
                .find_map(|io| io.kind.image_media_key().map(str::to_string))
        })
    };
    let check = |doc: &DocumentTree, what: &str| -> Result<()> {
        let body: Vec<engine::Block> = doc.blocks.iter().cloned().collect();
        let keys = (
            key_of(&body),
            doc.headers.get("rId7").and_then(|b| key_of(b)),
            doc.footers.get("rId8").and_then(|b| key_of(b)),
        );
        let blob = |k: &Option<String>| {
            k.as_deref()
                .and_then(|k| doc.media.get(k))
                .map(|b| b.data.as_slice())
        };
        if doc.media.len() != 2
            || blob(&keys.0) != Some(BODY)
            || blob(&keys.1) != Some(HEADER)
            || blob(&keys.2) != Some(BODY)
        {
            bail!("step 25 ({what}): part pictures resolve wrong: {keys:?}");
        }
        Ok(())
    };
    check(&archive.document, "read")?;
    println!("[roundtrip] step 25a OK — each part's rId5 resolves to its own picture");

    let edited = archive.document.insert_text(
        engine::LogicalPos {
            path: engine::BlockPath::top(0),
            offset: 0,
        },
        "Edited ",
    );
    let saved = format_docx::save_docx(&edited).context("UI save")?;
    let (before, after) = (zip_entries(&fixture)?, zip_entries(&saved)?);
    for (name, bytes) in before.iter().filter(|(n, _)| n != "word/document.xml") {
        if after.iter().find(|(n, _)| n == name).map(|(_, b)| b) != Some(bytes) {
            bail!("step 25: sibling {name} drifted");
        }
    }
    let doc_xml = String::from_utf8(extract_doc_xml(&saved)?).context("utf8")?;
    if !doc_xml.contains(r#"r:embed="rId5""#) {
        bail!("step 25: the body picture lost its part-local id");
    }
    check(&read_docx(&saved).context("re-read")?.document, "re-read")?;
    println!(
        "[roundtrip] step 25b OK — UI save keeps siblings byte-identical, the re-read resolves each part"
    );
    Ok(())
}

/// Every `(entry name, bytes)` of a saved package, in archive order.
fn zip_entries(docx: &[u8]) -> Result<Vec<(String, Vec<u8>)>> {
    use std::io::Read;
    let mut a = zip::ZipArchive::new(std::io::Cursor::new(docx)).context("open zip")?;
    let mut out = Vec::with_capacity(a.len());
    for i in 0..a.len() {
        let mut f = a.by_index(i).context("zip entry")?;
        let mut b = Vec::new();
        f.read_to_end(&mut b).context("read entry")?;
        out.push((f.name().to_owned(), b));
    }
    Ok(out)
}

/// Issue #135 — step 21: open the Word-shaped package (two pictures),
/// insert a third picture through the model, `write_docx`: three media
/// parts (the originals byte-identical), a new image relationship whose id
/// collides with no rels part, a `<Default>` for the new extension, every
/// other sibling byte-identical, and the re-read resolves all three.
/// Issue #134 — step 22: the live editor's save path. engine-wasm
/// `SaveDocx` / `SaveDocument` hold only the tree and call
/// `format_docx::save_docx`; the tree must carry its source package (and
/// keep it through the crash-recovery snapshot codec), so a UI save of an
/// opened Word package re-emits every sibling part byte-identical, every
/// `headerReference` / `footerReference` resolves, and the file is the
/// same one the harness path (`write_docx` with the archive) writes. An
/// engine-authored document still saves through `build_minimal_docx`.
fn run_package_ui_save() -> Result<()> {
    use engine::{BlockPath, LogicalPos};
    let fixture = build_word_package_parts_docx();
    let archive = read_docx(&fixture).context("read word_package_parts")?;
    if archive.document.source_package.is_none() {
        bail!("step 22: the opened tree does not retain its source package");
    }
    /* The crash-recovery snapshot envelope round-trips the package. */
    let snap = engine::snapshot::encode(&archive.document).context("snapshot encode")?;
    let restored: DocumentTree = engine::snapshot::decode(&snap)
        .context("snapshot decode")?
        .payload;
    if restored.source_package != archive.document.source_package {
        bail!("step 22: snapshot lost the source package");
    }
    let para = restored.nth_paragraph(4).context("paragraph 4")?;
    let edited = restored.insert_text(
        LogicalPos {
            path: BlockPath::top(4),
            offset: para.text.len() as u32,
        },
        INSERT_TEXT,
    );
    let ui = format_docx::save_docx(&edited).context("UI-path save")?;
    if ui != write_docx(&archive, &edited).context("harness save")? {
        bail!("step 22: UI-path save differs from write_docx with the archive");
    }
    format_docx::check_document_xml_well_formed(&ui).context("well-formed document.xml")?;
    let source = zip_entries(&fixture)?;
    let out = zip_entries(&ui)?;
    for (name, bytes) in &source {
        if name == "word/document.xml" {
            continue;
        }
        match out.iter().find(|(n, _)| n == name) {
            Some((_, b)) if b == bytes => {}
            Some(_) => bail!("step 22: sibling {name} not byte-identical"),
            None => bail!("step 22: UI save dropped {name}"),
        }
        if name.ends_with(".xml") {
            format_docx::check_part_xml_well_formed(&ui, name)
                .with_context(|| format!("step 22: {name} well-formed"))?;
        }
    }
    if out.len() != source.len() {
        bail!(
            "step 22: {} entries saved, {} in the source",
            out.len(),
            source.len()
        );
    }
    let reread = read_docx(&ui).context("re-read")?;
    let rels = format_docx::parts::rels::parse_rels_xml(
        reread
            .part_by_name("word/_rels/document.xml.rels")
            .context("rels")?,
    )
    .context("rels parse")?;
    let mut refs = 0;
    for s in reread.document.effective_sections() {
        for rid in [
            &s.header_refs.default,
            &s.header_refs.first,
            &s.header_refs.even,
            &s.footer_refs.default,
            &s.footer_refs.first,
            &s.footer_refs.even,
        ]
        .into_iter()
        .flatten()
        {
            let target = rels
                .get(rid)
                .with_context(|| format!("step 22: {rid} unresolved"))?;
            let entry = format_docx::parts::rels::resolve_target(target);
            if reread.part_by_name(&entry).is_none() {
                bail!("step 22: {rid} points at missing part {entry}");
            }
            refs += 1;
        }
    }
    if refs != 2 {
        bail!("step 22: expected a header + a footer reference, found {refs}");
    }
    let delta = extract_doc_xml(&ui)?
        .len()
        .abs_diff(extract_doc_xml(&fixture)?.len());
    if delta > 2 * INSERT_TEXT.len() {
        bail!(
            "step 22: document.xml delta {delta} B > 2 x {}",
            INSERT_TEXT.len()
        );
    }
    /* From scratch: no package, the minimal-package writer. */
    let fresh = DocumentTree::from_paragraphs(["fresh".to_string()]);
    if format_docx::save_docx(&fresh).context("fresh save")?
        != build_minimal_docx(&fresh).context("minimal")?
    {
        bail!("step 22: an engine-authored document must save through build_minimal_docx");
    }
    println!(
        "[roundtrip] step 22 OK — UI-path save keeps {} sibling parts byte-identical, {refs} header/footer refs resolve, Δ {delta} B",
        source.len() - 1
    );
    Ok(())
}

fn run_package_media_insert() -> Result<()> {
    use engine::{BlockPath, ImageBlob, InlineKind, LogicalPos};
    let fixture = build_word_package_parts_docx();
    let archive = read_docx(&fixture).context("read word_package_parts")?;
    let para = archive
        .document
        .nth_paragraph(3)
        .context("picture paragraph")?;
    let gif: &[u8] = b"GIF89a\x01\x00\x01\x00";
    let edited = archive.document.insert_inline_image_at(
        LogicalPos {
            path: BlockPath::top(3),
            offset: para.text.len() as u32,
        },
        ImageBlob {
            content_type: "image/gif".into(),
            data: gif.to_vec(),
        },
        914_400,
        914_400,
    );
    let saved = write_docx(&archive, &edited).context("write")?;
    format_docx::check_document_xml_well_formed(&saved).context("well-formed document.xml")?;
    let source = zip_entries(&fixture)?;
    let out = zip_entries(&saved)?;
    let get = |all: &[(String, Vec<u8>)], name: &str| -> Option<Vec<u8>> {
        all.iter().find(|(n, _)| n == name).map(|(_, b)| b.clone())
    };
    let media: Vec<&str> = out
        .iter()
        .map(|(n, _)| n.as_str())
        .filter(|n| n.starts_with("word/media/"))
        .collect();
    if media
        != [
            "word/media/image1.png",
            "word/media/image2.png",
            "word/media/image3.gif",
        ]
    {
        bail!("step 21: media parts {media:?}");
    }
    if get(&out, "word/media/image3.gif").as_deref() != Some(gif) {
        bail!("step 21: new media bytes differ");
    }
    for (name, bytes) in &source {
        if matches!(
            name.as_str(),
            "word/document.xml" | "word/_rels/document.xml.rels" | "[Content_Types].xml"
        ) {
            continue;
        }
        if get(&out, name).as_ref() != Some(bytes) {
            bail!("step 21: sibling {name} not byte-identical");
        }
    }
    let rels_bytes = get(&out, "word/_rels/document.xml.rels").context("rels")?;
    let rels =
        format_docx::opc::relationships::parse_relationships(&rels_bytes).context("rels parse")?;
    let new = rels
        .by_id("rId12")
        .context("step 21: new relationship rId12 missing")?;
    if new.target != "media/image3.gif" || !new.rel_type.ends_with("/image") {
        bail!("step 21: new relationship {new:?}");
    }
    let mut ids: Vec<&str> = rels.items.iter().map(|r| r.id.as_str()).collect();
    ids.sort_unstable();
    ids.dedup();
    if ids.len() != rels.items.len() {
        bail!("step 21: duplicate relationship ids");
    }
    let ct = String::from_utf8(get(&out, "[Content_Types].xml").context("content types")?)?;
    if ct.matches(r#"<Default Extension="gif""#).count() != 1 {
        bail!("step 21: gif content-type default missing or duplicated");
    }
    let reread = read_docx(&saved).context("re-read")?;
    let para = reread
        .document
        .nth_paragraph(3)
        .context("re-read paragraph")?;
    let ids: Vec<String> = para
        .inline_objects
        .iter()
        .filter_map(|io| match &io.kind {
            InlineKind::Image { rel_id, .. } => Some(rel_id.clone()),
            _ => None,
        })
        .collect();
    /* Issue #188 — the blobs resolve through the part-resolved media key. */
    let keys: Vec<&str> = para
        .inline_objects
        .iter()
        .filter_map(|io| io.kind.image_media_key())
        .collect();
    if ids != ["rId9", "rId10", "rId12"]
        || keys.iter().any(|k| !reread.document.media.contains_key(*k))
    {
        bail!("step 21: re-read picture ids {ids:?}");
    }
    println!(
        "[roundtrip] step 21 OK — an inserted picture adds media + rels + content type, originals byte-identical"
    );
    Ok(())
}

/* ========================================================= --fixtures ==== */

fn run_fixtures(dir: &Path) -> Result<()> {
    let manifest_path = dir.join(MANIFEST_NAME);
    let manifest_bytes = std::fs::read(&manifest_path)
        .with_context(|| format!("read {}", manifest_path.display()))?;
    let manifest: ManifestFile =
        serde_json::from_slice(&manifest_bytes).context("parse manifest")?;

    let mut docx_files: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("walk {}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|ext| ext == "docx"))
        .collect();
    docx_files.sort();

    if docx_files.is_empty() {
        bail!("no .docx fixtures in {}", dir.display());
    }

    let mut failures: Vec<String> = Vec::new();
    for path in &docx_files {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("?")
            .to_owned();
        match validate_fixture(path, &manifest) {
            Ok(()) => println!("[fixtures] PASS {name}"),
            Err(e) => {
                println!("[fixtures] FAIL {name}: {e:#}");
                failures.push(name);
            }
        }
    }

    /* Cross-check: every manifest entry has a matching file. */
    for name in manifest.fixtures.keys() {
        let path = dir.join(name);
        if !path.exists() {
            println!("[fixtures] FAIL {name}: manifest entry has no matching .docx");
            failures.push(name.clone());
        }
    }

    if failures.is_empty() {
        println!("\nPASS — {} fixtures, all green", docx_files.len());
        Ok(())
    } else {
        bail!(
            "{} fixture failure(s): {}",
            failures.len(),
            failures.join(", ")
        )
    }
}

fn validate_fixture(path: &Path, manifest: &ManifestFile) -> Result<()> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow::anyhow!("non-utf8 filename"))?;
    let entry = manifest
        .fixtures
        .get(name)
        .with_context(|| format!("no manifest entry for `{name}`"))?;

    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let archive_a = read_docx(&bytes).context("read_docx")?;

    /* 1. Manifest assertions. */
    let got_count = archive_a.document.paragraph_count();
    if got_count != entry.asserts.paragraph_count {
        bail!(
            "paragraph_count: expected {}, got {got_count}",
            entry.asserts.paragraph_count
        );
    }
    for (i, expected) in entry.asserts.paragraph_texts.iter().enumerate() {
        let got = archive_a.document.paragraph_text(i as u32);
        if got != Some(expected.as_str()) {
            bail!("paragraph_text({i}): expected `{expected}`, got {got:?}");
        }
    }

    /* 2. Re-emit, guard, re-parse. */
    let edited_bytes = write_docx(&archive_a, &archive_a.document).context("write_docx")?;
    assert_document_xml_well_formed(&edited_bytes)?;
    let archive_b = read_docx(&edited_bytes).context("re-read")?;

    /* 3. Siblings byte-identical. */
    for (sibling_name, raw_a) in &archive_a.other_entries {
        let raw_b = archive_b
            .other_entries
            .iter()
            .find(|(n, _)| n == sibling_name)
            .map(|(_, b)| b)
            .with_context(|| format!("sibling `{sibling_name}` missing on re-read"))?;
        if raw_a != raw_b {
            bail!("sibling `{sibling_name}` drifted on round-trip");
        }
    }

    /* 4. document.xml drift bound. */
    let doc_a = extract_doc_xml(&bytes).context("extract original document.xml")?;
    let doc_b = extract_doc_xml(&edited_bytes).context("extract re-emitted document.xml")?;
    let drift = (doc_b.len() as isize - doc_a.len() as isize).unsigned_abs();
    let bound = entry.roundtrip.document_xml_drift_bytes;
    if drift > bound {
        bail!(
            "document.xml drift {drift} B exceeds bound {bound} B \
             (original {} B → re-emitted {} B)",
            doc_a.len(),
            doc_b.len()
        );
    }

    /* 5. Semantic equality across the round-trip. */
    if !documents_equivalent(&archive_a, &archive_b) {
        bail!("semantic round-trip mismatch — second parse differs from first");
    }

    Ok(())
}

/// Paragraph-by-paragraph equality on text + spans + props. Skips
/// `Block::Table` content — Phase 5 PR 1 treats tables as opaque
/// passthrough; their bytes are validated by the sibling-entry check.
fn documents_equivalent(a: &DocxArchive, b: &DocxArchive) -> bool {
    let pa: Vec<_> = a
        .document
        .blocks
        .iter()
        .filter_map(engine::Block::as_paragraph)
        .collect();
    let pb: Vec<_> = b
        .document
        .blocks
        .iter()
        .filter_map(engine::Block::as_paragraph)
        .collect();
    if pa.len() != pb.len() {
        return false;
    }
    for (x, y) in pa.iter().zip(pb.iter()) {
        if x.text != y.text || x.props != y.props {
            return false;
        }
        if x.spans.len() != y.spans.len() {
            return false;
        }
        for (sx, sy) in x.spans.iter().zip(y.spans.iter()) {
            if sx != sy {
                return false;
            }
        }
    }
    true
}

/* ========================================================== --gen-seed ==== */

fn run_gen_seed(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("mkdir {}", dir.display()))?;
    let mut manifest = ManifestFile {
        fixtures: BTreeMap::new(),
    };
    for fx in seed_fixtures() {
        let bytes = build_minimal_docx(&fx.doc).context("build seed")?;
        /* Issue #109 — pin an explicit A4 `<w:pgSz>` into every seed
        fixture EXCEPT `pPr_bidi_rtl.docx`: `build_minimal_docx` already
        regenerates that one file with different bytes than what's
        committed today (a pre-existing gap, unrelated to #109 — see
        `ppr_fixtures`), so it is left exactly as-is rather than pinned
        on top of an already-drifting base. */
        let (bytes, drift_bound) = if fx.name == "pPr_bidi_rtl.docx" {
            (bytes, 0)
        } else {
            (pin_explicit_a4_sect_pr(&bytes), sect_pr_compaction_delta())
        };
        let path = dir.join(fx.name);
        std::fs::write(&path, &bytes).with_context(|| format!("write {}", path.display()))?;

        let texts: Vec<String> = fx
            .doc
            .blocks
            .iter()
            .filter_map(engine::Block::as_paragraph)
            .map(|p| p.text.clone())
            .collect();
        manifest.fixtures.insert(
            fx.name.to_owned(),
            FixtureEntry {
                generator: fx.generator.into(),
                phase_introduced: fx.phase,
                asserts: FixtureAsserts {
                    paragraph_count: texts.len() as u32,
                    paragraph_texts: texts,
                },
                roundtrip: RoundtripBounds {
                    document_xml_drift_bytes: drift_bound,
                },
            },
        );
        println!("[gen-seed] wrote {} ({} B)", path.display(), bytes.len());
    }
    /* Phase 3 fixtures don't fit `build_minimal_docx` (they need a custom
    `word/styles.xml`); each carries its own raw-byte builder. */
    for fx in prebuilt_fixtures() {
        let path = dir.join(fx.name);
        std::fs::write(&path, &fx.bytes).with_context(|| format!("write {}", path.display()))?;
        manifest.fixtures.insert(fx.name.to_owned(), fx.entry);
        println!("[gen-seed] wrote {} ({} B)", path.display(), fx.bytes.len());
    }
    let manifest_path = dir.join(MANIFEST_NAME);
    let manifest_json = serde_json::to_string_pretty(&manifest).context("serialize manifest")?;
    std::fs::write(&manifest_path, format!("{manifest_json}\n"))
        .with_context(|| format!("write {}", manifest_path.display()))?;
    println!("[gen-seed] wrote {}", manifest_path.display());
    Ok(())
}

struct SeedFixture {
    name: &'static str,
    phase: u8,
    generator: &'static str,
    doc: DocumentTree,
}

fn seed_fixtures() -> Vec<SeedFixture> {
    let mut out = vec![
        SeedFixture {
            name: "simple_text.docx",
            phase: 1,
            generator: "build_minimal_docx",
            doc: DocumentTree::from_text("hello world"),
        },
        SeedFixture {
            name: "simple_arabic.docx",
            phase: 1,
            generator: "build_minimal_docx",
            doc: DocumentTree::from_text("السلام عليكم"),
        },
        SeedFixture {
            name: "simple_xml_escapes.docx",
            phase: 1,
            generator: "build_minimal_docx",
            doc: DocumentTree::from_text("<a> & </a>"),
        },
    ];
    out.extend(ppr_fixtures());
    out
}

/* --- Phase 2: paragraph-properties fixtures.
Each one isolates one `<w:pPr>` child element. The Word-authored
ground-truth `.docx` files were not in the tree at Phase 2 kickoff;
these are `handcrafted` via our own writer so the harness exercises the
pPr reader + writer end-to-end. Phase 3 / 5 swap in true Word fixtures.

Issue #109 — `run_gen_seed` pins an explicit A4 `<w:pgSz>` into every one
of these EXCEPT `pPr_bidi_rtl.docx`: regenerating that one fixture via
`build_minimal_docx` already produces different bytes than what's
committed today (a pre-existing gap this PR did not introduce and does
not fix — the RTL/`Alignment::End` combination the writer emits for it
has drifted from the committed file at some point since it was last
regenerated). Pinning pgSz on top of an already-drifting base would
just be a second, unrelated change riding the same commit, so it is
left untouched; its committed bytes still lack an explicit `<w:pgSz>`,
which incidentally keeps at least one fixture exercising the
`<w:sectPr>`-omits-`<w:pgSz>` reader fallback this issue is about. */
fn ppr_fixtures() -> Vec<SeedFixture> {
    let mk = |name, props: ParaProperties, text: &str| SeedFixture {
        name,
        phase: 2,
        generator: "handcrafted",
        doc: DocumentTree::from_rich_paragraphs([Paragraph {
            text: text.to_owned(),
            spans: Vec::new(),
            props,
            list_item: None,
            resolved_marker: None,
            resolved_list_indent: None,
            dirty: false,
            source_xml: None,
            inline_objects: Vec::new(),
            hyperlinks: Vec::new(),
            revisions: Vec::new(),
            fields: Vec::new(),
            style_id: None,
            direct_overrides: ParaProperties::default(),
            section_end: None,
            bookmarks: Vec::new(),
            body_xml: None,
            source_markup: None,
            mark_revision: None,
        }]),
    };
    vec![
        mk(
            "pPr_jc_center.docx",
            ParaProperties {
                alignment: Some(Alignment::Center),
                ..Default::default()
            },
            "centered heading",
        ),
        mk(
            "pPr_ind_firstline.docx",
            ParaProperties {
                indent: Indent {
                    start_twips: 720,
                    first_line_twips: 360,
                    ..Default::default()
                },
                ..Default::default()
            },
            "first line is indented further than the body of this paragraph",
        ),
        mk(
            "pPr_spacing.docx",
            ParaProperties {
                spacing: Spacing {
                    before_twips: 120,
                    after_twips: 240,
                },
                ..Default::default()
            },
            "paragraph with extra space above and below",
        ),
        mk(
            "pPr_bidi_rtl.docx",
            ParaProperties {
                direction: Some(TextDirection::Rtl),
                alignment: Some(Alignment::End),
                ..Default::default()
            },
            "السلام عليكم ورحمة الله وبركاته",
        ),
    ]
}

/* --- Phase 3: pre-built fixtures.
These ship as raw `.docx` bytes (not an engine `DocumentTree` we can
serialise via `build_minimal_docx`) because they need a custom
`word/styles.xml`. */

struct PrebuiltFixture {
    name: &'static str,
    bytes: Vec<u8>,
    entry: FixtureEntry,
}

fn prebuilt_fixtures() -> Vec<PrebuiltFixture> {
    vec![
        /* Issue #69 — a `<wp:anchor>` floating picture; passthrough drift 0
        on a zero-edit resave, exact regeneration in step 10. */
        PrebuiltFixture {
            name: "floating_image_anchor.docx",
            bytes: build_floating_image_anchor_docx(),
            entry: FixtureEntry {
                generator: "handcrafted".into(),
                phase_introduced: 11,
                asserts: FixtureAsserts {
                    paragraph_count: 3,
                    paragraph_texts: vec![
                        "before".into(),
                        "float \u{FFFC}here".into(),
                        "after".into(),
                    ],
                },
                roundtrip: RoundtripBounds::default(),
            },
        },
        /* Issue #82 — one floating picture per wrap mode; passthrough
        drift 0 on a zero-edit resave, exact regeneration in step 14
        (`run_wrap_modes_roundtrip` compares a resave against the pinned
        source by exact string equality). Issue #109's pgSz pin is
        deliberately NOT applied here for the same reason as
        `grab_bag_exotic.docx` / `table_cell_runs.docx` above — the
        writer's trailing-sectPr compaction would desync that comparison. */
        PrebuiltFixture {
            name: "image_wrap_modes.docx",
            bytes: build_image_wrap_modes_docx(),
            entry: FixtureEntry {
                generator: "handcrafted".into(),
                phase_introduced: 11,
                asserts: FixtureAsserts {
                    paragraph_count: 5,
                    paragraph_texts: WRAP_CASES
                        .iter()
                        .map(|(label, _, _)| format!("{label} \u{FFFC}text"))
                        .collect(),
                },
                roundtrip: RoundtripBounds::default(),
            },
        },
        PrebuiltFixture {
            name: "footnotes_endnotes.docx",
            bytes: build_footnotes_endnotes_docx(),
            entry: FixtureEntry {
                generator: "handcrafted".into(),
                phase_introduced: 11,
                asserts: FixtureAsserts {
                    paragraph_count: 3,
                    paragraph_texts: vec![
                        "Alpha body\u{FFFC} continues".into(),
                        "Beta body\u{FFFC}\u{FFFC}".into(),
                        "Gamma body\u{FFFC}\u{FFFC}".into(),
                    ],
                },
                roundtrip: RoundtripBounds::default(),
            },
        },
        /* Issue #202 — style-inherited paragraph direction. Passthrough
        at drift 0; the default harness's step 23 edits the style-RTL
        paragraph. */
        PrebuiltFixture {
            name: "pPr_bidi_style.docx",
            bytes: build_style_bidi_docx(),
            entry: FixtureEntry {
                generator: "handcrafted".into(),
                phase_introduced: 11,
                asserts: FixtureAsserts {
                    paragraph_count: 3,
                    paragraph_texts: vec!["Word مرحبا".into(), "Word مرحبا".into(), "plain".into()],
                },
                roundtrip: RoundtripBounds::default(),
            },
        },
        PrebuiltFixture {
            name: "style_cascade.docx",
            bytes: build_style_cascade_docx(),
            entry: FixtureEntry {
                generator: "handcrafted".into(),
                phase_introduced: 3,
                asserts: FixtureAsserts {
                    paragraph_count: 1,
                    paragraph_texts: vec!["hello cascade".into()],
                },
                /* Issue #109 — pinned A4 pgSz compacts back to the bare
                `<w:sectPr/>` footer on a no-op resave (`sect_pr_compaction_delta`). */
                roundtrip: RoundtripBounds {
                    document_xml_drift_bytes: sect_pr_compaction_delta(),
                },
            },
        },
        PrebuiltFixture {
            name: "list_bullet_numbered.docx",
            bytes: build_list_bullet_numbered_docx(),
            entry: FixtureEntry {
                generator: "handcrafted".into(),
                phase_introduced: 4,
                asserts: FixtureAsserts {
                    paragraph_count: 5,
                    paragraph_texts: vec![
                        "bullet alpha".into(),
                        "bullet beta".into(),
                        "first ordered item".into(),
                        "first nested item".into(),
                        "second ordered item".into(),
                    ],
                },
                roundtrip: RoundtripBounds {
                    document_xml_drift_bytes: sect_pr_compaction_delta(),
                },
            },
        },
        PrebuiltFixture {
            name: "table_2x2_opaque.docx",
            bytes: build_table_2x2_opaque_docx(),
            entry: FixtureEntry {
                generator: "handcrafted".into(),
                phase_introduced: 5,
                /* Two surrounding paragraphs; the table sits between them.
                Phase 5 PR 2 now fully parses rows + cells; the source
                bytes still ride the passthrough writer, so the only drift
                on a no-op resave is the pinned A4 pgSz (issue #109)
                compacting back to the bare sectPr footer. */
                asserts: FixtureAsserts {
                    paragraph_count: 2,
                    paragraph_texts: vec!["before".into(), "after".into()],
                },
                roundtrip: RoundtripBounds {
                    document_xml_drift_bytes: sect_pr_compaction_delta(),
                },
            },
        },
        /* Phase 5 PR 2 — full row/cell/tcPr feature coverage. Every
        fixture round-trips via Phase 3 passthrough: the captured
        `<w:tbl>` source bytes are emitted verbatim, so a no-op resave's
        only drift is the pinned A4 pgSz (issue #109) compacting back to
        the bare trailing sectPr footer. */
        PrebuiltFixture {
            name: "table_grid_span.docx",
            bytes: build_table_grid_span_docx(),
            entry: FixtureEntry {
                generator: "handcrafted".into(),
                phase_introduced: 5,
                asserts: FixtureAsserts {
                    paragraph_count: 1,
                    paragraph_texts: vec!["intro".into()],
                },
                roundtrip: RoundtripBounds {
                    document_xml_drift_bytes: sect_pr_compaction_delta(),
                },
            },
        },
        PrebuiltFixture {
            name: "table_vmerge.docx",
            bytes: build_table_vmerge_docx(),
            entry: FixtureEntry {
                generator: "handcrafted".into(),
                phase_introduced: 5,
                asserts: FixtureAsserts {
                    paragraph_count: 1,
                    paragraph_texts: vec!["intro".into()],
                },
                roundtrip: RoundtripBounds {
                    document_xml_drift_bytes: sect_pr_compaction_delta(),
                },
            },
        },
        PrebuiltFixture {
            name: "table_borders_double.docx",
            bytes: build_table_borders_double_docx(),
            entry: FixtureEntry {
                generator: "handcrafted".into(),
                phase_introduced: 5,
                asserts: FixtureAsserts {
                    paragraph_count: 1,
                    paragraph_texts: vec!["intro".into()],
                },
                roundtrip: RoundtripBounds {
                    document_xml_drift_bytes: sect_pr_compaction_delta(),
                },
            },
        },
        PrebuiltFixture {
            name: "table_shaded_header.docx",
            bytes: build_table_shaded_header_docx(),
            entry: FixtureEntry {
                generator: "handcrafted".into(),
                phase_introduced: 5,
                asserts: FixtureAsserts {
                    paragraph_count: 1,
                    paragraph_texts: vec!["intro".into()],
                },
                roundtrip: RoundtripBounds {
                    document_xml_drift_bytes: sect_pr_compaction_delta(),
                },
            },
        },
        PrebuiltFixture {
            name: "table_in_rtl_doc.docx",
            bytes: build_table_in_rtl_doc_docx(),
            entry: FixtureEntry {
                generator: "handcrafted".into(),
                phase_introduced: 5,
                asserts: FixtureAsserts {
                    paragraph_count: 1,
                    paragraph_texts: vec!["مقدمة".into()],
                },
                roundtrip: RoundtripBounds {
                    document_xml_drift_bytes: sect_pr_compaction_delta(),
                },
            },
        },
        /* Issue #84 — exotic (unmodeled) rPr / pPr / tblPr / trPr / tcPr
        children. Untouched it rides the passthrough at drift 0; the
        default harness's step 9 is the dirty-regeneration check. */
        PrebuiltFixture {
            name: "grab_bag_exotic.docx",
            bytes: build_grab_bag_exotic_docx(),
            entry: FixtureEntry {
                generator: "handcrafted".into(),
                phase_introduced: 9,
                asserts: FixtureAsserts {
                    paragraph_count: 2,
                    paragraph_texts: vec!["exotic run".into(), "after".into()],
                },
                roundtrip: RoundtripBounds::default(),
            },
        },
        /* Issue #110 — `word/document.xml` prefixed with a UTF-8 BOM, the
        way docx4j and Apache POI write it (`toc.docx`, `55733.docx`, …).
        quick-xml strips the BOM without counting it in
        `buffer_position()`, which used to shift every passthrough capture
        three bytes early and resave `</w<w:sectPr/>`. Base drift bound 3:
        the writer synthesizes its own declaration and never re-emits the
        BOM; every paragraph must otherwise splice byte-exact. Issue #109
        adds `sect_pr_compaction_delta()` on top — the pinned A4 pgSz
        compacts back to the bare sectPr footer on the same resave. */
        PrebuiltFixture {
            name: "bom_utf8_passthrough.docx",
            bytes: build_bom_utf8_passthrough_docx(),
            entry: FixtureEntry {
                generator: "handcrafted".into(),
                phase_introduced: 10,
                asserts: FixtureAsserts {
                    paragraph_count: 2,
                    paragraph_texts: vec!["first".into(), "second".into()],
                },
                /* Issue #112 — the BOM, the declaration and the pinned
                sectPr are all source bytes now: drift 0. */
                roundtrip: RoundtripBounds {
                    document_xml_drift_bytes: 0,
                },
            },
        },
        /* Issue #111 — 200 nested tables (Apache POI's `deep-table-cell.docx`
        goes to 5000). The reader must open it on a bounded stack in well
        under a second and the outer table rides the passthrough at drift 0
        whatever depth the typed model stops at. */
        /* Issue #100 — Word-shaped `w14:paraId` on every paragraph, bound
        only on the root. Passthrough writer, but step 12 compares two
        FRESH saves of the same edited tree (the UI path vs the archive
        path) rather than the pinned source, so pinning A4 pgSz (issue
        #109) here only adds the usual `sect_pr_compaction_delta()` —
        both save paths compact it identically, so `doc_ui == doc_archive`
        still holds. */
        PrebuiltFixture {
            name: "w14_paraid_word.docx",
            bytes: build_w14_paraid_docx(),
            entry: FixtureEntry {
                generator: "handcrafted".into(),
                phase_introduced: 11,
                asserts: FixtureAsserts {
                    paragraph_count: 3,
                    paragraph_texts: vec![
                        "first paragraph".into(),
                        "second paragraph".into(),
                        "after".into(),
                    ],
                },
                roundtrip: RoundtripBounds {
                    document_xml_drift_bytes: sect_pr_compaction_delta(),
                },
            },
        },
        /* Issue #81 — Word's multi-paragraph TOC shape (hyperlinked
        entries, nested PAGEREFs, dot leaders, `_Toc*` bookmarks).
        Passthrough at drift 0; step 15 edits + regenerates it. Issue
        #109's pgSz pin is deliberately NOT applied here — same reasoning
        as `grab_bag_exotic.docx` et al above: step 15a's untouched-save
        check (`run_toc_roundtrip`) is `extract_doc_xml(&bytes)? !=
        extract_doc_xml(&fixture)?`, an exact comparison against the
        pinned source with no edit to account for the writer's
        trailing-sectPr compaction. */
        PrebuiltFixture {
            name: "toc_word_shape.docx",
            bytes: build_toc_word_shape_docx(),
            entry: FixtureEntry {
                generator: "handcrafted".into(),
                phase_introduced: 11,
                asserts: FixtureAsserts {
                    paragraph_count: 5,
                    paragraph_texts: vec![
                        "Alpha\t1".into(),
                        "Beta\t1".into(),
                        "".into(),
                        "Alpha".into(),
                        "Beta".into(),
                    ],
                },
                roundtrip: RoundtripBounds::default(),
            },
        },
        /* Issue #101 — mixed run formatting + a picture inside table
        cells. Passthrough at drift 0; step 13 edits both cells and
        requires the regenerated document.xml equal SOURCE + edits
        exactly (`run_table_cell_runs_survival`'s `expected =
        doc_a.replacen(...)`). Issue #109's pgSz pin is deliberately
        NOT applied here — same reasoning as `grab_bag_exotic.docx` /
        `floating_image_anchor.docx` / `footnotes_endnotes.docx` above:
        the writer's trailing-sectPr compaction would desync that exact
        comparison, and fixing it is out of this change's scope. */
        PrebuiltFixture {
            name: "table_cell_runs.docx",
            bytes: build_table_cell_runs_docx(),
            entry: FixtureEntry {
                generator: "handcrafted".into(),
                phase_introduced: 11,
                asserts: FixtureAsserts {
                    paragraph_count: 2,
                    paragraph_texts: vec!["intro".into(), "after".into()],
                },
                roundtrip: RoundtripBounds::default(),
            },
        },
        /* Issue #83 — two floating text boxes (one RTL, one with a VML
        fallback) with square wrap. Passthrough at drift 0; the default
        harness's step 16 edits a story. */
        PrebuiltFixture {
            name: "text_boxes_wrap.docx",
            bytes: build_text_boxes_docx(),
            entry: FixtureEntry {
                generator: "handcrafted".into(),
                phase_introduced: 11,
                asserts: FixtureAsserts {
                    paragraph_count: 2,
                    paragraph_texts: vec![
                        format!("Intro \u{FFFC}{TB_PROSE}"),
                        format!("\u{FFFC}{TB_ARABIC}"),
                    ],
                },
                roundtrip: RoundtripBounds::default(),
            },
        },
        /* Issue #196 — a text box nested in a text box's story.
        Passthrough at drift 0; the default harness's step 20 edits the
        nested story; the e2e spec clicks into it. */
        /* Issue #206 — floating pictures inside a text box's story and
        inside the box nested in it. Passthrough at drift 0; the default
        harness's step 24 moves / re-wraps them; the e2e spec selects,
        drags, resizes and re-wraps them. */
        PrebuiltFixture {
            name: "text_box_pictures.docx",
            bytes: build_text_box_pictures_docx(),
            entry: FixtureEntry {
                generator: "handcrafted".into(),
                phase_introduced: 11,
                asserts: FixtureAsserts {
                    paragraph_count: 2,
                    paragraph_texts: vec![
                        "Intro paragraph.".into(),
                        "\u{FFFC}Host paragraph.".into(),
                    ],
                },
                roundtrip: RoundtripBounds::default(),
            },
        },
        PrebuiltFixture {
            name: "text_boxes_nested.docx",
            bytes: build_nested_text_boxes_docx(),
            entry: FixtureEntry {
                generator: "handcrafted".into(),
                phase_introduced: 11,
                asserts: FixtureAsserts {
                    paragraph_count: 2,
                    paragraph_texts: vec![
                        "Intro paragraph.".into(),
                        "\u{FFFC}Host paragraph.".into(),
                    ],
                },
                roundtrip: RoundtripBounds::default(),
            },
        },
        PrebuiltFixture {
            name: "table_nested_200_deep.docx",
            bytes: build_table_nested_deep_docx(200),
            entry: FixtureEntry {
                generator: "handcrafted".into(),
                phase_introduced: 10,
                asserts: FixtureAsserts {
                    paragraph_count: 1,
                    paragraph_texts: vec!["intro".into()],
                },
                roundtrip: RoundtripBounds {
                    document_xml_drift_bytes: sect_pr_compaction_delta(),
                },
            },
        },
        /* Issue #173 — `<w:jc>` + `<w:tblInd>` on fixed-width tables.
        Untouched it rides the passthrough at drift 0; the default
        harness's step 18 is the dirty-regeneration check. */
        PrebuiltFixture {
            name: "table_jc_tblind.docx",
            bytes: build_table_jc_tblind_docx(),
            entry: FixtureEntry {
                generator: "handcrafted".into(),
                phase_introduced: 10,
                asserts: FixtureAsserts {
                    paragraph_count: 4,
                    paragraph_texts: vec![
                        "intro".into(),
                        "mid one".into(),
                        "mid two".into(),
                        "after".into(),
                    ],
                },
                roundtrip: RoundtripBounds {
                    document_xml_drift_bytes: 0,
                },
            },
        },
        /* Issues #120 / #112 — every body-level construct the typed model
        does not represent, in a Word-shaped part (CRLF declaration, root
        attributes in Word's order, rsids / docGrid on the sectPr): an
        `<w:sdt>` envelope (nested, one empty) around body paragraphs and
        a table, a self-closing `<w:p …/>`, body-level bookmark / proofErr
        / commentRangeEnd markers and pretty-print whitespace. Zero-edit
        drift 0; the default harness's step 19 edits inside the control. */
        PrebuiltFixture {
            name: "body_level_passthrough.docx",
            bytes: build_body_level_passthrough_docx(),
            entry: FixtureEntry {
                generator: "handcrafted".into(),
                phase_introduced: 12,
                asserts: FixtureAsserts {
                    paragraph_count: 6,
                    paragraph_texts: vec![
                        "intro".into(),
                        "first inside".into(),
                        "second inside".into(),
                        "nested inside".into(),
                        String::new(),
                        "after".into(),
                    ],
                },
                roundtrip: RoundtripBounds {
                    document_xml_drift_bytes: 0,
                },
            },
        },
        /* Issue #248 — a pretty-printed Word table carrying every piece of
        table source markup (row / cell attributes, tblPrEx, tblGridChange,
        verified tblPr / trPr / tcPr spellings, row- and cell-level
        content controls, bookmarks between rows). Zero-edit drift 0; the
        default harness's step 30 edits and restructures it. */
        PrebuiltFixture {
            name: "table_source_markup.docx",
            bytes: table_markup::build_table_source_markup_docx(),
            entry: FixtureEntry {
                generator: "handcrafted".into(),
                phase_introduced: 12,
                asserts: FixtureAsserts {
                    paragraph_count: 2,
                    paragraph_texts: vec!["intro".into(), "after".into()],
                },
                roundtrip: RoundtripBounds {
                    document_xml_drift_bytes: 0,
                },
            },
        },
        /* Issue #119 — run-level objects the model keeps only as bytes: a
        DrawingML text box with its VML fallback (an #83 story), Word's VML
        horizontal rule, an OLE object, plus a picture with alt text and
        an `<a:extLst>`. Zero-edit drift 0; step 19 edits both paragraphs
        and resizes the picture. */
        PrebuiltFixture {
            name: "drawing_objects_preserved.docx",
            bytes: build_drawing_objects_preserved_docx(),
            entry: FixtureEntry {
                generator: "handcrafted".into(),
                phase_introduced: 12,
                asserts: FixtureAsserts {
                    paragraph_count: 2,
                    paragraph_texts: vec![
                        "a \u{FFFC}\u{FFFC}\u{FFFC} z".into(),
                        "pic \u{FFFC} end".into(),
                    ],
                },
                roundtrip: RoundtripBounds {
                    document_xml_drift_bytes: 0,
                },
            },
        },
        /* Issues #134 / #135 — a Word-shaped package: styles, numbering,
        settings, fontTable, theme, header + footer (+ header rels),
        comments, core / app props, custom XML and two body pictures.
        Zero-edit drift 0; the default harness's step 21 inserts a third
        picture through the model (#135). */
        PrebuiltFixture {
            name: "word_package_parts.docx",
            bytes: build_word_package_parts_docx(),
            entry: FixtureEntry {
                generator: "handcrafted".into(),
                phase_introduced: 12,
                asserts: FixtureAsserts {
                    paragraph_count: 5,
                    paragraph_texts: vec![
                        "Package title".into(),
                        "first item".into(),
                        "commented text".into(),
                        "pictures \u{FFFC}\u{FFFC}".into(),
                        "last paragraph".into(),
                    ],
                },
                roundtrip: RoundtripBounds {
                    document_xml_drift_bytes: 0,
                },
            },
        },
    ]
}

/// Issue #173 — one fixed-width (2 × 2000 twips) 1 × 2 table: `tbl_pr`
/// is the whole `<w:tblPr>…</w:tblPr>` (writer-canonical child order).
fn jc_tblind_table(tbl_pr: &str, a: &str, b: &str) -> String {
    format!(
        concat!(
            "<w:tbl>{tbl_pr}",
            r#"<w:tblGrid><w:gridCol w:w="2000"/><w:gridCol w:w="2000"/></w:tblGrid>"#,
            r#"<w:tr><w:tc><w:p><w:r><w:t xml:space="preserve">{a}</w:t></w:r></w:p></w:tc>"#,
            r#"<w:tc><w:p><w:r><w:t xml:space="preserve">{b}</w:t></w:r></w:p></w:tc></w:tr>"#,
            "</w:tbl>",
        ),
        tbl_pr = tbl_pr,
        a = a,
        b = b,
    )
}

/// Issue #173 — the three `<w:tblPr>`s of `table_jc_tblind.docx`, in
/// the writer's canonical shape (CT_TblPrBase order: tblW, jc, tblInd,
/// tblLayout) so a regenerated table is byte-identical to its source.
const JC_TBLIND_CENTER: &str = r#"<w:tblPr><w:tblW w:w="4000" w:type="dxa"/><w:jc w:val="center"/><w:tblLayout w:type="fixed"/></w:tblPr>"#;
const JC_TBLIND_END: &str = r#"<w:tblPr><w:tblW w:w="4000" w:type="dxa"/><w:jc w:val="end"/><w:tblLayout w:type="fixed"/></w:tblPr>"#;
const JC_TBLIND_INDENT: &str = r#"<w:tblPr><w:tblW w:w="4000" w:type="dxa"/><w:jc w:val="start"/><w:tblInd w:w="720" w:type="dxa"/><w:tblLayout w:type="fixed"/></w:tblPr>"#;

/// Issue #173 fixture: a centred, an end-aligned and a start-aligned +
/// 720-twip-indented fixed-width table, separated by paragraphs (Word
/// merges adjacent tables).
fn build_table_jc_tblind_docx() -> Vec<u8> {
    let p = |t: &str| format!(r#"<w:p><w:r><w:t xml:space="preserve">{t}</w:t></w:r></w:p>"#);
    let document_xml = format!(
        concat!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
            "\n",
            r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">"#,
            "<w:body>{intro}{t1}{mid1}{t2}{mid2}{t3}{after}{sect}</w:body></w:document>",
        ),
        intro = p("intro"),
        t1 = jc_tblind_table(JC_TBLIND_CENTER, "centre a", "centre b"),
        mid1 = p("mid one"),
        t2 = jc_tblind_table(JC_TBLIND_END, "end a", "end b"),
        mid2 = p("mid two"),
        t3 = jc_tblind_table(JC_TBLIND_INDENT, "indent a", "indent b"),
        after = p("after"),
        sect = BARE_SECT_PR,
    );
    package_document_xml(&document_xml)
}

/// Replicates `crates/format-docx/src/writer.rs`
/// `tests::build_style_cascade_docx`. Kept here so the gen-seed binary
/// doesn't depend on test-only symbols. BaseStyle (bold) → ChildStyle
/// (italic, basedOn BaseStyle); the single `<w:p>` references ChildStyle
/// and must round-trip with the cascade resolved to bold + italic.
fn build_style_cascade_docx() -> Vec<u8> {
    let styles_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
<w:style w:type="paragraph" w:styleId="BaseStyle"><w:name w:val="Base"/><w:rPr><w:b/></w:rPr></w:style>
<w:style w:type="paragraph" w:styleId="ChildStyle"><w:name w:val="Child"/><w:basedOn w:val="BaseStyle"/><w:rPr><w:i/></w:rPr></w:style>
</w:styles>"#;
    let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:pPr><w:pStyle w:val="ChildStyle"/></w:pPr><w:r><w:t xml:space="preserve">hello cascade</w:t></w:r></w:p>"#.to_owned() + A4_SECT_PR_EXPLICIT + "</w:body></w:document>";
    build_styled_docx(styles_xml, &document_xml)
}

/// Package a `word/styles.xml` + `word/document.xml` pair in the minimal
/// OPC skeleton (styles relationship included).
fn build_styled_docx(styles_xml: &str, document_xml: &str) -> Vec<u8> {
    use std::io::Write;
    use zip::write::{SimpleFileOptions, ZipWriter};

    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
<Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/>
</Types>"#;
    let dot_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/>
</Relationships>"#;

    let mut buf: Vec<u8> = Vec::new();
    {
        let mut zip = ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(0o644);
        for (name, body) in [
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", dot_rels),
            ("word/_rels/document.xml.rels", doc_rels),
            ("word/styles.xml", styles_xml),
            ("word/document.xml", document_xml),
        ] {
            zip.start_file(name, opts).unwrap();
            zip.write_all(body.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    buf
}

/// Phase 4 list fixture: a `word/numbering.xml` defining two abstractNums
/// (bullet at id 0, two-level decimal-then-lowerLetter at id 1) plus two
/// `<w:num>` instances. `document.xml` has 2 bullet paragraphs, then 2
/// numbered paragraphs at level 0 interleaved with 1 nested level-1
/// paragraph. The fixture exercises:
///
/// - bullet marker (literal `lvlText`, no `%N`),
/// - decimal `%1.` substitution,
/// - mixed-level `%1.%2.` substitution,
/// - deeper-level reset after a level-0 paragraph appears.
fn build_list_bullet_numbered_docx() -> Vec<u8> {
    use std::io::Write;
    use zip::write::{SimpleFileOptions, ZipWriter};

    let numbering_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
<w:abstractNum w:abstractNumId="0">
<w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="bullet"/><w:lvlText w:val="*"/></w:lvl>
</w:abstractNum>
<w:abstractNum w:abstractNumId="1">
<w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/></w:lvl>
<w:lvl w:ilvl="1"><w:start w:val="1"/><w:numFmt w:val="lowerLetter"/><w:lvlText w:val="%1.%2)"/></w:lvl>
</w:abstractNum>
<w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>
<w:num w:numId="2"><w:abstractNumId w:val="1"/></w:num>
</w:numbering>"#;
    let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t xml:space="preserve">bullet alpha</w:t></w:r></w:p><w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t xml:space="preserve">bullet beta</w:t></w:r></w:p><w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="2"/></w:numPr></w:pPr><w:r><w:t xml:space="preserve">first ordered item</w:t></w:r></w:p><w:p><w:pPr><w:numPr><w:ilvl w:val="1"/><w:numId w:val="2"/></w:numPr></w:pPr><w:r><w:t xml:space="preserve">first nested item</w:t></w:r></w:p><w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="2"/></w:numPr></w:pPr><w:r><w:t xml:space="preserve">second ordered item</w:t></w:r></w:p>"#.to_owned() + A4_SECT_PR_EXPLICIT + "</w:body></w:document>";
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
<Override PartName="/word/numbering.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml"/>
</Types>"#;
    let dot_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering" Target="numbering.xml"/>
</Relationships>"#;

    let mut buf: Vec<u8> = Vec::new();
    {
        let mut zip = ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(0o644);
        for (name, body) in [
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", dot_rels),
            ("word/_rels/document.xml.rels", doc_rels),
            ("word/numbering.xml", numbering_xml),
            ("word/document.xml", document_xml.as_str()),
        ] {
            zip.start_file(name, opts).unwrap();
            zip.write_all(body.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    buf
}

/// Phase 5 PR 1 table fixture: two paragraphs flanking a single 2×2
/// `<w:tbl>`. The table is parsed as an opaque `Block::Table` —
/// rows: vec![], source_xml: Some(raw) — and rides the Phase 3
/// passthrough on the writer side. Drift bound = 0.
fn build_table_2x2_opaque_docx() -> Vec<u8> {
    use std::io::Write;
    use zip::write::{SimpleFileOptions, ZipWriter};

    let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t xml:space="preserve">before</w:t></w:r></w:p><w:tbl><w:tblGrid><w:gridCol w:w="2880"/><w:gridCol w:w="2880"/></w:tblGrid><w:tr><w:tc><w:p><w:r><w:t xml:space="preserve">A1</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t xml:space="preserve">B1</w:t></w:r></w:p></w:tc></w:tr><w:tr><w:tc><w:p><w:r><w:t xml:space="preserve">A2</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t xml:space="preserve">B2</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p><w:r><w:t xml:space="preserve">after</w:t></w:r></w:p>"#.to_owned() + A4_SECT_PR_EXPLICIT + "</w:body></w:document>";
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#;
    let dot_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
</Relationships>"#;

    let mut buf: Vec<u8> = Vec::new();
    {
        let mut zip = ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(0o644);
        for (name, body) in [
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", dot_rels),
            ("word/_rels/document.xml.rels", doc_rels),
            ("word/document.xml", document_xml.as_str()),
        ] {
            zip.start_file(name, opts).unwrap();
            zip.write_all(body.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    buf
}

/// Build a minimal Phase 5 PR 2 table fixture. `inner_tbl_xml` is the
/// `<w:tbl>` element (without any wrapping) plus optional surrounding
/// content. `body_intro_text` is the leading paragraph; the whole body
/// becomes `<w:p>intro</w:p>` + inner_tbl_xml + the pinned A4
/// `<w:sectPr>` (issue #109 — see `A4_SECT_PR_EXPLICIT`). The table rides
/// the passthrough; a no-op resave's ONLY drift is the trailing sectPr
/// compacting back to `<w:sectPr/>` (`sect_pr_compaction_delta`).
fn build_table_fixture(body_intro_text: &str, inner_tbl_xml: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t xml:space="preserve">{body_intro_text}</w:t></w:r></w:p>{inner_tbl_xml}{A4_SECT_PR_EXPLICIT}</w:body></w:document>"#,
    );
    package_document_xml(&document_xml)
}

/// Wrap one complete `word/document.xml` part (declaration included) in
/// the minimal OPC skeleton the Phase 5+ handcrafted fixtures share.
fn package_document_xml(document_xml: &str) -> Vec<u8> {
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
</Relationships>"#;
    package_document_xml_with_rels(document_xml, doc_rels)
}

/// [`package_document_xml`] with a caller-supplied
/// `word/_rels/document.xml.rels` (hyperlink rows, issue #242).
fn package_document_xml_with_rels(document_xml: &str, doc_rels: &str) -> Vec<u8> {
    package_document_xml_with_parts(document_xml, doc_rels, &[])
}

/// [`package_document_xml_with_rels`] plus `extra` `(entry, body)` parts
/// (`word/comments.xml`, issue #243).
fn package_document_xml_with_parts(
    document_xml: &str,
    doc_rels: &str,
    extra: &[(&str, &str)],
) -> Vec<u8> {
    use std::io::Write;
    use zip::write::{SimpleFileOptions, ZipWriter};
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#;
    let dot_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#;
    let mut buf: Vec<u8> = Vec::new();
    {
        let mut zip = ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(0o644);
        for (name, body) in [
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", dot_rels),
            ("word/_rels/document.xml.rels", doc_rels),
            ("word/document.xml", document_xml),
        ]
        .into_iter()
        .chain(extra.iter().copied())
        {
            zip.start_file(name, opts).unwrap();
            zip.write_all(body.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    buf
}

fn build_table_grid_span_docx() -> Vec<u8> {
    let tbl = r#"<w:tbl><w:tblGrid><w:gridCol w:w="1440"/><w:gridCol w:w="1440"/><w:gridCol w:w="1440"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:gridSpan w:val="3"/></w:tcPr><w:p><w:r><w:t xml:space="preserve">spans all 3</w:t></w:r></w:p></w:tc></w:tr><w:tr><w:tc><w:p><w:r><w:t xml:space="preserve">a</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t xml:space="preserve">b</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t xml:space="preserve">c</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#;
    build_table_fixture("intro", tbl)
}

fn build_table_vmerge_docx() -> Vec<u8> {
    let tbl = r#"<w:tbl><w:tblGrid><w:gridCol w:w="1440"/><w:gridCol w:w="1440"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:vMerge w:val="restart"/></w:tcPr><w:p><w:r><w:t xml:space="preserve">spans down</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t xml:space="preserve">r1c2</w:t></w:r></w:p></w:tc></w:tr><w:tr><w:tc><w:tcPr><w:vMerge/></w:tcPr><w:p/></w:tc><w:tc><w:p><w:r><w:t xml:space="preserve">r2c2</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#;
    build_table_fixture("intro", tbl)
}

fn build_table_borders_double_docx() -> Vec<u8> {
    let tbl = r#"<w:tbl><w:tblPr><w:tblBorders><w:top w:val="double" w:sz="8" w:color="000000"/><w:left w:val="double" w:sz="8" w:color="000000"/><w:bottom w:val="double" w:sz="8" w:color="000000"/><w:right w:val="double" w:sz="8" w:color="000000"/></w:tblBorders></w:tblPr><w:tblGrid><w:gridCol w:w="2880"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:tcBorders><w:top w:val="double" w:sz="8" w:color="000000"/><w:bottom w:val="double" w:sz="8" w:color="000000"/></w:tcBorders></w:tcPr><w:p><w:r><w:t xml:space="preserve">bordered cell</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#;
    build_table_fixture("intro", tbl)
}

fn build_table_shaded_header_docx() -> Vec<u8> {
    let tbl = r#"<w:tbl><w:tblGrid><w:gridCol w:w="1440"/><w:gridCol w:w="1440"/></w:tblGrid><w:tr><w:trPr><w:tblHeader/></w:trPr><w:tc><w:tcPr><w:shd w:val="clear" w:color="auto" w:fill="FFEB78"/></w:tcPr><w:p><w:r><w:t xml:space="preserve">Header A</w:t></w:r></w:p></w:tc><w:tc><w:tcPr><w:shd w:val="clear" w:color="auto" w:fill="FFEB78"/></w:tcPr><w:p><w:r><w:t xml:space="preserve">Header B</w:t></w:r></w:p></w:tc></w:tr><w:tr><w:tc><w:p><w:r><w:t xml:space="preserve">a1</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t xml:space="preserve">b1</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#;
    build_table_fixture("intro", tbl)
}

fn build_table_in_rtl_doc_docx() -> Vec<u8> {
    let tbl = r#"<w:tbl><w:tblPr><w:bidiVisual/></w:tblPr><w:tblGrid><w:gridCol w:w="1440"/><w:gridCol w:w="1440"/></w:tblGrid><w:tr><w:tc><w:p><w:pPr><w:bidi/></w:pPr><w:r><w:t xml:space="preserve">يمين</w:t></w:r></w:p></w:tc><w:tc><w:p><w:pPr><w:bidi/></w:pPr><w:r><w:t xml:space="preserve">يسار</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#;
    build_table_fixture("مقدمة", tbl)
}

/// Issue #110 — the part starts with U+FEFF (the UTF-8 BOM, bytes
/// `EF BB BF`) exactly like docx4j / Apache POI emit it. Two adjacent
/// paragraphs + a trailing body `<w:sectPr/>` cover both splice
/// adjacencies the corruption showed up in.
fn build_bom_utf8_passthrough_docx() -> Vec<u8> {
    let document_xml = format!(
        concat!(
            "\u{FEFF}",
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
            "\n",
            r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">"#,
            r#"<w:body><w:p><w:r><w:t xml:space="preserve">first</w:t></w:r></w:p>"#,
            r#"<w:p><w:r><w:t xml:space="preserve">second</w:t></w:r></w:p>"#,
            "{sect_pr}</w:body></w:document>",
        ),
        sect_pr = A4_SECT_PR_EXPLICIT,
    );
    package_document_xml(&document_xml)
}

/// Issue #111 — `depth` tables nested one inside the other (one row, one
/// cell, one paragraph, one nested table per level).
fn build_table_nested_deep_docx(depth: usize) -> Vec<u8> {
    let mut tbl = String::new();
    for level in 0..depth {
        tbl.push_str(r#"<w:tbl><w:tblGrid><w:gridCol w:w="2400"/></w:tblGrid><w:tr><w:tc>"#);
        tbl.push_str(&format!(
            r#"<w:p><w:r><w:t xml:space="preserve">level {level}</w:t></w:r></w:p>"#
        ));
    }
    for _ in 0..depth {
        tbl.push_str("</w:tc></w:tr></w:tbl>");
    }
    build_table_fixture("intro", &tbl)
}

/* ============================================================= helpers ==== */

/* ================================ body passthrough (#120 / #112 / #119) ==== */

/// A Word-shaped root: `xmlns:w` is NOT first, foreign prefixes and
/// `mc:Ignorable` ride along, exactly as Word writes it.
const WORD_ROOT: &str = concat!(
    r#"<w:document xmlns:wpc="http://schemas.microsoft.com/office/word/2010/wordprocessingCanvas" "#,
    r#"xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006" "#,
    r#"xmlns:o="urn:schemas-microsoft-com:office:office" "#,
    r#"xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" "#,
    r#"xmlns:v="urn:schemas-microsoft-com:vml" "#,
    r#"xmlns:wp14="http://schemas.microsoft.com/office/word/2010/wordprocessingDrawing" "#,
    r#"xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" "#,
    r#"xmlns:w10="urn:schemas-microsoft-com:office:word" "#,
    r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" "#,
    r#"xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml" "#,
    r#"xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape" "#,
    r#"xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" "#,
    r#"xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture" "#,
    r#"mc:Ignorable="w14 wp14">"#,
);

/// Word's trailing sectPr: rsids, `w:gutter`, `<w:cols w:space>` and
/// `<w:docGrid>` — none of which the typed model carries.
const WORD_SECT_PR: &str = concat!(
    r#"<w:sectPr w:rsidR="00B44B3E" w:rsidSect="00E64C2A">"#,
    r#"<w:pgSz w:w="11906" w:h="16838"/>"#,
    r#"<w:pgMar w:top="1417" w:right="1417" w:bottom="1134" w:left="1417" w:header="708" w:footer="708" w:gutter="0"/>"#,
    r#"<w:cols w:space="708"/><w:docGrid w:linePitch="360"/></w:sectPr>"#,
);

/// `word/document.xml` the way Word writes it around `body`: CRLF after
/// the declaration and around the root's children.
fn word_document_xml(body: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n{WORD_ROOT}\r\n<w:body>{body}{WORD_SECT_PR}</w:body>\r\n</w:document>\r\n"
    )
}

/// Every #120 / #112 construct at once (see the fixture's manifest note).
const BODY_LEVEL_CONSTRUCTS: &str = concat!(
    r#"<w:p w:rsidR="00A1" w14:paraId="1F2E3D4C"><w:r><w:t>intro</w:t></w:r></w:p>"#,
    "\r\n  ",
    r#"<w:bookmarkStart w:id="0" w:name="_GoBack"/>"#,
    r#"<w:sdt><w:sdtPr><w:alias w:val="Block"/><w:id w:val="-2035718510"/>"#,
    r#"<w:rPr><w:b/></w:rPr><w:text w:multiLine="1"/></w:sdtPr><w:sdtEndPr><w:rPr><w:i/></w:rPr></w:sdtEndPr><w:sdtContent>"#,
    r#"<w:p><w:r><w:t xml:space="preserve">first inside</w:t></w:r></w:p>"#,
    r#"<w:sdt><w:sdtPr/><w:sdtContent/></w:sdt>"#,
    r#"<w:p><w:r><w:t xml:space="preserve">second inside</w:t></w:r></w:p>"#,
    r#"<w:sdt><w:sdtPr><w:tag w:val="nested"/></w:sdtPr><w:sdtContent>"#,
    r#"<w:p><w:r><w:t xml:space="preserve">nested inside</w:t></w:r></w:p>"#,
    r#"</w:sdtContent></w:sdt>"#,
    r#"</w:sdtContent></w:sdt>"#,
    r#"<w:bookmarkEnd w:id="0"/>"#,
    r#"<w:p w:rsidR="009B100C" w:rsidRDefault="009B100C" w:rsidP="00A54197"/>"#,
    r#"<w:proofErr w:type="spellStart"/>"#,
    r#"<w:sdt><w:sdtPr><w:tag w:val="table"/></w:sdtPr><w:sdtContent>"#,
    r#"<w:tbl><w:tblGrid><w:gridCol w:w="2400"/></w:tblGrid><w:tr><w:tc><w:p/></w:tc></w:tr></w:tbl>"#,
    r#"</w:sdtContent></w:sdt>"#,
    r#"<w:p><w:r><w:t>after</w:t></w:r></w:p>"#,
    r#"<w:commentRangeEnd w:id="3"/>"#,
    "\r\n  ",
);

fn build_body_level_passthrough_docx() -> Vec<u8> {
    package_document_xml(&word_document_xml(BODY_LEVEL_CONSTRUCTS))
}

/// The text box (DrawingML choice + VML fallback), the VML rule and the
/// OLE object of `drawing_objects_preserved.docx`, plus its picture.
const TEXT_BOX_OBJECT: &str = concat!(
    r#"<mc:AlternateContent><mc:Choice Requires="wps"><w:drawing>"#,
    r#"<wp:inline distT="0" distB="0" distL="0" distR="0"><wp:extent cx="1828800" cy="914400"/>"#,
    r#"<wp:effectExtent l="0" t="0" r="0" b="0"/><wp:docPr id="1" name="Text Box 1"/>"#,
    r#"<a:graphic><a:graphicData uri="http://schemas.microsoft.com/office/word/2010/wordprocessingShape">"#,
    r#"<wps:wsp><wps:cNvSpPr txBox="1"/><wps:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="1828800" cy="914400"/></a:xfrm>"#,
    r#"<a:prstGeom prst="rect"><a:avLst/></a:prstGeom></wps:spPr>"#,
    r#"<wps:txbx><w:txbxContent><w:p><w:r><w:t>in the box</w:t></w:r></w:p></w:txbxContent></wps:txbx>"#,
    r#"<wps:bodyPr rot="0"/></wps:wsp></a:graphicData></a:graphic></wp:inline></w:drawing></mc:Choice>"#,
    r##"<mc:Fallback><w:pict><v:shape id="Text Box 1" o:spid="_x0000_s1026" type="#_x0000_t202" style="width:144pt;height:1in">"##,
    r#"<v:textbox><w:txbxContent><w:p><w:r><w:t>in the box</w:t></w:r></w:p></w:txbxContent></v:textbox></v:shape></w:pict></mc:Fallback>"#,
    r#"</mc:AlternateContent>"#,
);
const VML_RULE_OBJECT: &str = r##"<w:pict><v:rect id="_x0000_i1025" style="width:0;height:1.5pt" o:hralign="center" o:hrstd="t" o:hr="t" fillcolor="#a0a0a0" stroked="f"/></w:pict>"##;
const OLE_OBJECT: &str = r##"<w:object w:dxaOrig="1440" w:dyaOrig="720"><v:shape id="_x0000_i1027" type="#_x0000_t75" style="width:72pt;height:36pt" o:ole=""><v:imagedata r:id="rId9" o:title=""/></v:shape><o:OLEObject Type="Embed" ProgID="Package" ShapeID="_x0000_i1027" DrawAspect="Content" ObjectID="_1234" r:id="rId10"/></w:object>"##;
const PICTURE_OBJECT: &str = concat!(
    r#"<w:drawing><wp:inline distT="0" distB="0" distL="0" distR="0" wp14:anchorId="1A2B3C4D">"#,
    r#"<wp:extent cx="914400" cy="457200"/><wp:effectExtent l="0" t="0" r="0" b="0"/>"#,
    r#"<wp:docPr id="3" name="Picture 3" descr="alt text that must survive"/>"#,
    r#"<wp:cNvGraphicFramePr><a:graphicFrameLocks noChangeAspect="1"/></wp:cNvGraphicFramePr>"#,
    r#"<a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:pic>"#,
    r#"<pic:nvPicPr><pic:cNvPr id="3" name="photo.png"/><pic:cNvPicPr/></pic:nvPicPr>"#,
    r#"<pic:blipFill><a:blip r:embed="rId5"><a:extLst><a:ext uri="{28A0092B-C50C-407E-A947-70E740481C1C}"/></a:extLst></a:blip>"#,
    r#"<a:stretch><a:fillRect/></a:stretch></pic:blipFill>"#,
    r#"<pic:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="914400" cy="457200"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></pic:spPr>"#,
    r#"</pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing>"#,
);

fn drawing_objects_body() -> String {
    format!(
        concat!(
            r#"<w:p><w:r><w:t xml:space="preserve">a </w:t></w:r><w:r>{tb}</w:r>"#,
            r#"<w:r><w:rPr><w:noProof/></w:rPr>{rule}</w:r><w:r>{ole}</w:r>"#,
            r#"<w:r><w:t xml:space="preserve"> z</w:t></w:r></w:p>"#,
            r#"<w:p><w:r><w:t xml:space="preserve">pic </w:t></w:r><w:r><w:rPr><w:noProof/></w:rPr>{pic}</w:r>"#,
            r#"<w:r><w:t xml:space="preserve"> end</w:t></w:r></w:p>"#,
        ),
        tb = TEXT_BOX_OBJECT,
        rule = VML_RULE_OBJECT,
        ole = OLE_OBJECT,
        pic = PICTURE_OBJECT
    )
}

fn build_drawing_objects_preserved_docx() -> Vec<u8> {
    package_document_xml(&word_document_xml(&drawing_objects_body()))
}

/// Issues #120 / #112 / #119 — step 19: the body-level passthrough
/// contract. (a) both fixtures resave byte-identical with zero edits, on
/// the archive AND the UI save path; (b) an edit inside a content control
/// regenerates that paragraph only — envelope, markers, prolog, root tag
/// and sectPr are the source bytes; (c) an edit in a paragraph holding a
/// text box, a VML rule, an OLE object and a picture re-emits every object
/// byte for byte; (d) a resized picture regenerates from the typed fields.
fn run_body_passthrough_roundtrip() -> Result<()> {
    use engine::{BlockPath, LogicalPos};

    let body_xml = word_document_xml(BODY_LEVEL_CONSTRUCTS);
    let body_docx = build_body_level_passthrough_docx();
    let drawing_xml = word_document_xml(&drawing_objects_body());
    let drawing_docx = build_drawing_objects_preserved_docx();

    /* (a) zero-edit, both save paths. */
    for (label, docx, xml) in [
        ("body_level_passthrough", &body_docx, &body_xml),
        ("drawing_objects_preserved", &drawing_docx, &drawing_xml),
    ] {
        let parsed = read_docx(docx).with_context(|| format!("read {label}"))?;
        if !parsed.document.document_envelope.is_captured() {
            bail!("{label}: the document envelope was not captured");
        }
        let resaved = write_docx(&parsed, &parsed.document).context("write_docx")?;
        assert_document_xml_well_formed(&resaved)?;
        if extract_doc_xml(&resaved)? != xml.as_bytes() {
            bail!("{label}: zero-edit archive resave is not byte-identical");
        }
        let ui = build_minimal_docx(&parsed.document).context("build_minimal_docx")?;
        assert_document_xml_well_formed(&ui)?;
        if extract_doc_xml(&ui)? != xml.as_bytes() {
            bail!("{label}: zero-edit UI-path resave is not byte-identical");
        }
    }
    println!(
        "[roundtrip] step 19a OK — body-level markup, envelope and objects resave byte-identical on both save paths"
    );

    /* (b) edit inside the content control. */
    let parsed = read_docx(&body_docx).context("read body fixture")?;
    let edited = parsed.document.insert_text(
        LogicalPos {
            path: BlockPath::top(1),
            offset: "first".len() as u32,
        },
        "+X",
    );
    let bytes = write_docx(&parsed, &edited).context("write edited body")?;
    assert_document_xml_well_formed(&bytes)?;
    let out = String::from_utf8(extract_doc_xml(&bytes)?).context("utf8")?;
    let expected = body_xml.replacen("first inside", "first+X inside", 1);
    if out != expected {
        bail!(
            "edit inside a content control must change only its paragraph\n--- expected ---\n{expected}\n--- got ---\n{out}"
        );
    }
    let back = read_docx(&bytes).context("re-read edited body")?;
    if back.document.paragraph_text(1) != Some("first+X inside") {
        bail!("edited paragraph did not persist inside the control");
    }
    println!(
        "[roundtrip] step 19b OK — an edit inside an <w:sdt> keeps its envelope, markers and sectPr byte-for-byte"
    );

    /* (c) edit a paragraph holding preserved objects. */
    let parsed = read_docx(&drawing_docx).context("read drawing fixture")?;
    let p0 = parsed
        .document
        .nth_paragraph(0)
        .context("first paragraph")?;
    if p0.inline_objects.len() != 3 {
        bail!(
            "expected 3 objects in paragraph 0, got {}",
            p0.inline_objects.len()
        );
    }
    if !matches!(
        p0.inline_objects[0].kind,
        engine::InlineKind::TextBox { .. }
    ) {
        bail!("the AlternateContent text box must read as a text-box story");
    }
    let edited = parsed
        .document
        .insert_text(
            LogicalPos {
                path: BlockPath::top(0),
                offset: 1,
            },
            "bc",
        )
        .insert_text(
            LogicalPos {
                path: BlockPath::top(1),
                offset: 0,
            },
            "A ",
        );
    let bytes = write_docx(&parsed, &edited).context("write edited drawings")?;
    assert_document_xml_well_formed(&bytes)?;
    let out = String::from_utf8(extract_doc_xml(&bytes)?).context("utf8")?;
    let expected =
        drawing_xml
            .replacen("a </w:t>", "abc </w:t>", 1)
            .replacen("pic </w:t>", "A pic </w:t>", 1);
    if out != expected {
        bail!(
            "editing around preserved objects must re-emit them byte-for-byte\n--- expected ---\n{expected}\n--- got ---\n{out}"
        );
    }
    let back = read_docx(&bytes).context("re-read edited drawings")?;
    if back
        .document
        .nth_paragraph(0)
        .map(|p| p.inline_objects.len())
        != Some(3)
    {
        bail!("objects lost on re-read");
    }
    println!(
        "[roundtrip] step 19c OK — text box, VML rule, OLE object and picture survive an edit in their paragraph byte-for-byte"
    );

    /* (d) a resized picture regenerates. */
    let mut resized = parsed.document.clone();
    let mut p1 = resized
        .nth_paragraph(1)
        .context("second paragraph")?
        .clone();
    if let engine::InlineKind::Image { width_emu, .. } = &mut p1.inline_objects[0].kind {
        *width_emu = 1_828_800;
    } else {
        bail!("paragraph 1 must hold the picture");
    }
    p1.dirty = true;
    p1.source_xml = None;
    resized.blocks.set(1, engine::Block::Paragraph(p1));
    let bytes = write_docx(&parsed, &resized).context("write resized")?;
    assert_document_xml_well_formed(&bytes)?;
    let out = String::from_utf8(extract_doc_xml(&bytes)?).context("utf8")?;
    if !out.contains(r#"<wp:extent cx="1828800" cy="457200"/>"#) {
        bail!("resized picture must regenerate with the new extent:\n{out}");
    }
    if !out.contains(TEXT_BOX_OBJECT) || !out.contains(VML_RULE_OBJECT) || !out.contains(OLE_OBJECT)
    {
        bail!("the untouched objects must still be verbatim after a picture resize");
    }
    println!(
        "[roundtrip] step 19d OK — a resized picture regenerates from the typed fields, everything else stays verbatim"
    );
    Ok(())
}

/// Issue #110 — every `write_docx` in this harness is followed by a strict
/// re-parse of the saved `word/document.xml`. A misaligned passthrough
/// splice (`</w<w:sectPr/>`) is unparseable XML; it must fail here, loudly,
/// before any byte-drift arithmetic gets a chance to call it "3 bytes".
fn assert_document_xml_well_formed(docx: &[u8]) -> Result<()> {
    format_docx::check_document_xml_well_formed(docx)
        .context("saved word/document.xml is not well-formed XML (issue #110 guard)")
}

fn extract_doc_xml(bytes: &[u8]) -> Result<Vec<u8>> {
    use std::io::Read;
    let mut a = zip::ZipArchive::new(std::io::Cursor::new(bytes))?;
    let mut f = a.by_name("word/document.xml")?;
    let mut out = Vec::new();
    f.read_to_end(&mut out)?;
    Ok(out)
}

/* ============================================================ issue #251 ==== */

/// Issue #251 — per-new-`<w:r>` size allowance for the secondary
/// (informational-turned-advisory) size bound. Deliberately duplicated
/// from `tools/corpus-native/src/pipeline.rs::NEW_RUN_ALLOWANCE_BYTES`
/// rather than shared through a library crate — these are two independent
/// CLI binaries and this is a ~5-line pure function, not worth a new
/// workspace member. See that module's doc comment for how the 48 B figure
/// was picked (the exact 43 B markup cost of an empty
/// `<w:r><w:t xml:space="preserve"></w:t></w:r>` wrapper, rounded up).
const NEW_RUN_ALLOWANCE_BYTES: usize = 48;

/// Issue #251 — count `<w:r>` / `<w:r ...>` / `<w:r/>` run-element open
/// tags in a `document.xml` byte slice. A cheap heuristic (a literal-byte
/// scan, not a real XML walk): see the corpus-native twin of this function
/// for the full rationale.
fn count_run_open_tags(xml: &[u8]) -> usize {
    xml.windows(4)
        .enumerate()
        .filter(|(i, w)| {
            *w == *b"<w:r" && matches!(xml.get(i + 4), Some(b' ') | Some(b'>') | Some(b'/'))
        })
        .count()
}

/// Issue #251 (originally #199) — `(prefix_len, original_span,
/// edited_span)`: the byte offset the ORIGINAL and edited parts start to
/// differ at, and the lengths of the region between the longest common
/// prefix and the longest common suffix of the two parts. 0 for
/// `original_span` means the edited save is a pure insertion — nothing of
/// the source was lost or respelled. Deliberately duplicated from
/// `tools/corpus-native/src/pipeline.rs::rewritten_region` — see the note
/// on [`NEW_RUN_ALLOWANCE_BYTES`].
fn rewritten_region(orig: &[u8], edited: &[u8]) -> (usize, u64, u64) {
    let prefix = orig.iter().zip(edited).take_while(|(a, b)| a == b).count();
    let max_suffix = orig.len().min(edited.len()) - prefix;
    let suffix = orig
        .iter()
        .rev()
        .zip(edited.iter().rev())
        .take(max_suffix)
        .take_while(|(a, b)| a == b)
        .count();
    (
        prefix,
        (orig.len() - prefix - suffix) as u64,
        (edited.len() - prefix - suffix) as u64,
    )
}

/* ================================================================= main ==== */

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("--fixtures") => {
            let dir = args
                .get(1)
                .map(String::as_str)
                .unwrap_or(DEFAULT_FIXTURES_DIR);
            run_fixtures(Path::new(dir))
        }
        Some("--gen-seed") => {
            let dir = args
                .get(1)
                .map(String::as_str)
                .unwrap_or(DEFAULT_FIXTURES_DIR);
            run_gen_seed(Path::new(dir))
        }
        Some(other) => Err(anyhow::anyhow!(
            "unknown mode `{other}` (expected --fixtures or --gen-seed, or no args for default)"
        )),
        None => run_default(),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("FAIL: {e:#}");
            ExitCode::FAILURE
        }
    }
}
