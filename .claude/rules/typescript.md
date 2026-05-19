---
description: TypeScript + worker + browser conventions.
paths:
  - "ts/**/*.ts"
  - "ts/**/*.tsx"
  - "ts/**/*.html"
  - "tools/**/*.mjs"
  - "ts/tsconfig.json"
  - "ts/vite.config.ts"
---

# TypeScript / browser rules

## Config
- TS strict + `noUncheckedIndexedAccess` + `noImplicitOverride` + `exactOptionalPropertyTypes`.
- `target: ES2024`, `module: ESNext`, `moduleResolution: Bundler`.
- `types: ["node", "vite/client"]` — `vite.config.ts` uses `node:path`.

## tsify-next interop
- `Option<T>` in Rust → `T | undefined` in `.d.ts`. **Pass `undefined`, not `null`** from TS.
- `Vec<u8>` with `#[serde(with = "serde_bytes")]` + `#[tsify(type = "Uint8Array")]` → pass `Uint8Array` directly. Without `serde_bytes`, decoder demands a number-array and balks at `Uint8Array`.

## Worker boot
- `wasm-pack --target web` output. `init({ module_or_path: new URL('../../crates/engine-wasm/pkg/engine_wasm_bg.wasm', import.meta.url) })`.
- `OffscreenCanvas` transferred via `canvas.transferControlToOffscreen()` exactly once at INIT; included in `postMessage` `transfer` list.
- `crossOriginIsolated === true` is an invariant; warn if false.

## RPC channel
- Worker handles two top-level message kinds: `INIT` (one-shot setup) and `COMMAND` (issued by main after init).
- Main side: Promise-based `dispatch(cmd) → Event` with `{ type: 'COMMAND', id, cmd }` requests + `{ type: 'COMMAND_RESULT', id, event }` replies. Track `pending` map by id.
- Expose `window.__dispatch` for Playwright test hooks. Also `window.__paintIdle` and `window.__engineReady`.

## Vite config
- `server.fs.allow` must include the workspace root so the worker can import `../crates/engine-wasm/pkg/*`.
- `server.headers` + `preview.headers`: COOP `same-origin`, COEP `require-corp`, CORP `same-origin`.
- `worker.format: 'es'`.
- `strictPort: true` to avoid silent port shifts.

## DOM / input
- Single hidden `<textarea id="input">` overlays canvas at `opacity: 0.01` (NOT `0` — Safari skips events on opacity:0), `pointer-events: auto`.
- `beforeinput` listener handles `insertText`. Other `inputType`s (delete, paragraph) wired in Phase 4.
- Test mode (`?test=<case>`): hide chrome (`status.style.display = 'none'`).

## CSS conventions
- Canvas sized in main thread before `transferControlToOffscreen()`. Per-case dimensions in `canvasSizeForCase(testCase)`.
- A4 cases: 595 × 842 pt (canvas pixels = engine `A4Page::a4()` 1:1).

## Watch out for
- `cwd` drifts between bash sessions. Prefer absolute paths in scripts.
- `chrome --virtual-time-budget` does NOT wait for real I/O. Use `page.waitForFunction(() => window.__paintIdle)`.
- Filter `Failed to load resource` console errors (browser auto-fetches `/favicon.ico`).
- Worker `console.log` goes to `console`, not main-thread devtools tabs.
