# CLAUDE.md — engineering DNA

Booting into this repo? Read this first. Everything below is a learned-the-hard-way invariant from Phases 1–4. Don't relitigate without a measurement that contradicts it.

**Phase status:** Phases 1 (PoC), 2 (worker bridge + memory), 3 (canvas rendering + native RTL), and 4 (headless UI shell — Solid.js, pointer + IME input, accessibility) are **complete**. Phase 5 (`PHASE_5_HARDENING_RELEASE.md`) — the **engineering** deliverables (D5.1–D5.5 QA harnesses + fuzzing, D5.7 telemetry, D5.8 release pipeline) are **complete**; the cut is `v0.5.0-beta.1`. D5.6 (external security audit), D5.9 (operator runbook) and D5.10 (Arabic typography sign-off) are human / external deliverables still pending — this is a **beta**, not the final MVP.

---

## Architecture (non-negotiable)

- **Rust → WASM core.** Engine lives in `crates/engine-wasm/`. No vendored binary blobs, ever. If a feature requires C++, vendor the **source** under `vendor/` and build it in CI.
- **Headless UI.** No `iframe`. The TypeScript shell owns the canvas, the worker, and the DOM chrome. The engine never touches the DOM.
- **Single dedicated Web Worker.** WASM is loaded exactly once in `ts/src/engine/engine.worker.ts`. `OffscreenCanvas` is `transferControlToOffscreen()`-ed at INIT and never re-transferred — that call is one-shot per element, so crash recovery swaps in a **fresh** `<canvas>` element.
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
  visual-diff/    Playwright + pixelmatch golden farm (tiered, D5.1)
  memory-profile/ engine + JS heap snapshot harness (D5.2)
  perf/           cold-start + insert-latency + open-doc harness (D5.3)
  pdf-validate/   veraPDF PDF/A-1b validation harness (D5.4)
  perf-fixtures/  generates the synthetic perf .docx load files
  shape-regression/  rustybuzz output snapshots
  roundtrip/      .docx open → edit → save → byte-diff harness
fuzz/             cargo-fuzz crate, own workspace (D5.5)
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
- Worker ↔ main RPC: the production path is **`EngineClient`** (`ts/src/engine/engine-client.ts`) — `id`-routed `{ id, cmd }` requests, `{ id, ok, evt }` replies, a `pending` map, `subscribe()` for unidirectional events (the a11y tree rides this). The Phase-1 `{ type: 'COMMAND', id, cmd }` / `{ type: 'COMMAND_RESULT', id, event }` harness path still lives in the worker (visual-diff `?test=` cases only), driven by `ts/src/harness/visual-diff.ts`. Expose `window.__dispatch` for tests.
- Hidden `<textarea>` (`components/HiddenInput.tsx`) is the only legitimate text-input source. `beforeinput` is the canonical event; when `e.isComposing` is set, defer to the composition handlers.

## Phase 2 — worker bridge, event log, crash recovery

- **`EngineClient`** (`ts/src/engine-client.ts`) is the typed main-thread RPC layer: spawns the worker, matches replies by `id`, exposes `dispatch` / `subscribe` / `recover`, and `loadFont` / `openDocument` (which pass byte buffers as `Transferable`s — zero-copy).
- **Worker dual-protocol** (`ts/src/engine/engine.worker.ts`): the `EngineClient` `id`-routed path and the Phase-1 `?test=` visual-diff harness path coexist; a worker instance only ever sees one. The interactive editor uses `EngineClient`; `harness/visual-diff.ts` keeps the harness path alive for the `?test=` goldens.
- **Bridge schema** is split across `crates/bridge/src/{common,command,event}.rs`. Every layer landed **additively**: §4–§5 on the Phase-1 PoC subset, then Phase 4's pointer / IME / clipboard / a11y commands on §4–§5 (see the Phase 4 section). The discipline is permanent — extend, never break a consumer.
- **IndexedDB event log** (`ts/src/event-log.ts`): one `engine-log` DB; stores `commands` / `snapshots` / `meta`; snapshots pruned to the newest 3. The worker logs **off the critical path** — `handleClientCommand` posts the RPC reply *before* `logCommand()` runs (D2.8 backpressure; sustains 1000+ cmds/s).
- **Crash recovery**: a WASM trap (`/RuntimeError|unreachable/`) → worker posts `{ trap: true }` + `self.close()` → `EngineClient.onTrap` rejects pending + fires the UI `onCrash` callback → `App` bumps the `canvasGen` signal, remounting `EditorCanvas` with a fresh `<canvas>`, and calls `recover()` → respawn + `Command::Recover`. `loadLatestEventLog` returns `snapshotSeq` / `lastSeq` so the recovered worker resumes `logSequence` (never restarts at 0).
- **e2e suite**: `ts/e2e/*.spec.ts` + `ts/playwright.config.ts` — `@playwright/test` with `channel: 'chrome'` (system Chrome, no download); `webServer` auto-boots Vite. Run: `pnpm exec playwright test` from `ts/`.

## Phase 3 — rendering, RTL, box model

- **Hierarchical box model** (`layout/boxes.rs`): `PageBox → ParagraphBox → LineBox → VisualRun → PositionedGlyph`. Every box's `origin` is parent-relative; the renderer accumulates origins down the tree. `layout_paragraph` owns all geometry — line stacking and the alignment offset are baked into `LineBox.origin`, so the renderer is a pure traversal.
- **ICU 2.x** — `icu_segmenter` / `icu_properties` bumped 1.5 → 2.2; `LineSegmenter::new_auto` now takes `LineBreakOptions`.
- **Priority-band Kashida** (`text-pipeline/justify_kashida.rs`): candidates from Unicode `Joining_Type`, scored into Microsoft P1–P5 bands; one Kashida per word at its best stroke. Width is an `x_advance` bump, not yet a `U+0640` tatweel glyph.
- **FontStack** (`text-pipeline/fonts.rs`, §13.A): per-script font fallback. `build_line` segments runs by BiDi level × script × style span.
- **Rich text** — `engine::Paragraph` carries `Vec<StyleRun>` style spans; `Command::ApplyFormatting` applies font-size + colour, plus bold/italic/underline **flags** stored on `SpanStyle` in Phase 4 (rendering of bold/italic faces + underline strokes deferred — BACKLOG.md).
- **PDF export** (`format-pdf`, D3.7): box tree → single-page PDF, Y-axis inverted, full `Type0`/`CIDFontType2` font embedding. Not PDF/A-1b — see `BACKLOG.md`.
- **DirtyTracker** (`render/dirty.rs`, D3.8): bounding-rect invalidation; `render_canvas2d` clips fills/strokes and culls off-region glyph runs (`put_image_data` ignores the canvas clip).
- **Vello/WebGPU** plumbed and reachable via `Engine::init_vello`, but Canvas2D stays the active renderer.

## Phase 4 — headless UI shell

- **Solid.js shell.** `ts/src/index.tsx` is the entry: a `?test=<case>` query routes to the preserved visual-diff harness and never mounts Solid; otherwise it mounts `App.tsx`. Built with `vite-plugin-solid`. `ts/src/` is split into `engine/`, `components/`, `input/`, `state/`, `styles/`, `harness/`.
- **`EditorCanvas`** (`components/EditorCanvas.tsx`) owns the `<canvas>` and `transferControlToOffscreen()`s it once. Crash recovery is a Solid remount: `App` bumps a `canvasGen` signal, `<For each={[canvasGen()]}>` disposes the dead `<canvas>` and mounts a fresh one. The canvas carries **no `tabindex`** — a focusable canvas steals focus from the hidden textarea.
- **The engine owns the selection.** It holds `selection` (anchor + caret) and `composition` state; every interactive edit is caret-relative, advances the engine caret, and emits `SelectionChanged`. The worker queue serializes commands, so a stale UI-side caret never misplaces text — fast typing and async clipboard stay correct. This is *the* invariant that makes the UI race-free.
- **Hit-testing** (`engine-wasm`): `document_geometry` flattens the box tree into per-line `CaretSlot`s (absolute x ↔ source byte), inverting the renderer's coordinate walk. pixel→logical, logical→caret-rect, and selection rects all go through it. Selection rects are per-line bounding boxes (discontinuous BiDi → BACKLOG).
- **`HiddenInput`** (`components/HiddenInput.tsx`): the `<textarea>` is the OS text-input citizen. `beforeinput` → engine commands; IME via `Begin`/`Update`/`EndComposition`, committed on end (no on-canvas preview); native `copy`/`cut`/`paste` events → the async `navigator.clipboard`. It tracks the caret so IME popups anchor, and refocuses on `pointerup` — a canvas click blurs focus to `<body>` first.
- **Caret / selection / a11y are DOM overlays**, not canvas-drawn — `CaretOverlay`, `SelectionOverlay`, and a visually-hidden `AccessibilityTree` (`role="document"`, one `<p dir>` per paragraph, `<span>` per style run; the browser's UAX-#9 handles BiDi for the screen reader). The worker broadcasts a full `A11yTree` after every doc mutation. Engine geometry is device-px; overlays divide by `devicePixelRatio`.
- **Schema growth (additive).** Phase 4 added the `HitTest`, `SelectWordAt`, `DeleteAtCaret`, `RequestAccessibilityTree`, `GetSelectionAsClipboard`, `PastePlain` commands; the `HitResult` and `ClipboardPayload` events; the `Point` type; and `can_undo`/`can_redo` on `SelectionChanged`. The dead `AccessibilityTreeChanged` + `A11yDelta`/`A11yNode` were repurposed (the event now carries `A11yTree`) — done only because they had zero consumers.

## Phase 5 — hardening, QA harnesses, telemetry, release

The Phase 5 **engineering** work is complete (`v0.5.0-beta.1`). D5.6 / D5.9 /
D5.10 are external/human sign-offs, not code.

- **Visual-diff farm (D5.1).** `tools/visual-diff/run.mjs` gained a `TIERS`
  config + farm mode — `--tier A|B|C` runs every committed golden in one
  invocation at the tier tolerance; single-case mode is preserved. The §3
  200-doc tier corpus is not populated, so the farm runs the real Phase-3
  goldens under `tools/visual-diff/golden/`.
- **Memory snapshot (D5.2).** `tools/memory-profile/run.mjs` loads each
  `tests/perf/{50,100,250,500}p.docx` and checks the engine WASM heap + JS
  heap against the §5 budgets. `tools/perf-fixtures` (a workspace crate)
  generates those `.docx` files with `build_minimal_docx`.
- **Performance harness (D5.3).** `tools/perf/run.mjs` measures cold start,
  insert-char p95 (one-page seeded doc) and open-50p-doc against §6 tier
  budgets. `--strict` gates cold start + insert p95; open-doc is reported but
  **ungated** — it is bounded by the deferred incremental relayout (BACKLOG).
- **PDF/A-1b (D5.4).** `format-pdf` emits true PDF/A-1b for `PdfProfile::A1b`;
  `crates/format-pdf/build.rs` synthesizes the sRGB ICC profile — no binary
  blob in the tree. `tools/pdf-validate` is the veraPDF harness.
- **Fuzzing (D5.5).** `fuzz/` is a cargo-fuzz crate in its **own workspace** —
  `docx_reader` fuzzes `read_docx`, `rpc_command` fuzzes `Command` JSON.
  Compile-checked on stable; `cargo +nightly fuzz run` is the nightly flow.
- **Telemetry (D5.7).** Schema in `crates/bridge/src/telemetry.rs`; the UI
  collector `ts/src/state/telemetry.ts` batches samples and `console.log`s
  them every 60 s — a **mock** transport (no live collector for the MVP).
- **Release pipeline (D5.8).** `.github/workflows/release.yml` is
  tag-triggered (`v*`): builds the WASM artifact + static site + SBOM and
  publishes a GitHub Release. Cosign signing is a commented-out stub.
- **CI.** A **non-blocking** `qa-harness` job in `ci.yml` runs the visual-diff,
  memory and perf harnesses — non-blocking because golden pixel-reproducibility
  on the GitHub runner is unproven across machines.

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
- `cargo check --manifest-path fuzz/Cargo.toml` — the D5.5 fuzz crate compiles.
- The non-blocking `qa-harness` job runs the D5.1–D5.3 browser harnesses
  (`tools/visual-diff` farm, `tools/memory-profile`, `tools/perf`).

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

Phases 1–4 are complete, and Phase 5's engineering deliverables shipped at
`v0.5.0-beta.1`. Deferred scope — full rich-text rendering (bold/italic faces,
underline strokes), tatweel-glyph Kashida, Vello activation, dynamic line
height, discontinuous BiDi selection rects, inline IME preview, pending
formatting, rich clipboard, paragraph alignment + per-span font family,
accessibility deltas, and **incremental relayout** — is recorded in
[`BACKLOG.md`](BACKLOG.md).

Phase 5 → MVP hand-off:

- The bridge schema grew **additively** through Phases 4–5 — the D5.7
  `telemetry` module is the latest addition. Phase-1 PoC commands
  (`RenderPage`, `RasterizeGlyph`, `ShapeAndRasterize`, `LoadDocx`, `SaveDocx`)
  are still live for the visual-diff `?test=` harness.
- `Command::Recover` is still a stub — real recovery needs `Engine::snapshot()`
  (event-log snapshots are empty placeholders). `EngineStats.last_paint_ms` /
  `last_command_ms` and `Event::Painted.paint_ms` are still `0.0` dummies — the
  D5.7 telemetry pipeline is wired and will carry real numbers once they are.
- Interactive editing re-lays-out the whole document on every edit. Fine for a
  one-page document (insert p95 ≈ 10 ms, D5.3); **incremental relayout** is the
  open performance item — the D5.3 harness keeps open-50p-doc ungated for it.
- Remaining for the MVP `v0.1.0`: D5.6 (external security audit), D5.9
  (operator runbook), D5.10 (Arabic typography sign-off), then the §10 exit
  gate. This `v0.5.0-beta.1` cut is the engineering-complete beta.
