---
description: Visual-diff harness rules — Playwright + golden management.
paths:
  - "tools/visual-diff/**"
  - "tests/corpus/**"
  - "ts/src/index.ts"
  - "ts/src/engine.worker.ts"
---

# Visual-diff rules

## Harness setup
- **Playwright with `channel: 'chrome'`** — uses system Chrome, NO 150 MB chromium download.
- `chromium.launch({ headless: true, channel: 'chrome' })`.
- Per-case viewport in `VIEWPORTS` map at top of `tools/visual-diff/run.mjs`:
  - A4 cases (`a4-justified-mixed`, `editing-arabic`, `docx-round-trip`): `595 × 842`
  - Single-glyph + single-word cases (`glyph-a`, `hello-latin`, `hello-arabic`): `400 × 400`
- `VIEWPORT=WxH` env var overrides.

## Waiting for paint
- **Never use `chrome --virtual-time-budget` alone** for capture. It does NOT wait for real network/WASM I/O — produces blank screenshots.
- Canonical readiness signal: `page.waitForFunction(() => window.__paintIdle === true, { timeout: 15000 })`.
- The worker posts `{ type: 'IDLE' }` after the test-case sequence completes; `index.ts` sets `window.__paintIdle = true` on receipt.

## Golden management
- Goldens live in `tools/visual-diff/golden/<case>.png`. Committed.
- `UPDATE=1 node run.mjs <case>` regenerates and overwrites the golden.
- **Every regeneration MUST be eyeballed** in the diff before merging. The harness only confirms reproducibility, not correctness.
- Tolerance: `0.02` (2 %) default; per-case override via 2nd CLI arg.

## Test-case routing
- `?test=<case>` query param hides UI chrome (status div, page padding) so the canvas is the only content captured.
- The worker switches on `testCase` to decide which fonts to load + what to render.
- Adding a new case: (1) register viewport in `run.mjs` `VIEWPORTS` map; (2) add case in `engine.worker.ts` switch; (3) add case in `canvasSizeForCase()` if A4; (4) run `UPDATE=1` to seed golden.

## Console error filtering
- Filter `Failed to load resource` errors — these are browser auto-fetches (`/favicon.ico`) not page errors.
- Real `pageerror` events fail the test.

## CI thresholds
- Tier A (deterministic): 100 % pass rate, ≤ 0.5 % per-case diff.
- Tier B (LibreOffice parity, Phase 3+): ≥ 80 % pass rate, ≤ 2 % per-case diff.
- Tier C (Word parity, best-effort): ≥ 60 % pass rate, ≤ 5 % per-case diff.

Phase 1 PoC ships with all 6 cases at **0.000 % diff**.
