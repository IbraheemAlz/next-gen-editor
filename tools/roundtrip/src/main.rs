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

    println!("\nPASS");
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

    /* 2. Re-emit and re-parse. */
    let edited_bytes = write_docx(&archive_a, &archive_a.document).context("write_docx")?;
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
            dirty: false,
            source_xml: None,
            inline_objects: Vec::new(),
            hyperlinks: Vec::new(),
            revisions: Vec::new(),
            fields: Vec::new(),
            style_id: None,
            direct_overrides: ParaProperties::default(),
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
    use std::io::Write;
    use zip::write::{SimpleFileOptions, ZipWriter};
    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t xml:space="preserve">{body_intro_text}</w:t></w:r></w:p>{inner_tbl_xml}<w:sectPr/></w:body></w:document>"#,
    );
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

/* ============================================================= helpers ==== */

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
