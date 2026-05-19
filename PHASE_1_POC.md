# Phase 1 — Engine Proof of Concept (PoC)

> **Parent:** [`MASTER_PLAN.md`](./MASTER_PLAN.md) §2.
> **Owning tracks:** A (Engine Core), F (DevOps).
> **Calendar:** Months 0–6.
> **Exit gate:** §11.

---

## 1. Objective

Prove that a Rust → WebAssembly pipeline can:

1. Load and render `.docx` content containing mixed Arabic + English text on a `<canvas>`.
2. Accept basic editing commands (insert, delete, cursor).
3. Round-trip a `.docx` file (open → edit → save) with ≥95 % byte equivalence in unedited regions.
4. Hit the artifact budget: **≤15 MB compressed WASM, ≤3 s cold-start on M2-class hardware.**

PoC is a **feasibility filter**. No UI polish, no production server, no styling beyond what proves the architecture.

---

## 2. Deliverables

| ID | Deliverable | Acceptance signal |
| --- | --- | --- |
| D1.1 | Rust workspace + WASM toolchain | `wasm-pack build --release` green; CI passing |
| D1.2 | Engine ↔ Worker boot | TS demo loads engine, awaits `Ready`, sends ping in <500 ms |
| D1.3 | OffscreenCanvas binding | First non-trivial pixel rendered from WASM on canvas |
| D1.4 | Font loader | Noto Sans Arabic + Inter resident in WASM; glyph metrics dumped to console |
| D1.5 | Static text PoC | `"Hello مرحبا"` rendered (single line, mixed direction) |
| D1.6 | Cursor + text insert | Click sets caret; keypress inserts; arrow keys navigate |
| D1.7 | `.docx` round-trip | Tier-1 5-page fixture: open → save → diff ≤1 % outside edit region |
| D1.8 | CI green | Native + WASM tests + size budget all green on main |

---

## 3. Repository scaffold

```
next-gen-editor/
├── Cargo.toml                     (workspace root)
├── rust-toolchain.toml            (pinned stable channel)
├── .cargo/config.toml             (wasm32-unknown-unknown flags)
├── crates/
│   ├── bridge/                    (RPC types shared with TS via tsify)
│   ├── engine/                    (pure Rust facade; no wasm dep)
│   ├── engine-wasm/               (#[wasm_bindgen] surface)
│   ├── text-pipeline/             (BiDi, shape, line-break, justify)
│   ├── layout/                    (paragraph/page/table boxes)
│   ├── render/                    (display list + Vello + Canvas2D backends)
│   └── format-docx/               (reader/writer)
├── ts/
│   ├── package.json
│   ├── tsconfig.json
│   ├── vite.config.ts
│   ├── src/
│   │   ├── index.ts               (PoC demo entry)
│   │   ├── engine-client.ts
│   │   └── engine.worker.ts
│   └── public/
│       └── fonts/                 (Noto Sans Arabic, Inter)
├── infra/
│   ├── server/Caddyfile           (local dev w/ COOP/COEP)
│   └── docker/Dockerfile          (production static server)
├── tools/
│   ├── shape-regression/          (HarfBuzz parity)
│   ├── bidi-regression/           (UCD BidiTest sampled)
│   ├── visual-diff/               (pixelmatch harness)
│   └── perf/                      (cold-start, scroll fps)
├── tests/
│   └── corpus/                    (.docx fixtures)
└── .github/workflows/
    └── ci.yml
```

---

## 4. Canonical `Cargo.toml`

```toml
[workspace]
members = [
    "crates/bridge",
    "crates/engine",
    "crates/engine-wasm",
    "crates/text-pipeline",
    "crates/layout",
    "crates/render",
    "crates/format-docx",
]
resolver = "2"

[workspace.package]
version = "0.1.0"
edition = "2024"
license = "MIT OR Apache-2.0"
rust-version = "1.85"

[workspace.dependencies]
# Bridge / FFI
wasm-bindgen = "0.2"
wasm-bindgen-futures = "0.4"
js-sys = "0.3"
web-sys = { version = "0.3", features = [
    "DedicatedWorkerGlobalScope", "MessagePort", "MessageEvent",
    "OffscreenCanvas", "OffscreenCanvasRenderingContext2d",
    "ImageBitmap", "ImageData", "console",
] }
console_error_panic_hook = "0.1"
serde = { version = "1", features = ["derive"] }
serde-wasm-bindgen = "0.6"
tsify = { version = "0.5", default-features = false, features = ["js"] }

# Text / Unicode
icu = "1.5"
icu_bidi = "1.5"
icu_segmenter = "1.5"
icu_normalizer = "1.5"
icu_locale = "1.5"
rustybuzz = "0.20"
swash = "0.2"
read-fonts = "0.27"

# Structures
im = "15"
smallvec = "1"
indexmap = "2"
lru = "0.12"

# Codecs
quick-xml = "0.36"
rc-zip = "5"

# Errors / logging
thiserror = "1"
anyhow = "1"
tracing = "0.1"

[profile.release]
opt-level = "z"
lto = "fat"
codegen-units = 1
panic = "abort"
strip = true
```

---

## 5. WASM target flags (`.cargo/config.toml`)

```toml
[build]
target = "wasm32-unknown-unknown"

[target.wasm32-unknown-unknown]
rustflags = [
    "-C", "target-feature=+bulk-memory,+mutable-globals,+sign-ext,+nontrapping-fptoint,+simd128",
    "-C", "link-arg=--initial-memory=67108864",   # 64 MB
    "-C", "link-arg=--max-memory=2147483648",     # 2 GB
    "-C", "link-arg=--stack-size=1048576",        # 1 MB stack
    "-C", "link-arg=--export=__heap_base",
]
```

---

## 6. `rust-toolchain.toml`

```toml
[toolchain]
channel = "1.85"
components = ["clippy", "rustfmt", "rust-src"]
targets = ["wasm32-unknown-unknown"]
profile = "minimal"
```

---

## 7. PoC engine surface (`crates/engine-wasm/src/lib.rs`)

```rust
use wasm_bindgen::prelude::*;
use bridge::{Command, Event};

#[wasm_bindgen(start)]
pub fn boot() {
    console_error_panic_hook::set_once();
}

#[wasm_bindgen]
pub struct Engine {
    inner: engine::Engine,
}

#[wasm_bindgen]
impl Engine {
    #[wasm_bindgen(constructor)]
    pub fn new(canvas: web_sys::OffscreenCanvas) -> Result<Engine, JsValue> {
        let inner = engine::Engine::new(canvas).map_err(jsv)?;
        Ok(Engine { inner })
    }

    pub async fn dispatch(&mut self, cmd: JsValue) -> Result<JsValue, JsValue> {
        let cmd: Command = serde_wasm_bindgen::from_value(cmd)?;
        let evt: Event = self.inner.apply(cmd).await.map_err(jsv)?;
        Ok(serde_wasm_bindgen::to_value(&evt)?)
    }

    pub async fn load_font(&mut self, id: String, bytes: Vec<u8>) -> Result<(), JsValue> {
        self.inner.load_font(id, bytes).await.map_err(jsv)
    }

    pub fn stats(&self) -> JsValue {
        serde_wasm_bindgen::to_value(&self.inner.stats()).unwrap()
    }
}

fn jsv<E: std::fmt::Display>(e: E) -> JsValue {
    JsValue::from_str(&e.to_string())
}
```

---

## 8. PoC bridge types (`crates/bridge/src/lib.rs`)

PoC carries only what's needed for milestones D1.2 → D1.7. Full schema lands in Phase 2.

```rust
use serde::{Serialize, Deserialize};
use tsify::Tsify;

#[derive(Serialize, Deserialize, Tsify, Clone, Debug)]
#[tsify(into_wasm_abi, from_wasm_abi)]
#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Command {
    Ping,
    OpenDocx { bytes: Vec<u8> },
    SaveDocx,
    InsertText { at: LogicalPos, text: String },
    SetSelection { caret: LogicalPos },
    RequestPaint { viewport: Rect },
}

#[derive(Serialize, Deserialize, Tsify, Clone, Debug)]
#[tsify(into_wasm_abi, from_wasm_abi)]
#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Event {
    Pong,
    DocumentLoaded { pages: u32 },
    DocumentSaved { bytes: Vec<u8> },
    Painted { dirty: Rect, paint_ms: f32 },
    SelectionChanged { caret: Rect },
    Error { message: String },
}

#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug)]
pub struct LogicalPos { pub para: u32, pub offset: u32 }

#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug)]
pub struct Rect { pub x: f32, pub y: f32, pub w: f32, pub h: f32 }
```

---

## 9. Canvas binding mechanic

Main thread transfers the canvas to the worker exactly once:

```ts
// ts/src/index.ts
const canvas = document.querySelector<HTMLCanvasElement>('#doc')!;
const dpr = window.devicePixelRatio;
canvas.width = canvas.clientWidth * dpr;
canvas.height = canvas.clientHeight * dpr;
const offscreen = canvas.transferControlToOffscreen();

const worker = new Worker(new URL('./engine.worker.ts', import.meta.url), { type: 'module' });
worker.postMessage({ type: 'INIT', canvas: offscreen, fonts: ['inter.ttf', 'noto-arabic.ttf'] }, [offscreen]);
```

Worker forwards to WASM Engine and gets the 2D context (PoC) or WebGPU context (Phase 3):

```rust
let ctx: web_sys::OffscreenCanvasRenderingContext2d = canvas
    .get_context("2d")?
    .ok_or_else(|| JsValue::from_str("no 2d ctx"))?
    .dyn_into()?;
ctx.set_fill_style(&JsValue::from_str("#00ff00"));
ctx.fill_rect(0.0, 0.0, 10.0, 10.0); // PoC sentinel: green square confirms binding
```

---

## 10. Six-month milestone breakdown

| Week | Task | Owner | Done when |
| --- | --- | --- | --- |
| 1 | Workspace skeleton, `cargo new` for each crate, root `Cargo.toml` lockable, `rust-toolchain.toml`, `.cargo/config.toml` | A | `cargo check --workspace` green |
| 2 | GitHub Actions: `cargo fmt`, `cargo clippy -D warnings`, `cargo test`, `wasm-pack build`, size-budget step (15 MB cap) | F | CI green on empty crates |
| 3 | `wasm-bindgen` hello world: `Engine::dispatch(Command::Ping) → Event::Pong` round-trip in headless Chrome via `wasm-bindgen-test` | A | `wasm-pack test --headless --chrome` green |
| 4 | TS scaffold (Vite + TS strict), worker boot, OffscreenCanvas transfer, green-square sentinel | A + F | Demo page renders green square |
| 5 | `swash` + `read-fonts` integration; load `inter.ttf` from `/fonts/` via `fetch`; expose glyph metrics struct | C | `console.log` dumps glyph metrics for 'A' |
| 6 | `swash` raster of one ASCII glyph via 2D canvas; visual diff vs golden | C | Single glyph rendered, diff ≤2 % |
| 7 | `rustybuzz` integration; shape `"hello"`; visual diff | C | Word "hello" matches golden |
| 8 | Load Noto Sans Arabic; shape `"السلام"`; visual diff | C | Arabic word rendered with correct joining forms |
| 9 | Full Arabic test corpus (50 strings) vs HarfBuzz CLI reference; integrate into `tools/shape-regression/` | C | All strings match `hb-shape` output |
| 10 | `icu_bidi` integration; analyze `"Hello عربي world"`; assert paragraph dir + reordered levels | C | Unit test passes |
| 11 | Render mixed-direction line with correct visual reorder; visual diff | C | Mixed line matches golden |
| 12 | `icu_segmenter::LineSegmenter`; greedy line break; multi-line Arabic paragraph | C + A | Long Arabic paragraph wraps + visual matches |
| 13 | Kashida candidate identification (UAX rules); simple Kashida distribute | C | Justified Arabic line visually matches Word reference |
| 14 | Page model (A4, margins); single page renders multi-paragraph fixture | A | Page renders with correct margins |
| 15 | Document model: persistent tree via `im`; `Command::InsertText` mutates tree | A | Tree mutation tested natively |
| 16 | Cursor + caret rect; `Command::SetSelection` returns `Event::SelectionChanged` with rect | A | Click on canvas updates caret |
| 17 | `Command::InsertText` end-to-end: type via keyboard, document tree mutates, repaint dirty region only | A + E | Typing in demo works |
| 18 | Undo/redo via snapshot stack | A | Ctrl+Z / Ctrl+Y restores state |
| 19 | `.docx` reader: ZIP via `rc-zip` + `word/document.xml` via `quick-xml`; paragraphs + runs with basic char fmt | B | Tier-1 fixture loads into document model |
| 20 | Visual: opened doc matches reference golden for fixture (Tier-A tolerance) | B + G | Visual diff <2 % |
| 21 | `.docx` writer: write-back preserving unedited XML byte-equivalent where possible | B | Diff <1 % outside edit region |
| 22 | Edit-and-save round-trip: open → insert paragraph → save → reopen → verify | B | E2E test green |
| 23 | Performance pass: cold start, paint p95; tune until budgets hit | F + A | Cold start <3 s, paint p95 <16 ms |
| 24 | PoC review + exit-gate validation; write PoC retrospective; sign off Phase 2 kickoff | All | Exit-gate §11 all green |

---

## 11. Exit gate

Each command must exit 0. Failure → escalate to architecture review before Phase 2 starts.

```bash
# 1. Build
wasm-pack build --target web --release crates/engine-wasm

# 2. Size budget (15 MB)
test "$(wc -c < crates/engine-wasm/pkg/engine_wasm_bg.wasm)" -lt 15728640

# 3. Native tests
cargo test --workspace --release

# 4. WASM tests
wasm-pack test --headless --chrome crates/engine-wasm

# 5. Shape regression vs HarfBuzz CLI
cargo run --release --bin shape-regression -- tests/corpus/shape/

# 6. BiDi conformance (sampled UCD BidiTest)
cargo run --release --bin bidi-regression -- tests/corpus/bidi/BidiTest-sampled.txt

# 7. Visual: hello-arabic golden
node tools/visual-diff/run.mjs --case hello-arabic --tol 0.02

# 8. Cold start budget (M2 hardware, 3 runs averaged)
node tools/perf/cold-start.mjs --max-ms 3000

# 9. Round-trip on Tier-1 corpus
node tools/roundtrip/run.mjs tests/corpus/tier-1/*.docx --tol 0.01
```

---

## 12. Risk register (PoC-specific)

| # | Risk | Likelihood | Detection | Mitigation |
| --- | --- | --- | --- | --- |
| 1 | `rustybuzz` lacks needed GSUB/GPOS for Arabic feature corpus | Med | Week 9 corpus diff | Escape to HarfBuzz-WASM (extra 800 KB; deferred until proven needed) |
| 2 | OffscreenCanvas missing in Safari Tech Preview by month 6 | Low | Browser matrix CI | Fallback: post `ImageBitmap` from worker to main; render via main-thread canvas |
| 3 | WASM artifact >15 MB at week 4 | Med | CI size step | Audit `icu` features; switch to `icu_provider_blob` with locale subset |
| 4 | Cold start >3 s by week 23 | Med | CI perf step | Lazy-instantiate non-core crates; stream WASM via `WebAssembly.compileStreaming` |
| 5 | `.docx` round-trip diff >1 % | High | Week 22 test | Defer fidelity polish to Phase 5; PoC passes if structure intact, diff acceptable for unedited regions |
| 6 | `wasm-bindgen` ABI churn breaks build | Low | CI | Pin exact crate versions; renovate weekly |

---

## 13. Hand-off into Phase 2

PoC outputs:

- Stable workspace, crates, toolchain.
- Working `Engine::dispatch` with PoC-subset `Command/Event` schema.
- Worker boot + canvas transfer pattern.
- Font + shape + BiDi + line-break + Kashida pipeline (single-pass MVP fidelity).
- `.docx` reader + writer (Tier-1 fixtures only).
- Visual-diff harness scaffold ready for full corpus in Phase 3+.

Phase 2 expands the bridge to full `Command/Event` schema, hardens memory + crash recovery, and adds COOP/COEP + SAB.
