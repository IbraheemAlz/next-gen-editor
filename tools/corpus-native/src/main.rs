//! `corpus-native` — issue #88 real-document corpus harness, native driver.
//!
//! Walks a directory of `.docx` files and runs [`pipeline::run_one`] on each:
//! `read_docx` -> full layout (`crates/layout`) -> PDF export of every page
//! (`crates/format-pdf`) -> `write_docx` -> `read_docx` again, asserting no
//! panic, sibling byte-identity, `document.xml` stability, plain-text
//! equality, and stable page count (plus an optional scripted-edit fidelity
//! check, issue #251: primarily `source_bytes_rewritten == 0`, plus a
//! secondary ≤2×N + new-run-allowance size bound kept informational). One
//! JSON object per document, streamed to `--out` as
//! JSONL — this binary does *not* bucket or summarize; that is
//! `tools/corpus/report.mjs`'s job (issue #88 Scope §3), which reads the
//! JSONL this produces.
//!
//! ```text
//! cargo run -p corpus-native --release -- \
//!     --corpus-dir /data/corpus/files \
//!     --out /data/corpus/results.jsonl
//! ```
//!
//! ## Production layout timing (issue #318)
//!
//! The reduced layout above flattens tables, so it can never see a
//! table-layout cost. Each document is therefore also laid out by the
//! PRODUCTION pipeline (`engine-wasm`'s `Engine::build_pages`, through its
//! canvas-less `fuzz-native` surface) and the record carries
//! `engine_layout_ms`, `engine_page_count`, `engine_fingerprint` (the
//! `layout::geometry_fingerprint`, so two runs on two engine builds diff
//! document by document) and `engine_degradations`. The stage runs under a
//! per-document budget, `--layout-budget-ms` (default
//! [`DEFAULT_LAYOUT_BUDGET_MS`]): a layout still running past it reports
//! `outcome: "timeout"`, `stage: "engine_layout"` with every other column
//! intact, instead of stalling the run until `--timeout-secs` kills the
//! worker.
//!
//! Issue #418 — the budget is **CPU time** (`getrusage`), not wall clock: a
//! loaded machine stretches wall time but not the CPU a layout needs, so
//! it no longer produces false timeouts (a wall-clock backstop of 8x the
//! budget still ends a layout that blocks without burning CPU). The record
//! carries both `engine_layout_cpu_ms` and `engine_layout_wall_ms`. A
//! document that times out is run once more, alone, and recorded as
//! `timeout` only if the retry times out too (`timeout_retry` holds the
//! first attempt; `recovered: true` means the first timeout was noise). `--no-engine-layout` skips the stage; `--time` prints one
//! timing line per document and the slowest production layouts at the
//! end.
//!
//! ## Why every document runs in its own subprocess
//!
//! `pipeline::run_one` wraps every stage in `catch_unwind`, which catches
//! ordinary Rust panics — but a "wild" `.docx` corpus (this one's Apache POI
//! source literally includes files named things like
//! `51921-Word-Crash067.docx`) can also trigger a **stack overflow**, which
//! is not a panic: it is Rust's guard-page handler calling `abort()` on the
//! **whole process**, unconditionally, with no way to catch it from within
//! that process. A first version of this driver ran documents on plain
//! threads and confirmed this empirically — a real stack overflow partway
//! through the local 170-document corpus took the entire batch down with
//! it. So: `main()` re-execs itself as `--worker <path>` per document, in a
//! child process; a crash there is contained to that one JSONL record
//! ([`Outcome::Crash`]), and a hang is bounded by `--timeout-secs`
//! ([`Outcome::Timeout`], child killed).
//!
//! Exit code reflects whether the RUN completed, not what it found — a
//! document panicking or crashing is the harness doing its job, not the
//! tool failing. Exit is non-zero only for a setup problem (missing/empty
//! corpus dir, or `--worker` invoked on an unreadable file).

mod drift;
mod fonts;
mod nativelayout;
mod panics;
mod pipeline;
mod regen;

use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

struct Args {
    corpus_dir: PathBuf,
    out: PathBuf,
    limit: Option<usize>,
    with_edit: bool,
    timeout_secs: u64,
    worker: Option<PathBuf>,
    /// Issue #112 — `--dump-drift DIR`: write `<name>-<hash>.orig.xml` /
    /// `<name>.resaved.xml` for every document whose zero-edit resave is
    /// not byte-identical, so the drift can be diffed.
    dump_drift: Option<PathBuf>,
    /// Issue #318 — `--no-engine-layout` / `--layout-budget-ms N`.
    engine: pipeline::EngineLayoutOpts,
    /// Issue #318 — `--time`: one timing line per document on stderr and
    /// the slowest production layouts in the summary.
    time: bool,
    /// Issue #384 — `--regen-check`: regenerate every clean paragraph
    /// with no edit and histogram the mismatches by class.
    regen_check: bool,
}

/// Issue #318 — default per-document production-layout budget. Every
/// document of the local corpus lays out in well under a second; ten is
/// generous headroom for a slow CI box while still far below the
/// `--timeout-secs` backstop.
const DEFAULT_LAYOUT_BUDGET_MS: u64 = 10_000;

fn parse_args() -> Args {
    let mut corpus_dir = PathBuf::from("/data/corpus/files");
    let mut out = PathBuf::from("corpus-results.jsonl");
    let mut limit = None;
    let mut with_edit = true;
    let mut timeout_secs: u64 = 60;
    let mut worker = None;
    let mut dump_drift = None;
    let mut engine = pipeline::EngineLayoutOpts {
        enabled: true,
        budget: Duration::from_millis(DEFAULT_LAYOUT_BUDGET_MS),
    };
    let mut time = false;
    let mut regen_check = false;

    let raw: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < raw.len() {
        match raw[i].as_str() {
            "--corpus-dir" => {
                i += 1;
                if let Some(v) = raw.get(i) {
                    corpus_dir = PathBuf::from(v);
                }
            }
            "--out" => {
                i += 1;
                if let Some(v) = raw.get(i) {
                    out = PathBuf::from(v);
                }
            }
            "--limit" => {
                i += 1;
                if let Some(v) = raw.get(i) {
                    limit = v.parse::<usize>().ok();
                }
            }
            "--no-edit" => with_edit = false,
            "--timeout-secs" => {
                i += 1;
                if let Some(v) = raw.get(i) {
                    if let Ok(v) = v.parse::<u64>() {
                        timeout_secs = v;
                    }
                }
            }
            "--worker" => {
                i += 1;
                if let Some(v) = raw.get(i) {
                    worker = Some(PathBuf::from(v));
                }
            }
            "--dump-drift" => {
                i += 1;
                if let Some(v) = raw.get(i) {
                    dump_drift = Some(PathBuf::from(v));
                }
            }
            "--no-engine-layout" => engine.enabled = false,
            "--layout-budget-ms" => {
                i += 1;
                if let Some(v) = raw.get(i).and_then(|v| v.parse::<u64>().ok()) {
                    engine.budget = Duration::from_millis(v);
                }
            }
            "--time" => time = true,
            "--regen-check" => regen_check = true,
            other => {
                eprintln!("[corpus-native] warning: unrecognized arg `{other}`");
            }
        }
        i += 1;
    }

    Args {
        corpus_dir,
        out,
        limit,
        with_edit,
        timeout_secs,
        worker,
        dump_drift,
        engine,
        time,
        regen_check,
    }
}

/// `--worker <path>` entry point: run the pipeline on exactly one file and
/// print its JSON record to stdout. This process is expected to sometimes
/// die abnormally (that IS the thing being tested) — the parent driver
/// interprets a non-JSON stdout / non-zero exit as [`pipeline::Outcome::Crash`].
fn run_worker(
    path: &Path,
    with_edit: bool,
    dump_drift: Option<&Path>,
    engine: pipeline::EngineLayoutOpts,
    regen_check: bool,
) -> ExitCode {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!(
                "[corpus-native worker] failed to read {}: {e}",
                path.display()
            );
            return ExitCode::FAILURE;
        }
    };
    let fonts = fonts::bundled_stack();
    let label = path.to_string_lossy();
    let rec = pipeline::run_one(
        &label,
        &bytes,
        &fonts,
        with_edit,
        dump_drift,
        engine,
        regen_check,
    );
    match serde_json::to_string(&rec) {
        Ok(json) => {
            println!("{json}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("[corpus-native worker] failed to serialize record: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Run one document in a fresh `--worker` subprocess of `exe`, with a hard
/// wall-clock `timeout`. See the module docs for why this is a process, not
/// a thread. Reads the child's stdout to EOF on a helper thread so a slow
/// child can never deadlock this driver on a full pipe buffer; a timeout
/// kills the child outright (a real, unlike-a-thread cancellation). The
/// worker inherits the per-document switches of `args` (`--no-edit`,
/// `--dump-drift`, and the issue #318 production-layout stage).
fn run_in_subprocess(
    exe: &Path,
    doc_path: &Path,
    label: &str,
    size_bytes: u64,
    timeout: Duration,
    args: &Args,
) -> pipeline::DocResult {
    let mut cmd = Command::new(exe);
    cmd.arg("--worker").arg(doc_path);
    if !args.with_edit {
        cmd.arg("--no-edit");
    }
    if let Some(dir) = &args.dump_drift {
        cmd.arg("--dump-drift").arg(dir);
    }
    if args.regen_check {
        cmd.arg("--regen-check");
    }
    if args.engine.enabled {
        cmd.arg("--layout-budget-ms")
            .arg(args.engine.budget.as_millis().to_string());
    } else {
        cmd.arg("--no-engine-layout");
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return pipeline::DocResult::crashed(label, size_bytes, &format!("spawn failed: {e}"));
        }
    };
    let mut stdout = match child.stdout.take() {
        Some(s) => s,
        None => {
            let _ = child.kill();
            return pipeline::DocResult::crashed(label, size_bytes, "worker stdout unavailable");
        }
    };

    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = stdout.read_to_string(&mut buf);
        let _ = tx.send(buf);
    });

    let stdout_buf = match rx.recv_timeout(timeout) {
        Ok(buf) => buf,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return pipeline::DocResult::timed_out(label, size_bytes, timeout.as_secs());
        }
    };

    let status = match child.wait() {
        Ok(s) => s,
        Err(e) => {
            return pipeline::DocResult::crashed(label, size_bytes, &format!("wait failed: {e}"));
        }
    };

    if status.success() {
        match serde_json::from_str::<pipeline::DocResult>(stdout_buf.trim()) {
            Ok(rec) => rec,
            Err(e) => pipeline::DocResult::crashed(
                label,
                size_bytes,
                &format!("worker exited 0 but stdout wasn't a valid record: {e}"),
            ),
        }
    } else {
        pipeline::DocResult::crashed(label, size_bytes, &describe_exit_status(&status))
    }
}

#[cfg(unix)]
fn describe_exit_status(status: &std::process::ExitStatus) -> String {
    use std::os::unix::process::ExitStatusExt;
    match status.signal() {
        Some(sig) => format!(
            "worker killed by signal {sig} ({}) — likely a stack overflow / segfault \
             the panic-catching layer cannot see",
            signal_name(sig)
        ),
        None => format!("worker exited with status {status}"),
    }
}

#[cfg(not(unix))]
fn describe_exit_status(status: &std::process::ExitStatus) -> String {
    format!("worker exited with status {status}")
}

#[cfg(unix)]
fn signal_name(sig: i32) -> &'static str {
    match sig {
        4 => "SIGILL",
        6 => "SIGABRT",
        8 => "SIGFPE",
        9 => "SIGKILL",
        11 => "SIGSEGV",
        _ => "signal",
    }
}

/// Recursively collect every `*.docx` path under `root`, sorted for
/// deterministic run order (bisecting a regression across two runs relies
/// on stable ordering).
///
/// Issue #421 — symlinks are followed (a corpus assembled from symlinks to
/// `/data/corpus/files` subsets is the natural way to select a slice): the
/// entry type comes from `fs::metadata` (which resolves the link), not
/// `DirEntry::file_type` (which reports the link itself). A symlinked
/// directory is descended too, guarded against loops by the set of
/// canonical directories already walked (a link back up the tree, or two
/// links to one directory, is walked once). A dangling link is skipped.
/// Files keep the path they were reached through, so the JSONL label is the
/// name in the corpus dir, not the link target.
fn collect_docx_files(root: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut visited: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    visited.insert(std::fs::canonicalize(root)?);
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            let Ok(meta) = std::fs::metadata(&path) else {
                continue; // dangling symlink / vanished entry
            };
            if meta.is_dir() {
                if let Ok(real) = std::fs::canonicalize(&path)
                    && visited.insert(real)
                {
                    stack.push(path);
                } // else: a directory already walked (symlink loop / alias)
            } else if meta.is_file()
                && path
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e.eq_ignore_ascii_case("docx"))
            {
                out.push(path);
            }
        }
    }
    out.sort();
    Ok(out)
}

fn main() -> ExitCode {
    panics::install();
    let args = parse_args();

    if let Some(worker_path) = &args.worker {
        return run_worker(
            worker_path,
            args.with_edit,
            args.dump_drift.as_deref(),
            args.engine,
            args.regen_check,
        );
    }

    if !args.corpus_dir.is_dir() {
        eprintln!(
            "[corpus-native] corpus dir `{}` does not exist or is not a directory",
            args.corpus_dir.display()
        );
        eprintln!("[corpus-native] run `node tools/corpus/fetch.mjs` first, or pass --corpus-dir");
        return ExitCode::FAILURE;
    }

    let mut files = match collect_docx_files(&args.corpus_dir) {
        Ok(f) => f,
        Err(e) => {
            eprintln!(
                "[corpus-native] failed to walk `{}`: {e}",
                args.corpus_dir.display()
            );
            return ExitCode::FAILURE;
        }
    };
    if files.is_empty() {
        eprintln!(
            "[corpus-native] no .docx files under `{}` — nothing to run",
            args.corpus_dir.display()
        );
        return ExitCode::FAILURE;
    }
    if let Some(limit) = args.limit {
        files.truncate(limit);
    }

    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("[corpus-native] can't find my own executable path: {e}");
            return ExitCode::FAILURE;
        }
    };

    let out_file = match File::create(&args.out) {
        Ok(f) => f,
        Err(e) => {
            eprintln!(
                "[corpus-native] failed to create `{}`: {e}",
                args.out.display()
            );
            return ExitCode::FAILURE;
        }
    };
    let mut writer = BufWriter::new(out_file);

    println!(
        "[corpus-native] {} documents under {} -> {}",
        files.len(),
        args.corpus_dir.display(),
        args.out.display()
    );

    let timeout = Duration::from_secs(args.timeout_secs);
    let run_start = Instant::now();
    let mut ok = 0usize;
    let mut errors = 0usize;
    let mut panicked = 0usize;
    let mut timed_out = 0usize;
    let mut crashed = 0usize;
    /* Issue #112 — zero-edit `document.xml` drift, bucketed by the
    construct the first differing byte falls in (see `drift.rs`). */
    let mut noedit_checked = 0usize;
    let mut noedit_identical = 0usize;
    /* Issue #434 — documents whose main part is regenerate-only. */
    let mut noedit_regenerate_only = 0usize;
    let mut drift_histogram: std::collections::BTreeMap<String, usize> =
        std::collections::BTreeMap::new();
    /* Issue #134 — the UI save path (`format_docx::save_docx`). */
    let mut ui_checked = 0usize;
    let mut ui_siblings_identical = 0usize;
    let mut ui_matches_write_docx = 0usize; /* Issue #250 — pure-insertion edited saves (0 source bytes rewritten),
    plain typing vs the same net edit in track-changes mode. */
    let mut edit_checked = 0usize;
    let mut pure_plain = 0usize;
    let mut pure_tracked = 0usize;
    let mut stale_plain = 0usize;
    let mut stale_tracked = 0usize;
    /* Issue #251 — the scripted-edit fidelity bound (primary:
    `source_bytes_rewritten == 0`) and the secondary size bound (2×N + a
    new-run allowance), plus a root-cause histogram for every document
    that still rewrites source bytes (tracked against issues #242-#249). */
    let mut fidelity_ok_count = 0usize;
    let mut secondary_bound_violations = 0usize;
    /* Issue #282 — a comment added to an untouched paragraph / a source
    comment deleted. */
    let mut comment_checked = 0usize;
    let mut comment_insert_pure = 0usize;
    let mut comment_insert_anchored = 0usize;
    let mut comment_delete_checked = 0usize;
    let mut comment_delete_pure = 0usize;
    let mut comment_delete_clean = 0usize;
    /* Issue #419 — the paragraph-property probe. */
    let mut ppr_checked = 0usize;
    let mut ppr_ind_only = 0usize;
    let mut ppr_reread_ok = 0usize;
    let mut ppr_rewritten = 0u64;
    /* Issue #371 — the ModifyStyle probe. */
    let mut style_checked = 0usize;
    let mut style_only_element = 0usize;
    let mut style_reread_ok = 0usize;
    let mut style_delta_le_element = 0usize;
    let mut rewrite_causes: std::collections::BTreeMap<String, (usize, String, u64)> =
        std::collections::BTreeMap::new();
    /* Issues #325 / #394 — documents / parts read through the namespace
    prefix normaliser (regenerate-only parts). */
    let mut normalized_docs = 0usize;
    let mut normalized_parts = 0usize;
    /* Issue #318 — production-layout timings `(ms, label)`, the
    documents that blew the budget, and a degradation-reason histogram. */
    let mut engine_times: Vec<(u128, String)> = Vec::new();
    let mut engine_over_budget: Vec<String> = Vec::new();
    /* Issue #418 — first-attempt timeouts that the lone retry cleared. */
    let mut timeouts_recovered = 0usize;
    let mut engine_reasons: std::collections::BTreeMap<String, usize> =
        std::collections::BTreeMap::new();
    /* Issue #355 — theme-font resolution: documents with a theme part,
    documents with at least one run whose face now comes from the theme
    where it previously fell back, and those runs. */
    let mut themed_docs = 0usize;
    let mut theme_resolving_docs = 0usize;
    let mut theme_resolved_runs = 0u64;
    let mut theme_runs = 0u64;
    let mut theme_faces: std::collections::BTreeMap<String, usize> =
        std::collections::BTreeMap::new();
    /* Issue #384 — `--regen-check`: paragraphs regenerated / mismatching
    (all, and those with text), documents with a mismatch, and the
    mismatching paragraphs per class. */
    let mut regen_docs = 0usize;
    let mut regen_docs_mismatched = 0usize;
    let mut regen_checked = 0u64;
    let mut regen_nonempty_checked = 0u64;
    let mut regen_mismatched = 0u64;
    let mut regen_nonempty_mismatched = 0u64;
    let mut regen_classes: std::collections::BTreeMap<String, (u64, String)> =
        std::collections::BTreeMap::new();
    for (i, path) in files.iter().enumerate() {
        let label = path
            .strip_prefix(&args.corpus_dir)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        let size_bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);

        let mut rec = run_in_subprocess(&exe, path, &label, size_bytes, timeout, &args);
        /* Issue #418 — a timeout under machine load says little about the
        document (two different documents "timed out" at load 30-60 and lay
        out in 2-3.5 s alone). Run it once more, alone - the driver is
        serial, so nothing of ours competes - and record `timeout` only if
        that attempt times out too. The first attempt's numbers ride along
        in `timeout_retry`. */
        if rec.outcome == pipeline::Outcome::Timeout {
            let first = pipeline::TimeoutRetry {
                first_stage: rec.stage.clone(),
                first_wall_ms: rec.engine_layout_wall_ms,
                first_cpu_ms: rec.engine_layout_cpu_ms,
                recovered: false,
            };
            eprintln!("[corpus-native] {label}: timeout, retrying once alone");
            let mut second = run_in_subprocess(&exe, path, &label, size_bytes, timeout, &args);
            let recovered = second.outcome != pipeline::Outcome::Timeout;
            second.timeout_retry = Some(pipeline::TimeoutRetry { recovered, ..first });
            if recovered {
                timeouts_recovered += 1;
            }
            rec = second;
        }
        match rec.outcome {
            pipeline::Outcome::Ok => ok += 1,
            pipeline::Outcome::Error => errors += 1,
            pipeline::Outcome::Panic => panicked += 1,
            pipeline::Outcome::Timeout => timed_out += 1,
            pipeline::Outcome::Crash => crashed += 1,
        }
        if let Some(ms) = rec.engine_layout_ms {
            engine_times.push((ms, label.clone()));
        }
        if rec.outcome == pipeline::Outcome::Timeout
            && rec.stage.as_deref() == Some("engine_layout")
        {
            engine_over_budget.push(label.clone());
        }
        let mut seen: Vec<&String> = Vec::new();
        for reason in &rec.engine_degradations {
            if !seen.contains(&reason) {
                seen.push(reason);
                *engine_reasons.entry(reason.clone()).or_insert(0) += 1;
            }
        }
        if args.time {
            let ms = |v: Option<u128>| v.map_or_else(|| "-".to_string(), |v| v.to_string());
            eprintln!(
                "[corpus-native] time {label}: outcome={:?} engine_layout_ms={} \
                 engine_layout_cpu_ms={} engine_pages={} reduced_layout_ms={} elapsed_ms={}",
                rec.outcome,
                ms(rec.engine_layout_ms),
                ms(rec.engine_layout_cpu_ms),
                rec.engine_page_count
                    .map_or_else(|| "-".to_string(), |v| v.to_string()),
                ms(rec.layout_ms),
                rec.elapsed_ms
            );
        }
        if rec.main_part_regenerate_only {
            noedit_regenerate_only += 1;
        } else if let Some(identical) = rec.document_xml_byte_identical {
            noedit_checked += 1;
            if identical {
                noedit_identical += 1;
            } else {
                let key = rec
                    .first_drift_context
                    .clone()
                    .unwrap_or_else(|| "<unknown>".into());
                *drift_histogram.entry(key).or_insert(0) += 1;
            }
        }

        if let Some(ec) = &rec.edit_check {
            edit_checked += 1;
            pure_plain += usize::from(ec.source_bytes_rewritten == 0);
            pure_tracked += usize::from(ec.tracked_source_bytes_rewritten == Some(0));
            stale_plain += usize::from(ec.markup_in_step == Some(false));
            stale_tracked += usize::from(ec.tracked_markup_in_step == Some(false));
        }

        if let Some(cc) = &rec.comment_check {
            if cc.insert_block.is_some() {
                comment_checked += 1;
                comment_insert_pure += usize::from(cc.insert_pure_insertion == Some(true));
                comment_insert_anchored += usize::from(cc.insert_anchored == Some(true));
            }
            if cc.delete_id.is_some() {
                comment_delete_checked += 1;
                comment_delete_pure += usize::from(cc.delete_pure_deletion == Some(true));
                comment_delete_clean +=
                    usize::from(cc.delete_anchors_left == Some(0) && cc.delete_gone == Some(true));
            }
        }

        if let Some(pc) = &rec.ppr_check {
            ppr_checked += 1;
            ppr_ind_only += usize::from(pc.ind_only);
            ppr_reread_ok += usize::from(pc.reread_ok);
            ppr_rewritten += pc.source_bytes_rewritten;
        }

        if let Some(sc) = &rec.style_check {
            style_checked += 1;
            style_only_element += usize::from(sc.only_element);
            style_reread_ok += usize::from(sc.reread_ok);
            style_delta_le_element +=
                usize::from(sc.styles_xml_delta_bytes <= sc.element_delta_bytes);
        }

        /* Issues #325 / #394 — regenerate-only (normalised) parts. */
        if !rec.normalized_parts.is_empty() {
            normalized_docs += 1;
            normalized_parts += rec.normalized_parts.len();
        }

        if let Some(t) = &rec.theme_fonts {
            themed_docs += usize::from(t.has_theme);
            theme_resolving_docs += usize::from(t.newly_resolved > 0);
            theme_resolved_runs += t.newly_resolved;
            theme_runs += t.runs;
            for face in &t.faces {
                *theme_faces.entry(face.clone()).or_insert(0) += 1;
            }
        }

        if let Some(rc) = &rec.regen_check {
            regen_docs += 1;
            regen_docs_mismatched += usize::from(rc.mismatched > 0);
            regen_checked += u64::from(rc.checked);
            regen_nonempty_checked += u64::from(rc.nonempty_checked);
            regen_mismatched += u64::from(rc.mismatched);
            regen_nonempty_mismatched += u64::from(rc.nonempty_mismatched);
            for (class, n) in &rc.classes {
                regen_classes
                    .entry(class.clone())
                    .or_insert((0, label.clone()))
                    .0 += u64::from(*n);
            }
        }

        if let Some(identical) = rec.ui_save_siblings_identical {
            ui_checked += 1;
            ui_siblings_identical += usize::from(identical);
            ui_matches_write_docx += usize::from(rec.ui_save_matches_write_docx == Some(true));
        }

        if let Some(ec) = &rec.edit_check {
            if ec.fidelity_ok {
                fidelity_ok_count += 1;
            }
            if !ec.within_secondary_bound {
                secondary_bound_violations += 1;
            }
            if let Some(cause) = &ec.rewrite_cause {
                let entry = rewrite_causes.entry(cause.clone()).or_insert((
                    0usize,
                    label.clone(),
                    size_bytes,
                ));
                entry.0 += 1;
                if size_bytes < entry.2 {
                    entry.1 = label.clone();
                    entry.2 = size_bytes;
                }
            }
        }

        if let Err(e) = writeln!(
            writer,
            "{}",
            serde_json::to_string(&rec).unwrap_or_default()
        ) {
            eprintln!("[corpus-native] failed writing JSONL: {e}");
            return ExitCode::FAILURE;
        }
        /* Flush every record — a nightly run can be killed by the workflow's
        1h timeout mid-corpus; partial JSONL must still be usable by
        `report.mjs` rather than losing the whole buffered tail. */
        let _ = writer.flush();

        if (i + 1) % 25 == 0 || i + 1 == files.len() {
            eprintln!(
                "[corpus-native] {}/{} — ok={ok} error={errors} panic={panicked} crash={crashed} timeout={timed_out} ({:.1}s elapsed)",
                i + 1,
                files.len(),
                run_start.elapsed().as_secs_f32()
            );
        }
    }

    println!(
        "[corpus-native] done: {} documents, ok={ok} error={errors} panic={panicked} crash={crashed} timeout={timed_out}, {:.1}s total",
        files.len(),
        run_start.elapsed().as_secs_f32()
    );
    println!(
        "[corpus-native] UI-path save (#134): siblings byte-identical {ui_siblings_identical}/{ui_checked}, \
         byte-identical to write_docx {ui_matches_write_docx}/{ui_checked}"
    );
    println!(
        "[corpus-native] edited save pure insertion (#250): plain {pure_plain}/{edit_checked}, \
         track-changes {pure_tracked}/{edit_checked}; stale source markup: plain {stale_plain}, \
         track-changes {stale_tracked}"
    );
    /* Issue #112 — the drift histogram, largest bucket first. */
    println!(
        "[corpus-native] zero-edit document.xml byte-identical: {noedit_identical}/{noedit_checked}{}",
        if noedit_regenerate_only > 0 {
            format!(
                " ({noedit_regenerate_only} regenerate-only main part(s) not compared — \
                 normalised #325 or repaired #434)"
            )
        } else {
            String::new()
        }
    );
    if !drift_histogram.is_empty() {
        let mut buckets: Vec<(&String, &usize)> = drift_histogram.iter().collect();
        buckets.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
        println!("[corpus-native] first-differing-element histogram (docs):");
        for (key, count) in buckets {
            println!("[corpus-native]   {count:5}  {key}");
        }
    }
    /* Issue #251 — the fidelity bound is now primary; the old ≤2×N size
    bound is reported informationally via the JSONL `edit_check.bound_bytes`
    / `within_bound` columns, not summarized here. */
    println!(
        "[corpus-native] scripted-edit fidelity bound (source_bytes_rewritten == 0): \
         {fidelity_ok_count}/{edit_checked}"
    );
    println!(
        "[corpus-native] scripted-edit secondary size bound (<=2xN + new-run allowance) \
         violated: {secondary_bound_violations}/{edit_checked}"
    );
    println!(
        "[corpus-native] comment added to an untouched paragraph (#282): pure insertion \
         {comment_insert_pure}/{comment_checked}, re-read anchored {comment_insert_anchored}/{comment_checked}; \
         source comment deleted: pure deletion {comment_delete_pure}/{comment_delete_checked}, \
         no anchor or body left {comment_delete_clean}/{comment_delete_checked}"
    );
    println!(
        "[corpus-native] paragraph-property change (#419): only <w:ind> respelled \
         {ppr_ind_only}/{ppr_checked}, re-read with the new indent {ppr_reread_ok}/{ppr_checked} \
         ({ppr_rewritten} source bytes rewritten in all)"
    );
    println!(
        "[corpus-native] ModifyStyle (#371): only the edited <w:style> changed \
         {style_only_element}/{style_checked}, styles.xml delta <= the element's \
         {style_delta_le_element}/{style_checked}, re-read {style_reread_ok}/{style_checked}"
    );
    if !rewrite_causes.is_empty() {
        let mut buckets: Vec<(&String, &(usize, String, u64))> = rewrite_causes.iter().collect();
        buckets.sort_by(|a, b| b.1.0.cmp(&a.1.0).then(a.0.cmp(b.0)));
        println!(
            "[corpus-native] rewrite root-cause histogram (docs whose edited save still \
             rewrote source bytes, issues #242-#249):"
        );
        for (cause, (count, example_path, example_bytes)) in buckets {
            println!(
                "[corpus-native]   {count:5}  {cause:<14} e.g. {example_path} ({example_bytes} B)"
            );
        }
    }
    if args.regen_check {
        println!(
            "[corpus-native] regen-check (#384): {regen_mismatched}/{regen_checked} clean paragraphs \
             do not regenerate byte-identically ({regen_nonempty_mismatched}/{regen_nonempty_checked} \
             with text) in {regen_docs_mismatched}/{regen_docs} documents"
        );
        let mut classes: Vec<(&String, &(u64, String))> = regen_classes.iter().collect();
        classes.sort_by(|a, b| b.1.0.cmp(&a.1.0).then(a.0.cmp(b.0)));
        for (class, (n, example)) in classes {
            println!("[corpus-native]   {n:5}  {class:<16} e.g. {example}");
        }
    }
    println!(
        "[corpus-native] theme fonts (#355): {theme_resolving_docs}/{} documents resolve a theme \
         font where they previously fell back ({theme_resolved_runs}/{theme_runs} runs); \
         {themed_docs} carry a theme part",
        files.len()
    );
    println!(
        "[corpus-native] non-canonical namespace prefixes (#325/#394) or malformed XML repaired \
         (#434): {normalized_docs} documents, {normalized_parts} parts normalised (regenerate-only: \
         their zero-edit save is not byte-identical to the source)"
    );
    if !theme_faces.is_empty() {
        let mut faces: Vec<(&String, &usize)> = theme_faces.iter().collect();
        faces.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
        let list: Vec<String> = faces.iter().map(|(f, n)| format!("{f} ({n})")).collect();
        println!("[corpus-native]   theme faces (docs): {}", list.join(", "));
    }
    /* Issue #318 — the production layout's cost and degradations. */
    if args.engine.enabled {
        engine_times.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        let total: u128 = engine_times.iter().map(|(ms, _)| ms).sum();
        println!(
            "[corpus-native] production layout (#318): {} laid out, {total} ms total, \
             {} over the {} ms CPU budget (#418)",
            engine_times.len(),
            engine_over_budget.len(),
            args.engine.budget.as_millis()
        );
        for label in &engine_over_budget {
            println!("[corpus-native]   over budget (confirmed on retry): {label}");
        }
        if args.time {
            for (ms, label) in engine_times.iter().take(10) {
                println!("[corpus-native]   {ms:7} ms  {label}");
            }
        }
        if !engine_reasons.is_empty() {
            println!("[corpus-native] production layout degradation reasons (docs):");
            for (reason, count) in &engine_reasons {
                println!("[corpus-native]   {count:5}  {reason}");
            }
        }
    }
    println!("[corpus-native] timeouts cleared by the lone retry (#418): {timeouts_recovered}");
    println!("[corpus-native] JSONL written to {}", args.out.display());
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Issue #421 — a corpus dir made of symlinks (to files, to a
    /// directory, a loop back up the tree, a dangling link) yields the
    /// `.docx` files, each once per link, and terminates.
    #[cfg(unix)]
    #[test]
    fn collect_docx_files_follows_symlinks_with_a_loop_guard() {
        use std::os::unix::fs::symlink;
        let base = std::env::temp_dir().join(format!(
            "corpus-native-symlinks-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        let real = base.join("real");
        let sub = real.join("sub");
        let corpus = base.join("corpus");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::create_dir_all(&corpus).unwrap();
        std::fs::write(real.join("a.docx"), b"a").unwrap();
        std::fs::write(sub.join("b.DOCX"), b"b").unwrap();
        std::fs::write(real.join("notes.txt"), b"x").unwrap();
        // File symlinks, one with a non-docx target name but docx link name.
        symlink(real.join("a.docx"), corpus.join("link-a.docx")).unwrap();
        symlink(sub.join("b.DOCX"), corpus.join("link-b.docx")).unwrap();
        // A link to a .txt named .docx still counts by the link's name.
        symlink(real.join("notes.txt"), corpus.join("not-a-doc.txt")).unwrap();
        // A directory symlink, and a loop back to the corpus root.
        symlink(&sub, corpus.join("dirlink")).unwrap();
        symlink(&corpus, sub.join("loop")).unwrap();
        symlink(&corpus, corpus.join("self")).unwrap();
        // Dangling.
        symlink(base.join("missing.docx"), corpus.join("dangling.docx")).unwrap();

        let found = collect_docx_files(&corpus).unwrap();
        let names: Vec<String> = found
            .iter()
            .map(|p| {
                p.strip_prefix(&corpus)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(
            names,
            vec!["dirlink/b.DOCX", "link-a.docx", "link-b.docx"],
            "found {names:?}"
        );
        let _ = std::fs::remove_dir_all(&base);
    }
}
