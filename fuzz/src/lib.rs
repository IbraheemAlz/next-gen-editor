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
pub mod util;

use arbitrary::Unstructured;

/// `docx_reader` (D5.5) — build a schema-shaped WML document inside a
/// minimal OPC package from `data` (never raw bytes fed straight to the
/// parser — see `docx_gen`), then feed the assembled `.docx` bytes to
/// `format_docx::read_docx`. The reader must never panic; returning `Err`
/// on a deliberately-hostile package is the correct, expected outcome.
pub fn run_docx_reader(data: &[u8]) {
    let mut u = Unstructured::new(data);
    let Some(bytes) = docx_gen::build_docx(&mut u) else {
        return;
    };
    let _ = format_docx::read_docx(&bytes);
}

/// `docx_roundtrip` (D5.5, new target) — read -> write -> read must be
/// stable and never panic. Runs the cycle twice: the writer's own
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
    let Ok(archive_b) = format_docx::read_docx(&written_a) else {
        panic!(
            "docx_roundtrip: read_docx parsed the ORIGINAL package but \
             rejected write_docx's own output — the writer produced an \
             archive its own reader can't parse back"
        );
    };
    let doc_b = archive_b.document.clone();
    let Ok(written_b) = format_docx::write_docx(&archive_b, &doc_b) else {
        panic!("docx_roundtrip: second write_docx failed after a successful first round-trip");
    };
    let _ = format_docx::read_docx(&written_b);
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
/// itself), the undo stack never exceeds its 100-snapshot bound, and the
/// live selection always resolves inside the current document.
pub fn run_rpc_command(data: &[u8]) {
    let mut u = Unstructured::new(data);
    let seed_text = command_gen::gen_seed_text(&mut u);
    let mut engine = engine_wasm::Engine::new_headless(engine::DocumentTree::from_text(&seed_text));
    let commands = command_gen::gen_command_sequence(&mut u, MAX_COMMAND_SEQUENCE);
    for cmd in commands {
        /* Kept only for the failure message: the variant name (not the
        payload, which would defeat the smoke driver's per-message
        dedup) tells triage WHICH command broke an invariant. Formatted
        lazily — `assert!`'s message arguments run only on failure. */
        let keep = cmd.clone();
        let _evt = engine.apply_sync(cmd);
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
pub fn run_layout_paginate(data: &[u8]) {
    let mut u = Unstructured::new(data);
    let doc = layout_gen::gen_document_tree(&mut u);
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
    let alpha = if data.len() % 2 == 0 {
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
