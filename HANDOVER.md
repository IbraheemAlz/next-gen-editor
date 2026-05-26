# Handover — Windows toolchain bring-up + GPU/PDF gate closure

**Branch:** `windows-bringup-gpu-pdf`
**Author:** Sadeq Majed Al-Sayed Ahmad (`info@logatta.com`)
**Date:** 2026-05-26
**Closes:** #1

## What I did

Stood up the full Windows toolchain on a fresh machine, ran every CI gate in the repo, fixed every harness and engine bug that surfaced, and flipped the Vello/WebGPU renderer on so GPU verification can happen in dev.

## Tests run + results

All gates executed against the real engine on this machine (Chrome 148.0.7778.168, discrete GPU with WebGPU enabled).

| Gate | Command | Result |
|---|---|---|
| Format | `cargo fmt --all -- --check` | ✓ clean |
| Lint | `cargo clippy --workspace --all-targets -- -D warnings` | ✓ clean |
| Native unit tests | `cargo test --workspace` | ✓ all pass |
| WASM unit tests | `wasm-pack test --headless --chrome crates/engine-wasm` | ✓ 1/1 pass |
| WASM size budget | `wasm-pack build --release crates/engine-wasm --target web` | ✓ 4.08 MiB / 15 MiB |
| Shape regression | `cargo run -p shape-regression --release` | ✓ 6/6 pass |
| `.docx` round-trip | `cargo run -p roundtrip --release` | ✓ PASS (Δ 20 B, bound 40 B) |
| Fuzz crate compile | `cargo check --manifest-path fuzz/Cargo.toml` | ✓ compiles |
| TS e2e (Playwright) | `pnpm exec playwright test` | ✓ 7/7 pass |
| Visual-diff tier A | `node tools/visual-diff/run.mjs --tier A` | ✓ 7/7 (0.000%) |
| Visual-diff tier B | `node tools/visual-diff/run.mjs --tier B` | ✓ 7/7 |
| Visual-diff tier C | `node tools/visual-diff/run.mjs --tier C` | ✓ 7/7 |
| Perf tier-2 | `node tools/perf/run.mjs` | ✓ cold 355 ms, p95 5.12 ms, open-50p 2349 ms |
| Memory profile 50p | `node tools/memory-profile/run.mjs` | ✓ engine 83.6 / 128 MiB, jsHeap 4.5 / 64 MiB |
| Memory profile 100p | (same) | ✓ engine 101.8 / 192 MiB, jsHeap 6.0 / 96 MiB |
| Memory profile 250p / 500p | (same) | ✶ engine-bound (see "Deferred" below) |
| PDF/A-1b structural | `node tools/pdf-validate/run.mjs` | ✓ 8/8 markers |
| PDF/A-1b veraPDF | `verapdf -f 1b tmp/pdf-validate/seeded-default.pdf` | ✓ **isCompliant=true** |
| **GPU / Vello e2e** | Vello on, Playwright | ✓ 7/7 |
| **GPU / Vello visual-diff** | Vello on, all 3 tiers | ✓ 7/7 / 7/7 / 7/7 (goldens regenerated) |
| **GPU / Vello perf** | Vello on | ✓ tier-2 budgets met |

## Errors fixed

### 1. `wasm-pack test` — chromedriver / Chrome version skew

**Error**
```
chromedriver version: 149.0.7827.22
Error: http status: 404
```
`wasm-bindgen-test-runner` ships chromedriver 149; system Chrome is 148.

**Fix.** Downloaded chromedriver 148.0.7778.167 from the Chrome-for-Testing CDN and replaced the binary `wasm-bindgen-test-runner` caches at `%LOCALAPPDATA%\.wasm-pack\chromedriver-*\chromedriver.exe`. After the swap: `1 passed; 0 failed`.

### 2. `tools/perf` — stale `LogicalPos` shape

**Error**
```
decode command: Error: missing field `path`
```
Harness sent `INSERT_TEXT.at = { para: 0, offset: i }`, but the bridge schema migrated to `LogicalPos = { path: BlockPath, offset: u32 }` in Phase 5 PR 4 (table-cell-aware caret).

**Fix** — `tools/perf/run.mjs`:
```diff
-                at: { para: 0, offset: i },
+                at: { path: { steps: [{ kind: 'BLOCK', idx: 0 }] }, offset: i },
```

### 3. `crates/format-pdf` — PDF/A-1b non-compliant: missing per-CID `/W` widths

**Error** (from veraPDF):
```
isCompliant="false"
Glyph width 473 in the embedded font program is not consistent with
the Widths entry of the font dictionary (value 1000)
... (×4400 errors of the same kind)
```
The CID font dictionary only carried `/DW 1000`, no `/W` array. PDF/A-1b §6.3.5 requires per-CID widths matching the embedded font program's `hmtx`.

**Fix**
- `crates/text-pipeline/src/fonts.rs` — added `LoadedFont::widths_em1000() -> Vec<f32>` that reads each glyph's `hmtx` advance via `rustybuzz::Face::glyph_hor_advance` and scales to the 1000-em PDF convention.
- `crates/format-pdf/src/lib.rs` — replaced the hard-coded `cid.default_width(1000.0)` with `cid.default_width(0.0)` + `cid.widths().consecutive(0, face.widths_em1000())`.

After: `isCompliant="true"`. PDF size 216 KB → 243 KB (the `/W` array for one full Amiri face).

### 4. `tools/memory-profile` — hung past 50p

The harness reused one browser across all four profiles; the second `newContext()` then hung on `LOAD_DOCX` due to accumulated WASM state, and the `REQUEST_STATS` reply was queued behind a 100+ s synchronous auto-repaint.

**Fix** — `tools/memory-profile/run.mjs`:
- Launch a fresh browser per profile.
- Bump `page.goto` / `waitForFunction` timeouts to 60 s / 120 s.
- Race the `REQUEST_STATS` dispatch against a 30-min ceiling so a stuck dispatch errors instead of hanging indefinitely.
- Treat a `null` stats reply as **fail** (was previously treated as 0 MiB and silently passed).
- Log progress at every step.

After: 50p and 100p both produce real heap numbers, both within budget.

### 5. Vello / WebGPU runtime activation (Issue #1)

`detect_backend` was hardcoded to `Canvas2d`, with the wgpu probe kept reachable but commented out of the decision.

**Fix**
- `crates/render/src/backend.rs` — `detect_backend` now calls `request_gpu_device()` and returns `Vello` when the adapter is acquirable; dropped the `#[allow(dead_code)]` from `request_gpu_device`.
- `ts/src/engine/engine.worker.ts` — the harness INIT path (`?test=` visual-diff cases) now also picks Vello via `detect_backend`. Previously only the interactive `EngineClient` path did, so the goldens always ran on Canvas2D even after the engine flip.
- `tools/visual-diff/golden/*.png` — regenerated on Vello (the seven Canvas2D-captured goldens differed from Vello rasters by 0.14–2.4%). Original Canvas2D goldens preserved at `tools/visual-diff/golden.canvas2d.bak/` (gitignored).

After:
- Probe says `renderer: "vello"`.
- e2e 7/7 on Vello.
- visual-diff tier A 7/7 (0.000%) on Vello.
- perf tier-2 passes on Vello (cold start +120 ms vs Canvas2D for WebGPU device acquisition).

Closes issue #1 acceptance criteria 1–4:
1. ✓ Application runs on GPU hardware with Chrome WebGPU.
2. ✓ `window.__renderer === 'vello'`.
3. ✓ Vello golden suite captured at ~0.5 % tolerance.
4. ✓ Vello selected as default when a WebGPU adapter is present.

## Deferred (not in scope of this PR)

- **Memory-profile 250p / 500p.** The engine relays out the entire document on every mutation (BACKLOG.md item #13 — viewport culling / incremental relayout). `REQUEST_STATS` queues behind a multi-minute auto-repaint for 250 + page documents; the 30-min harness ceiling is reached. Not a harness bug — a known engine arch deferral.
- **Multi-page Vello.** Vello backend defaults its canvas to 300×150 when content exceeds one page (the Phase-6 multi-page Vello bug). All visual-diff goldens are single-page, so the bug does not bite the suite, but real multi-page documents will. Tracked in BACKLOG.md §4 — "Separate golden suite" + "Vello image decoding".
- **`/W` width array — subsetting.** The PR emits the full font's widths; subsetting would shrink the PDF further (BACKLOG.md item #3 remaining).
- **D5.6 / D5.9 / D5.10.** External security audit, operator runbook, Arabic typography sign-off — human deliverables, out of scope.

## Toolchain installed (portable, no admin needed)

| Tool | Version | Source |
|---|---|---|
| Rust | 1.95.0 (via rustup) | `winget install Rustlang.Rustup` |
| MSVC Build Tools | 2022 (Desktop C++) | `winget install Microsoft.VisualStudio.2022.BuildTools` |
| `wasm32-unknown-unknown` | — | `rustup target add wasm32-unknown-unknown` |
| `wasm-pack` | 0.15.0 | `cargo install wasm-pack` |
| Node | 24.16.0 | `winget install OpenJS.NodeJS.LTS` |
| pnpm | 11.3.0 | `npm install -g pnpm` |
| chromedriver | 148.0.7778.167 | Chrome-for-Testing CDN → `.driver/` |
| Oracle JDK | 21.0.11 | portable zip → `.tools/jdk-21.0.11/` |
| veraPDF | 1.26.5 | installer jar → `.tools/verapdf/` |

`WINDOWS_SETUP.md` documents the install steps end-to-end so the next teammate can replicate the bring-up in ~20 minutes.

## How to verify locally

```powershell
# from repo root
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -p shape-regression --release
cargo run -p roundtrip --release
wasm-pack build --release crates/engine-wasm --target web

# in another shell — start the dev server
cd ts; pnpm dev

# in the original shell
cd ts; pnpm exec playwright test
node ../tools/visual-diff/run.mjs --tier A
node ../tools/perf/run.mjs
node ../tools/memory-profile/run.mjs    # 50p + 100p will pass; 250p/500p engine-bound
node ../tools/pdf-validate/run.mjs
../.tools/verapdf/verapdf.bat -f 1b ../tmp/pdf-validate/seeded-default.pdf
```

## Files changed (commit summary)

**Engine (BACKLOG #3 + #4 partial):**
- `crates/render/src/backend.rs` — `detect_backend` returns Vello when WebGPU adapter present
- `crates/text-pipeline/src/fonts.rs` — added `LoadedFont::widths_em1000()`
- `crates/format-pdf/src/lib.rs` — emit per-CID `/W` array

**Worker:**
- `ts/src/engine/engine.worker.ts` — harness INIT path also uses `detect_backend`

**Harnesses:**
- `tools/perf/run.mjs` — `LogicalPos` shape migration
- `tools/memory-profile/run.mjs` — fresh browser per profile + honest timeout

**Goldens:**
- `tools/visual-diff/golden/*.png` — regenerated on Vello (7 files)

**Misc:**
- `.gitignore` — ignore `.tools/`, `.driver/`, `.claude/scheduled_tasks.lock`, golden backup
- `fuzz/Cargo.lock` — auto-touched by `cargo check`

**Docs:**
- `HANDOVER.md` (this file)
- `TEST_REPORT.md` — gate-by-gate detail
- `WINDOWS_SETUP.md` — fresh-machine bring-up guide
