//! Structure-aware generators + per-target run functions (D5.5, issue #90).
//!
//! Every `fuzz_targets/*.rs` file is a thin `fuzz_target!` wrapper around
//! one `run_*` function here. Factoring the actual logic into this library
//! crate means `examples/smoke.rs` — a plain, stable-Rust binary with no
//! libFuzzer / nightly dependency — can drive the exact same code with a
//! deterministic PRNG and the committed `corpus/` seeds (binding rule 6:
//! nightly is not installed on this machine, so this is how the generators
//! are proven to work without `cargo +nightly fuzz run`).

pub mod command_gen;
pub mod docx_gen;
pub mod layout_gen;
pub mod snapshot_gen;
pub mod util;

use arbitrary::Unstructured;

/// `docx_reader` (D5.5) — build a schema-shaped WML document inside a
/// minimal OPC package from `data` (never raw bytes fed straight to the
/// parser — see `docx_gen`), then feed the assembled `.docx` bytes to
/// `format_docx::read_docx`. The reader must never panic; returning `Err`
/// on a deliberately-hostile package is the correct, expected outcome.
pub fn run_docx_reader(data: &[u8]) {
    /* Issue #348 — an input that IS a ZIP package (the committed `.docx`
    seeds, a hostile package from `corpus/docx_reader/hostile_*`, a mutation
    of either) is also read as-is, under tight limits so a compression
    bomb seed reaches the typed refusal in milliseconds. */
    if data.starts_with(b"PK\x03\x04") {
        let _ = read_raw_package(data);
    }
    let mut u = Unstructured::new(data);
    let Some(bytes) = docx_gen::build_docx(&mut u) else {
        return;
    };
    let _ = format_docx::read_docx(&bytes);
}

/// Issue #348 — the resource bounds the fuzz targets read raw packages
/// under: the stock XML shape caps, byte budgets small enough that a bomb
/// is refused without inflating megabytes per iteration.
pub const FUZZ_PACKAGE_LIMITS: format_docx::PackageLimits = format_docx::PackageLimits {
    max_part_bytes: 4 * 1024 * 1024,
    max_total_bytes: 8 * 1024 * 1024,
    ..format_docx::PackageLimits::DEFAULT
};

/// Issue #348 — `data` read directly as a `.docx` package under
/// [`FUZZ_PACKAGE_LIMITS`].
pub fn read_raw_package(data: &[u8]) -> Result<format_docx::DocxArchive, format_docx::DocxError> {
    format_docx::read_docx_with_limits(
        data,
        engine::DefaultPageSize::A4,
        true,
        &FUZZ_PACKAGE_LIMITS,
    )
}

/// `docx_roundtrip` (D5.5, new target) — read -> write -> read must be
/// stable, never panic, and (issue #358) preserve the document's text. Runs the cycle twice: the writer's own
/// passthrough-vs-resynthesize split (`DocxArchive.other_entries` verbatim,
/// `word/document.xml` freshly serialized) means a bug that only manifests
/// on the SECOND save (e.g. a `dirty` flag that doesn't reset) would slip
/// past a single read/write/read.
pub fn run_docx_roundtrip(data: &[u8]) {
    let mut u = Unstructured::new(data);
    let Some(bytes) = docx_gen::build_docx(&mut u) else {
        return;
    };
    let Ok(archive_a) = format_docx::read_docx(&bytes) else {
        return;
    };
    let doc_a = archive_a.document.clone();
    let Ok(written_a) = format_docx::write_docx(&archive_a, &doc_a) else {
        // A parseable archive that the writer can't re-serialize is a real
        // bug, but `write_docx` returning `Err` (not panicking) is the
        // contract — nothing further to exercise on this input.
        return;
    };
    let archive_b = match format_docx::read_docx(&written_a) {
        Ok(a) => a,
        Err(e) => {
            if trace_enabled() {
                eprintln!(
                    "[docx_roundtrip] re-read refused: {e}\n  source document.xml: {}\n  saved document.xml: {}",
                    String::from_utf8_lossy(&document_xml_of(&bytes)),
                    String::from_utf8_lossy(&document_xml_of(&written_a)),
                );
            }
            panic!(
                "docx_roundtrip: read_docx parsed the ORIGINAL package but \
                 rejected write_docx's own output — the writer produced an \
                 archive its own reader can't parse back"
            );
        }
    };
    /* Issue #434 — the writer's output is well-formed: the reader repairs
    a malformed source up front (`MalformedPart { repaired: true }`), so
    the re-read must need no repair. Only a part the FIRST read already
    found beyond repair (and kept as it was) may be reported again. */
    if let Some(part) = needless_repair(&archive_a.warnings, &archive_b.warnings) {
        if trace_enabled() {
            eprintln!(
                "[docx_roundtrip] the save needed a repair ({part}):\n  warnings: {:?}\n  saved document.xml: {}",
                archive_b.warnings,
                String::from_utf8_lossy(&document_xml_of(&written_a)),
            );
        }
        panic!("docx_roundtrip: write_docx produced a part that is not well-formed");
    }
    /* Issue #358 — read => write => read preserves the text (every
    generated and spliced package that parsed at all). No exception any
    more (issue #435): a prefix no `xmlns:` declares (the generator's root
    omits the drawing / mc bindings one time in eight) is bound on the
    root by the reader's up-front repair, so the first read already sees
    what the writer's save makes every later read see. */
    if trace_enabled() && doc_a.to_plain_text() != archive_b.document.to_plain_text() {
        eprintln!(
            "[docx_roundtrip] text drift:\n  before: {:?}\n  after:  {:?}\n  source document.xml: {}\n  saved document.xml: {}",
            doc_a.to_plain_text(),
            archive_b.document.to_plain_text(),
            String::from_utf8_lossy(&document_xml_of(&bytes)),
            String::from_utf8_lossy(&document_xml_of(&written_a)),
        );
    }
    assert!(
        doc_a.to_plain_text() == archive_b.document.to_plain_text(),
        "docx_roundtrip: a zero-edit save changed the document text"
    );
    let doc_b = archive_b.document.clone();
    let Ok(written_b) = format_docx::write_docx(&archive_b, &doc_b) else {
        panic!("docx_roundtrip: second write_docx failed after a successful first round-trip");
    };
    let _ = format_docx::read_docx(&written_b);
}

/// Issue #434 — the first part the re-read of a save reports as malformed
/// (`DocxWarning::MalformedPart`) that the first read did not already find
/// beyond repair: a part the writer produced not well-formed.
fn needless_repair(
    first: &[format_docx::DocxWarning],
    second: &[format_docx::DocxWarning],
) -> Option<String> {
    use format_docx::DocxWarning::MalformedPart;
    second.iter().find_map(|w| match w {
        MalformedPart { part, .. }
            if !first.iter().any(
                |f| matches!(f, MalformedPart { part: p, repaired: false, .. } if p == part),
            ) =>
        {
            Some(part.clone())
        }
        _ => None,
    })
}

/// `word/document.xml` of a package (empty when unreadable) — trace output.
fn document_xml_of(docx: &[u8]) -> Vec<u8> {
    use std::io::Read;
    let Ok(mut z) = zip::ZipArchive::new(std::io::Cursor::new(docx)) else {
        return Vec::new();
    };
    let Ok(mut f) = z.by_name("word/document.xml") else {
        return Vec::new();
    };
    let mut v = Vec::new();
    let _ = f.read_to_end(&mut v);
    v
}

/// Cap on how many commands one fuzz input drives — bounds wall-clock per
/// run regardless of how much entropy `data` happens to carry.
const MAX_COMMAND_SEQUENCE: usize = 64;

/// `rpc_command` (D5.5, scaled up) — a structure-aware sequence of
/// `Command`s (insert/delete/format/table/section/story ops, per issue #90
/// scope) against a seeded document, driven end to end through the real
/// `Engine::apply` dispatcher (via `Engine::apply_sync`, the native-fuzzing
/// entry point — see `crates/engine-wasm`'s `fuzz-native` feature) including
/// the auto-repaint -> layout pipeline, then the native glyph rasterizer.
/// Invariants asserted after every command: no panic (the fuzz harness
/// itself), the undo stack never exceeds its 100-snapshot bound, the
/// live selection always resolves inside the current document, and
/// (issue #341) a command answered with `Event::Error` changed nothing
/// (`Engine::state_fingerprint_for_fuzzing`).
pub fn run_rpc_command(data: &[u8]) {
    let mut u = Unstructured::new(data);
    let seed_text = command_gen::gen_seed_text(&mut u);
    let mut engine = engine_wasm::Engine::new_headless(engine::DocumentTree::from_text(&seed_text));
    let commands = command_gen::gen_command_sequence(&mut u, MAX_COMMAND_SEQUENCE);
    let trace = trace_enabled();
    for (step, cmd) in commands.into_iter().enumerate() {
        if trace {
            eprintln!(
                "[rpc_command] step {step}: {}",
                format!("{cmd:?}").chars().take(300).collect::<String>()
            );
        }
        /* Kept only for the failure message: the variant name (not the
        payload, which would defeat the smoke driver's per-message
        dedup) tells triage WHICH command broke an invariant. Formatted
        lazily — `assert!`'s message arguments run only on failure. */
        let keep = cmd.clone();
        /* Issue #407 — a command carrying a NaN / ±inf number anywhere
        must be refused by the dispatcher's finite() guard. */
        let non_finite = cmd.first_non_finite();
        /* Issue #341 — "error => no mutation": the document, selection,
        active story and undo depth before the command. */
        let before = engine.state_fingerprint_for_fuzzing();
        let started = trace.then(std::time::Instant::now);
        let evt = engine.apply_sync(cmd);
        if let Some(t) = started {
            eprintln!(
                "[rpc_command]   -> {} in {} ms",
                format!("{evt:?}").chars().take(80).collect::<String>(),
                t.elapsed().as_millis()
            );
        }
        if let Some(bad) = &non_finite {
            assert!(
                matches!(
                    &evt,
                    bridge::Event::Error {
                        kind: Some(bridge::ErrorKind::InvalidArgument),
                        ..
                    }
                ),
                "{} carried a non-finite {} but was not refused: {}",
                variant_name(&keep),
                bad.field,
                format!("{evt:?}").chars().take(160).collect::<String>()
            );
        }
        if let bridge::Event::Error { message, .. } = &evt
            && !is_post_commit_report_error(message)
        {
            let after = engine.state_fingerprint_for_fuzzing();
            let changed: Vec<&str> = engine_wasm::Engine::FINGERPRINT_PARTS
                .iter()
                .zip(before.iter().zip(after.iter()))
                .filter(|(_, (b, a))| b != a)
                .map(|(n, _)| *n)
                .collect();
            assert!(
                changed.is_empty(),
                "{} answered Event::Error but changed: {} [{}]",
                variant_name(&keep),
                changed.join(", "),
                format!("{evt:?}").chars().take(160).collect::<String>()
            );
        }
        assert!(
            engine.undo_depth() <= 100,
            "undo depth exceeded its 100-snapshot bound after {}",
            variant_name(&keep)
        );
        // Issue #117 — `SetSelection` / `ExtendSelection` used to store the
        // wire range verbatim (the #90 finding that made this a known
        // fail); every selection path now clamps through the engine's
        // `clamp_pos`, and the check is story-aware, so this must hold
        // after EVERY command. A fail here is a real regression.
        assert!(
            engine.selection_is_valid(),
            "live selection escaped the document bounds after {}",
            variant_name(&keep)
        );
    }
    // Layout + the native (browser-free) rasterizer, exercised end to end
    // over whatever the command sequence left the document as.
    if engine.ensure_layout_for_fuzzing().is_ok() {
        let _ = engine.rasterize_last_layout_for_fuzzing();
    }
}

/// Issue #422 — `ENGINE_FUZZ_TRACE=1` prints every generated command
/// before it runs (`run_rpc_command`), so a slow or memory-hungry input
/// found by `examples/smoke.rs --log-inputs` can be pinned to one command.
fn trace_enabled() -> bool {
    static TRACE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *TRACE.get_or_init(|| std::env::var_os("ENGINE_FUZZ_TRACE").is_some_and(|v| v != "0"))
}

/// Issue #341 — the one documented exception to "`Event::Error` => no
/// mutation". Some commands COMMIT their edit (or move the selection, which
/// is the whole command) and only then build the reply: the selection
/// geometry (`selection_changed`) or the auto-repaint. On a headless engine
/// that has not had its first `RenderPage` there is no selection, layout
/// config or font yet, so that reply step answers an `Event::Error` for an
/// edit that already landed (the `LoadDocx` doc comment states the same
/// contract: "the document itself is loaded"). The shell never reaches this
/// state — it paints before it edits — so these messages are reply-stage
/// degradations, not lost atomicity. Anything else answering `Error` must
/// have left the document, selection, story, name and undo depth alone.
fn is_post_commit_report_error(message: &str) -> bool {
    message.starts_with("selection_changed: no active selection")
        || message.starts_with("build_pages: no layout config cached")
        || (message.starts_with("font `") && message.ends_with("not loaded"))
}

/// Commands a post-restore "one apply round" drives.
const MAX_RESTORE_ROUND: usize = 4;

/// Restore `snapshot` (+ detached `package`) into a fresh engine and prove
/// the result is usable: `Ok` or a typed error, never a panic; on `Ok`
/// the engine invariants hold, one `apply` round (commands from `tail`)
/// keeps them, and the same bytes sent through `Command::Recover` answer
/// `Event::Recovered`. Returns the first restore's outcome.
fn restore_and_check(
    snapshot: &[u8],
    package: Option<&[u8]>,
    tail: &[u8],
) -> Result<(u8, bool), String> {
    let fresh = || engine_wasm::Engine::new_headless(engine::DocumentTree::from_text("fresh"));
    let commands = |tail: &[u8]| {
        command_gen::gen_command_sequence(&mut Unstructured::new(tail), MAX_RESTORE_ROUND)
    };
    let mut engine = fresh();
    let outcome = engine.restore_for_fuzzing(snapshot, package);
    if outcome.is_ok() {
        if let Err(why) = engine.check_invariants_for_fuzzing() {
            panic!("snapshot_decode: right after a successful restore: {why}");
        }
        for cmd in commands(tail) {
            let name = variant_name(&cmd);
            let _ = engine.apply_sync(cmd);
            if let Err(why) = engine.check_invariants_for_fuzzing() {
                panic!("snapshot_decode: after restore then {name}: {why}");
            }
        }
        if engine.ensure_layout_for_fuzzing().is_ok() {
            let _ = engine.rasterize_last_layout_for_fuzzing();
        }
    }
    // The production entry: Recover = restore + replayed tail.
    let mut recovering = fresh();
    let evt = recovering.apply_sync(bridge::Command::Recover {
        snapshot: snapshot.to_vec(),
        log_tail: commands(tail),
        renderer_downgrade: None,
        package: package.map(<[u8]>::to_vec),
    });
    assert!(
        matches!(evt, bridge::Event::Recovered { .. }),
        "Command::Recover must answer Event::Recovered for any snapshot bytes"
    );
    if let Err(why) = recovering.check_invariants_for_fuzzing() {
        panic!("snapshot_decode: after Command::Recover: {why}");
    }
    outcome
}

/// `snapshot_decode` (issue #341) — `engine::snapshot` envelopes, valid
/// and mutated, through `Engine::restore` / `Command::Recover`.
///
/// Three input shapes: a committed seed container
/// (`snapshot_gen::SEED_MAGIC`: snapshot + detached package), a raw
/// `NGES` envelope (libFuzzer's own byte mutations of one), or — the
/// structure-aware path — arbitrary bytes that build a session, snapshot it
/// with the engine, and mutate the MessagePack tree
/// (`snapshot_gen::mutate_snapshot`). An unmutated snapshot must restore.
pub fn run_snapshot_decode(data: &[u8]) {
    let tail = &data[data.len().saturating_sub(64)..];
    if let Some((snapshot, package)) = snapshot_gen::parse_seed(data) {
        let _ = restore_and_check(snapshot, package, tail);
        return;
    }
    if data.starts_with(b"NGES") {
        let _ = restore_and_check(data, None, tail);
        return;
    }
    let mut u = Unstructured::new(data);
    if u.is_empty() {
        return;
    }
    let mut session = snapshot_gen::base_engine(&mut u);
    let detach = u.ratio(1, 2).unwrap_or(false);
    let Some((bytes, _, package)) = snapshot_gen::capture(&mut session, detach) else {
        return;
    };
    let mutate = u.ratio(5, 6).unwrap_or(true);
    let snapshot = if mutate {
        snapshot_gen::mutate_snapshot(&mut u, &bytes, package.as_deref())
    } else {
        bytes
    };
    // The detached package itself: intact, absent, or one byte off.
    let mut package = package;
    let mut tampered = false;
    match u.int_in_range(0u8..=7).unwrap_or(0) {
        0 => {
            tampered = package.is_some();
            package = None;
        }
        1 => {
            if let Some(p) = package.as_mut()
                && !p.is_empty()
            {
                let i = u.choose_index(p.len()).unwrap_or(0);
                p[i] ^= 0x55;
                tampered = true;
            }
        }
        _ => {}
    }
    let tail = u.take_rest();
    let outcome = restore_and_check(&snapshot, package.as_deref(), tail);
    if !mutate {
        let (_, package_lost) = outcome
            .unwrap_or_else(|e| panic!("an unmutated engine snapshot failed to restore: {e}"));
        assert!(
            tampered || !package_lost,
            "an unmutated snapshot with its intact package reported the package lost"
        );
    }
}

/// The `Command` variant's name (`InsertTable`, `SetSelection`, …) — the
/// leading identifier of its `Debug` rendering.
fn variant_name(cmd: &bridge::Command) -> String {
    let dbg = format!("{cmd:?}");
    dbg.split(|c: char| !c.is_ascii_alphanumeric())
        .next()
        .unwrap_or("?")
        .to_string()
}

/// Page-count bound standing in for a wall-clock watchdog — see the doc
/// comment on `Engine::layout_page_count_for_fuzzing` for why an in-process
/// thread-based watchdog was not used. A bounded random paragraph/table
/// tree laid out on A4 should never legitimately need this many pages;
/// blowing past it means the paginator is not making forward progress.
const MAX_PAGES: usize = 2000;

/// `layout_paginate` (D5.5, new target) — random paragraph/table/section
/// trees straight into the paginator (`Engine::ensure_layout_for_fuzzing`,
/// which calls `build_pages` -> `Paginator` -> `layout_paragraph` exactly
/// like a real `RenderPage` would), independent of the `Command`-sequence
/// surface `rpc_command` covers. Bypasses OOXML entirely — this is about
/// the layout engine's robustness against extreme/malformed structural
/// trees (zero-size cells, mismatched grid/row column counts, degenerate
/// page geometry), not about `.docx` parsing or the RPC surface.
///
/// Issue #318 — a [`layout_gen::NESTING_MAGIC`]-prefixed input is a tower
/// of nested tables up to [`layout_gen::MAX_FUZZ_NESTING`] deep instead
/// (`corpus/layout_paginate/seed_nested_200`): the cost of nested-table
/// layout used to grow like `F(2·depth)`, so a deep tower hung the target
/// (libFuzzer `-timeout`) instead of degrading with `NestingCapped`.
pub fn run_layout_paginate(data: &[u8]) {
    let doc = layout_gen::gen_layout_document(data);
    let mut engine = engine_wasm::Engine::new_headless(doc);
    if engine.ensure_layout_for_fuzzing().is_err() {
        // A missing font / layout config is an engine setup error, not a
        // paginator bug — `new_headless` always seeds both, so in practice
        // this arm is unreached, but treat it as a clean bail-out rather
        // than asserting (no layout ran, so there is nothing to bound).
        return;
    }
    assert!(
        engine.layout_page_count_for_fuzzing() <= MAX_PAGES,
        "paginator emitted more than {MAX_PAGES} pages for a bounded random \
         input — treat as a non-terminating / runaway layout bug"
    );
    let _ = engine.rasterize_last_layout_for_fuzzing();
}

/// `format_pdf_image_decode` (D5.5, issue #227) — feed raw bytes STRAIGHT
/// to `format_pdf::prepare_image` for every enabled decoder (PNG/JPEG/
/// GIF/WebP/BMP/TIFF; `format_pdf::sniff`'s magic-byte + content-type
/// dispatch decides which one runs). Deliberately does NOT consume any
/// prefix bytes as separate "control" fields the way `command_gen`'s
/// generators do — the committed corpus seeds under
/// `fuzz/corpus/image_decode/` are raw, real (adversarial) image bytes
/// with a format's magic sequence at byte 0, same as the media a `.docx`
/// relationship actually carries; consuming a prefix would shift that
/// magic out of place and desync every committed seed from the format it
/// was built to exercise. `content_type` / `alpha` / `allow_cmyk` still
/// vary — from the data's own bytes, without shifting it — so the
/// declared-content-type fallback path (WMF/SVG have no magic) and both
/// `AlphaMode`s get real coverage too.
///
/// Asserts the typed-skip contract (module doc of `format_pdf`'s
/// `image.rs`, issue #208's allocation-bounds audit): never a panic, no
/// allocation described by anything past `MAX_IMAGE_PIXELS`, and a
/// SUCCESSFUL decode is XObject-ready — real dimensions, a sample buffer
/// sized exactly `width * height * channel_count` (channel count from
/// `ImageColor`) for `ImageEncoding::Raw`, and non-empty passthrough
/// bytes for `ImageEncoding::Dct`; any alpha mask is exactly
/// `width * height` bytes.
pub fn run_format_pdf_image_decode(data: &[u8]) {
    const CONTENT_TYPES: &[&str] = &[
        "image/png",
        "image/jpeg",
        "image/gif",
        "image/webp",
        "image/bmp",
        "image/x-bmp",
        "image/tiff",
        "image/x-wmf",
        "image/x-emf",
        "image/svg+xml",
        "application/octet-stream",
        "",
    ];
    // `sniff` checks the real magic bytes FIRST for every format that has
    // one — a "wrong" declared type here never hides a real PNG/JPEG/…
    // from its own decoder, it only feeds the "declared X, actually
    // garbage" and WMF/SVG-by-content-type-alone paths, which have no
    // magic of their own to sniff.
    let content_type =
        CONTENT_TYPES[data.first().copied().unwrap_or(0) as usize % CONTENT_TYPES.len()];
    let alpha = if data.len().is_multiple_of(2) {
        format_pdf::AlphaMode::SoftMask
    } else {
        format_pdf::AlphaMode::FlattenOnWhite
    };
    let allow_cmyk = data.first().is_some_and(|b| b & 0x80 != 0);
    match format_pdf::prepare_image(data, content_type, alpha, allow_cmyk) {
        Ok(img) => {
            assert!(
                img.width > 0 && img.height > 0,
                "a successfully decoded image must have real dimensions"
            );
            assert!(
                u64::from(img.width) * u64::from(img.height) <= format_pdf::MAX_IMAGE_PIXELS,
                "decoded image ({}, {}) exceeds MAX_IMAGE_PIXELS — check_dimensions \
                 must run before any decode this size could complete",
                img.width,
                img.height
            );
            let pixels = img.width as usize * img.height as usize;
            match img.encoding {
                format_pdf::ImageEncoding::Dct => assert!(
                    !img.data.is_empty(),
                    "a DCT-passthrough stream must carry the source JPEG bytes"
                ),
                format_pdf::ImageEncoding::Raw => {
                    let channels = match img.color {
                        format_pdf::ImageColor::Gray => 1,
                        format_pdf::ImageColor::Rgb => 3,
                        format_pdf::ImageColor::Cmyk => 4,
                    };
                    assert_eq!(
                        img.data.len(),
                        pixels * channels,
                        "raw sample buffer must be exactly width * height * channels \
                         for a {:?} image",
                        img.color
                    );
                }
            }
            if let Some(alpha) = &img.alpha {
                assert_eq!(
                    alpha.len(),
                    pixels,
                    "an /SMask alpha buffer must be exactly one byte per pixel"
                );
            }
        }
        Err(_) => {
            // A typed `ImageSkipReason` is the correct, expected outcome
            // for hostile or unsupported bytes — nothing further to
            // assert; the exporter turns this into a `PdfWarning` and
            // leaves the image's rect blank (never a panic).
        }
    }
}

/// Issue #348 — the hostile `.docx` seeds of `corpus/docx_reader/`: `(file
/// name, package bytes, the limit [`read_raw_package`] must refuse it with
/// — `None` when it must read)`. Built by `format_docx::test_fixtures`, so
/// no blob is hand-maintained; `examples/regen-seeds` writes them.
pub fn hostile_docx_seeds() -> Vec<(&'static str, Vec<u8>, Option<format_docx::PackageLimit>)> {
    use format_docx::PackageLimit;
    use format_docx::test_fixtures as fx;
    vec![
        (
            "hostile_declared_4gib_part.docx",
            fx::lying_size_docx(4 * 1024 * 1024 * 1024),
            None,
        ),
        (
            "hostile_declared_u64_max_part.docx",
            fx::lying_size_docx(u64::MAX - 1),
            None,
        ),
        (
            "hostile_bomb_16mib_zeros.docx",
            fx::compressible_bomb_docx(16 * 1024 * 1024),
            Some(PackageLimit::PartBytes),
        ),
        (
            "hostile_sdt_5000_nested.docx",
            fx::nested_sdt_docx(5000),
            Some(PackageLimit::XmlDepth),
        ),
        (
            "table_nested_60_deep.docx",
            fx::nested_tables_docx(60),
            None,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// libFuzzer always executes the zero-length input first, and under
    /// `-fork` every worker attributes an exit-time failure to it (that is
    /// how the nightly lane produced the empty reproducers of issues 323
    /// and 324). Every target body must take it without panicking. The
    /// leak that actually tripped LeakSanitizer is only observable under
    /// the nightly sanitizer runtime; its stable guards are
    /// `LoadedFont::parse`'s unit test and the process-wide font shared by
    /// `Engine::new_headless`.
    #[test]
    fn every_target_body_accepts_the_empty_input() {
        run_docx_reader(&[]);
        run_docx_roundtrip(&[]);
        run_rpc_command(&[]);
        run_layout_paginate(&[]);
        run_snapshot_decode(&[]);
        run_format_pdf_image_decode(&[]);
    }

    /// Issue #358 — `dictionaries/docx.dict` parses as a libFuzzer
    /// dictionary (`[name=]"value"` per line, `\\` / `\"` / `\xNN`
    /// escapes, `#` comments) and every word fits libFuzzer's 64-byte
    /// limit, so a typo cannot silently weaken the nightly `-dict=` legs.
    #[test]
    fn docx_dictionary_is_well_formed() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("dictionaries/docx.dict");
        let dict = std::fs::read_to_string(&path).expect("fuzz/dictionaries/docx.dict");
        let mut words = std::collections::HashSet::new();
        for (n, line) in dict.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let q = line
                .find('"')
                .unwrap_or_else(|| panic!("line {}: no quoted value", n + 1));
            let name = &line[..q];
            assert!(
                name.is_empty()
                    || name.strip_suffix('=').is_some_and(|k| !k.is_empty()
                        && k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')),
                "line {}: bad keyword {name:?}",
                n + 1
            );
            let body = &line[q..];
            assert!(
                body.len() >= 2 && body.ends_with('"'),
                "line {}: unterminated",
                n + 1
            );
            let inner = &body.as_bytes()[1..body.len() - 1];
            let mut word = Vec::new();
            let mut i = 0;
            while i < inner.len() {
                match inner[i] {
                    b'\\' => match inner.get(i + 1) {
                        Some(&c @ (b'\\' | b'"')) => {
                            word.push(c);
                            i += 2;
                        }
                        Some(b'x') => {
                            let hex = inner
                                .get(i + 2..i + 4)
                                .and_then(|h| std::str::from_utf8(h).ok())
                                .and_then(|h| u8::from_str_radix(h, 16).ok());
                            word.push(hex.unwrap_or_else(|| panic!("line {}: bad \\x", n + 1)));
                            i += 4;
                        }
                        _ => panic!("line {}: bad escape", n + 1),
                    },
                    b'"' => panic!("line {}: unescaped quote", n + 1),
                    c => {
                        word.push(c);
                        i += 1;
                    }
                }
            }
            assert!(
                !word.is_empty() && word.len() <= 64,
                "line {}: {} bytes",
                n + 1,
                word.len()
            );
            assert!(words.insert(word), "line {}: duplicate word", n + 1);
        }
        assert!(words.len() > 150, "only {} words", words.len());
        assert!(words.contains(b"<w:fldChar w:fldCharType=\"begin\"/>".as_slice()));
    }

    /// Issues #439 / #434 — every committed `docx_roundtrip` reproducer
    /// (`corpus/docx_roundtrip/repro_*`, raw fuzz inputs the generator
    /// turns into malformed packages) holds the read => write => read
    /// invariant. The generator-independent spellings of the same shapes
    /// are `format_docx`'s `reader_well_formed_tests`.
    #[test]
    fn committed_docx_roundtrip_reproducers_hold() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus/docx_roundtrip");
        let mut seen = 0;
        for entry in std::fs::read_dir(&dir).expect("corpus/docx_roundtrip") {
            let path = entry.expect("entry").path();
            if path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("repro_"))
            {
                run_docx_roundtrip(&std::fs::read(&path).expect("seed"));
                seen += 1;
            }
        }
        assert!(seen >= 1, "the #439 reproducer is committed");
    }

    /// Issue #422 — the committed reproducer scenario runs to completion
    /// (one solid stroke per patterned underline, not millions of fills).
    #[test]
    fn the_422_reproducer_seed_completes() {
        run_rpc_command(&command_gen::Scenario::PatternedUnderlineGiantImage.seed_bytes());
    }

    /// Issue #341 — the committed `snapshot_decode` seeds are exactly what
    /// [`snapshot_gen::snapshot_seeds`] builds (re-run `examples/regen-seeds`
    /// after changing the engine's snapshot shape), and every one reaches
    /// `restore` without a panic: the valid ones restore, the hostile ones
    /// are refused with a typed error.
    #[test]
    fn snapshot_seeds_are_committed() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus/snapshot_decode");
        let seeds = snapshot_gen::snapshot_seeds();
        assert!(seeds.len() >= 8, "expected the full seed set");
        for (name, bytes) in seeds {
            let committed = std::fs::read(dir.join(name))
                .unwrap_or_else(|e| panic!("{name}: {e} — run examples/regen-seeds"));
            assert!(
                committed == bytes,
                "{name} is stale — run examples/regen-seeds"
            );
            run_snapshot_decode(&bytes);
            let (snapshot, package) = snapshot_gen::parse_seed(&bytes).expect("seed container");
            let mut e = engine_wasm::Engine::new_headless(engine::DocumentTree::new());
            let got = e.restore_for_fuzzing(snapshot, package);
            let hostile = ["bad_magic", "truncated", "unsupported_version"]
                .iter()
                .any(|h| name.contains(h));
            assert_eq!(got.is_err(), hostile, "{name}: {got:?}");
        }
    }

    /// Issue #341 — the v1 seed really is a format-1 envelope naming an FNV
    /// key, and the v2 one a `sha256-` key; both restore their package.
    #[test]
    fn snapshot_seeds_cover_both_package_key_formats() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus/snapshot_decode");
        let read = |n: &str| std::fs::read(dir.join(n)).expect(n);
        let key_of = |snap: &[u8]| {
            let Some(snapshot_gen::Mp::Map(m)) = snapshot_gen::Mp::parse(&snap[5..]) else {
                panic!("payload is a map")
            };
            m.into_iter()
                .find_map(|(k, v)| match (k, v) {
                    (snapshot_gen::Mp::Str(k), snapshot_gen::Mp::Str(v))
                        if k == b"package_hash" =>
                    {
                        String::from_utf8(v).ok()
                    }
                    _ => None,
                })
                .expect("package_hash")
        };
        let v1 = read("seed_v1_detached_pkg_fnv");
        let (snap, pkg) = snapshot_gen::parse_seed(&v1).unwrap();
        assert_eq!(snap[4], 1);
        assert!(key_of(snap).starts_with("pkg-"));
        let mut e = engine_wasm::Engine::new_headless(engine::DocumentTree::new());
        assert_eq!(e.restore_for_fuzzing(snap, pkg), Ok((1, false)));
        let v2 = read("seed_v2_detached_sha256");
        let (snap, pkg) = snapshot_gen::parse_seed(&v2).unwrap();
        assert!(key_of(snap).starts_with("sha256-"));
        let mut e = engine_wasm::Engine::new_headless(engine::DocumentTree::new());
        assert_eq!(e.restore_for_fuzzing(snap, pkg), Ok((2, false)));
        // The package missing / mismatched is reported, not fatal.
        for n in [
            "seed_v2_detached_package_missing",
            "seed_v2_detached_package_mismatch",
        ] {
            let b = read(n);
            let (snap, pkg) = snapshot_gen::parse_seed(&b).unwrap();
            let mut e = engine_wasm::Engine::new_headless(engine::DocumentTree::new());
            assert_eq!(e.restore_for_fuzzing(snap, pkg), Ok((2, true)), "{n}");
        }
    }

    /// Issue #341 — re-encoding an unmutated snapshot tree reproduces the
    /// engine's bytes (the mutator edits the tree, not the encoding), and a
    /// mutated one still never panics the restore.
    #[test]
    fn snapshot_tree_round_trips_and_mutations_do_not_panic() {
        let (_, bytes) = snapshot_gen::snapshot_seeds()
            .into_iter()
            .find(|(n, _)| *n == "seed_v2_inline_media_refs")
            .expect("seed");
        let (snap, _) = snapshot_gen::parse_seed(&bytes).unwrap();
        let tree = snapshot_gen::Mp::parse(&snap[5..]).expect("msgpack");
        let mut again = snap[..5].to_vec();
        tree.encode(&mut again);
        assert!(again == snap, "Mp must re-encode what rmp_serde wrote");
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        for _ in 0..200 {
            let mut noise = Vec::new();
            for _ in 0..96 {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                noise.push(state as u8);
            }
            let mutated = snapshot_gen::mutate_snapshot(&mut Unstructured::new(&noise), snap, None);
            run_snapshot_decode(&mutated);
        }
    }

    /// Issue #348 — the committed hostile `docx_reader` seeds are exactly
    /// what [`hostile_docx_seeds`] builds (re-run `examples/regen-seeds`
    /// after changing a builder), and each reaches its expected outcome
    /// through the raw-package path without a panic.
    #[test]
    fn hostile_docx_seeds_are_committed_and_typed() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus/docx_reader");
        for (name, bytes, expect) in hostile_docx_seeds() {
            let committed = std::fs::read(dir.join(name))
                .unwrap_or_else(|e| panic!("{name}: {e} — run examples/regen-seeds"));
            assert!(
                committed == bytes,
                "{name} is stale — run examples/regen-seeds"
            );
            run_docx_reader(&bytes);
            let got = read_raw_package(&bytes);
            match expect {
                Some(limit) => assert!(
                    matches!(
                        &got,
                        Err(format_docx::DocxError::PackageTooLarge { limit: l, .. }) if *l == limit
                    ),
                    "{name}: {:?}",
                    got.as_ref().map(|_| "read")
                ),
                None => assert!(got.is_ok(), "{name}: {:?}", got.err()),
            }
        }
    }
}
