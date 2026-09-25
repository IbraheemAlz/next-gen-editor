//! Per-document pipeline (issue #88 Scope §2): `read_docx` -> full layout
//! (`crates/layout`, via [`crate::nativelayout`]) -> PDF export of every
//! page (`crates/format-pdf`, the native "render" — Canvas2D is
//! browser-only) -> `write_docx` -> `read_docx` again, asserting no panic,
//! sibling byte-identity, `document.xml` stability with no edits, plain-text
//! equality, and stable page count. Optionally (`--with-edit`, on by
//! default) applies one scripted edit and re-checks the round-trip
//! harness's `document.xml` delta bound (`.claude/rules/docx.md`: delta ≤
//! 2 × inserted UTF-8 bytes).
//!
//! Every stage runs through [`crate::panics::catch`] so a panic on one
//! document degrades to a single JSONL record instead of aborting the
//! batch — the entire point of the harness is to survive the crashes it is
//! looking for.

use crate::drift;
use crate::nativelayout;
use crate::panics::{self, CaughtPanic};
use format_docx::DocxArchive;
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::time::Instant;
use text_pipeline::FontStack;

/// Text inserted by the optional scripted-edit check. Plain ASCII so its
/// UTF-8 byte length is trivially its `.len()`.
const EDIT_MARKER: &str = " [corpus-native-probe]";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Ok,
    Error,
    Panic,
    /// The per-document wall-clock budget (`main.rs`'s `--timeout-secs`)
    /// elapsed before the pipeline returned. `catch_unwind` only catches
    /// panics — an infinite loop or decompression-bomb-style blowup hangs
    /// forever otherwise, and some real-world `.docx` corpora (this one's
    /// sources included) deliberately contain regression fixtures for
    /// exactly that class of bug. `main.rs` fills this variant in, not
    /// `run_one` — it never sees its own timeout.
    Timeout,
    /// The document's worker *process* died outright (killed by a signal —
    /// SIGSEGV, or Rust's own stack-overflow guard-page abort — or an
    /// unparseable/empty stdout). `catch_unwind` cannot catch a stack
    /// overflow: it is not a panic, it is the process's own abort. `main.rs`
    /// runs every document in a subprocess (`--worker`) specifically to
    /// contain this class of crash; it fills this variant in when the
    /// subprocess doesn't come back with a normal JSON record.
    Crash,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct EditCheck {
    pub inserted_bytes: usize,
    pub document_xml_delta_bytes: u64,
    pub bound_bytes: u64,
    pub within_bound: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DocResult {
    pub path: String,
    pub size_bytes: u64,
    pub outcome: Outcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stage: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub panic_signature: Option<String>,
    pub elapsed_ms: u128,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layout_ms: Option<u128>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paragraph_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_count_before: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_count_after: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_count_stable: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plain_text_equal: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sibling_bytes_identical: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sibling_drift_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document_xml_unchanged: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document_xml_delta_noedit_bytes: Option<u64>,
    /// Issue #112 — `document.xml` reproduced byte for byte by the
    /// zero-edit resave (the size delta above can be 0 while the bytes
    /// still differ).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document_xml_byte_identical: Option<bool>,
    /// Issue #112 — raw offset of the first differing byte (see
    /// [`crate::drift`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_drift_offset: Option<usize>,
    /// Issue #112 — innermost element of the ORIGINAL part the first
    /// differing byte falls in (`w:sectPr`, `#text`, `<prolog>`, …).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_drift_element: Option<String>,
    /// Issue #112 — `parent/element` bucket key for the drift histogram
    /// (`w:body/w:sdt`, `/w:document`, `<prolog>`, …).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_drift_context: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pdf_bytes_len: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pdf_pages: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edit_check: Option<EditCheck>,
}

impl DocResult {
    fn new(path: &str, size_bytes: u64) -> Self {
        Self {
            path: path.to_string(),
            size_bytes,
            outcome: Outcome::Ok,
            stage: None,
            message: None,
            panic_signature: None,
            elapsed_ms: 0,
            layout_ms: None,
            paragraph_count: None,
            page_count_before: None,
            page_count_after: None,
            page_count_stable: None,
            plain_text_equal: None,
            sibling_bytes_identical: None,
            sibling_drift_bytes: None,
            document_xml_unchanged: None,
            document_xml_delta_noedit_bytes: None,
            document_xml_byte_identical: None,
            first_drift_offset: None,
            first_drift_element: None,
            first_drift_context: None,
            pdf_bytes_len: None,
            pdf_pages: None,
            edit_check: None,
        }
    }

    /// Built by `main.rs` when a document's worker subprocess doesn't exit
    /// within the wall-clock budget — see [`Outcome::Timeout`].
    pub fn timed_out(path: &str, size_bytes: u64, timeout_secs: u64) -> Self {
        let mut rec = Self::new(path, size_bytes);
        rec.outcome = Outcome::Timeout;
        rec.message = Some(format!("exceeded {timeout_secs}s wall-clock budget"));
        rec.elapsed_ms = (timeout_secs as u128) * 1000;
        rec
    }

    /// Built by `main.rs` when a document's worker subprocess died without
    /// producing a valid JSON record — see [`Outcome::Crash`].
    pub fn crashed(path: &str, size_bytes: u64, message: &str) -> Self {
        let mut rec = Self::new(path, size_bytes);
        rec.outcome = Outcome::Crash;
        rec.message = Some(truncate(message, MAX_MESSAGE_LEN));
        rec
    }

    fn mark_panic(&mut self, stage: &str, p: &CaughtPanic) {
        self.outcome = Outcome::Panic;
        self.stage = Some(stage.to_string());
        self.message = Some(truncate(&p.message, MAX_MESSAGE_LEN));
        self.panic_signature = Some(panics::normalize_signature(stage, &p.location, &p.message));
    }

    fn mark_error(&mut self, stage: &str, message: &str) {
        self.outcome = Outcome::Error;
        self.stage = Some(stage.to_string());
        self.message = Some(truncate(message, MAX_MESSAGE_LEN));
    }
}

/// Cap a message before it goes in the JSONL — a handful of pathological
/// inputs could otherwise produce a multi-megabyte panic/error string
/// (e.g. a `Debug`-formatted giant XML fragment) and blow up both the
/// per-line JSON size and, worse, the pipe buffer `main.rs`'s subprocess
/// wrapper reads a worker's stdout through.
const MAX_MESSAGE_LEN: usize = 4000;

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...<truncated>", &s[..end])
}

fn extract_doc_xml(bytes: &[u8]) -> anyhow::Result<Vec<u8>> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))?;
    let mut file = archive.by_name("word/document.xml")?;
    let mut out = Vec::new();
    file.read_to_end(&mut out)?;
    Ok(out)
}

/// Sibling-entry byte-identity (`.claude/rules/docx.md` invariant #1):
/// every non-`word/document.xml` archive entry must survive a save
/// verbatim. Returns `(identical, drift_bytes)` — drift is the summed size
/// delta of every entry that changed, plus the full size of any entry that
/// went missing.
fn compare_siblings(a: &DocxArchive, b: &DocxArchive) -> (bool, u64) {
    let mut identical = true;
    let mut drift: u64 = 0;
    for (name, bytes_a) in &a.other_entries {
        match b.other_entries.iter().find(|(n, _)| n == name) {
            Some((_, bytes_b)) if bytes_b == bytes_a => {}
            Some((_, bytes_b)) => {
                identical = false;
                drift += bytes_a.len().abs_diff(bytes_b.len()) as u64;
            }
            None => {
                identical = false;
                drift += bytes_a.len() as u64;
            }
        }
    }
    (identical, drift)
}

/// Run the full pipeline on one document's raw bytes. `path_label` is the
/// path relative to the corpus root, used only for the JSONL record and
/// panic-reproduction pointer — never touched as a filesystem path here.
pub fn run_one(
    path_label: &str,
    bytes: &[u8],
    fonts: &FontStack,
    with_edit: bool,
    dump_drift: Option<&std::path::Path>,
) -> DocResult {
    let mut rec = DocResult::new(path_label, bytes.len() as u64);
    let overall_start = Instant::now();
    /* Issue #112 — `--dump-drift DIR`: write the original and the resaved
    `document.xml` of every document that does not round-trip byte for
    byte (or whose resave fails the well-formedness guard) so the drift
    can be diffed by hand. */
    let dump = |suffix: &str, xml: &[u8]| {
        if let Some(dir) = dump_drift {
            let stem = std::path::Path::new(path_label)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "document".into());
            let _ = std::fs::create_dir_all(dir);
            let _ = std::fs::write(dir.join(format!("{stem}.{suffix}.xml")), xml);
        }
    };

    macro_rules! stage {
        ($stage:literal, $expr:expr) => {{
            match panics::catch(|| $expr) {
                Ok(Ok(v)) => v,
                Ok(Err(e)) => {
                    rec.mark_error($stage, &e.to_string());
                    rec.elapsed_ms = overall_start.elapsed().as_millis();
                    return rec;
                }
                Err(p) => {
                    rec.mark_panic($stage, &p);
                    rec.elapsed_ms = overall_start.elapsed().as_millis();
                    return rec;
                }
            }
        }};
    }
    macro_rules! stage_infallible {
        ($stage:literal, $expr:expr) => {{
            match panics::catch(|| $expr) {
                Ok(v) => v,
                Err(p) => {
                    rec.mark_panic($stage, &p);
                    rec.elapsed_ms = overall_start.elapsed().as_millis();
                    return rec;
                }
            }
        }};
    }

    /* 1. read_docx. */
    let archive_a: DocxArchive = stage!("read_docx_1", format_docx::read_docx(bytes));
    rec.paragraph_count = Some(archive_a.document.paragraph_count());

    /* 2. Full layout (native — `crates/layout`, no browser). */
    let layout_t0 = Instant::now();
    let (pages_a, para_texts_a) = stage_infallible!(
        "layout_1",
        nativelayout::layout_document(&archive_a.document, fonts)
    );
    rec.layout_ms = Some(layout_t0.elapsed().as_millis());
    rec.page_count_before = Some(pages_a.len());

    /* 3. PDF export of every page (native "render" — `crates/format-pdf`). */
    let para_texts_refs: Vec<&str> = para_texts_a.iter().map(String::as_str).collect();
    let mut pdf_bytes: Vec<u8> = Vec::new();
    /* Issue #121 — images embed from the document's media parts. */
    stage!(
        "pdf_export",
        format_pdf::export_pdf_with_media(
            &pages_a,
            fonts,
            &para_texts_refs,
            &archive_a.document.media,
            format_pdf::PdfProfile::Plain,
            &mut pdf_bytes,
        )
    );
    rec.pdf_bytes_len = Some(pdf_bytes.len());
    rec.pdf_pages = Some(pages_a.len());

    /* 4. write_docx with NO edits — the "unchanged" resave. */
    let resaved_bytes: Vec<u8> = stage!(
        "write_docx_noedit",
        format_docx::write_docx(&archive_a, &archive_a.document)
    );

    /* Issue #112 — dump a drifting resave before any guard can end the
    record, so a malformed resave is diffable too. */
    if let (Ok(orig), Ok(resaved)) = (extract_doc_xml(bytes), extract_doc_xml(&resaved_bytes))
        && orig != resaved
    {
        dump("orig", &orig);
        dump("resaved", &resaved);
    }

    /* 4b. Issue #110 — strict well-formedness of the saved part, BEFORE
    our own reader gets a say. A misaligned passthrough splice is
    unparseable XML; it must surface as its own stage, never as a
    downstream "read_docx_2" error or a byte-delta number. */
    stage!(
        "wellformed_noedit",
        format_docx::check_document_xml_well_formed(&resaved_bytes)
    );

    /* 5. read_docx again. */
    let archive_b: DocxArchive = stage!("read_docx_2", format_docx::read_docx(&resaved_bytes));

    /* 6a. Sibling parts byte-identical. */
    let (sibling_identical, sibling_drift) = compare_siblings(&archive_a, &archive_b);
    rec.sibling_bytes_identical = Some(sibling_identical);
    rec.sibling_drift_bytes = Some(sibling_drift);

    /* 6b. `document.xml` unchanged when nothing was edited. */
    if let (Ok(doc_xml_orig), Ok(doc_xml_resaved)) =
        (extract_doc_xml(bytes), extract_doc_xml(&resaved_bytes))
    {
        let delta = (doc_xml_resaved.len() as i64 - doc_xml_orig.len() as i64).unsigned_abs();
        rec.document_xml_unchanged = Some(delta == 0);
        rec.document_xml_delta_noedit_bytes = Some(delta);
        /* Issue #112 — true byte identity + the construct the first
        differing byte belongs to, so the corpus histograms by bucket. */
        match drift::first_difference(&doc_xml_orig, &doc_xml_resaved) {
            None => rec.document_xml_byte_identical = Some(true),
            Some(offset) => {
                let point = drift::locate(&doc_xml_orig, offset);
                rec.document_xml_byte_identical = Some(false);
                rec.first_drift_offset = Some(point.offset);
                rec.first_drift_element = Some(point.element);
                rec.first_drift_context = Some(point.context);
            }
        }
    }

    /* 6c. No text loss. */
    let plain_a = archive_a.document.to_plain_text();
    let plain_b = archive_b.document.to_plain_text();
    rec.plain_text_equal = Some(plain_a == plain_b);

    /* 6d. Stable page count across the reopen. */
    let (pages_b, _para_texts_b) = stage_infallible!(
        "layout_2",
        nativelayout::layout_document(&archive_b.document, fonts)
    );
    rec.page_count_after = Some(pages_b.len());
    rec.page_count_stable = Some(pages_b.len() == pages_a.len());

    /* 7. Optional scripted edit + the round-trip harness's ≤2×N bound
    (`.claude/rules/docx.md`). Mirrors `tools/roundtrip`'s default-mode
    check: edit the FIRST parse, save, compare `document.xml` against the
    ORIGINAL file's bytes. */
    if with_edit {
        let end = stage_infallible!("edit_end_of_document", archive_a.document.end_of_document());
        let edited_doc = stage_infallible!(
            "edit_insert_text",
            archive_a.document.insert_text(end, EDIT_MARKER)
        );
        let edited_bytes: Vec<u8> = stage!(
            "edit_write_docx",
            format_docx::write_docx(&archive_a, &edited_doc)
        );
        /* Issue #110 — same strict guard on the edited save. */
        stage!(
            "wellformed_edit",
            format_docx::check_document_xml_well_formed(&edited_bytes)
        );
        if let (Ok(doc_xml_orig), Ok(doc_xml_edited)) =
            (extract_doc_xml(bytes), extract_doc_xml(&edited_bytes))
        {
            let delta = (doc_xml_edited.len() as i64 - doc_xml_orig.len() as i64).unsigned_abs();
            let bound = (EDIT_MARKER.len() as u64) * 2;
            rec.edit_check = Some(EditCheck {
                inserted_bytes: EDIT_MARKER.len(),
                document_xml_delta_bytes: delta,
                bound_bytes: bound,
                within_bound: delta <= bound,
            });
        }
    }

    rec.elapsed_ms = overall_start.elapsed().as_millis();
    rec
}
