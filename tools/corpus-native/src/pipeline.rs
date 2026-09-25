//! Per-document pipeline (issue #88 Scope §2): `read_docx` -> full layout
//! (`crates/layout`, via [`crate::nativelayout`]) -> PDF export of every
//! page (`crates/format-pdf`, the native "render" — Canvas2D is
//! browser-only) -> `write_docx` -> `read_docx` again, asserting no panic,
//! sibling byte-identity, `document.xml` stability with no edits, plain-text
//! equality, and stable page count. Optionally (`--with-edit`, on by
//! default) applies one scripted edit and re-checks the round-trip
//! harness's edit-drift bound (`.claude/rules/docx.md`, issue #251):
//! primarily `source_bytes_rewritten == 0` (no original byte respelled),
//! plus a secondary size bound of `2 × inserted UTF-8 bytes` + an
//! allowance for any run(s) the edit had to create. The old size-only
//! `≤ 2×N` number is kept as an informational column (`bound_bytes` /
//! `within_bound`).
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
    /// Informational only since issue #251 — the raw `document.xml` size
    /// delta. Kept for the historical record; it cannot distinguish a
    /// faithful insertion (which may legitimately need a new `<w:r>`) from
    /// a lossy regeneration, so it is no longer a pass/fail bound on its
    /// own. See `fidelity_ok` / `within_secondary_bound`.
    pub document_xml_delta_bytes: u64,
    /// Informational only since issue #251 — the old `2 × inserted_bytes`
    /// number, kept as a column. Superseded by `secondary_bound_bytes`.
    pub bound_bytes: u64,
    /// Informational only since issue #251 — `document_xml_delta_bytes <=
    /// bound_bytes`. Superseded by `within_secondary_bound`.
    pub within_bound: bool,
    /// Issue #199 / #251 — bytes of the ORIGINAL `document.xml` the edited
    /// save rewrote: the span between the longest common prefix and suffix
    /// of the two parts. 0 means the save is a pure insertion (nothing of
    /// the source was lost or respelled); `document_xml_delta_bytes` alone
    /// cannot tell (a regeneration that DROPS bytes can hide inside the
    /// old bound). **This is the primary edit-drift bound (issue #251):
    /// a faithful save must have `source_bytes_rewritten == 0`.**
    #[serde(default)]
    pub source_bytes_rewritten: u64,
    /// Issue #199 — bytes of the edited part inside that span (the
    /// inserted text plus whatever markup carries it).
    #[serde(default)]
    pub edited_region_bytes: u64,
    /// Issue #250 — [`Self::source_bytes_rewritten`] for the SAME net edit
    /// made in track-changes mode (tracked insertion of the marker plus a
    /// few extra bytes, then a tracked delete of those extra bytes — the
    /// "backspace over my own insertion" path that removes text). A
    /// regenerated paragraph whose source markup went stale loses its
    /// source runs, so this count tracks the plain one only while every
    /// tracked edit path keeps `Paragraph::source_markup` in step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tracked_source_bytes_rewritten: Option<u64>,
    /// Issue #250 — the edited paragraph's source markup is still in step
    /// with its text after the plain / tracked edit (`None`: the paragraph
    /// carries no source markup).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub markup_in_step: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tracked_markup_in_step: Option<bool>,
    /// Issue #251 — `source_bytes_rewritten == 0`, spelled out as its own
    /// bool so a JSONL/report consumer doesn't have to re-derive the
    /// primary bound from the raw counter.
    #[serde(default)]
    pub fidelity_ok: bool,
    /// Issue #251 — the secondary (informational-turned-advisory) size
    /// bound: `2 × inserted_bytes` plus an allowance for any run(s) a
    /// faithful insertion had to create. See [`NEW_RUN_ALLOWANCE_BYTES`]
    /// for how the allowance is sized.
    #[serde(default)]
    pub new_run_allowance_bytes: u64,
    /// Issue #251 — `bound_bytes + new_run_allowance_bytes`.
    #[serde(default)]
    pub secondary_bound_bytes: u64,
    /// Issue #251 — `document_xml_delta_bytes <= secondary_bound_bytes`.
    #[serde(default)]
    pub within_secondary_bound: bool,
    /// Issue #251 — set only when `source_bytes_rewritten > 0`: a cheap
    /// substring-heuristic guess at which construct forced the rewrite
    /// (`hyperlink` / `comment anchor` / `form field` / `sdt` /
    /// `fldSimple` / `move` / `table` / `rPr` / `other`), cross-referenced
    /// against the filed root-cause issues #242-#249. Not a substitute for
    /// a real diff — see [`classify_rewrite`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rewrite_cause: Option<String>,
}

/// Issue #250 — `Some(in step)` for the paragraph at `pos`, `None` when it
/// carries no source markup.
fn markup_in_step(doc: &engine::DocumentTree, pos: &engine::LogicalPos) -> Option<bool> {
    let p = doc.paragraph_at_path(&pos.path)?;
    let m = p.source_markup.as_deref()?;
    Some(m.offsets_valid(p.text.len()))
}

/// Issue #250 — the scripted edit made in track-changes mode: a tracked
/// insertion of the marker plus a few extra bytes, then a tracked delete
/// of the extra bytes (inside the reviewer's own pending insertion, so the
/// text is removed rather than marked). The net text equals the plain
/// edit's. `None` when the tracked pipeline panicked or failed to write.
fn tracked_edit(
    archive: &DocxArchive,
    end: engine::LogicalPos,
) -> Option<(engine::DocumentTree, Vec<u8>)> {
    const TRACKED_EXTRA: &str = "xyz";
    let author = String::from("corpus-native");
    let date = String::from("2026-01-01T00:00:00Z");
    panics::catch(|| {
        let doc = archive.document.tracked_insert_text(
            end.clone(),
            &format!("{EDIT_MARKER}{TRACKED_EXTRA}"),
            author.clone(),
            date.clone(),
        );
        let from = end.offset + EDIT_MARKER.len() as u32;
        let doc = doc.tracked_delete_range(
            engine::LogicalPos::new(end.path.clone(), from),
            engine::LogicalPos::new(end.path.clone(), from + TRACKED_EXTRA.len() as u32),
            author,
            date,
        );
        let bytes = format_docx::write_docx(archive, &doc).ok()?;
        Some((doc, bytes))
    })
    .ok()
    .flatten()
}

/// Issue #251 — per-new-`<w:r>` size allowance for the secondary bound.
///
/// The choice, documented per issue #251's ask: rather than one flat
/// allowance, count the actual number of new run-open tags the edit
/// needed (a cheap heuristic, [`count_run_open_tags`]) and multiply by a
/// fixed per-run cost. The minimal markup an empty run adds is exactly 43
/// bytes (`<w:r><w:t xml:space="preserve"></w:t></w:r>`, the shape issue
/// #251 measured); 48 rounds that up with a few bytes of slack for the
/// rare case a carried-over `<w:rPr>` needs to ride along too.
const NEW_RUN_ALLOWANCE_BYTES: u64 = 48;

/// Issue #251 — count `<w:r>` / `<w:r ...>` / `<w:r/>` run-element open
/// tags in a `document.xml` byte slice. A cheap heuristic (a literal-byte
/// scan, not a real XML walk): it only counts a `<w:r` match whose next
/// byte closes the element name (space, `>`, or `/`), so `<w:rPr>`,
/// `<w:rFonts>`, `<w:rsid...>` etc. never match. Good enough to size the
/// new-run allowance and to spot a construct that gained/lost runs; not a
/// substitute for `format_docx`'s real parser.
fn count_run_open_tags(xml: &[u8]) -> usize {
    xml.windows(4)
        .enumerate()
        .filter(|(i, w)| {
            *w == *b"<w:r" && matches!(xml.get(i + 4), Some(b' ') | Some(b'>') | Some(b'/'))
        })
        .count()
}

/// Issue #251 — priority-ordered substring markers used to guess which
/// construct forced an edited save to rewrite original bytes, cross-
/// referenced against the filed root-cause issues. Checked in order;
/// first match wins.
const REWRITE_CAUSE_MARKERS: &[(&str, &str)] = &[
    ("w:hyperlink", "hyperlink"),              // #242
    ("w:commentRangeStart", "comment anchor"), // #243
    ("w:commentRangeEnd", "comment anchor"),   // #243
    ("w:commentReference", "comment anchor"),  // #243
    ("w:ffData", "form field"),                // #244
    ("w:sdt", "sdt"),                          // #245
    ("w:fldSimple", "fldSimple"),              // #246
    ("_GoBack", "fldSimple"),                  // #246
    ("moveFrom", "move"),                      // #247
    ("moveTo", "move"),                        // #247
    ("w:tbl", "table"),                        // #248
    ("<w:tc", "table"),                        // #248
    ("<w:tr", "table"),                        // #248
    ("w:rPr", "rPr"),                          // #249
];

/// Bytes of context inspected on each side of the rewritten span when
/// classifying it — enough to see an immediately-enclosing element
/// (`<w:hyperlink>`, `<w:sdt>`) without walking the full tree. The
/// scripted edit always lands at `end_of_document()`, so the enclosing
/// construct is always close by.
const REWRITE_CAUSE_WINDOW: usize = 400;

/// Issue #251 — classify why an edited save rewrote original bytes, by a
/// cheap substring scan of the ORIGINAL xml around the rewritten span
/// (see [`REWRITE_CAUSE_MARKERS`] / [`REWRITE_CAUSE_WINDOW`]). Not a real
/// diff — good enough to bucket the corpus against issues #242-#249.
fn classify_rewrite(orig: &[u8], region_start: usize, region_len: u64) -> &'static str {
    if let Some(shape) = classify_one_byte_rewrite(orig, region_start, region_len) {
        return shape;
    }
    let win_start = region_start.saturating_sub(REWRITE_CAUSE_WINDOW);
    let win_end = (region_start + region_len as usize + REWRITE_CAUSE_WINDOW).min(orig.len());
    let window = orig.get(win_start..win_end.max(win_start)).unwrap_or(&[]);
    let text = String::from_utf8_lossy(window);
    for (needle, label) in REWRITE_CAUSE_MARKERS {
        if text.contains(needle) {
            return label;
        }
    }
    "other"
}

/// Issue #248 — two one-byte "rewrites" that are really an insertion the
/// single-region metric cannot express, tagged by shape BEFORE the
/// substring markers (which would otherwise blame whatever construct is
/// nearby, typically a table):
///
/// - `"empty <w:p/>"` (#267): text typed into a self-closing `<w:p …/>`
///   opens it — the `/` of `/>` is the one rewritten byte;
/// - `"t preserve"` (#199 rule): a bare source `<w:t>` gains
///   `xml:space="preserve"` because the inserted text put whitespace at
///   its edge — two insertions (attribute + text) around the source `>`.
fn classify_one_byte_rewrite(
    orig: &[u8],
    region_start: usize,
    region_len: u64,
) -> Option<&'static str> {
    if region_len != 1 {
        return None;
    }
    let before = orig.get(..region_start)?;
    match orig.get(region_start)? {
        b'/' if orig.get(region_start + 1) == Some(&b'>') => {
            let tag_start = before.iter().rposition(|&b| b == b'<')?;
            let tag = &before[tag_start..];
            let is_p = tag.starts_with(b"<w:p")
                && matches!(tag.get(4), Some(b' ' | b'\t' | b'\r' | b'\n') | None);
            is_p.then_some("empty <w:p/>")
        }
        b'>' if before.ends_with(b"<w:t") => Some("t preserve"),
        _ => None,
    }
}

/// Issue #199 / #251 — `(prefix_len, original_span, edited_span)`: the
/// byte offset the ORIGINAL and edited parts start to differ at, and the
/// lengths of the region between the longest common prefix and the
/// longest common suffix of the two parts.
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
    /// Issue #134 — the live editor's save path (`format_docx::save_docx`,
    /// what engine-wasm `SaveDocx` / `SaveDocument` call with the tree
    /// alone, using its retained `source_package`): every source sibling
    /// entry re-emitted byte-identical, none dropped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ui_save_siblings_identical: Option<bool>,
    /// Issue #134 — the UI-path save is byte-identical to the harness
    /// path (`write_docx` against the source archive), for the zero-edit
    /// save and — with the scripted edit — the edited one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ui_save_matches_write_docx: Option<bool>,
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
            ui_save_siblings_identical: None,
            ui_save_matches_write_docx: None,
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

    /* 6e. Issue #134 — the UI save path. The live editor holds only the
    tree; `save_docx` must find the retained source package on it and
    write through `write_docx`, not synthesize a minimal package. */
    let ui_bytes: Vec<u8> = stage!(
        "ui_save_noedit",
        format_docx::save_docx(&archive_a.document)
    );
    stage!(
        "ui_save_wellformed",
        format_docx::check_document_xml_well_formed(&ui_bytes)
    );
    rec.ui_save_matches_write_docx = Some(ui_bytes == resaved_bytes);
    let archive_ui: DocxArchive = stage!("ui_save_reread", format_docx::read_docx(&ui_bytes));
    rec.ui_save_siblings_identical = Some(compare_siblings(&archive_a, &archive_ui).0);

    /* 7. Optional scripted edit + the issue #251 fidelity bound
    (`.claude/rules/docx.md`). Mirrors `tools/roundtrip`'s default-mode
    check: edit the FIRST parse, save, compare `document.xml` against the
    ORIGINAL file's bytes. */
    if with_edit {
        let end = stage_infallible!("edit_end_of_document", archive_a.document.end_of_document());
        let edited_doc = stage_infallible!(
            "edit_insert_text",
            archive_a.document.insert_text(end.clone(), EDIT_MARKER)
        );
        let edited_bytes: Vec<u8> = stage!(
            "edit_write_docx",
            format_docx::write_docx(&archive_a, &edited_doc)
        );
        /* Issue #134 — the UI path writes the same edited file. */
        let ui_edited: Vec<u8> = stage!("ui_save_edit", format_docx::save_docx(&edited_doc));
        if ui_edited != edited_bytes {
            rec.ui_save_matches_write_docx = Some(false);
        }
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
            let (rewrite_start, source_bytes_rewritten, edited_region_bytes) =
                rewritten_region(&doc_xml_orig, &doc_xml_edited);
            let fidelity_ok = source_bytes_rewritten == 0;
            let new_runs = count_run_open_tags(&doc_xml_edited)
                .saturating_sub(count_run_open_tags(&doc_xml_orig))
                as u64;
            let new_run_allowance_bytes = new_runs * NEW_RUN_ALLOWANCE_BYTES;
            let secondary_bound_bytes = bound + new_run_allowance_bytes;
            let within_secondary_bound = delta <= secondary_bound_bytes;
            let rewrite_cause = (!fidelity_ok).then(|| {
                classify_rewrite(&doc_xml_orig, rewrite_start, source_bytes_rewritten).to_string()
            });
            /* Issue #199 / #251 — `--dump-drift DIR` also dumps an edited
            save that breaks the fidelity or secondary bound, so the
            regeneration drift can be diffed against the original. */
            if !fidelity_ok || !within_secondary_bound {
                dump("edit-orig", &doc_xml_orig);
                dump("edited", &doc_xml_edited);
            }
            /* Issue #250 — the same net edit in track-changes mode. */
            let (tracked_source_bytes_rewritten, tracked_markup_in_step) =
                match tracked_edit(&archive_a, end.clone()) {
                    Some((doc, bytes)) => (
                        extract_doc_xml(&bytes)
                            .ok()
                            .map(|x| rewritten_region(&doc_xml_orig, &x).1),
                        markup_in_step(&doc, &end),
                    ),
                    None => (None, None),
                };
            rec.edit_check = Some(EditCheck {
                inserted_bytes: EDIT_MARKER.len(),
                document_xml_delta_bytes: delta,
                bound_bytes: bound,
                within_bound: delta <= bound,
                source_bytes_rewritten,
                edited_region_bytes,
                tracked_source_bytes_rewritten,
                markup_in_step: markup_in_step(&edited_doc, &end),
                tracked_markup_in_step,
                fidelity_ok,
                new_run_allowance_bytes,
                secondary_bound_bytes,
                within_secondary_bound,
                rewrite_cause,
            });
        }
    }

    rec.elapsed_ms = overall_start.elapsed().as_millis();
    rec
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewritten_region_reports_prefix_and_spans() {
        // Pure append: no rewrite, everything after the shared prefix is new.
        let (start, orig_span, edited_span) = rewritten_region(b"abc", b"abcXYZ");
        assert_eq!(start, 3);
        assert_eq!(orig_span, 0);
        assert_eq!(edited_span, 3);

        // A respelled middle byte: both sides report a 1-byte span at the
        // same offset.
        let (start, orig_span, edited_span) = rewritten_region(b"abcdef", b"abcXef");
        assert_eq!(start, 3);
        assert_eq!(orig_span, 1);
        assert_eq!(edited_span, 1);

        // Byte-identical: no span at all.
        let (_, orig_span, edited_span) = rewritten_region(b"same", b"same");
        assert_eq!(orig_span, 0);
        assert_eq!(edited_span, 0);
    }

    #[test]
    fn count_run_open_tags_ignores_lookalike_elements() {
        let xml = br#"<w:r><w:rPr><w:rFonts w:ascii="Arial"/></w:rPr><w:t>a</w:t></w:r><w:r/><w:r w:rsidR="1"><w:t>b</w:t></w:r>"#;
        // Three real `<w:r...>` opens: `<w:r>`, `<w:r/>`, `<w:r w:rsidR=...>`.
        // `<w:rPr>` / `<w:rFonts>` must not count.
        assert_eq!(count_run_open_tags(xml), 3);
    }

    #[test]
    fn count_run_open_tags_handles_short_input() {
        assert_eq!(count_run_open_tags(b""), 0);
        assert_eq!(count_run_open_tags(b"<w:"), 0);
    }

    #[test]
    fn classify_rewrite_prioritizes_markers_in_issue_order() {
        let hyperlink =
            br#"<w:p><w:hyperlink r:id="rId1"><w:r><w:t>x</w:t></w:r></w:hyperlink></w:p>"#;
        let region_start = hyperlink
            .windows(5)
            .position(|w| w == b"<w:t>")
            .expect("needle");
        assert_eq!(classify_rewrite(hyperlink, region_start, 3), "hyperlink");

        let comment = br#"<w:p><w:commentRangeStart w:id="0"/><w:r><w:t>x</w:t></w:r><w:commentRangeEnd w:id="0"/></w:p>"#;
        let region_start = comment
            .windows(5)
            .position(|w| w == b"<w:t>")
            .expect("needle");
        assert_eq!(classify_rewrite(comment, region_start, 3), "comment anchor");

        let table =
            br#"<w:tbl><w:tr><w:tc><w:p><w:r><w:t>x</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#;
        let region_start = table
            .windows(5)
            .position(|w| w == b"<w:t>")
            .expect("needle");
        assert_eq!(classify_rewrite(table, region_start, 3), "table");

        let plain = br#"<w:p><w:r><w:t>x</w:t></w:r></w:p>"#;
        let region_start = plain
            .windows(5)
            .position(|w| w == b"<w:t>")
            .expect("needle");
        assert_eq!(classify_rewrite(plain, region_start, 3), "other");
    }

    /// Issue #248 — the two one-byte insertion shapes are tagged by shape,
    /// not by the table they happen to sit next to.
    #[test]
    fn classify_rewrite_tags_one_byte_insertion_shapes() {
        let empty_p = br#"<w:tbl><w:tr><w:tc><w:p/></w:tc></w:tr></w:tbl><w:p w:rsidR="1"/>"#;
        let slash = empty_p.len() - 2;
        assert_eq!(classify_rewrite(empty_p, slash, 1), "empty <w:p/>");
        let bare_t =
            br#"<w:tbl><w:tr><w:tc><w:p><w:r><w:t>x</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#;
        let gt = bare_t.windows(5).position(|w| w == b"<w:t>").unwrap() + 4;
        assert_eq!(classify_rewrite(bare_t, gt, 1), "t preserve");
        /* A self-closing non-paragraph element stays with the markers. */
        let pr = br#"<w:tbl><w:tblPr/></w:tbl>"#;
        let slash = pr.windows(2).position(|w| w == b"/>").unwrap();
        assert_eq!(classify_rewrite(pr, slash, 1), "table");
    }

    #[test]
    fn classify_rewrite_clamps_window_at_document_edges() {
        // A rewrite near byte 0 (window start would underflow) and one at
        // the very end (window end would overflow) must not panic.
        let xml = br#"<w:t>x</w:t>"#;
        let _ = classify_rewrite(xml, 0, 1);
        let _ = classify_rewrite(xml, xml.len(), 1);
        let _ = classify_rewrite(xml, xml.len() + 1000, 1);
    }
}
