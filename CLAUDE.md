# CLAUDE.md — engineering DNA

Booting into this repo? Read this first. Everything below is a learned-the-hard-way invariant from Phase 1 PoC. Don't relitigate without a measurement that contradicts it.

---

## Architecture (non-negotiable)

- **Rust → WASM core.** Engine lives in `crates/engine-wasm/`. No vendored binary blobs, ever. If a feature requires C++, vendor the **source** under `vendor/` and build it in CI.
- **Headless UI.** No `iframe`. The TypeScript shell owns the canvas, the worker, and the DOM chrome. The engine never touches the DOM.
- **Single dedicated Web Worker.** WASM is loaded exactly once in `ts/src/engine.worker.ts`. `OffscreenCanvas` is `transferControlToOffscreen()`-ed at INIT and never re-transferred.
- **Cross-origin isolated.** Vite dev and prod serve with `Cross-Origin-Opener-Policy: same-origin`, `Cross-Origin-Embedder-Policy: require-corp`, `Cross-Origin-Resource-Policy: same-origin`. SAB depends on this; check `self.crossOriginIsolated` on boot.
- **Memory budget.** Compressed WASM artifact ≤ **15 MiB** (CI gate). Initial WASM heap 64 MiB, max 2 GiB (linker flags). Per-worker soft budget ≤ 256 MiB on a 50-page document.

## Toolchain (pinned and load-bearing)

- Rust **1.85.1** via `rust-toolchain.toml`. Do not bump without verifying every crate's MSRV.
- Targets: `wasm32-unknown-unknown` + native.
- `wasm-pack` via **Homebrew** (`brew install wasm-pack`). `cargo install wasm-pack --locked` fails on 1.85 because some locked transitive deps require rustc ≥ 1.86.
- `wasm-opt` is invoked automatically by `wasm-pack build --release`.
- `pnpm` for TS. Node ≥ 22.

### `.cargo/config.toml` rules

- **Do not** set `[build] target = "wasm32-unknown-unknown"` at the global level. That breaks `cargo check --workspace` on native tooling.
- **Do** set `[target.wasm32-unknown-unknown] rustflags = [...]` for wasm-specific linker args.
- Stack size on modern `wasm-ld`: pass as **two** args, `-z` then `stack-size=N`. The old `--stack-size=N` form is rejected.
- SIMD-128 + bulk-memory + mutable-globals + sign-ext + nontrapping-fptoint are on.

### Build profile

- `[profile.release]`: `opt-level = "z"`, `lto = "thin"`, `codegen-units = 1`, `panic = "abort"`, `strip = true`.
- **Do not use `lto = "fat"`** for the wasm artifact. `compiler-builtins` ships precompiled object files for intrinsics that aren't LLVM bitcode; fat LTO rejects them.

## Workspace layout

```
crates/
  bridge/         RPC Command/Event types (serde + tsify-next)
  engine/         Pure-Rust document model (im::Vector) + UndoStack
  engine-wasm/    #[wasm_bindgen] surface; orchestrates everything
  text-pipeline/  fonts + shape (rustybuzz) + bidi + line_break + justify
  layout/         A4Page + LineBox + paragraph layout (per-line BiDi)
  render/         Canvas2D backend; Vello backend later
  format-docx/    .docx reader (zip + quick-xml) + writer (preserves siblings)
ts/               Vite + TS shell, worker, dispatch channel
tools/
  visual-diff/    Playwright + pixelmatch golden suite
  shape-regression/  rustybuzz output snapshots
  roundtrip/      .docx open → edit → save → byte-diff harness
```

## Crate / dep conventions

- **`tsify-next`**, not `tsify` (original unmaintained since 2022). `default-features = false, features = ["js"]`.
- **`unicode-bidi`**, not `icu_bidi` (icu_bidi is not on crates.io at version 1.5).
- **`serde_bytes`** + `#[tsify(type = "Uint8Array")]` for `Vec<u8>` fields that must travel as binary. Without this, serde-wasm-bindgen rejects `Uint8Array` with `invalid type: byte array, expected a sequence` and falls back to a 4-8× heap-inflated number array.
- Bridge command/event enums **always** carry `#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE")]`. TS sees `{ type: "INSERT_TEXT", ... }`.
- Workspace member `Cargo.toml`s inherit via `.workspace = true` for `version`, `edition`, `license`, `rust-version`. Don't duplicate.
- Rust 1.85 lacks let-chains. Split `if let X && let Y` into nested `if let`.

## TS / browser conventions

- TS strict, `noUncheckedIndexedAccess`, `noImplicitOverride`, `exactOptionalPropertyTypes`.
- `tsify-next` renders `Option<T>` as `T | undefined`. Pass **`undefined`**, not `null`, from TS.
- `web-sys 0.3.98+`: `set_fill_style_str(&str)`, not the deprecated `set_fill_style(&JsValue)`.
- Worker boot via wasm-pack `--target web` output. `init({ module_or_path: new URL(...) })` — the bare-URL form is deprecated.
- Worker ↔ main RPC: Promise-based `dispatch(cmd) → Event` channel with `{ type: 'COMMAND', id, cmd }` request and `{ type: 'COMMAND_RESULT', id, event }` reply. Expose `window.__dispatch` for tests.
- Hidden `<textarea>` overlay is the only legitimate text-input source. `beforeinput` is the canonical event; ignore `e.isComposing`.

## Validation (CI gates, all -D warnings)

- `cargo fmt --all -- --check` clean.
- `cargo clippy --workspace --all-targets -- -D warnings` clean.
- `cargo test --workspace` (native unit tests).
- `wasm-pack test --headless --chrome crates/engine-wasm` (browser unit tests).
- `wasm-pack build --release` then assert artifact `< 15728640` bytes.
- `cargo run -p shape-regression --release` — 0 failed on the corpus.
- `cargo run -p roundtrip --release` — PASS.
- `tools/visual-diff` on the goldens — every case ≤ **2 %** pixel diff (most cases 0.000 %).

## Visual-diff harness

- **Playwright with `channel: 'chrome'`** — uses system Chrome, no 150 MB chromium download.
- `chrome --virtual-time-budget` alone **does not wait for real I/O** (network, WASM compile). Use `page.waitForFunction(() => window.__paintIdle)` instead.
- Per-case viewport mapping in `tools/visual-diff/run.mjs` `VIEWPORTS` map. A4 cases get 595×842 (1 pt = 1 px); single-glyph cases get 400×400.
- Tests pass `?test=<case>` which hides UI chrome so the canvas is the only thing in the screenshot.
- `UPDATE=1` env var regenerates the golden. Every regeneration must be eyeballed in the diff before merging.

## Editor invariants

- Document model is **immutable + structurally shared** (`im::Vector<Paragraph>`). Cloning a tree is O(1).
- **`UndoStack`** is bounded (depth 100). Pushing a new snapshot truncates the redo branch.
- After every mutation, if a `layout_cfg` was cached from a prior `RenderPage`/`RenderDocument`, the engine **auto-repaints** the full document. No dirty-rect optimization in PoC; Phase 3.
- BiDi runs **per line**, not paragraph-wide. UAX #9 requires this. Don't flatten visual order across line breaks.
- Line break opportunities come from `icu_segmenter::LineSegmenter::new_auto()`. Greedy fit is fine for PoC; Knuth-Plass is Phase 3.

## `.docx` round-trip invariants

- The reader stashes every non-`word/document.xml` archive entry verbatim in `DocxArchive.other_entries`.
- The writer emits those entries **byte-identical** + a freshly serialized `word/document.xml`. Don't re-serialize content types or rels.
- Round-trip diff bound: `word/document.xml` byte delta ≤ 2 × UTF-8 byte size of the inserted text. Tighter than that is suspicious (probably overwrote unrelated regions). Looser means whitespace creep.
- XML escapes: `&` `<` `>` only. `xml:space="preserve"` on every `<w:t>` to keep trailing whitespace.

## Bash / agent ergonomics

- **Working dir drifts** between Bash tool calls. Use absolute paths or `cd /home/ibrahim/Desktop/code/next-gen-editor &&` at the top of every multi-step command.
- Long-running processes (vite dev, wasm-pack build) run in `run_in_background: true`.
- Don't `git add .` blindly. Stage by explicit path.
- Commit messages: heredoc + `Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>`.

## Things to never do

- ❌ Vendor binary blobs (the `ranuts/document` anti-pattern).
- ❌ `iframe`-based editor.
- ❌ WASM on the main thread.
- ❌ `lto = "fat"` for wasm builds.
- ❌ `icu_bidi` dep (doesn't exist).
- ❌ Mix BiDi paragraph-wide visual order with line breaking.
- ❌ Skip the COOP/COEP server config "just for now".
- ❌ Use `chrome --screenshot` + `--virtual-time-budget` for runtime assertions (use Playwright `waitForFunction`).
- ❌ Regenerate goldens without visually diffing.

## Where the deferred work landed

Phase 1 PoC limitations are tagged in the next-phase plan documents. See:

- `PHASE_2_BRIDGE_MEMORY.md` §13 — RPC channel & schema completion deferred items.
- `PHASE_3_RENDER_RTL.md` §6 (Kashida), §8 (line break), §13 (Phase 1 deferrals).
- `PHASE_4_HEADLESS_UI.md` §6–§8 (textarea, caret, selection rendering).
- `PHASE_5_HARDENING_RELEASE.md` §3–§4 (visual-diff + roundtrip CI lifting).

Don't add Phase 2+ scope to Phase 1 backports.
