# Next-Gen Web Document Editor

A Rust + WebAssembly document editor with **first-class Arabic / RTL support**.
No `iframe`s, no vendored binary blobs — the engine is built from source.

[![ci](https://github.com/IbraheemAlz/next-gen-editor/actions/workflows/ci.yml/badge.svg)](https://github.com/IbraheemAlz/next-gen-editor/actions/workflows/ci.yml)
[![pages](https://github.com/IbraheemAlz/next-gen-editor/actions/workflows/pages.yml/badge.svg)](https://github.com/IbraheemAlz/next-gen-editor/actions/workflows/pages.yml)
![license](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)

> **Status — Phase 5 engineering complete (`v0.5.0-beta.1`).**
> A fully interactive editor: a Solid.js UI shell over the Rust/WASM engine —
> click-to-place caret, drag selection, native IME (Arabic + CJK), a
> formatting toolbar, the system clipboard, drag-drop `.docx` open, and a
> screen-reader accessibility tree — on the Phase 1–3 engine (BiDi,
> priority-band Kashida, multi-script font fallback, hierarchical box model).
> Phase 5 added **PDF/A-1b export**, a **QA harness farm** (visual-diff /
> memory / performance), a **`cargo-fuzz`** suite, **telemetry** scaffolding
> and a tag-triggered **release pipeline**. This is a **beta** — the external
> security audit, operator runbook and Arabic typography sign-off are still
> pending. Roadmap in [`MASTER_PLAN.md`](MASTER_PLAN.md); deferred scope in
> [`BACKLOG.md`](BACKLOG.md).

## Live demo

**https://ibraheemalz.github.io/next-gen-editor/**

Click to place the caret and type — Arabic and English edit in real time
(mixed BiDi). Drag to select, format with the toolbar, `Ctrl+C` / `Ctrl+V`
to copy-paste, or drop a `.docx` file onto the page to open it.

## Why this exists

Existing browser editors either ship a 2-year-stale OnlyOffice WASM blob
(57 MB, unbuildable) behind an `iframe`, or have no real RTL support at
all. This project builds a lean engine from source, in Rust, with Arabic
as a day-one requirement rather than an afterthought.

## Features

- **Rust → WASM engine** — ~3.2 MB artifact, ~21 % of the 15 MiB budget.
- **Native Arabic shaping** — `rustybuzz` (pure-Rust HarfBuzz); correct
  cursive joining, initial/medial/final forms.
- **Unicode BiDi** — `unicode-bidi`, per-line resolution for mixed
  Arabic / English paragraphs.
- **Priority-band Kashida** — Microsoft P1–P5 justification bands, with
  candidates resolved from the Unicode `Joining_Type` property.
- **Multi-script font fallback** — a `FontStack` resolves a covering font
  per script, so Arabic and Latin share a line on one baseline.
- **Hierarchical box model** — `PageBox → ParagraphBox → LineBox →
  VisualRun → PositionedGlyph`, parent-relative coordinates.
- **Rich-text formatting** — per-character style spans (font size +
  colour) applied via `ApplyFormatting`.
- **PDF / PDF-A export** — single-page PDF with full `Type0`/`CIDFontType2`
  font embedding (`pdf-writer`); a PDF/A-1b archival mode adds an embedded
  sRGB output intent + XMP metadata, with the ICC profile synthesized at
  build time rather than vendored.
- **Incremental repaint** — a `DirtyTracker` clips Canvas2D draws to the
  changed region.
- **`.docx` round-trip** — read + write, sibling archive entries
  preserved byte-identical.
- **Interactive UI shell** — a Solid.js app over the engine: click-to-place
  caret, drag selection, double-click word select, DOM caret + selection
  overlays, and a formatting toolbar (bold / italic / underline, size,
  colour, undo / redo).
- **Native IME + clipboard** — a hidden-`<textarea>` input path; Arabic
  types directly, CJK composes through the OS IME and commits correctly.
  Async system clipboard (copy / cut / paste) and drag-drop `.docx` open.
- **Screen-reader accessibility** — a synchronized shadow DOM mirrors the
  document (`role="document"`, one `<p dir>` per paragraph); BiDi is handled
  by `dir` + the browser's UAX-#9, with an `aria-live` region for
  announcements.
- **Headless architecture** — WASM engine in a dedicated Web Worker,
  `OffscreenCanvas` rendering, typed RPC bridge. No `iframe`. A WebGPU /
  Vello renderer is plumbed for a future activation.
- **QA + release infrastructure** — a tiered Playwright visual-diff farm,
  memory-snapshot and performance harnesses, a `cargo-fuzz` suite (`.docx`
  reader + RPC schema), mock telemetry batching, and a tag-triggered release
  pipeline that builds the artifact + an SBOM.

## Architecture

```
Browser main thread          Dedicated Web Worker
──────────────────           ────────────────────
TS shell + <canvas>   ◄────►  Rust WASM engine
hidden <textarea>     RPC      ├ document model + undo + style spans
                               ├ text pipeline (BiDi, shape, break, justify, fonts)
                               ├ layout (hierarchical box model)
                               ├ Canvas2D renderer (+ Vello plumbed)
                               ├ .docx reader / writer
                               └ PDF export
```

Engineering invariants are documented in [`CLAUDE.md`](CLAUDE.md).

## Build from source

Prerequisites: Rust 1.95 (`rust-toolchain.toml` pins it), `wasm-pack`,
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

The `dist/` folder is a self-contained static site. Serve it with the
cross-origin isolation headers (`COOP: same-origin`, `COEP: require-corp`)
that the Vite config already sets for dev + preview — `SharedArrayBuffer`
and the engine worker depend on them.

## Project structure

```
crates/
  bridge/         RPC Command/Event types (serde + tsify-next)
  engine/         document model (im::Vector) + undo + style spans
  engine-wasm/    #[wasm_bindgen] surface
  text-pipeline/  fonts + FontStack, shaping, BiDi, line break, justify, script
  layout/         hierarchical box model + paragraph layout
  render/         Canvas2D + Vello backends, DirtyTracker
  format-docx/    .docx reader + writer
  format-pdf/     PDF export with font embedding
ts/               Vite + TypeScript shell, worker, dispatch channel
tools/
  visual-diff/    Playwright golden farm (tiered)
  memory-profile/ engine + JS heap snapshot harness
  perf/           cold-start + insert-latency + open-doc harness
  pdf-validate/   veraPDF PDF/A-1b validation harness
  perf-fixtures/  generates the synthetic perf .docx load files
  shape-regression/  rustybuzz output snapshots
  roundtrip/      .docx open → edit → save → byte-diff harness
fuzz/             cargo-fuzz crate — .docx reader + RPC command targets
MASTER_PLAN.md    macro architecture + 5-phase roadmap
PHASE_*.md        per-phase execution plans
BACKLOG.md        deferred scope + technical debt
```

## Roadmap

| Phase | Scope |
| --- | --- |
| **1 — PoC** ✅ | Engine, shaping, BiDi, layout, editing, `.docx` round-trip |
| **2** ✅ | Worker bridge hardening, full RPC schema, memory + crash recovery |
| **3** ✅ | Box model, priority Kashida, font fallback, rich text, PDF export, dirty tracking |
| **4** ✅ | Headless UI shell: Solid.js, pointer + caret + selection, IME, toolbar, accessibility, clipboard, drag-drop |
| **5 — Hardening** 🚧 | QA harness farm (visual-diff / memory / perf), `cargo-fuzz`, PDF/A-1b export, telemetry, release pipeline — engineering complete (`v0.5.0-beta.1`); external security audit, operator runbook + Arabic typography sign-off pending |

See [`MASTER_PLAN.md`](MASTER_PLAN.md) and the `PHASE_*.md` documents.

## Known limitations

- Bold / italic / underline are stored and round-trip (the toolbar reflects
  them), but not yet rendered — bold/italic font faces and underline strokes
  are deferred, along with paragraph alignment, rich (HTML / `.docx`)
  clipboard, and `.docx` run-formatting round-trip — see
  [`BACKLOG.md`](BACKLOG.md).
- Kashida elongation widens glyph advances; true `U+0640` tatweel-glyph
  insertion is deferred.
- Vello / WebGPU is plumbed but Canvas2D is the active renderer.
- Opening or editing a multi-page document relays out every paragraph — fast
  for one page (insert p95 ≈ 10 ms), but the synthetic 50-page open takes
  ~9 s. Incremental relayout is the open performance item — see
  [`BACKLOG.md`](BACKLOG.md).

## Licenses

Code is dual-licensed **MIT OR Apache-2.0** — see [`LICENSE-MIT`](LICENSE-MIT)
and [`LICENSE-APACHE`](LICENSE-APACHE).

Bundled fonts under `ts/public/fonts/` ship under their own licenses:

| Font | License |
| --- | --- |
| Amiri | SIL Open Font License 1.1 |
| Noto Naskh Arabic | SIL Open Font License 1.1 |
| Liberation Sans | SIL Open Font License 1.1 |
