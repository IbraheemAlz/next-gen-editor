//! Stable-Rust smoke driver for the D5.5 (issue #90) fuzz generators.
//!
//! Nightly Rust is not installed in this environment and must not be
//! installed (binding rule 1) — `cargo +nightly fuzz run` is the real
//! libFuzzer flow, exercised only in CI (`.github/workflows/fuzz-nightly.yml`).
//! This binary proves the six targets' generators + `run_*` bodies work
//! *right now* on stable, in two passes per target:
//!
//! 1. **Corpus pass** — every file under `fuzz/corpus/<target>/` (binding
//!    rule 6: "asserts no panic on the seed corpus"). A panic here is a
//!    hard failure — it means a committed seed is bad, or a real
//!    regression. This pass's exit code is what actually gates.
//! 2. **Random sweep** — a deterministic pseudo-random byte stream (a tiny
//!    inline xorshift64; no `rand` dependency needed just to vary bytes
//!    per iteration) exercising far more of each generator's structural
//!    space than a handful of committed corpus files ever could. Panics found
//!    here are exactly what fuzzing exists to find — they're reported
//!    (deduplicated by message, one repro each) but do NOT fail the run;
//!    see the PR description for the real findings this surfaced and why
//!    each either is or isn't a genuine product bug.
//!
//! Run with: `cargo run --manifest-path fuzz/Cargo.toml --example smoke --release`
//! (debug works too; release matters once inputs start building large
//! tables/documents — a few hundred iterations in debug can take minutes).
//!
//! The engine is driven through `engine-wasm`'s `fuzz-native` feature, which
//! `fuzz/Cargo.toml` enables on its dependency — the same feature set the
//! `cargo test -p engine-wasm --features fuzz-native` CI step (issue #321)
//! runs the bridge-level tests under.
//!
//! Flags (all optional):
//! - `--target <name>`     run only that target (`rpc_command`, …).
//! - `--iterations <n>`    random-sweep inputs per target (default 500).
//! - `--strict`            ALSO fail (exit 1) when the random sweep panics —
//!   the post-#114–#118 contract for `rpc_command` is a clean sweep, so CI
//!   can hold the line instead of just reporting.
//! - `--from <i>` / `--to <i>`  run only sweep inputs `from..to` (the byte
//!   stream is still generated from index 0, so input `i` is identical to
//!   a full run's input `i`) — bisect a slow or memory-hungry input without
//!   re-running everything before it.
//! - `--no-corpus`         skip the corpus pass (with `--from`, mostly).
//! - `--log-inputs`        one line per input: target, index, length, time,
//!   RSS before/after (`/proc/self/statm`) and the input's peak live heap.
//!
//! ## Memory guards (issue #422)
//!
//! A random-sweep `rpc_command` input once drove this driver to 46.5 GB of
//! anonymous memory; the kernel OOM killer then took the whole terminal
//! scope down with it. Three layers now stop that, inside out:
//!
//! 1. A counting `#[global_allocator]` aborts the run past a live-heap
//!    ceiling (`SMOKE_RSS_LIMIT_MB`, default 2048 — libFuzzer's
//!    `-rss_limit_mb` / `-malloc_limit_mb` default) or on a single
//!    allocation that large, after printing the target, the sweep index
//!    and the input (hex) — and writing it to `SMOKE_ARTIFACT_DIR` when set,
//!    as `oom-<target>-<index>`.
//! 2. `RLIMIT_AS` (Linux) caps the address space at `SMOKE_AS_LIMIT_MB`
//!    (default 8192): an allocation the ceiling somehow missed fails and
//!    aborts this process instead of the machine.
//! 3. Run it under a memory-capped scope anyway (CLAUDE.md "Bash / agent
//!    ergonomics"):
//!    `systemd-run --user --scope -p MemoryMax=16G --quiet -- cargo run …`
//!
//! A watchdog thread also aborts on a single input running longer than
//! `SMOKE_TIMEOUT_SECS` (default 300, `0` = off — libFuzzer's `-timeout`),
//! with the same report (`timeout-<target>-<index>` in `SMOKE_ARTIFACT_DIR`).
//!
//! The peak RSS (`VmHWM`) and the peak live heap are printed at exit.

use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::BTreeMap;
use std::panic::{self, AssertUnwindSafe};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, AtomicUsize, Ordering};

/* ------------------------------------------------------------------ */
/* Memory guards (issue #422)                                          */
/* ------------------------------------------------------------------ */

/// Live heap bytes, the process-wide and the current input's peak.
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
static INPUT_PEAK: AtomicUsize = AtomicUsize::new(0);
/// Live-heap ceiling in bytes; `0` disables the check (set in `main`).
static LIMIT: AtomicUsize = AtomicUsize::new(0);
/// Set once the ceiling tripped: every later allocation (the report's own)
/// passes straight through.
static TRIPPED: AtomicBool = AtomicBool::new(false);
/// The input being fed, for the trip report — raw parts, so recording it
/// never allocates (the allocator reads them; a `Mutex` could deadlock).
static CUR_TARGET: AtomicPtr<u8> = AtomicPtr::new(std::ptr::null_mut());
static CUR_TARGET_LEN: AtomicUsize = AtomicUsize::new(0);
static CUR_DATA: AtomicPtr<u8> = AtomicPtr::new(std::ptr::null_mut());
static CUR_DATA_LEN: AtomicUsize = AtomicUsize::new(0);
/// Sweep index of the input being fed; `usize::MAX` = a corpus file.
static CUR_INDEX: AtomicUsize = AtomicUsize::new(usize::MAX);
/// When the input being fed started, in ms since [`epoch`]; `0` = idle.
static CUR_STARTED_MS: AtomicU64 = AtomicU64::new(0);

/// Process-relative clock for the watchdog (never 0 once fed).
fn epoch() -> &'static std::time::Instant {
    static EPOCH: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    EPOCH.get_or_init(std::time::Instant::now)
}

fn now_ms() -> u64 {
    epoch().elapsed().as_millis() as u64 + 1
}

/// `System`, counting live bytes and enforcing [`LIMIT`] — the stable
/// equivalent of libFuzzer's `-rss_limit_mb` / `-malloc_limit_mb`.
struct Counting;

#[global_allocator]
static ALLOC: Counting = Counting;

impl Counting {
    fn grow(&self, add: usize) {
        let now = LIVE.fetch_add(add, Ordering::Relaxed).saturating_add(add);
        PEAK.fetch_max(now, Ordering::Relaxed);
        INPUT_PEAK.fetch_max(now, Ordering::Relaxed);
        let limit = LIMIT.load(Ordering::Relaxed);
        if limit != 0 && (now > limit || add > limit) && !TRIPPED.swap(true, Ordering::SeqCst) {
            oom_report_and_abort(now, add, limit);
        }
    }
}

// SAFETY: every method delegates to `System` with the caller's layout; the
// bookkeeping is lock-free and never unwinds (the trip path aborts).
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        self.grow(layout.size());
        // SAFETY: forwarded unchanged.
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        self.grow(layout.size());
        // SAFETY: forwarded unchanged.
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        // SAFETY: forwarded unchanged.
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if new_size > layout.size() {
            self.grow(new_size - layout.size());
        } else {
            LIVE.fetch_sub(layout.size() - new_size, Ordering::Relaxed);
        }
        // SAFETY: forwarded unchanged.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

/// The input being fed right now: `(target, sweep index or "corpus",
/// bytes)`, read from the raw parts `feed` records (no lock — the reader
/// may be the allocator itself).
fn current_input() -> (String, String, Vec<u8>) {
    let target = {
        let p = CUR_TARGET.load(Ordering::SeqCst);
        let n = CUR_TARGET_LEN.load(Ordering::SeqCst);
        if p.is_null() {
            "<none>".to_string()
        } else {
            // SAFETY: points at a `&'static str` recorded by `feed`.
            String::from_utf8_lossy(unsafe { std::slice::from_raw_parts(p, n) }).into_owned()
        }
    };
    let data: Vec<u8> = {
        let p = CUR_DATA.load(Ordering::SeqCst);
        let n = CUR_DATA_LEN.load(Ordering::SeqCst);
        if p.is_null() {
            Vec::new()
        } else {
            // SAFETY: `feed` records the slice it is running and clears it
            // after; the slice outlives the call that tripped (the OOM
            // path runs inside that call; the watchdog only reads while
            // `CUR_STARTED_MS` says an input is running and aborts right
            // after).
            unsafe { std::slice::from_raw_parts(p, n) }.to_vec()
        }
    };
    let index = match CUR_INDEX.load(Ordering::SeqCst) {
        usize::MAX => "corpus".to_string(),
        i => i.to_string(),
    };
    (target, index, data)
}

/// Print the input, persist it as `<kind>-<target>-<index>` under
/// `SMOKE_ARTIFACT_DIR` (when set), then abort.
fn report_input_and_abort(kind: &str, target: &str, index: &str, data: &[u8]) -> ! {
    eprintln!(
        "[{target}] input #{index} ({} bytes, hex): {}",
        data.len(),
        hex(data)
    );
    if let Some(dir) = std::env::var_os("SMOKE_ARTIFACT_DIR") {
        let path = Path::new(&dir).join(format!("{kind}-{target}-{index}"));
        match std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&path, data)) {
            Ok(()) => eprintln!("[{target}] reproducer written to {}", path.display()),
            Err(e) => eprintln!("[{target}] could not write {}: {e}", path.display()),
        }
    }
    std::process::abort();
}

/// The ceiling tripped: name the input (and persist it), then abort — the
/// run cannot continue past an input that would have taken the machine.
#[cold]
fn oom_report_and_abort(now: usize, add: usize, limit: usize) -> ! {
    let (target, index, data) = current_input();
    eprintln!(
        "\n[{target}] OUT OF MEMORY on input #{index}: live heap {} MiB (+{} MiB in one \
         allocation) exceeds the {} MiB ceiling (SMOKE_RSS_LIMIT_MB); rss {} MiB",
        now >> 20,
        add >> 20,
        limit >> 20,
        rss_bytes() >> 20
    );
    eprintln!(
        "[{target}] allocation site:\n{}",
        std::backtrace::Backtrace::force_capture()
    );
    report_input_and_abort("oom", &target, &index, &data);
}

/// libFuzzer's `-timeout` for the stable driver: a thread that aborts the
/// run (with the input report) once one input has run `secs` seconds.
fn spawn_watchdog(secs: u64) {
    if secs == 0 {
        return;
    }
    let _ = epoch();
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(std::time::Duration::from_millis(500));
            let started = CUR_STARTED_MS.load(Ordering::SeqCst);
            if started == 0 || now_ms().saturating_sub(started) < secs * 1000 {
                continue;
            }
            if TRIPPED.swap(true, Ordering::SeqCst) {
                return; // the OOM report is already running
            }
            let (target, index, data) = current_input();
            eprintln!(
                "\n[{target}] TIMEOUT on input #{index}: still running after {secs} s \
                 (SMOKE_TIMEOUT_SECS); rss {} MiB",
                rss_bytes() >> 20
            );
            report_input_and_abort("timeout", &target, &index, &data);
        }
    });
}

/// Resident set size now (`/proc/self/statm`, Linux; 0 elsewhere).
fn rss_bytes() -> usize {
    std::fs::read_to_string("/proc/self/statm")
        .ok()
        .and_then(|s| s.split_whitespace().nth(1)?.parse::<usize>().ok())
        .map_or(0, |pages| pages * 4096)
}

/// Peak resident set size (`VmHWM` in `/proc/self/status`, Linux).
fn peak_rss_bytes() -> usize {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            let line = s.lines().find(|l| l.starts_with("VmHWM:"))?;
            line.split_whitespace().nth(1)?.parse::<usize>().ok()
        })
        .map_or(0, |kb| kb * 1024)
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Cap the address space so an allocation the ceiling missed fails here,
/// not on the machine (Linux; `SMOKE_AS_LIMIT_MB`, `0` = leave it alone).
fn cap_address_space() {
    #[cfg(target_os = "linux")]
    {
        let mb = env_usize("SMOKE_AS_LIMIT_MB", 8192);
        if mb == 0 {
            return;
        }
        let bytes = (mb as libc::rlim_t) << 20;
        let lim = libc::rlimit {
            rlim_cur: bytes,
            rlim_max: bytes,
        };
        // SAFETY: plain syscall on a stack value.
        if unsafe { libc::setrlimit(libc::RLIMIT_AS, &lim) } != 0 {
            eprintln!(
                "smoke: setrlimit(RLIMIT_AS, {mb} MiB) failed: {}",
                std::io::Error::last_os_error()
            );
        } else {
            println!("smoke: RLIMIT_AS = {mb} MiB");
        }
    }
}

/// Minimal xorshift64* PRNG — deterministic across runs (fixed seed), no
/// dependency needed just to generate "different bytes every iteration".
struct Xorshift64(u64);

impl Xorshift64 {
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }

    fn bytes(&mut self, len: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(len);
        while out.len() < len {
            out.extend_from_slice(&self.next_u64().to_le_bytes());
        }
        out.truncate(len);
        out
    }
}

/// One distinct panic message: how many inputs hit it, and the first
/// reproducer seen (good enough for triage — `cargo fuzz tmin` does real
/// minimization in the nightly CI flow).
struct PanicBucket {
    count: usize,
    example: Vec<u8>,
}

type Panics = BTreeMap<String, PanicBucket>;

/// Feeds one input. `index` is its sweep index (`None` for a corpus file);
/// `log` prints the per-input line (`--log-inputs`).
fn feed(
    name: &'static str,
    run: &impl Fn(&[u8]),
    data: &[u8],
    index: Option<usize>,
    log: bool,
    ran: &mut usize,
    panics: &mut Panics,
) {
    *ran += 1;
    CUR_TARGET.store(name.as_ptr().cast_mut(), Ordering::SeqCst);
    CUR_TARGET_LEN.store(name.len(), Ordering::SeqCst);
    CUR_DATA.store(data.as_ptr().cast_mut(), Ordering::SeqCst);
    CUR_DATA_LEN.store(data.len(), Ordering::SeqCst);
    CUR_INDEX.store(index.unwrap_or(usize::MAX), Ordering::SeqCst);
    let rss_before = rss_bytes();
    INPUT_PEAK.store(LIVE.load(Ordering::Relaxed), Ordering::Relaxed);
    let prev_hook = panic::take_hook();
    panic::set_hook(Box::new(|_| {})); // keep stdout clean; we print our own summary
    let started = std::time::Instant::now();
    CUR_STARTED_MS.store(now_ms(), Ordering::SeqCst);
    let result = panic::catch_unwind(AssertUnwindSafe(|| run(data)));
    CUR_STARTED_MS.store(0, Ordering::SeqCst);
    panic::set_hook(prev_hook);
    CUR_DATA.store(std::ptr::null_mut(), Ordering::SeqCst);
    CUR_DATA_LEN.store(0, Ordering::SeqCst);
    if log {
        let at = index.map_or_else(|| "corpus".to_string(), |i| i.to_string());
        println!(
            "[{name}] #{at} len={} ms={} rss_before={}MiB rss_after={}MiB input_peak_heap={}MiB",
            data.len(),
            started.elapsed().as_millis(),
            rss_before >> 20,
            rss_bytes() >> 20,
            INPUT_PEAK.load(Ordering::Relaxed) >> 20
        );
    }
    /* A single input that takes seconds is a hang candidate libFuzzer
    would report as a `timeout-*`; flag it (SMOKE_SLOW_MS, default 5000). */
    let slow_ms: u128 = std::env::var("SMOKE_SLOW_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(5000);
    if started.elapsed().as_millis() >= slow_ms {
        eprintln!(
            "[{name}] SLOW input ({} ms, {} bytes, hex): {}",
            started.elapsed().as_millis(),
            data.len(),
            hex(data)
        );
    }
    if let Err(e) = result {
        let msg = e
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| e.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "<non-string panic payload>".to_string());
        panics
            .entry(msg)
            .and_modify(|b| b.count += 1)
            .or_insert_with(|| PanicBucket {
                count: 1,
                example: data.to_vec(),
            });
    }
}

fn print_panics(label: &str, name: &str, ran: usize, panics: &Panics) {
    println!(
        "[{name}] {label}: ran {ran} inputs, {} distinct panic message(s)",
        panics.len()
    );
    for (msg, bucket) in panics {
        println!("  x{}: {msg}", bucket.count);
        println!(
            "    example input ({} bytes, hex): {}",
            bucket.example.len(),
            hex(&bucket.example)
        );
    }
}

fn hex(data: &[u8]) -> String {
    data.iter().map(|b| format!("{b:02x}")).collect()
}

/// Returns `(corpus_panics, sweep_panics)` for one target. `corpus_dir_name`
/// is usually just `name` — it differs for `format_pdf_image_decode`
/// (issue #227), whose seeds live under `corpus/image_decode/` (issue
/// #208's naming, predating this target; the directory describes the
/// SEEDS' subject, not any one target's bin name) rather than
/// `corpus/format_pdf_image_decode/`.
fn run_target(
    name: &'static str,
    corpus_dir_name: &str,
    opts: &Options,
    run: impl Fn(&[u8]),
) -> (Panics, Panics) {
    let mut corpus_panics = Panics::new();
    let mut corpus_ran = 0usize;
    let corpus_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("corpus")
        .join(corpus_dir_name);
    if !opts.no_corpus
        && let Ok(entries) = std::fs::read_dir(&corpus_dir)
    {
        let mut paths: Vec<_> = entries.flatten().map(|e| e.path()).collect();
        paths.sort();
        for path in paths {
            if let Ok(bytes) = std::fs::read(&path) {
                if opts.log_inputs {
                    println!("[{name}] corpus file {}", path.display());
                }
                feed(
                    name,
                    &run,
                    &bytes,
                    None,
                    opts.log_inputs,
                    &mut corpus_ran,
                    &mut corpus_panics,
                );
            }
        }
    }
    print_panics("corpus", name, corpus_ran, &corpus_panics);

    let mut sweep_panics = Panics::new();
    let mut sweep_ran = 0usize;
    let mut rng = Xorshift64(0x9E3779B97F4A7C15 ^ (name.len() as u64 + 1));
    let to = opts.to.unwrap_or(opts.iterations);
    for i in 0..to {
        let len = (rng.next_u64() % 512) as usize + (i % 64);
        let data = rng.bytes(len);
        if i < opts.from {
            continue; // same stream as a full run, input i is input i
        }
        feed(
            name,
            &run,
            &data,
            Some(i),
            opts.log_inputs,
            &mut sweep_ran,
            &mut sweep_panics,
        );
    }
    print_panics("random sweep", name, sweep_ran, &sweep_panics);

    (corpus_panics, sweep_panics)
}

type TargetFn = fn(&[u8]);

struct Options {
    only: Option<String>,
    iterations: usize,
    strict: bool,
    from: usize,
    to: Option<usize>,
    no_corpus: bool,
    log_inputs: bool,
}

fn parse_args() -> Options {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut opts = Options {
        only: None,
        iterations: 500,
        strict: false,
        from: 0,
        to: None,
        no_corpus: false,
        log_inputs: false,
    };
    let number = |v: Option<&String>, flag: &str| -> usize {
        v.and_then(|s| s.parse().ok()).unwrap_or_else(|| {
            eprintln!("smoke: {flag} needs a number");
            std::process::exit(2);
        })
    };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--target" => {
                opts.only = args.get(i + 1).cloned();
                i += 1;
            }
            "--iterations" => {
                opts.iterations = number(args.get(i + 1), "--iterations");
                i += 1;
            }
            "--from" => {
                opts.from = number(args.get(i + 1), "--from");
                i += 1;
            }
            "--to" => {
                opts.to = Some(number(args.get(i + 1), "--to"));
                i += 1;
            }
            "--no-corpus" => opts.no_corpus = true,
            "--log-inputs" => opts.log_inputs = true,
            "--strict" => opts.strict = true,
            other => {
                eprintln!("smoke: unknown flag {other} (see the module docs)");
                std::process::exit(2);
            }
        }
        i += 1;
    }
    opts
}

/// Issue #177 acceptance — "smoke sweep reports coverage per variant".
/// Prints how many of each `Command` variant `command_gen`'s curated +
/// blind generators produced across the just-finished `rpc_command`
/// corpus + sweep phases, and separately calls out any variant
/// `classify_variant` marks `curated: true` that this run never hit
/// (a live signal the curated arm's odds need tuning, distinct from
/// the blind-only variants that are *expected* to be rare or absent).
fn print_variant_coverage() {
    let snap = engine_fuzz::command_gen::coverage_snapshot();
    println!(
        "[rpc_command] issue #177 coverage: {} distinct Command variant(s) generated this run",
        snap.len()
    );
    for (name, count) in &snap {
        println!("  {name}: {count}");
    }
    let curated_names: Vec<&str> = [
        "InsertText",
        "DeleteRange",
        "DeleteAtCaret",
        "ReplaceRange",
        "SplitParagraph",
        "ApplyFormatting",
        "SetParagraphAlign",
        "SetParagraphDirection",
        "ToggleList",
        "SetLineSpacing",
        "SetParagraphIndent",
        "InsertTable",
        "InsertRow",
        "DeleteRow",
        "MergeCells",
        "SetCellShading",
        "SetCellBorders",
        "InsertSectionBreak",
        "SetColumns",
        "EnterHeaderFooter",
        "ExitHeaderFooter",
        "SetZoom",
        "SetDeviceScale",
        "SetRenderDate",
        "InsertTextBox",
        "SetTableProperties",
        "MoveImage",
        "SetSelection",
        "ExtendSelection",
        "SelectAll",
        "MoveCaret",
        "Undo",
        "Redo",
        "InsertToc",
        "UpdateFields",
        "InsertField",
        "InsertFootnote",
        "InsertEndnote",
        "InsertImage",
        "SetImageWrap",
    ]
    .to_vec();
    let missed: Vec<&&str> = curated_names
        .iter()
        .filter(|name| !snap.contains_key(*name))
        .collect();
    if !missed.is_empty() {
        println!(
            "  (curated but not generated this run — odds may be too low, or {} \
             iteration(s) just weren't enough: {missed:?})",
            snap.values().sum::<usize>()
        );
    }
}

fn main() {
    let opts = parse_args();
    cap_address_space();
    let limit_mb = env_usize("SMOKE_RSS_LIMIT_MB", 2048);
    LIMIT.store(limit_mb << 20, Ordering::SeqCst);
    if limit_mb != 0 {
        println!("smoke: live-heap ceiling = {limit_mb} MiB per input (SMOKE_RSS_LIMIT_MB)");
    }
    let timeout_secs = env_usize("SMOKE_TIMEOUT_SECS", 300) as u64;
    spawn_watchdog(timeout_secs);
    if timeout_secs != 0 {
        println!("smoke: per-input timeout = {timeout_secs} s (SMOKE_TIMEOUT_SECS)");
    }
    let targets: [(&str, &str, TargetFn); 6] = [
        ("docx_reader", "docx_reader", engine_fuzz::run_docx_reader),
        (
            "docx_roundtrip",
            "docx_roundtrip",
            engine_fuzz::run_docx_roundtrip,
        ),
        ("rpc_command", "rpc_command", engine_fuzz::run_rpc_command),
        (
            "layout_paginate",
            "layout_paginate",
            engine_fuzz::run_layout_paginate,
        ),
        (
            "snapshot_decode",
            "snapshot_decode",
            engine_fuzz::run_snapshot_decode,
        ),
        (
            "format_pdf_image_decode",
            "image_decode",
            engine_fuzz::run_format_pdf_image_decode,
        ),
    ];
    if let Some(only) = &opts.only
        && !targets.iter().any(|(name, _, _)| name == only)
    {
        eprintln!("smoke: unknown target {only}");
        std::process::exit(2);
    }

    let mut corpus_clean = true;
    let mut sweep_clean = true;
    for (name, corpus_dir_name, run) in targets {
        if opts.only.as_deref().is_some_and(|only| only != name) {
            continue;
        }
        // Issue #177 — the coverage counters are a `rpc_command`-only
        // concern (`command_gen::gen_command_sequence` is the only
        // caller of `record_coverage`); reset right before this
        // target's corpus + sweep phases so the snapshot below covers
        // exactly this run, not any earlier `--target rpc_command`
        // invocation in the same process.
        if name == "rpc_command" {
            engine_fuzz::command_gen::reset_coverage();
        }
        let (corpus_panics, sweep_panics) = run_target(name, corpus_dir_name, &opts, run);
        corpus_clean &= corpus_panics.is_empty();
        sweep_clean &= sweep_panics.is_empty();
        if name == "rpc_command" {
            print_variant_coverage();
        }
    }

    println!();
    println!(
        "smoke: peak RSS {} MiB (VmHWM), peak live heap {} MiB",
        peak_rss_bytes() >> 20,
        PEAK.load(Ordering::Relaxed) >> 20
    );
    if !corpus_clean {
        eprintln!("smoke: FAIL — the committed seed corpus panicked (see above)");
        std::process::exit(1);
    }
    println!("smoke: seed corpus clean on every target");
    if !sweep_clean {
        if opts.strict {
            eprintln!("smoke: FAIL — --strict and the random sweep panicked (see above)");
            std::process::exit(1);
        }
        println!(
            "smoke: the random sweep found panics above — every one is a real finding to triage"
        );
    } else {
        println!("smoke: random sweep also clean");
    }
}
