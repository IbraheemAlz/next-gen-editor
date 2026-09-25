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
    let bound = insert_len_utf8 * 2;
    if doc_diff > bound {
        bail!(
            "document.xml diff {doc_diff} B exceeds bound {bound} B (insert {insert_len_utf8} B × 2)"
        );
    }
    println!("[roundtrip] step 6b OK — document.xml diff within bound");

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
            "<w:sectPr/></w:body></w:document>",
        ),
        root = WORD_ROOT_OPEN,
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
            InlineKind::Image { rel_id, width_emu: 914400, height_emu: 457200 } if rel_id == "rId5"
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
    if c1b.inline_objects.len() != 1 || !archive_b.document.media.contains_key("rId5") {
        bail!("cell picture lost on save: {:?}", c1b.inline_objects);
    }
    println!(
        "[roundtrip] step 13 OK — table cell runs, grab bags and pictures survive a cell edit (Δ {drift} B)"
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
                roundtrip: RoundtripBounds::default(),
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
pPr reader + writer end-to-end. Phase 3 / 5 swap in true Word fixtures. */
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
        drift 0 on a zero-edit resave, exact regeneration in step 14. */
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
                roundtrip: RoundtripBounds::default(),
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
                roundtrip: RoundtripBounds::default(),
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
                bytes still ride the passthrough writer so drift = 0. */
                asserts: FixtureAsserts {
                    paragraph_count: 2,
                    paragraph_texts: vec!["before".into(), "after".into()],
                },
                roundtrip: RoundtripBounds::default(),
            },
        },
        /* Phase 5 PR 2 — full row/cell/tcPr feature coverage. Every
        fixture round-trips via Phase 3 passthrough (drift = 0): the
        captured `<w:tbl>` source bytes are emitted verbatim. */
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
                roundtrip: RoundtripBounds::default(),
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
                roundtrip: RoundtripBounds::default(),
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
                roundtrip: RoundtripBounds::default(),
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
                roundtrip: RoundtripBounds::default(),
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
                roundtrip: RoundtripBounds::default(),
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
        three bytes early and resave `</w<w:sectPr/>`. Drift bound 3: the
        writer synthesizes its own declaration and never re-emits the BOM;
        every paragraph must otherwise splice byte-exact. */
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
                roundtrip: RoundtripBounds {
                    document_xml_drift_bytes: 3,
                },
            },
        },
        /* Issue #111 — 200 nested tables (Apache POI's `deep-table-cell.docx`
        goes to 5000). The reader must open it on a bounded stack in well
        under a second and the outer table rides the passthrough at drift 0
        whatever depth the typed model stops at. */
        /* Issue #100 — Word-shaped `w14:paraId` on every paragraph, bound
        only on the root. Passthrough at drift 0; step 12 saves it through
        the UI path. */
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
                roundtrip: RoundtripBounds::default(),
            },
        },
        /* Issue #81 — Word's multi-paragraph TOC shape (hyperlinked
        entries, nested PAGEREFs, dot leaders, `_Toc*` bookmarks).
        Passthrough at drift 0; step 15 edits + regenerates it. */
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
        cells. Passthrough at drift 0; step 13 edits both cells. */
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
                roundtrip: RoundtripBounds::default(),
            },
        },
    ]
}

/// Replicates `crates/format-docx/src/writer.rs`
/// `tests::build_style_cascade_docx`. Kept here so the gen-seed binary
/// doesn't depend on test-only symbols. BaseStyle (bold) → ChildStyle
/// (italic, basedOn BaseStyle); the single `<w:p>` references ChildStyle
/// and must round-trip with the cascade resolved to bold + italic.
fn build_style_cascade_docx() -> Vec<u8> {
    use std::io::Write;
    use zip::write::{SimpleFileOptions, ZipWriter};

    let styles_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
<w:style w:type="paragraph" w:styleId="BaseStyle"><w:name w:val="Base"/><w:rPr><w:b/></w:rPr></w:style>
<w:style w:type="paragraph" w:styleId="ChildStyle"><w:name w:val="Child"/><w:basedOn w:val="BaseStyle"/><w:rPr><w:i/></w:rPr></w:style>
</w:styles>"#;
    let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:pPr><w:pStyle w:val="ChildStyle"/></w:pPr><w:r><w:t xml:space="preserve">hello cascade</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#;
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
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t xml:space="preserve">bullet alpha</w:t></w:r></w:p><w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t xml:space="preserve">bullet beta</w:t></w:r></w:p><w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="2"/></w:numPr></w:pPr><w:r><w:t xml:space="preserve">first ordered item</w:t></w:r></w:p><w:p><w:pPr><w:numPr><w:ilvl w:val="1"/><w:numId w:val="2"/></w:numPr></w:pPr><w:r><w:t xml:space="preserve">first nested item</w:t></w:r></w:p><w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="2"/></w:numPr></w:pPr><w:r><w:t xml:space="preserve">second ordered item</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#;
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
            ("word/document.xml", document_xml),
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
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t xml:space="preserve">before</w:t></w:r></w:p><w:tbl><w:tblGrid><w:gridCol w:w="2880"/><w:gridCol w:w="2880"/></w:tblGrid><w:tr><w:tc><w:p><w:r><w:t xml:space="preserve">A1</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t xml:space="preserve">B1</w:t></w:r></w:p></w:tc></w:tr><w:tr><w:tc><w:p><w:r><w:t xml:space="preserve">A2</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t xml:space="preserve">B2</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p><w:r><w:t xml:space="preserve">after</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#;
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
            ("word/document.xml", document_xml),
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
/// becomes `<w:p>intro</w:p>` + inner_tbl_xml + `<w:sectPr/>`. Drift
/// bound = 0 — every fixture rides the passthrough.
fn build_table_fixture(body_intro_text: &str, inner_tbl_xml: &str) -> Vec<u8> {
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t xml:space="preserve">{body_intro_text}</w:t></w:r></w:p>{inner_tbl_xml}<w:sectPr/></w:body></w:document>"#,
    );
    package_document_xml(&document_xml)
}

/// Wrap one complete `word/document.xml` part (declaration included) in
/// the minimal OPC skeleton the Phase 5+ handcrafted fixtures share.
fn package_document_xml(document_xml: &str) -> Vec<u8> {
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
            ("word/document.xml", document_xml),
        ] {
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
    let document_xml = concat!(
        "\u{FEFF}",
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
        "\n",
        r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">"#,
        r#"<w:body><w:p><w:r><w:t xml:space="preserve">first</w:t></w:r></w:p>"#,
        r#"<w:p><w:r><w:t xml:space="preserve">second</w:t></w:r></w:p>"#,
        r#"<w:sectPr/></w:body></w:document>"#,
    );
    package_document_xml(document_xml)
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
