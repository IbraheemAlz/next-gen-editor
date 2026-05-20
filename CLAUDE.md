# CLAUDE.md — engineering DNA

Booting into this repo? Read this first. Everything below is a learned-the-hard-way invariant from Phases 1–3. Don't relitigate without a measurement that contradicts it.

**Phase status:** Phases 1 (PoC), 2 (worker bridge + memory), and 3 (canvas rendering + native RTL) are **complete** — exit gates green. Phase 4 (headless UI: caret, selection, IME — `PHASE_4_HEADLESS_UI.md`) is next.

---

## Architecture (non-negotiable)

- **Rust → WASM core.** Engine lives in `crates/engine-wasm/`. No vendored binary blobs, ever. If a feature requires C++, vendor the **source** under `vendor/` and build it in CI.
- **Headless UI.** No `iframe`. The TypeScript shell owns the canvas, the worker, and the DOM chrome. The engine never touches the DOM.
- **Single dedicated Web Worker.** WASM is loaded exactly once in `ts/src/engine.worker.ts`. `OffscreenCanvas` is `transferControlToOffscreen()`-ed at INIT and never re-transferred — that call is one-shot per element, so crash recovery swaps in a **fresh** `<canvas>` element.
- **Cross-origin isolated.** Vite dev and prod serve with `Cross-Origin-Opener-Policy: same-origin`, `Cross-Origin-Embedder-Policy: require-corp`, `Cross-Origin-Resource-Policy: same-origin`. SAB depends on this; check `self.crossOriginIsolated` on boot.
- **Memory budget.** Compressed WASM artifact ≤ **15 MiB** (CI gate). Initial WASM heap 64 MiB, max 2 GiB (linker flags). Per-worker soft budget ≤ 256 MiB on a 50-page document.

## Toolchain (pinned and load-bearing)

- Rust **1.95.0** via `rust-toolchain.toml`. Do not bump without verifying every crate's MSRV.
- Targets: `wasm32-unknown-unknown` + native.
- `wasm-pack` via **Homebrew** (`brew install wasm-pack`).
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
  engine/         Pure-Rust document model (im::Vector) + UndoStack + style spans
  engine-wasm/    #[wasm_bindgen] surface; orchestrates everything
  text-pipeline/  fonts + FontStack + shape + bidi + line_break + justify + script
  layout/         hierarchical box model (PageBox→ParagraphBox→LineBox→VisualRun)
  render/         backend-agnostic DisplayList; Canvas2D + Vello backends; DirtyTracker
  format-docx/    .docx reader (zip + quick-xml) + writer (preserves siblings)
  format-pdf/     PDF export (pdf-writer) — box tree + full font embedding
ts/               Vite + TS shell, worker, EngineClient, event log, e2e suite
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
- **let-chains** (`if let X && let Y`) are available (Rust ≥ 1.88; toolchain 1.95).

## TS / browser conventions

- TS strict, `noUncheckedIndexedAccess`, `noImplicitOverride`, `exactOptionalPropertyTypes`.
- `tsify-next` renders `Option<T>` as `T | undefined`. Pass **`undefined`**, not `null`, from TS.
- `web-sys 0.3.98+`: `set_fill_style_str(&str)`, not the deprecated `set_fill_style(&JsValue)`.
- Worker boot via wasm-pack `--target web` output. `init({ module_or_path: new URL(...) })` — the bare-URL form is deprecated.
- Worker ↔ main RPC: the production path is **`EngineClient`** (`ts/src/engine-client.ts`) — `id`-routed `{ id, cmd }` requests, `{ id, ok, evt }` replies, a `pending` map, `subscribe()` for unidirectional events. The Phase-1 `{ type: 'COMMAND', id, cmd }` / `{ type: 'COMMAND_RESULT', id, event }` harness path still lives in the worker (visual-diff `?test=` cases only). Expose `window.__dispatch` for tests.
- Hidden `<textarea>` overlay is the only legitimate text-input source. `beforeinput` is the canonical event; ignore `e.isComposing`.

## Phase 2 — worker bridge, event log, crash recovery

- **`EngineClient`** (`ts/src/engine-client.ts`) is the typed main-thread RPC layer: spawns the worker, matches replies by `id`, exposes `dispatch` / `subscribe` / `recover`, and `loadFont` / `openDocument` (which pass byte buffers as `Transferable`s — zero-copy).
- **Worker dual-protocol** (`ts/src/engine.worker.ts`): the `EngineClient` `id`-routed path and the Phase-1 `?test=` visual-diff harness path coexist; a worker instance only ever sees one. The interactive editor uses `EngineClient`; `index.ts` keeps the harness path alive for the `?test=` goldens.
- **Bridge schema** is split across `crates/bridge/src/{common,command,event}.rs`. The §4–§5 schema landed **additively** on the Phase-1 PoC subset — Phase-1 commands/events are kept and tagged `// TODO: Deprecate in Phase 3`. The §4–§5 schema is **frozen**.
- **IndexedDB event log** (`ts/src/event-log.ts`): one `engine-log` DB; stores `commands` / `snapshots` / `meta`; snapshots pruned to the newest 3. The worker logs **off the critical path** — `handleClientCommand` posts the RPC reply *before* `logCommand()` runs (D2.8 backpressure; sustains 1000+ cmds/s).
- **Crash recovery**: a WASM trap (`/RuntimeError|unreachable/`) → worker posts `{ trap: true }` + `self.close()` → `EngineClient.onTrap` rejects pending + fires the UI `onCrash` callback → `index.ts` swaps in a fresh `<canvas>` and calls `recover()` → respawn + `Command::Recover`. `loadLatestEventLog` returns `snapshotSeq` / `lastSeq` so the recovered worker resumes `logSequence` (never restarts at 0).
- **e2e suite**: `ts/e2e/*.spec.ts` + `ts/playwright.config.ts` — `@playwright/test` with `channel: 'chrome'` (system Chrome, no download); `webServer` auto-boots Vite. Run: `pnpm exec playwright test` from `ts/`.

## Phase 3 — rendering, RTL, box model

- **Hierarchical box model** (`layout/boxes.rs`): `PageBox → ParagraphBox → LineBox → VisualRun → PositionedGlyph`. Every box's `origin` is parent-relative; the renderer accumulates origins down the tree. `layout_paragraph` owns all geometry — line stacking and the alignment offset are baked into `LineBox.origin`, so the renderer is a pure traversal.
- **ICU 2.x** — `icu_segmenter` / `icu_properties` bumped 1.5 → 2.2; `LineSegmenter::new_auto` now takes `LineBreakOptions`.
- **Priority-band Kashida** (`text-pipeline/justify_kashida.rs`): candidates from Unicode `Joining_Type`, scored into Microsoft P1–P5 bands; one Kashida per word at its best stroke. Width is an `x_advance` bump, not yet a `U+0640` tatweel glyph.
- **FontStack** (`text-pipeline/fonts.rs`, §13.A): per-script font fallback. `build_line` segments runs by BiDi level × script × style span.
- **Rich text** — `engine::Paragraph` carries `Vec<StyleRun>` style spans; `Command::ApplyFormatting` applies font-size + colour (bold/italic/underline/bg deferred).
- **PDF export** (`format-pdf`, D3.7): box tree → single-page PDF, Y-axis inverted, full `Type0`/`CIDFontType2` font embedding. Not PDF/A-1b — see `BACKLOG.md`.
- **DirtyTracker** (`render/dirty.rs`, D3.8): bounding-rect invalidation; `render_canvas2d` clips fills/strokes and culls off-region glyph runs (`put_image_data` ignores the canvas clip).
- **Vello/WebGPU** plumbed and reachable via `Engine::init_vello`, but Canvas2D stays the active renderer.

## Validation (CI gates, all -D warnings)

- `cargo fmt --all -- --check` clean.
- `cargo clippy --workspace --all-targets -- -D warnings` clean.
- `cargo test --workspace` (native unit tests).
- `wasm-pack test --headless --chrome crates/engine-wasm` (browser unit tests).
- `wasm-pack build --release` then assert artifact `< 15728640` bytes.
- `cargo run -p shape-regression --release` — 0 failed on the corpus.
- `cargo run -p roundtrip --release` — PASS.
- `tools/visual-diff` on the goldens — every case ≤ **2 %** pixel diff (most cases 0.000 %).
- `pnpm exec playwright test` (from `ts/`) — the 7 Phase 2 exit-gate e2e specs in `ts/e2e/` all green.

## Visual-diff harness

- **Playwright with `channel: 'chrome'`** — uses system Chrome, no 150 MB chromium download.
- `chrome --virtual-time-budget` alone **does not wait for real I/O** (network, WASM compile). Use `page.waitForFunction(() => window.__paintIdle)` instead.
- Per-case viewport mapping in `tools/visual-diff/run.mjs` `VIEWPORTS` map. A4 cases get 595×842 (1 pt = 1 px); single-glyph cases get 400×400.
- Tests pass `?test=<case>` which hides UI chrome so the canvas is the only thing in the screenshot.
- `UPDATE=1` env var regenerates the golden. Every regeneration must be eyeballed in the diff before merging.

## Editor invariants

- Document model is **immutable + structurally shared** (`im::Vector<Paragraph>`). Cloning a tree is O(1).
- **`UndoStack`** is bounded (depth 100). Pushing a new snapshot truncates the redo branch.
- After every mutation, if a `layout_cfg` was cached, the engine **auto-repaints** the full document and invalidates the `DirtyTracker`. `Command::RequestPaint` does a clipped partial repaint (D3.8); the auto-repaint path stays full.
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
- ❌ Block the RPC reply on the IndexedDB event-log write — log off the critical path.
- ❌ Re-call `transferControlToOffscreen()` on a consumed canvas — swap in a fresh `<canvas>`.

## Where the deferred work landed

Phases 1–3 are complete. Phase 3's deferred scope — full rich text, tatweel-glyph Kashida, PDF/A-1b, Vello activation, dynamic line height — is recorded in [`BACKLOG.md`](BACKLOG.md).

Phase 3 → 4 hand-off:

- The §4–§5 bridge schema stays **frozen**. Phase-4 UI work is engine-internal behind `Engine::dispatch` plus TS-shell wiring.
- Phase-1 PoC commands (`RenderPage`, `RasterizeGlyph`, `ShapeAndRasterize`, `LoadDocx`, `SaveDocx`) are still live for the visual-diff `?test=` harness — retire them once the editor drives the engine purely through the §4 schema.
- `Command::Recover` is still a stub — real recovery needs `Engine::snapshot()` (event-log snapshots are empty placeholders). `EngineStats.last_paint_ms` / `last_command_ms` are still `0.0` dummies.
- Phase 4 (`PHASE_4_HEADLESS_UI.md`): caret, selection rendering, the hidden-textarea input path, IME. Build on the box model — `VisualRun.source_range` + `PositionedGlyph.cluster` give the glyph↔source mapping that cursor hit-testing needs.
