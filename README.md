# Next-Gen Web Document Editor

A Rust + WebAssembly document editor with **first-class Arabic / RTL support**.
No `iframe`s, no vendored binary blobs — the engine is built from source.

[![ci](https://github.com/IbraheemAlz/next-gen-editor/actions/workflows/ci.yml/badge.svg)](https://github.com/IbraheemAlz/next-gen-editor/actions/workflows/ci.yml)
[![pages](https://github.com/IbraheemAlz/next-gen-editor/actions/workflows/pages.yml/badge.svg)](https://github.com/IbraheemAlz/next-gen-editor/actions/workflows/pages.yml)
![license](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)

> **Status — Phase 1 proof-of-concept complete (v0.0.1).**
> Document model, editing, undo/redo, `.docx` round-trip, native Arabic
> shaping + BiDi + A4 layout. The full multi-phase roadmap lives in
> [`MASTER_PLAN.md`](MASTER_PLAN.md).

## Live demo

**https://ibraheemalz.github.io/next-gen-editor/**

Click the page and type. Arabic and English both render (mixed BiDi).
`Ctrl+Z` / `Ctrl+Y` undo / redo.

## Why this exists

Existing browser editors either ship a 2-year-stale OnlyOffice WASM blob
(57 MB, unbuildable) behind an `iframe`, or have no real RTL support at
all. This project builds a lean engine from source, in Rust, with Arabic
as a day-one requirement rather than an afterthought.

## Features (Phase 1 PoC)

- **Rust → WASM engine** — ~1.9 MB artifact, 12 % of the 15 MiB budget.
- **Native Arabic shaping** — `rustybuzz` (pure-Rust HarfBuzz); correct
  cursive joining, initial/medial/final forms.
- **Unicode BiDi** — `unicode-bidi`, per-line resolution for mixed
  Arabic / English paragraphs.
- **Line breaking** — `icu_segmenter` (UAX #14).
- **A4 page layout** — 595 × 842 pt, justification with a basic Kashida
  elongation strategy.
- **Document model** — immutable `im::Vector` tree, bounded undo/redo.
- **`.docx` round-trip** — read + write, sibling archive entries
  preserved byte-identical.
- **Interactive editor** — type Arabic + English, live repaint.
- **Headless architecture** — WASM engine in a dedicated Web Worker,
  `OffscreenCanvas` rendering, typed RPC bridge. No `iframe`.

## Architecture

```
Browser main thread          Dedicated Web Worker
──────────────────           ────────────────────
TS shell + <canvas>   ◄────►  Rust WASM engine
hidden <textarea>     RPC      ├ document model + undo
                               ├ text pipeline (BiDi, shape, break, justify)
                               ├ layout (A4 page, paragraphs)
                               ├ Canvas2D renderer
                               └ .docx reader / writer
```

Engineering invariants are documented in [`CLAUDE.md`](CLAUDE.md).

## Build from source

Prerequisites: Rust 1.85 (`rust-toolchain.toml` pins it), `wasm-pack`,
Node ≥ 22, `pnpm`.

```bash
# Build the WASM engine
wasm-pack build --target web --release crates/engine-wasm

# Run the dev server
cd ts
pnpm install
pnpm dev          # → http://localhost:5173
```

Production build:

```bash
wasm-pack build --target web --release crates/engine-wasm
cd ts && pnpm install && pnpm build    # → ts/dist/
```

The `dist/` folder is a self-contained static site — serve it from any
static host. (For `SharedArrayBuffer`-dependent features in later phases
you'll need `COOP`/`COEP` headers; Phase 1 does not require them.)

## Project structure

```
crates/
  bridge/         RPC Command/Event types (serde + tsify-next)
  engine/         document model (im::Vector) + undo stack
  engine-wasm/    #[wasm_bindgen] surface
  text-pipeline/  fonts, shaping, BiDi, line break, justify
  layout/         A4 page + paragraph layout
  render/         Canvas2D backend
  format-docx/    .docx reader + writer
ts/               Vite + TypeScript shell, worker, dispatch channel
tools/
  visual-diff/    Playwright + pixelmatch golden suite
  shape-regression/  rustybuzz output snapshots
  roundtrip/      .docx open → edit → save → byte-diff harness
MASTER_PLAN.md    macro architecture + 5-phase roadmap
PHASE_*.md        per-phase execution plans
```

## Roadmap

| Phase | Scope |
| --- | --- |
| **1 — PoC** ✅ | Engine, shaping, BiDi, layout, editing, `.docx` round-trip |
| 2 | Worker bridge hardening, full RPC schema, memory + crash recovery |
| 3 | Vello/WebGPU renderer, font fallback, real Kashida, font shaping fidelity |
| 4 | Headless UI: caret, selection, IME, accessibility |
| 5 | Hardening: visual-diff farm, fuzzing, PDF/A export, release validation |

See [`MASTER_PLAN.md`](MASTER_PLAN.md) and the `PHASE_*.md` documents.

## Known Phase 1 limitations

- Editing appends at end-of-document; no click-to-place caret (Phase 4).
- No backspace / newline key handling yet (Phase 2 commands + Phase 4 wiring).
- Kashida is a coarse uniform elongation, not Microsoft priority bands (Phase 3).
- No engine-side font fallback — the interactive editor uses the dual-script
  Amiri font to cover Latin + Arabic (Phase 3 adds a proper FontStack).

## Licenses

Code is dual-licensed **MIT OR Apache-2.0** — see [`LICENSE-MIT`](LICENSE-MIT)
and [`LICENSE-APACHE`](LICENSE-APACHE).

Bundled fonts under `ts/public/fonts/` ship under their own licenses:

| Font | License |
| --- | --- |
| Amiri | SIL Open Font License 1.1 |
| Noto Naskh Arabic | SIL Open Font License 1.1 |
| Liberation Sans | SIL Open Font License 1.1 |
