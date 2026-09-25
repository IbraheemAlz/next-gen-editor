//! `corpus-native` — issue #88 real-document corpus harness, native driver.
//!
//! Walks a directory of `.docx` files and runs [`pipeline::run_one`] on each:
//! `read_docx` -> full layout (`crates/layout`) -> PDF export of every page
//! (`crates/format-pdf`) -> `write_docx` -> `read_docx` again, asserting no
//! panic, sibling byte-identity, `document.xml` stability, plain-text
//! equality, and stable page count (plus an optional scripted-edit ≤2×N
//! bound check). One JSON object per document, streamed to `--out` as
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
    /// Issue #112 — `--dump-drift DIR`: write `<name>.orig.xml` /
    /// `<name>.resaved.xml` for every document whose zero-edit resave is
    /// not byte-identical, so the drift can be diffed.
    dump_drift: Option<PathBuf>,
}

fn parse_args() -> Args {
    let mut corpus_dir = PathBuf::from("/data/corpus/files");
    let mut out = PathBuf::from("corpus-results.jsonl");
    let mut limit = None;
    let mut with_edit = true;
    let mut timeout_secs: u64 = 60;
    let mut worker = None;
    let mut dump_drift = None;

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
    }
}

/// `--worker <path>` entry point: run the pipeline on exactly one file and
/// print its JSON record to stdout. This process is expected to sometimes
/// die abnormally (that IS the thing being tested) — the parent driver
/// interprets a non-JSON stdout / non-zero exit as [`pipeline::Outcome::Crash`].
fn run_worker(path: &Path, with_edit: bool, dump_drift: Option<&Path>) -> ExitCode {
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
    let rec = pipeline::run_one(&label, &bytes, &fonts, with_edit, dump_drift);
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
/// kills the child outright (a real, unlike-a-thread cancellation).
fn run_in_subprocess(
    exe: &Path,
    doc_path: &Path,
    label: &str,
    size_bytes: u64,
    with_edit: bool,
    timeout: Duration,
    dump_drift: Option<&Path>,
) -> pipeline::DocResult {
    let mut cmd = Command::new(exe);
    cmd.arg("--worker").arg(doc_path);
    if !with_edit {
        cmd.arg("--no-edit");
    }
    if let Some(dir) = dump_drift {
        cmd.arg("--dump-drift").arg(dir);
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
fn collect_docx_files(root: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                stack.push(path);
            } else if file_type.is_file()
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
        return run_worker(worker_path, args.with_edit, args.dump_drift.as_deref());
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
    let mut drift_histogram: std::collections::BTreeMap<String, usize> =
        std::collections::BTreeMap::new();

    for (i, path) in files.iter().enumerate() {
        let label = path
            .strip_prefix(&args.corpus_dir)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        let size_bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);

        let rec = run_in_subprocess(
            &exe,
            path,
            &label,
            size_bytes,
            args.with_edit,
            timeout,
            args.dump_drift.as_deref(),
        );
        match rec.outcome {
            pipeline::Outcome::Ok => ok += 1,
            pipeline::Outcome::Error => errors += 1,
            pipeline::Outcome::Panic => panicked += 1,
            pipeline::Outcome::Timeout => timed_out += 1,
            pipeline::Outcome::Crash => crashed += 1,
        }
        if let Some(identical) = rec.document_xml_byte_identical {
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
    /* Issue #112 — the drift histogram, largest bucket first. */
    println!(
        "[corpus-native] zero-edit document.xml byte-identical: {noedit_identical}/{noedit_checked}"
    );
    if !drift_histogram.is_empty() {
        let mut buckets: Vec<(&String, &usize)> = drift_histogram.iter().collect();
        buckets.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
        println!("[corpus-native] first-differing-element histogram (docs):");
        for (key, count) in buckets {
            println!("[corpus-native]   {count:5}  {key}");
        }
    }
    println!("[corpus-native] JSONL written to {}", args.out.display());
    ExitCode::SUCCESS
}
