# CLAUDE.md — engineering DNA

Booting into this repo? Read this first. Everything below is a learned-the-hard-way invariant from Phases 1–4. Don't relitigate without a measurement that contradicts it.

## Project management: GitHub Issues is the single source of truth

**GitHub Issues is the absolute single source of truth for all project
management** — bugs, missing features, and technical debt. Do not create,
maintain, or read local markdown files for backlogs, roadmaps, or missing
features. Rely exclusively on the `gh` CLI (`gh issue list`, `gh issue
create`, `gh issue view`) for querying and creating tasks.

This repo used to carry a set of hand-maintained backlog/tracker docs under
`plans/` (`BACKLOG.md`, `CONSOLIDATED_MASTER_BACKLOG.md`,
`UI_SURFACE_MAPPING.md`, `DEFERRED_FEATURES_TRACKER.md`,
`LEGACY_BACKLOG_PLAN.md`, `CORE_SPRINTS_PLAN.md`, `SUPPORTED_FEATURES.md`).
These were deleted (2026-07-03) and their content migrated to GitHub Issues
to stop tracking from fragmenting across the repo and the issue tracker.
The historical narrative below (phase-by-phase what-shipped-when) is kept
because it's project history, not an open backlog — but any *currently
open* gap belongs in `gh issue list`, not a new markdown file. The
remaining `plans/*.md` docs (`PHASE_*.md`, `MASTER_PLAN.md`,
`OOXML_ROADMAP.md`, `UX_BEHAVIOR_SPEC.md`, `ECMA_376_COMPLIANCE_AUDIT.md`)
are architecture/spec/behavior references, not backlogs, and stay.

**Phase status:** Phases 1 (PoC), 2 (worker bridge + memory), 3 (canvas rendering + native RTL), and 4 (headless UI shell — Solid.js, pointer + IME input, accessibility) are **complete**. Phase 5 (`PHASE_5_HARDENING_RELEASE.md`) — the **engineering** deliverables (D5.1–D5.5 QA harnesses + fuzzing, D5.7 telemetry, D5.8 release pipeline) are **complete**. Twelve post-`beta.1` backlog / tech-debt sprints then closed the bulk of the old backlog doc (since migrated to GitHub Issues) — rich-text decorations + faces (with faux bold / italic on both Canvas2D and Vello), Kashida ink, incremental relayout, typography, `.docx` interoperability, the Vello/WebGPU activation, fine-grained accessibility deltas, the inline IME preview, and the core arrow-key / multi-click / select-all navigation layer (cut `v0.5.0-beta.3`). The "Monaco Standard" SDK split (`packages/`) and the Sprint 1–14 (UI Edition) waves followed — the `@nge/ui` shelf (zoom, images, revisions, comments, page setup, ruler, Dev HUD) plus viewport-culled lazy pagination (`LazyLayoutState` + `Command::ExpandLayout`); the cut is now `v0.6.0-beta.2`. D5.6 (external security audit), D5.9 (operator runbook) and D5.10 (Arabic typography sign-off) are human / external deliverables still pending — this is a **beta**, not the final MVP.

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
  format-pdf/     PDF export (pdf-writer) — box tree + subset font embedding
ts/               Vite + TS shell, worker, EngineClient, event log, e2e suite
packages/         pnpm workspace — Monaco Standard SDK split (post-beta.3)
  core/           @nge/core — Locked Surface + Headless API (Solid.js primitives)
  ui/             @nge/ui — default UI components (vanilla CSS, .nge-* prefix)
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
- **Every `Command` variant is classified once, in `crates/bridge/src/meta.rs`** (issue #342 — `command_meta!`, no wildcard, pinned to `EXPECTED_VARIANT_COUNT`): `logged` (the worker's event-log filter), `read_only` (`EngineClient.writesInFlight` / `writeEpoch`), `new_document` (the #268 pinned-snapshot set), `story` (the engine's `story_gate`), `status` (`Implemented` / `Partial { issue }` / `Stub { issue }`). Never re-add a hand-kept command list in the worker, the client or the engine. A bridge change regenerates `packages/core/src/commandMeta.generated.ts` (`NGE_UPDATE_COMMAND_META=1 cargo test -p bridge`; `cargo test` fails while it is stale) and classifies the variant in `packages/core/src/facadeMap.ts` (tsc refuses a facade method that dispatches a `Stub`).
- Workspace member `Cargo.toml`s inherit via `.workspace = true` for `version`, `edition`, `license`, `rust-version`. Don't duplicate.
- **let-chains** (`if let X && let Y`) are available (Rust ≥ 1.88; toolchain 1.95).

## TS / browser conventions

- TS strict, `noUncheckedIndexedAccess`, `noImplicitOverride`, `exactOptionalPropertyTypes`.
- `tsify-next` renders `Option<T>` as `T | undefined`. Pass **`undefined`**, not `null`, from TS.
- `web-sys 0.3.98+`: `set_fill_style_str(&str)`, not the deprecated `set_fill_style(&JsValue)`.
- Worker boot via wasm-pack `--target web` output. `init({ module_or_path: new URL(...) })` — the bare-URL form is deprecated.
- Worker ↔ main RPC: the production path is **`EngineClient`** (`ts/src/engine/engine-client.ts`) — `id`-routed `{ id, cmd }` requests, `{ id, ok, evt }` replies, a `pending` map, `subscribe()` for unidirectional events (the a11y tree rides this). The Phase-1 `{ type: 'COMMAND', id, cmd }` / `{ type: 'COMMAND_RESULT', id, event }` harness path still lives in the worker (visual-diff `?test=` cases only), driven by `ts/src/harness/visual-diff.ts`. Expose `window.__dispatch` for tests — but only through `ts/src/dev-hooks.ts` (issue #340, see "Live validation hooks" below); never assign engine handles to `window` unconditionally.
- Hidden `<textarea>` (`components/HiddenInput.tsx`) is the only legitimate text-input source. `beforeinput` is the canonical event; when `e.isComposing` is set, defer to the composition handlers.

## Phase 2 — worker bridge, event log, crash recovery

- **`EngineClient`** (`ts/src/engine-client.ts`) is the typed main-thread RPC layer: spawns the worker, matches replies by `id`, exposes `dispatch` / `subscribe` / `recover`, and `loadFont` / `openDocument` (which pass byte buffers as `Transferable`s — zero-copy).
- **Worker dual-protocol** (`ts/src/engine/engine.worker.ts`): the `EngineClient` `id`-routed path and the Phase-1 `?test=` visual-diff harness path coexist; a worker instance only ever sees one. The interactive editor uses `EngineClient`; `harness/visual-diff.ts` keeps the harness path alive for the `?test=` goldens.
- **Bridge schema** is split across `crates/bridge/src/{common,command,event}.rs`. Every layer landed **additively**: §4–§5 on the Phase-1 PoC subset, then Phase 4's pointer / IME / clipboard / a11y commands on §4–§5 (see the Phase 4 section). The discipline is permanent — extend, never break a consumer.
- **IndexedDB event log** (`ts/src/event-log.ts`): one `engine-log` DB; stores `commands` / `snapshots` / `meta` / `packages`; snapshots pruned to the newest 3. Snapshots are persisted **detached** (issue #212, `Command::Snapshot.detach_package`): an opened `.docx`'s retained source package (#134) is stored once per document in `packages` under its content key (`Event::Snapshot.package_hash` — `sha256-<hex>` since issue #269, snapshot format v2; the v1 `pkg-<len>-<fnv>` key and FNV media references are still accepted on restore for one release), handed back on `Command::Recover.package`, and garbage-collected when no retained snapshot names it — a missing/mismatched package falls back to the minimal-package writer. The worker logs **off the critical path** — `handleClientCommand` posts the RPC reply *before* `logCommand()` runs (D2.8 backpressure; sustains 1000+ cmds/s).
- **Crash recovery**: a WASM trap (`/RuntimeError|unreachable/`) → worker posts `{ trap: true }` + `self.close()` → `EngineClient.onTrap` rejects pending + fires the UI `onCrash` callback → `App` bumps the `canvasGen` signal, remounting `EditorCanvas` with a fresh `<canvas>`, and calls `recover()` → respawn + `Command::Recover`. `loadRecoveryLog` returns every retained snapshot (newest first, then the snapshot-less base) + the command rows + `lastSeq`, so the recovered worker resumes `logSequence` (never restarts at 0) and falls back to an older snapshot when the newest will not restore (issue #241). Pruning only ever drops commands at or before a *pruned* snapshot, in the same transaction — never without a retained snapshot — so every retained snapshot keeps its full tail. Issue #268 — candidates are **scored**, not just tried until one restores: full tail + package present > full tail with its package lost (`Event::Recovered.package_lost` — since issue #314 a package hash counts as stored only once its `packages` transaction commits and is cleared on failure, so a snapshot can no longer name a package that was never written; the fallback stays for logs written before #314) > the pinned base alone > nothing, and `RecoveryInfo` reports `packageFallbacks` / `packageLost` / `pinnedBase` / `tailDropped` (+ `rendererDowngraded` / `baseSnapshotAt` / `cause`). Issue #315: every *degraded* recovery (tail dropped, log truncated, package lost, renderer switched) is shown to the user by the `@nge/ui` `RecoveryBanner` (`role="alert"`, dismissible, says what was lost and what to do), mirrored in Dev HUD rows and carried on the telemetry `CRASH` sample as additive `bridge::RecoveryFlags`; a recovery that lost nothing stays silent. Each document's first snapshot is **pinned** (`meta` row `pinned`, re-pinned after `OPEN_DOCUMENT` / `LOAD_DOCX` / `RENDER_PAGE`): exempt from pruning (its tail is not), so an all-but-pinned-unreadable log restores the document as of that snapshot instead of losing it.
- **Recovery = base snapshot + replayed tail (issue #85).** `Command::Snapshot` → `Event::Snapshot { bytes }` is the versioned `engine::snapshot` envelope (`NGES` magic + format-version byte + named-field MessagePack; every model struct is `#[serde(default)]`, maps serialize sorted so equal states are byte-identical). It carries the document tree (styles, numbering, header/footer stories, media, comments), a size-bounded undo window, the selection, the active story, sticky formatting, review flags and the layout config. The worker snapshots every `SNAPSHOT_EVERY` logged commands *inside* the command task after the reply (so the seq is exact) and on a 1.5 s idle timer; the IndexedDB write stays off the critical path. `Command::Recover { snapshot, log_tail }` restores, then replays the tail through `apply` with the layout config stashed (no fonts yet → nothing may paint), and answers `Recovered { applied_commands, snapshot_restored, renderer }` — the renderer is re-probed on the fresh canvas and reported by the engine itself (#66). `setupEngine(restored = true)` re-loads fonts and re-asserts the device scale instead of re-seeding. `ARM_TRAP` (`EngineClient.armTrap`) is the fault-injection hook: a real `Engine.debug_force_trap` after K logged commands, log flushed first. The #99 Vello crash-loop streak is **persisted** (issue #240, `meta` row `renderer-streak`: count + timestamp + renderer + a `live` token; a clean `pagehide` leaves the token in `localStorage`, so only a generation that died with its tab counts): a boot within 24 h of reaching `VELLO_TRAP_LIMIT` starts on Canvas2D without probing (`INIT.forceRenderer`), and the Dev HUD shows the sticky fallback with a "Retry vello" action (`EngineClient.retryGpuRenderer`) — since issue #270 the retry is **in place**: the worker is retired (everything already sent is applied and logged, a snapshot is taken, writes flushed, then `self.close()`), and the shell respawns it through the normal recovery path without a trap, so the document, zoom and the page survive — never `window.location.reload()`, which starts a new session.
- **Crash overlay never loses the document (issue #330).** `TrapOverlay` (shown between `TRAP` and `RECOVERED`) has no `window.location.reload()` fallback any more: "Reload engine" = `EngineClient.restartInPlace()` (joins a recovery already under way, otherwise retires the worker like #270 — bounded by `RETIRE_TIMEOUT_MS` — and respawns through the normal recovery path, `RecoveryInfo.cause = 'engine-reload'`); "Reload page (keeps document)" = `prepareCarryOver()` (retire + a one-shot `sessionStorage` token) then reload, and the next boot's `init()` honours the token **once** by recovering from the log instead of `INIT` (`cause = 'page-reload'`; `RECOVER`'s reply now also carries `crossOriginIsolated`); "Discard document" (two-step confirm) is the only action that starts a new session. **A new session — `INIT` → `openEventLog` — is the only thing that clears the event log**; a plain F5 still does, with or without unsaved edits (no `beforeunload` guard yet). Buttons whose `EngineClientLike` method (`restartInPlace` / `prepareCarryOver`) is absent are hidden, not left as reloads. e2e: `crash-recovery.spec.ts` holds the recovery open with a gate on `client.recover` so the overlay can be clicked.
- **A failed snapshot write is retried on its own clock (issue #333).** `takeSnapshot`'s write failure rewinds `lastSnapshotAt` (so the position is snapshotted again, not skipped as "taken") and schedules a bounded retry — `SNAPSHOT_RETRY_DELAYS_MS` = 2 s, 4 s, 8 s, independent of new commands; a success resets the run. After the last retry also fails the worker posts `{ notice: 'CHECKPOINT', state: 'exhausted' }` (unsolicited, id-less like the a11y delta) → `EngineClient.checkpointStatus` / `onCheckpointStatus` → `createEditorState().checkpointFailing` → `RecoveryBanner` ("Changes are not being checkpointed … Save your work now", `data-kinds="checkpoint-failing"`, cleared by the next landed write). Every failed write also fires `onCheckpointFailure` → telemetry `ERROR / CHECKPOINT_FAILED` sample (`bridge::ErrorCode::CheckpointFailed`). e2e: `event-log-replay.spec.ts` mocks `IDBObjectStore.put` in the worker to abort the first N `snapshots` puts.
- **A plain reload with unsaved edits no longer loses them (issue #388).** The worker keeps a `clean` marker in the event-log `meta` store, folded from every command by the ONE shared rule `ts/src/engine/clean-state.ts` (`nextCleanState`: `new_document` commands → clean, other `mutates_doc` → dirty, a `SAVE_DOCX` that answered `DOCUMENT_SAVED` → clean, refused commands change nothing; `EngineClient` folds the same rule on the main thread for the synchronous answer, `hasUnsavedChanges`). `attachUnloadGuard` (`ts/src/state/unload-guard.ts`) raises the browser's `beforeunload` prompt while the document is not clean (`attachUnloadGuard(client, { enabled })` / `VITE_NGE_UNLOAD_GUARD=0` for hosts that autosave; a prepared carry-over, #330, is not guarded). At boot — no carry-over token — `EngineClient.init` calls `inspectActiveLog`: a log whose marker is `false` is copied into ONE `archive` row (`archiveActiveLog`, DB v3; a newer unsaved session replaces an older undecided one) BEFORE `INIT` clears the active stores, and `previousSession` / `onPreviousSession` publish it (an undecided offer survives further reloads). `@nge/ui` `RecoveryBanner` shows "Recover previous document?" with **Recover** (`recoverPreviousSession`: retire the fresh worker, `restoreArchive` swaps the archive in as the active log in one transaction, respawn through the normal recovery path, `cause = 'session-restore'`) and **Discard** (`discardPreviousSession`); the archive is cleared only by Discard or by Recover. A log with no marker (written before #388) reads as "unknown" and is never offered. e2e: `unsaved-session.spec.ts`.
- **The event log's health is a typed bridge event, and the command journal is retried too (issue #390).** The ad-hoc `{ notice: 'CHECKPOINT' }` worker message is gone: the worker broadcasts an additive `Event::CheckpointState { ok, failures, last_error, journal_failing }` (id-less, unsolicited like the a11y delta — no `Command`, `EXPECTED_VARIANT_COUNT` unchanged) on `subscribe()`; every event with `failures > 0` is exactly one failed attempt (the exhausting failure is a single `ok: false` event), `failures: 0` ends a run. Three failure sources share the 2/4/8 s clock: an engine-side `SNAPSHOT` dispatch that answers anything but `SNAPSHOT` (or throws) → `noteSnapshotFailed`, the snapshot's IndexedDB write (#333), and an `appendCommand` row write → `journalBacklog` + `drainJournal` (rounds, not rows: a keystroke burst failing together is ONE round; a later successful append drains an exhausted backlog). The failed seqs are also recorded best-effort in the `meta` row `journal-gap`; `RECOVER` counts the ones still missing after the restored base (plus holes in the retained tail) as `RecoveryInfo.journalGap` (reply field `journalGap`, `recoveryNotices` kind `journal-gap`). `@nge/core` `createEditorState().checkpointState()` (`CheckpointHealth`) mirrors the event (seeded from `engine.checkpointStatus`; `checkpointFailing()` is `!ok`); `RecoveryBanner` shows `journal-failing` ("Your edits are not being recorded", `checkpointNotices(failing, journalFailing)`) beside the #333 `checkpoint-failing` notice; telemetry gets `ERROR / CHECKPOINT_FAILED` per attempt and `ERROR / JOURNAL_FAILED` (`bridge::ErrorCode::JournalFailed`) once per exhausted run. e2e: `journal-failure.spec.ts` (mocked failing `commands` store).
- **A refused keyboard edit is visible (issue #364).** `Engine::tracked_delete` answers `Event::Error { kind: Some(ErrorKind::TrackedDeletionRefused) }` (additive `bridge::ErrorKind` variant) for a tracked deletion across a table-cell boundary / over a table. `@nge/core` `createEditorState().lastError()` (`EditorError`: `kind`, `command` parsed from the `<Command>: ` message prefix, `message`, running `count`, `at`) moves on EVERY `Event::Error` reply; `@nge/ui` `ErrorToast` (mounted in `SdkShelf`) renders the kinds that have copy in `ERROR_TOAST_COPY` in a persistent `.nge-toast` `role="status"` live region for 4 s (a newer error restarts the timer; `PackageTooLarge` keeps the File menu's presentation, untyped errors are HUD-only), and the Dev HUD has a "Last error" row. A new typed `ErrorKind` that the user must see needs a copy entry there. e2e: `error-toast.spec.ts` (review mode, selection across a table, real Backspace; fails without the toast).
- **e2e suite**: `ts/e2e/*.spec.ts` + `ts/playwright.config.ts` — `@playwright/test` with `channel: 'chrome'` (system Chrome, no download); `webServer` auto-boots Vite. Run: `pnpm exec playwright test` from `ts/`.
- **Race-class e2e specs use the synchronous `burst` helper (issue #310).** `page.keyboard` / `page.mouse` round-trip through CDP per event — slow enough that the worker answers between two simulated inputs and the shell's mirrored state is already fresh, so a spec passes against the very race it targets (the #286 real-keyboard spec did). `ts/e2e/helpers/editor.ts` exports `burst(page, steps)`, which fires the whole sequence (`'B'` Ctrl+B, `'BTN'` Bold button, `'ENTER'`, `{ pointerdown: { x, y, shift? } }` / `{ pointerup }` on a page canvas through the real `pointer.ts`, or any other string as `insertText`) from ONE synchronous `page.evaluate`, so no engine reply can interleave; read results via `documentText` / `settle` (worker round-trips, FIFO behind the burst). Calling `__dispatch` directly is NOT a race test — it bypasses `pointer.ts` / `HiddenInput`. **A new race spec must be shown to fail** against a deliberately re-introduced deferral in the shell (e.g. `setTimeout(…, 0)` around `placeCaret`/`extendTo`/the Enter dispatch/`toggleFormat`/`cmd.toggleFormatting`); a bare delay needs a trailing keystroke queued behind it in the burst (the readback arrives too late to notice a delay on its own). Keep one slow real-input smoke per scenario, but never as the only guard. The engine ignores a stale `at` on `SPLIT_PARAGRAPH` when a selection exists, so "stale mirror" regressions of Enter surface only as shell-side deferral.

## Phase 3 — rendering, RTL, box model

- **Hierarchical box model** (`layout/boxes.rs`): `PageBox → ParagraphBox → LineBox → VisualRun → PositionedGlyph`. Every box's `origin` is parent-relative; the renderer accumulates origins down the tree. `layout_paragraph` owns all geometry — line stacking and the alignment offset are baked into `LineBox.origin`, so the renderer is a pure traversal.
- **ICU 2.x** — `icu_segmenter` / `icu_properties` bumped 1.5 → 2.2; `LineSegmenter::new_auto` now takes `LineBreakOptions`.
- **Priority-band Kashida** (`text-pipeline/justify_kashida.rs`): candidates from Unicode `Joining_Type`, scored into Microsoft P1–P5 bands; one Kashida per word at its best stroke. Width is an `x_advance` bump, not yet a `U+0640` tatweel glyph.
- **FontStack** (`text-pipeline/fonts.rs`, §13.A): per-script font fallback. `build_line` segments runs by BiDi level × script × style span.
- **Rich text** — `engine::Paragraph` carries `Vec<StyleRun>` style spans; `Command::ApplyFormatting` applies font-size + colour, plus bold/italic/underline **flags** stored on `SpanStyle` in Phase 4 (rendering of bold/italic faces + underline strokes was deferred at the time — shipped later, see "Where the deferred work landed" below).
- **PDF export** (`format-pdf`, D3.7): box tree → single-page PDF, Y-axis inverted, full `Type0`/`CIDFontType2` font embedding. Not PDF/A-1b at the time — strict PDF/A-1b shipped later (D5.4, see below).
- **DirtyTracker** (`render/dirty.rs`, D3.8): bounding-rect invalidation; `render_canvas2d` clips fills/strokes and culls off-region glyph runs (`put_image_data` ignores the canvas clip).
- **Vello/WebGPU** is runtime-activated: the worker probes `detect_backend()` at INIT and boots `Engine::with_vello` when a WebGPU adapter is acquirable; Canvas2D is the fallback and the CI / golden-farm default (no GPU in dev/CI).

## Phase 4 — headless UI shell

- **Solid.js shell.** `ts/src/index.tsx` is the entry: a `?test=<case>` query routes to the preserved visual-diff harness and never mounts Solid; otherwise it mounts `App.tsx`. Built with `vite-plugin-solid`. `ts/src/` is split into `engine/`, `components/`, `input/`, `state/`, `styles/`, `harness/`.
- **`EditorCanvas`** (`components/EditorCanvas.tsx`) owns the `<canvas>` and `transferControlToOffscreen()`s it once. Crash recovery is a Solid remount: `App` bumps a `canvasGen` signal, `<For each={[canvasGen()]}>` disposes the dead `<canvas>` and mounts a fresh one. The canvas carries **no `tabindex`** — a focusable canvas steals focus from the hidden textarea.
- **The engine owns the selection.** It holds `selection` (anchor + caret) and `composition` state; every interactive edit is caret-relative, advances the engine caret, and emits `SelectionChanged`. The worker queue serializes commands, so a stale UI-side caret never misplaces text — fast typing and async clipboard stay correct. This is *the* invariant that makes the UI race-free.
- **Hit-testing** (`engine-wasm`): `document_geometry` flattens the box tree into per-line `CaretSlot`s (absolute x ↔ source byte), inverting the renderer's coordinate walk. pixel→logical, logical→caret-rect, and selection rects all go through it. Selection rects are per-line bounding boxes at the time (discontinuous BiDi rects shipped later, see "Where the deferred work landed" below).
- **`HiddenInput`** (`components/HiddenInput.tsx`): the `<textarea>` is the OS text-input citizen. `beforeinput` → engine commands; IME via `Begin`/`Update`/`EndComposition`, committed on end, with an inline underlined on-canvas preview during composition (shipped in Phase 5, see below); native `copy`/`cut`/`paste` events → the async `navigator.clipboard`. It tracks the caret so IME popups anchor, and refocuses on `pointerup` — a canvas click blurs focus to `<body>` first.
- **Caret / selection / a11y are DOM overlays**, not canvas-drawn — `CaretOverlay`, `SelectionOverlay`, and a visually-hidden `AccessibilityTree` (`role="document"`, one `<p dir>` per paragraph, `<span>` per style run; the browser's UAX-#9 handles BiDi for the screen reader). The worker broadcasts an `AccessibilityTreeDelta` (changed-paragraph patches) after every doc mutation (fine-grained deltas shipped in Phase 5, see below). Engine geometry is device-px; overlays divide by `devicePixelRatio`.
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
  **ungated** — it was bounded by incremental relayout, which shipped in Phase 5 (see "Where the deferred work landed" below).
- **PDF/A-1b (D5.4).** `format-pdf` emits true PDF/A-1b for `PdfProfile::A1b`;
  `crates/format-pdf/build.rs` synthesizes the sRGB ICC profile — no binary
  blob in the tree. `tools/pdf-validate` is the veraPDF harness.
- **Fuzzing (D5.5, scaled up by issue #90).** `fuzz/` is a cargo-fuzz crate
  in its **own workspace** with four structure-aware targets, all
  compile-checked on stable via `cargo check --manifest-path fuzz/Cargo.toml`
  (`cargo +nightly fuzz run` is the nightly flow, `.github/workflows/
  fuzz-nightly.yml`, ≥ 30 min/target with `-fork=4` so one already-known
  crash doesn't stop a whole session, crash minimization + `gh issue create`
  auto-filing):
  - `docx_reader` / `docx_roundtrip` (new) — `fuzz/src/docx_gen.rs` builds
    schema-shaped WML (random `pPr`/`rPr`/`tbl`/`sectPr` trees, valid and
    deliberately-invalid attributes) inside a minimal OPC zip, not raw
    bytes, then `read_docx` / `read → write → read`.
  - `rpc_command` — `fuzz/src/command_gen.rs` derives `arbitrary::Arbitrary`
    for `Command` (bridge's `arbitrary` feature, optional + off by default,
    zero cost to the wasm build) and drives sequences end to end through
    the real `Engine::apply` dispatcher via `Engine::apply_sync` (the
    `engine-wasm` `fuzz-native` feature, also off by default) — no browser
    needed: `Engine::new_headless` skips the `OffscreenCanvas` requirement,
    and `apply`'s auto-repaint still runs the full layout pipeline (only
    the final canvas blit is unreachable, and already skipped whenever no
    canvas is registered).
  - `layout_paginate` (new) — `fuzz/src/layout_gen.rs` builds random
    paragraph/table/section trees straight into the paginator; a page-count
    bound stands in for a termination watchdog (`Engine::
    layout_page_count_for_fuzzing`).
  - `fuzz/examples/smoke.rs` is a stable-only driver (no nightly needed)
    proving all four work: `cargo run --manifest-path fuzz/Cargo.toml
    --example smoke --release`.
  - Issue #229 — the `rpc_command` corpus's #186/#187 regression seeds are
    derived from `command_gen::Scenario`'s explicit builder (a fixed-prefix
    fast path in `gen_targeted_command`, immune to unrelated arms' byte-
    consumption changes) rather than hand-tuned raw bytes; regenerate them
    with `cargo run --manifest-path fuzz/Cargo.toml --example regen-seeds`,
    and `cargo test --manifest-path fuzz/Cargo.toml` (now also in `ci.yml`'s
    `rust-native` job) fails loudly if they go stale.
  - Bounded post-processing (`fuzz-nightly.yml`): crash minimization has a
    per-job budget (`minimize_budget_secs`, default 900 s; <= 180 s per
    artifact, crash/leak/oom only — `timeout-*` is never tmin'd; past the budget `.min` = the
    original) and filing is capped at 3 new issues per target per run, with
    all `timeout-*` artifacts folded into ONE issue (deduped on the smallest
    reproducer's sha256) and the overflow listed in the last issue's body.
    A finding never turns the job red; only infrastructure errors do.
- **Telemetry (D5.7).** Schema in `crates/bridge/src/telemetry.rs`; the UI
  collector `ts/src/state/telemetry.ts` batches samples and `console.log`s
  them every 60 s — a **mock** transport (no live collector for the MVP).
- **Release pipeline (D5.8).** `.github/workflows/release.yml` is
  tag-triggered (`v*`): builds the WASM artifact + static site + SBOM and
  publishes a GitHub Release. Cosign signing is a commented-out stub.
- **CI.** A **non-blocking** `qa-harness` job in `ci.yml` runs the
  `tools/visual-diff --tier A` farm — non-blocking because golden
  pixel-reproducibility on the GitHub runner is unproven across machines;
  `tools/memory-profile` / `tools/perf` are not wired into `ci.yml` at all
  (their heavier fixtures blew the runner's time cap — run them locally or
  on a nightly schedule). Issue #258 added `tools/pdf-validate --corpus
  tier-a --profile 1b` to the same job (~9 s locally for all 6 documents,
  reusing the wasm build the visual-diff step already needs). Issue #393
  installs veraPDF 1.30.2 on the runner (pinned official installer zip +
  SHA-256, headless IzPack install into `~/verapdf`, `actions/cache`d), so
  both pdf-validate steps run the real conformance checks under `--strict`
  (a missing veraPDF fails instead of skipping). The Playwright e2e suite (`ts/e2e/`)
  became a **blocking** `e2e` job later, issue #230 — see the Validation
  section.

## SDK architecture — the "Monaco Standard" (post-`beta.3`)

The shell is split into a headless SDK + an isolated default UI, following the
Monaco Editor's "Locked Surface with Headless Controls" precedent. The split is
**load-bearing**: violations are never acceptable shortcuts.

- **`pnpm` workspace** at the repo root. `pnpm-workspace.yaml` globs
  `packages/*`, `ts`, `tools/*`. Inter-package deps use `workspace:*`.
- **`@nge/core`** (`packages/core/`) — the Locked Surface + Headless API.
  - Contains: `EditorSurface` (owns `<canvas>` + hidden `<textarea>` +
    `transferControlToOffscreen` lifecycle), `EngineProvider` (Solid
    context), `createEditorCommands` (typed facade over `dispatch`),
    `createEditorState` (Solid signals derived from engine events).
  - Re-exports every bridge type (`Command`, `Event`, `TextAttrsPatch`,
    `BridgeCellBorders`, `InsertSide`, `PageOrientation`, `ListKind`,
    `ParagraphStyleId`, `STYLE_PRESETS`, …). Downstream UI imports
    **only from `@nge/core`** — never from `crates/engine-wasm/pkg`.
  - Built with **Solid.js primitives** (`createSignal`, `createEffect`,
    `onCleanup`, `useContext`). No React patterns.
- **`@nge/ui`** (`packages/ui/`) — the default UI shelf.
  - Strict **`.nge-*` CSS namespace prefix** on every selector.
    Theme variables on `:root` / `.nge-root` (e.g. `--nge-color-primary`,
    `--nge-space-3`, `--nge-radius-md`). Sibling `Component.css`
    imports per component.
  - **Zero Tailwind. Zero Shadcn. Zero external UI libraries.**
  - Modals use `solid-js/web` `<Portal>` (escapes the canvas z-index).
- **Framework rule.** The shell is **Solid.js** (`vite-plugin-solid`,
  `jsxImportSource: "solid-js"`). Default to Solid primitives, never
  React hooks / lifecycle. The existing `ts/src/App.tsx` is Solid; the
  SDK packages are Solid; downstream code is Solid.
- **Migration anchor.** `ts/src/sdk-bridge.tsx` wires the concrete
  `EngineClient` (still living in `ts/src/engine/`) into
  `EngineProvider` and mounts the `@nge/ui` shelf. Eventually
  `EngineClient` moves into `@nge/core`; that refactor is a separate
  PR.

## Clean-room reference protocol (load-bearing, legal)

Copyleft competitor source (OnlyOffice AGPL, LibreOffice MPL) is cloned for
*study* at `/data/code/reference/` — **outside** this repo. The full protocol
is `plans/cleanroom/PROTOCOL.md`. The hard rules:

- **The implementing context never opens any file under
  `/data/code/reference/`.** Only dedicated Reader subagents do, and their
  sole output is a sanitized methods memo (ideas, math, prose, paths-as-
  pointers — never code, identifiers, constants, or case-ordering).
- Never copy/symlink/commit anything from the reference trees into this repo.
- Reader memos live in `plans/cleanroom/` and pass a leakage review before
  use. Feature indexes: `plans/cleanroom/{onlyoffice,libreoffice}.yaml`.
- Copyleft study is the last resort — spec / UAX / literature / permissive
  code / black-box diffing come first (preference order in PROTOCOL.md).

This project is `MIT OR Apache-2.0`; the wall is what keeps it that way.

## "Honest UX" discipline — never ship Phantom UI

If an engine path is stubbed, missing, or partial, the UI must **visibly
gate** the affordance. Three discipline rules, in priority order:

1. **No silent dead buttons.** A button that dispatches into a
   `phase3_stub` / `Event::Error` path must render `disabled` with an
   amber **"Engine pending"** badge (the live instance is `@nge/ui`
   `ImageWrapPicker.tsx`; the snippet is in `.claude/rules/
   sdk-architecture.md`). The badge element carries
   `data-nge-command="<WIRE_NAME>"` + `data-nge-pending-issue="<n>"`
   matching the command's `bridge::meta` status — `tools/parity`
   (issue #342) fails CI on a badge without them, a badge that
   disagrees with meta, a `Partial` with no badge, or a `Stub` exposed
   on the facade or in the UI.
2. **Log the gap immediately.** When you discover a missing core
   feature, a pragmatic workaround, or tech debt outside the current
   sprint's scope, use the `gh-issue-logger` skill (`/gh-issue-logger`)
   **before** concluding your turn. The skill enforces the issue
   format and label discipline.
3. **Surface the wire shape, not the user wish.** When the spec calls
   for a command the bridge does not have, audit the bridge first
   (`grep crates/bridge/src/`). Surface the gap via `AskUserQuestion`
   before writing speculative Rust. Memory rule
   [[feedback_pragmatic_scope]] is load-bearing.

The standing backlog of "engine pending" issues lives in the GitHub
Issues tracker — labels `core-engine` / `ui` / `enhancement` /
`tech-debt`. Each Sprint X (UI Edition) JSDoc note that says "see Core
Engine backlog" references a real issue.

## Validation (CI gates, all -D warnings)

- `cargo fmt --all -- --check` clean.
- `cargo fmt --manifest-path fuzz/Cargo.toml -- --check` clean (issue #412 —
  `fuzz/` is its own workspace, so `--all` does not reach it).
- `cargo clippy --workspace --all-targets -- -D warnings` clean.
- `cargo test --workspace` (native unit tests), **plus** `cargo test -p
  engine-wasm --features fuzz-native` (issue #321): the bridge-level tests
  that drive real `Command`s through `Engine::apply` natively
  (`fuzz_native_surface_drives_engine_end_to_end`, the two `OpenDocument.defaults`
  bridge tests) are `cfg`-gated on that off-by-default
  feature, so `--workspace` alone collects 0 of them. CI's `rust-native`
  job runs it and fails if the named tests did not execute.
- `wasm-pack test --headless --chrome crates/engine-wasm` (browser unit tests).
- `wasm-pack build --release` then assert artifact `< 15728640` bytes.
- `cargo run -p shape-regression --release` — 0 failed on the corpus.
  **CI-enforced since issue #287** (`rust-native`, ~4 s warm).
- `cargo run -p roundtrip --release` — PASS, and `-- --fixtures` (33
  fixtures). **CI-enforced since issue #287** (`rust-native`, ~40 s warm +
  <1 s); the step tees its output to `roundtrip-dump/*.log`, uploaded as the
  `roundtrip-dump` artifact on failure (the harness has no separate
  diff-file dump — its `FAIL:` line carries the inline diff).
- `tools/visual-diff` on the goldens — every case ≤ **2 %** pixel diff (most cases 0.000 %).
- `pnpm -r test` (issue #332) — the TypeScript **unit** tests: `vitest`
  (pinned, workspace root dev dep; shared node-environment config in
  `vitest.shared.ts`, **no jsdom** — a module under test must not touch
  the DOM), `fake-indexeddb` for `ts/src/engine/event-log.ts`, a scripted
  fake `Worker` for `EngineClient`. Tests sit beside the code
  (`*.test.ts`, `src/**` of `ts/`, `packages/core`, `packages/ui`) and run in
  about a second. Pure TS logic (event-log scoring / pruning / package GC /
  the #314 write-confirm, the #333 retry schedule, `recoveryNotices()`,
  `nextCleanState`, `devHooksEnabled` / `resolveTelemetryEndpoint`) is
  tested here, **not** through Playwright; the e2e suite keeps what needs a
  real browser + the wasm engine. `engine.worker.ts` imports the wasm
  engine and cannot be loaded by vitest: extract a pure decision into its
  own module (as `retry-schedule.ts`) to unit-test it. CI runs it as the
  `unit` step of the `e2e` job, before Playwright.
- `pnpm exec playwright test` (from `ts/`) — the full e2e suite in `ts/e2e/`
  (`workers: 1`, well under a minute locally) all green.
  **Blocking since issue #230**: `ci.yml`'s `e2e` job reuses the `wasm`
  job's build (`actions/upload-artifact` / `download-artifact` of
  `crates/engine-wasm/pkg` — TS imports it by relative path, no npm
  indirection, so no Rust toolchain / wasm-pack rerun), ensures
  `google-chrome-stable` is present (`channel: 'chrome'` — no Playwright
  browser download), then runs `pnpm exec playwright test --reporter=line`
  with `CI=true`; `ts/test-results/` uploads on failure. `playwright.config.ts`
  sets `retries: process.env.CI ? 1 : 0` — a rare cold-Vite-dep-cache flake
  observed once locally ("Execution context was destroyed" mid-`evaluate`),
  not a mask for a repeatable failure; local runs stay retry-free.
- `cargo run -p parity --release -- --verify-issues` (issue #342) — the
  Command parity matrix (`bridge::meta` × facade map × UI badges × e2e ×
  fuzz) into the `rust-native` job summary; its floor also runs as
  `parity`'s unit tests in `cargo test --workspace`.
- `cargo check --manifest-path fuzz/Cargo.toml` — the D5.5 fuzz crate
  compiles. `cargo test --manifest-path fuzz/Cargo.toml` (issue #229) — the
  fuzz crate's own unit tests, including the #186/#187 regression-seed
  checks. Both run inside `ci.yml`'s blocking `rust-native` job, alongside
  fmt/clippy/`cargo test --workspace` — not a separate silent lane.
- CI (`ci.yml`), blocking: `rust-native` (fmt + clippy + `cargo test
  --workspace` + `--features fuzz-native` + shape-regression + roundtrip + the two fuzz-crate steps
  above; 30 min cap), `wasm` (build + size
  budget + `wasm-pack test` + the `engine-wasm-pkg` artifact upload),
  `e2e` (this suite, issue #230). Non-blocking (`continue-on-error: true`):
  `qa-harness` runs `tools/visual-diff --tier A` (capped at 3 min) then
  `tools/pdf-validate --corpus tier-a --profile 1b --strict` (issue #258,
  capped at 1 min; real veraPDF 1.30.2 since issue #393) then
  `tools/pdf-validate --native --strict` (issue #393, browserless Rust
  export of the Latin + Arabic CFF / TrueType fixtures, capped at 2 min;
  its `cargo test -p format-pdf --lib --no-run` pre-build is a separate
  uncapped step) — the whole job stays non-blocking because golden pixel-reproducibility on the GitHub
  runner's Chrome is still unproven across machines. `tools/memory-profile`
  and `tools/perf` are **not** wired into `ci.yml` at all — the heavier
  fixtures (100p/250p/500p) blew the runner's time cap; run them locally
  (`node tools/memory-profile/run.mjs --budgets`, `node tools/perf/run.mjs
  --strict`) or against a dedicated nightly runner.

## Visual-diff harness

- **Playwright with `channel: 'chrome'`** — uses system Chrome, no 150 MB chromium download.
- `chrome --virtual-time-budget` alone **does not wait for real I/O** (network, WASM compile). Use `page.waitForFunction(() => window.__paintIdle)` instead.
- Per-case viewport mapping in `tools/visual-diff/run.mjs` `VIEWPORTS` map. A4 cases get 595×842 (1 pt = 1 px); single-glyph cases get 400×400.
- Tests pass `?test=<case>` which hides UI chrome so the canvas is the only thing in the screenshot.
- `UPDATE=1` env var regenerates the golden. Every regeneration must be eyeballed in the diff before merging.

## Known issue — headless Chrome does not composite the interactive canvas

A headless-Chrome screenshot of the **full interactive app** (`localhost:5173/`,
the Solid `App`) shows a **blank page**. This is a headless-only compositing
artifact — **not an engine bug. Do not re-investigate it.**

- The engine renders correctly: `render_canvas2d` paints every glyph and a
  `get_image_data` readback confirms the pixels land on the `OffscreenCanvas`.
- Headless Chrome simply never syncs the `transferControlToOffscreen`
  placeholder `<canvas>` to the displayed DOM for the full app. It is not the
  DOM nesting, the canvas creation path, DPR, fonts, or a boot race — all were
  ruled out (Phase 5 backlog sprint 7 investigation).
- The `?test=<case>` visual-diff harness path **does** screenshot correctly in
  the same headless Chrome — so the golden suite is unaffected and trustworthy.
- The real app renders perfectly in a normal (non-headless) Chrome window.

**Verify interactive-app rendering in a real browser — never via a headless
screenshot.** Headless screenshots are valid only for the `?test=` harness.

### Live validation hooks (issue #340)

`window.__dispatch`, `__engineClient`, `__fontRegistry`,
`__setTelemetryEnabled`, `__telemetryFlush`, `__clipboardPrefetch` and
`__lastStats` — and the `?telemetryEndpoint=` and `?clipboardPrefetch=0`
URL parameters, and the `@nge/ui` `SettingsMenu`'s URL-driven renderer
switch (`<EngineProvider debugSurfaces>`, issue #389) — are
installed/honoured **only**
when `devHooksEnabled()` (`ts/src/dev-hooks.ts`): the Vite dev server
(`import.meta.env.DEV`), a `?test=` page, or a build made with
**`VITE_NGE_DEV_HOOKS=1`**. Live validation against a **built** bundle
(`vite build` + `vite preview`, or any non-dev deploy) therefore needs
`VITE_NGE_DEV_HOOKS=1 pnpm exec vite build`; a plain release build exposes no
engine handle on `window` (`ts/e2e/prod-build.spec.ts` builds both ways and
asserts it, plus no `#stats` box, `?clipboardPrefetch=0` ignored, no renderer
switch). The passive status flags — exactly `__paintIdle`, `__engineReady`,
`__renderer`, `__recovered` and `__bootMs` — are the ONLY unconditional
`window.__*` values: they are not capabilities and a production smoke test
waits on them. The visible stats readout is the Dev HUD (Ctrl+Shift+D), not a
fixed `#stats` box; production kill switches are build constants
(`VITE_NGE_CLIPBOARD_PREFETCH=0`, `VITE_NGE_UNLOAD_GUARD=0`) or provider
props, never URL parameters.
`playwright.config.ts` sets the flag on its dev server. The telemetry
collector endpoint is the build constant `VITE_NGE_TELEMETRY_ENDPOINT` (or
`<EngineProvider telemetryEndpoint>`); the URL parameter works only under the
flag. The SDK packages (`@nge/core`, `@nge/ui`) install no globals.

## Editor invariants

- Document model is **immutable + structurally shared** (`im::Vector<Paragraph>`). Cloning a tree is O(1).
- **`UndoStack`** is bounded (depth 100). Pushing a new snapshot truncates the redo branch.
- After every mutation, if a `layout_cfg` was cached, the engine **auto-repaints** the full document and invalidates the `DirtyTracker`. `Command::RequestPaint` does a clipped partial repaint (D3.8); the auto-repaint path stays full.
- BiDi runs **per line**, not paragraph-wide. UAX #9 requires this. Don't flatten visual order across line breaks.
- Line break opportunities come from `icu_segmenter::LineSegmenter::new_auto()`. Greedy fit is fine for PoC; Knuth-Plass is Phase 3.

## `.docx` round-trip invariants

- The reader stashes every non-`word/document.xml` archive entry verbatim in `DocxArchive.other_entries`.
- The writer emits those entries **byte-identical** + a freshly serialized `word/document.xml`. Don't re-serialize content types or rels.
- **Edit-drift bound (issue #251) — fidelity first, size second.** The
  primary bound is `edit_check.source_bytes_rewritten == 0`: an edited save
  must not respell or drop a single byte of the ORIGINAL `word/document.xml`
  — everything the edit changes must be a pure insertion. The secondary
  bound is size: `document.xml` byte delta ≤ `2 × inserted UTF-8 bytes` +
  a per-new-run allowance (48 B/run — the measured ≈43 B markup cost of an
  empty `<w:r><w:t xml:space="preserve"></w:t></w:r>` wrapper, rounded up;
  `new_run_count` comes from a cheap tag-count heuristic, not a real diff),
  since a faithful insertion may legitimately need to mint a new `<w:r>`
  (e.g. appending after a differently-styled run, or opening a self-closing
  `<w:p/>`). Both bounds live on `EditCheck` in `tools/corpus-native/src/
  pipeline.rs` and are asserted the same way in `tools/roundtrip`'s default
  step 6b/6c. The old size-only `≤ 2×N` bound is kept as an informational
  column (`bound_bytes` / `within_bound`) — it cannot distinguish a
  faithful insertion from a lossy regeneration that happens to land in
  bounds (issue #199: a fix that drove `source_bytes_rewritten` down from
  134 to a small residual simultaneously drove the old bound's violation
  count *up*, 82 → 92, because faithful new runs cost bytes a silent
  regeneration didn't).
- Every document that still rewrites source bytes gets a cheap root-cause
  tag (`hyperlink` / `comment anchor` / `form field` / `sdt` / `fldSimple`
  / `move` / `table` / `rPr` / `other`, plus the one-byte shapes
  `empty <w:p/>` (#267) and `t preserve`, issue #248) so the corpus can be
  tracked against the filed issues (#242–#249) — see
  `tools/corpus-native`'s `classify_rewrite` and `report.mjs`'s
  root-cause histogram.
- XML escapes: `&` `<` `>` only. `xml:space="preserve"` on every `<w:t>` to keep trailing whitespace.

## Bash / agent ergonomics

- **Working dir drifts** between Bash tool calls. Use absolute paths or `cd /home/ibrahim/Desktop/code/next-gen-editor &&` at the top of every multi-step command.
- Long-running processes (vite dev, wasm-pack build) run in `run_in_background: true`.
- Don't `git add .` blindly. Stage by explicit path.
- Commit messages: heredoc + a `Co-Authored-By:` trailer naming the model that wrote the change (e.g. `Claude Fable 5.1`, `Claude Opus 5.5`, `Claude Sonnet 5`, each `<noreply@anthropic.com>`); the session that merges adds its `Claude-Session:` link.
- **Parallel agents in git worktrees.** A shared `CARGO_TARGET_DIR` across
  worktrees is *unsound*: cargo fingerprints workspace-relative paths, so a
  sibling worktree's stale rlib (built from different sources) satisfies your
  fingerprint and you link against their version — phantom "missing field"
  errors and false-green gates. Rules: agents `touch` every `.rs` and rebuild
  immediately before their gates (or use a private target dir when disk
  allows); merge gates on `main` run only in the private `target-main/`
  cache (gitignored) that nothing else writes to; judge every gate by exit
  code. Large merge-conflict hunks are rebuilt by construction, never
  keep-both — the shared closing brace may belong to different modules.
  A merge gate on `main` must also compile the browser unit tests for
  wasm32 — `cargo test -p engine-wasm --target wasm32-unknown-unknown
  --no-run` — because `wasm-pack test` builds the whole test module for
  wasm32 and a native-only `cfg` on a helper that tests use breaks CI while
  every native gate stays green (2026-09-25 incident).
  Disk is the binding constraint on this 8-core / 15 GB box: the shared
  `target/` alone is ~20 GB.
  Playwright ports are per-checkout too (issue #205): `ts/playwright.config.ts`
  derives its port from a stable hash of the absolute repo path (5200–5999)
  instead of a fixed 5173, so a sibling worktree's already-running Vite
  server can no longer be silently reused as the wrong tree under test —
  override with `PW_PORT`, or opt back into reuse with `PW_REUSE_SERVER=1`.

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
- ❌ Default to React patterns. The shell + SDK are Solid.js. Use
  `createSignal` / `createEffect` / `<Show>` / `<For>` / `<Portal>`.
- ❌ Add a Tailwind / Shadcn / external UI library to `@nge/ui`. Vanilla
  CSS with `.nge-*` prefix + CSS variables. No exceptions.
- ❌ Import from `crates/engine-wasm/pkg` outside `@nge/core`. The SDK
  boundary owns that coupling.
- ❌ Ship a UI button that silently dispatches into a stubbed engine
  path ("Phantom UI"). Disable + amber "Engine pending" badge + filed
  GitHub issue is the only acceptable state.
- ❌ Discover a missing core engine feature mid-sprint and forget to
  file the issue. Use `/gh-issue-logger` before concluding the turn.

## Where the deferred work landed

Phases 1–4 are complete; Phase 5's engineering deliverables shipped at
`v0.5.0-beta.1`. Twelve post-`beta.1` backlog sprints then closed the bulk of
the old backlog (now migrated to GitHub Issues) — rich-text decorations +
bold/italic faces (with the Vello path now applying the same faux synthesis
as Canvas2D), tatweel-glyph Kashida, incremental relayout, dynamic line
height, paragraph auto-direction, discontinuous BiDi selection rects, the
toolbar pickers, pending formatting, rich clipboard + the `.docx` `<w:rPr>`
round-trip, PDF `FlateDecode` + `/ToUnicode`, fine-grained accessibility
deltas, the Vello/WebGPU render-path activation, the inline IME composition
preview, and core keyboard navigation (arrow keys with ideal-x, Shift-extend,
`Ctrl/Cmd+A`, triple-click paragraph selection) — cut `v0.5.0-beta.3`. The
SDK split and the Sprint 1–14 (UI Edition) waves then shipped the `@nge/ui`
shelf and viewport-culled lazy pagination (`LazyLayoutState` +
`Command::ExpandLayout`). The cut is `v0.6.0-beta.2`. PDF/A-2u and PDF/X-3
conformance shipped after that (GitHub issue #28, closed).

PDF font subsetting shipped too (issue #327: the `subsetter` crate, glyph
ids renumbered through content codes / `/W` / `/ToUnicode` / `/CIDSet`,
+67 KB raw wasm; `tools/pdf-validate` gates one-page exports at < 10 % of
their fonts' raw size), as did the CFF font type (issue #361: an `.otf`
with CFF outlines embeds as `CIDFontType0` + `FontFile3 /CIDFontType0C`;
`tools/pdf-validate --native` veraPDF-checks a test-time-synthesized CFF
font, no browser needed).

Still open, tracked in `gh issue list`: Vello as the
*default* renderer (issue #1 — the harness has a `--renderer vello` mode
with committed `golden/vello/` goldens, and runtime activation is verified
on real GPU hardware; the remaining gap is a GPU CI runner + default
promotion); IME `target_range` sub-segment styling (issue #2); and stable
per-paragraph accessibility ids.

Phase 5 → MVP hand-off:

- The bridge schema grew **additively** throughout — the latest additions are
  the D5.7 `telemetry` module and the §10 `AccessibilityTreeDelta`. Phase-1 PoC
  commands (`RenderPage`, `RasterizeGlyph`, `ShapeAndRasterize`, `LoadDocx`,
  `SaveDocx`) are still live for the visual-diff `?test=` harness.
- `Command::Recover` is real since issue #85 (`Engine::snapshot()` /
  `Engine::restore()`, persisted event-log snapshots, replayed tail — see
  the Phase 2 section). `EngineStats.last_paint_ms` / `last_command_ms` and
  `Event::Painted.paint_ms` are still `0.0` dummies — the D5.7 telemetry
  pipeline is wired and will carry real numbers once they are.
- Remaining for the MVP `v0.1.0`: D5.6 (external security audit), D5.9
  (operator runbook), D5.10 (Arabic typography sign-off), then the §10 exit
  gate. `v0.6.0-beta.2` is the engineering-complete beta.
