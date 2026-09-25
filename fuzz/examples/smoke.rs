//! Stable-Rust smoke driver for the D5.5 (issue #90) fuzz generators.
//!
//! Nightly Rust is not installed in this environment and must not be
//! installed (binding rule 1) — `cargo +nightly fuzz run` is the real
//! libFuzzer flow, exercised only in CI (`.github/workflows/fuzz-nightly.yml`).
//! This binary proves the five targets' generators + `run_*` bodies work
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
//! Flags (all optional):
//! - `--target <name>`     run only that target (`rpc_command`, …).
//! - `--iterations <n>`    random-sweep inputs per target (default 500).
//! - `--strict`            ALSO fail (exit 1) when the random sweep panics —
//!   the post-#114–#118 contract for `rpc_command` is a clean sweep, so CI
//!   can hold the line instead of just reporting.

use std::collections::BTreeMap;
use std::panic::{self, AssertUnwindSafe};
use std::path::Path;

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

fn feed(name: &str, run: &impl Fn(&[u8]), data: &[u8], ran: &mut usize, panics: &mut Panics) {
    *ran += 1;
    let prev_hook = panic::take_hook();
    panic::set_hook(Box::new(|_| {})); // keep stdout clean; we print our own summary
    let result = panic::catch_unwind(AssertUnwindSafe(|| run(data)));
    panic::set_hook(prev_hook);
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
    let _ = name; // kept for symmetry / future per-target tracing
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
    sweep_iterations: usize,
    run: impl Fn(&[u8]),
) -> (Panics, Panics) {
    let mut corpus_panics = Panics::new();
    let mut corpus_ran = 0usize;
    let corpus_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("corpus")
        .join(corpus_dir_name);
    if let Ok(entries) = std::fs::read_dir(&corpus_dir) {
        for entry in entries.flatten() {
            if let Ok(bytes) = std::fs::read(entry.path()) {
                feed(name, &run, &bytes, &mut corpus_ran, &mut corpus_panics);
            }
        }
    }
    print_panics("corpus", name, corpus_ran, &corpus_panics);

    let mut sweep_panics = Panics::new();
    let mut sweep_ran = 0usize;
    let mut rng = Xorshift64(0x9E3779B97F4A7C15 ^ (name.len() as u64 + 1));
    for i in 0..sweep_iterations {
        let len = (rng.next_u64() % 512) as usize + (i % 64);
        let data = rng.bytes(len);
        feed(name, &run, &data, &mut sweep_ran, &mut sweep_panics);
    }
    print_panics("random sweep", name, sweep_ran, &sweep_panics);

    (corpus_panics, sweep_panics)
}

type TargetFn = fn(&[u8]);

struct Options {
    only: Option<String>,
    iterations: usize,
    strict: bool,
}

fn parse_args() -> Options {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut opts = Options {
        only: None,
        iterations: 500,
        strict: false,
    };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--target" => {
                opts.only = args.get(i + 1).cloned();
                i += 1;
            }
            "--iterations" => {
                opts.iterations =
                    args.get(i + 1)
                        .and_then(|s| s.parse().ok())
                        .unwrap_or_else(|| {
                            eprintln!("smoke: --iterations needs a number");
                            std::process::exit(2);
                        });
                i += 1;
            }
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
    let targets: [(&str, &str, TargetFn); 5] = [
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
        let (corpus_panics, sweep_panics) = run_target(name, corpus_dir_name, opts.iterations, run);
        corpus_clean &= corpus_panics.is_empty();
        sweep_clean &= sweep_panics.is_empty();
        if name == "rpc_command" {
            print_variant_coverage();
        }
    }

    println!();
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
