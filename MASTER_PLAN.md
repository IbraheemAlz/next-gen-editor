# Next-Gen Web Document Editor — Engineering Master Plan

> **Status:** Architectural roadmap (pre-implementation).
> **Author:** Claude (acting Principal Architect).
> **Last updated:** 2026-05-19.

---

## 0. Context

We are building a **new web-based document editor** to replace dependence on outdated open-source solutions (e.g. `ranuts/document`, which vendors a 2-year-stale OnlyOffice 7.4/7.5 WASM blob, uses `iframe`-mode `DocsAPI.DocEditor`, and has zero Arabic/RTL support — see analysis in prior session).

Non-negotiable goals:

| Goal | Constraint |
| --- | --- |
| **.docx fidelity** | Pixel-perfect on print for Tier-1 features; managed expectations on Tier-3+ edge cases |
| **RTL / Arabic** | Native BiDi, shaping, Kashida justification, mixed-direction lines |
| **Headless UI** | No `iframe`. React/Vue chrome ↔ canvas-rendered engine over typed RPC |
| **Maintainability** | Modern toolchain, reproducible builds, no forks of 30-year-old C++ monoliths |
| **License** | LGPL/MPL/permissive only. **AGPL-3.0 disqualifies OnlyOffice** for our deployment model |
| **MVP scope** | Single-user edit + RTL. 18–24 month horizon |

Why a Rust-native core (vs. forking LibreOffice or OnlyOffice):

- **No 30-year codebase debt.** LibreOffice Core (LOWA via Allotropia) is research-grade WASM today; production-hardening it is a multi-year unpaid burden on a fork we don't control.
- **Memory safety by construction.** No `_malloc`/`_free` discipline, no segfaults in WASM heap, no UB. AI-assisted engineering benefits dramatically: Rust's type/borrow checker validates work in seconds instead of requiring weeks of manual C++ audit.
- **Modern, focused crate ecosystem already covers the hardest pieces** — `icu4x` (Google/Mozilla, designed for WASM), `rustybuzz` (pure-Rust HarfBuzz), `cosmic-text` (Arabic-tested), `swash`, `vello` (GPU 2D), `wasm-bindgen`. None require forking.
- **Smaller WASM artifact.** Target 8–15 MB vs. LibreOffice LOWA's 60–80 MB.
- **Honest tradeoff:** we own the layout engine. LibreOffice gives us a battle-tested engine but locks us into their architecture; Rust forces us to build layout but we control fidelity and roadmap.

---

## 1. Architecture Overview

```
┌──────────────────────────────────────────────────────────────┐
│  Browser main thread                                          │
│  ┌────────────────────────────────────────────────────────┐  │
│  │  Headless UI shell (React 19 / Vue 3.5 / Solid)         │  │
│  │  - Toolbars, menus, sidebars (DOM)                      │  │
│  │  - Reactive state (Zustand / Pinia) — UI-chrome only    │  │
│  │  - <canvas> element + transparent <textarea> for IME    │  │
│  └──────────┬──────────────────────────────────┬──────────┘  │
│             │ DOM events                       │ paint        │
│             │ (typed commands)                 │ (OffscreenCanvas transfer)
└─────────────┼──────────────────────────────────┼──────────────┘
              │                                  │
   postMessage / Comlink RPC          OffscreenCanvas + ImageBitmap
              │                                  │
┌─────────────▼──────────────────────────────────▼──────────────┐
│  Engine Worker (dedicated Web Worker, WASM thread isolated)    │
│  ┌─────────────────────────────────────────────────────────┐  │
│  │ Command Dispatcher  (serde-wasm-bindgen)                 │  │
│  └─────────────────────────────┬───────────────────────────┘  │
│  ┌─────────────────────────────▼───────────────────────────┐  │
│  │ Document Model  (persistent tree via `im` crate)         │  │
│  │  - Undo/redo as cheap structural snapshots               │  │
│  │  - Selection / cursor / IME composition                  │  │
│  │  - CRDT-ready spine (Yrs interop later for collab)       │  │
│  └─────────────────────────────┬───────────────────────────┘  │
│  ┌─────────────────────────────▼───────────────────────────┐  │
│  │ Layout Engine  (custom, BiDi-aware)                      │  │
│  │  - Paragraph / table / page boxes                        │  │
│  │  - Line breaking via `icu_segmenter`                     │  │
│  │  - Page master, headers/footers                          │  │
│  └─────────────────────────────┬───────────────────────────┘  │
│  ┌─────────────────────────────▼───────────────────────────┐  │
│  │ Text Pipeline                                             │  │
│  │  - `icu_bidi`        — Unicode Bidirectional Algorithm   │  │
│  │  - `icu_segmenter`   — line/word break                    │  │
│  │  - `icu_normalizer`  — NFC/NFKC                          │  │
│  │  - `rustybuzz`       — shaping (Arabic, Indic, complex)  │  │
│  │  - `swash`/`read-fonts` — font parsing + rasterization   │  │
│  └─────────────────────────────┬───────────────────────────┘  │
│  ┌─────────────────────────────▼───────────────────────────┐  │
│  │ Renderer  (display list → painter)                       │  │
│  │  - `vello` (WebGPU) primary; OffscreenCanvas 2D fallback │  │
│  │  - Tiled rasterization for scroll perf                   │  │
│  └─────────────────────────────┬───────────────────────────┘  │
│  ┌─────────────────────────────▼───────────────────────────┐  │
│  │ Format I/O                                                │  │
│  │  - .docx reader (`quick-xml` + `rc-zip` + custom OOXML)  │  │
│  │  - .docx writer (round-trip preserving)                  │  │
│  │  - PDF export (`pdf-writer` + font subset)               │  │
│  └───────────────────────────────────────────────────────────┘  │
└───────────────────────────────────────────────────────────────┘
```

Hard rules:
- **WASM never touches the main thread.** Engine lives in a dedicated worker.
- **All UI state is in TS land.** The engine emits paint commands + selection rectangles; the UI doesn't reach into engine memory.
- **All engine state is in WASM land.** UI cannot directly mutate document model; everything is a typed command.
- **One transferable boundary.** Either `SharedArrayBuffer` for large bin transfers (with COOP/COEP), or `Transferable` `ArrayBuffer`/`ImageBitmap` posts.

---

## 2. Phase 1 — Engine PoC (Months 0–6)

### 2.1 Decision: Skip the engine-fork comparison

Original prompt asked OnlyOffice vs LibreOffice. Both are eliminated:
- OnlyOffice → AGPL-3.0 → disqualified.
- LibreOffice LOWA → MPL/LGPL-OK, but Allotropia's WASM build is currently view-only, monolithic, and unmaintained as a productizable codebase. Forking and stabilizing it would burn 12+ months before any custom feature work.

**Decision: Build a focused Rust core. Use established crates for hard problems (Unicode, shaping, fonts, PDF). Write only what's missing (layout engine, .docx codec, command model, renderer glue).**

### 2.2 Crate stack (locked at PoC start)

| Concern | Crate | Why |
| --- | --- | --- |
| WASM bindings | `wasm-bindgen`, `wasm-bindgen-futures`, `js-sys`, `web-sys` | Industry standard |
| TS interop | `tsify`, `serde-wasm-bindgen` | Auto-generate `.d.ts` from Rust types |
| Build | `wasm-pack`, `wasm-opt` (binaryen) | Mature |
| Allocator | default → swap `wee_alloc` only if size budget demands it | Benchmark both |
| Document tree | `im` (Bodil Stokke's immutable structures) | Cheap snapshots = undo/redo |
| Async | `futures-util`; avoid `tokio` (heavy in WASM) | Light primitives |
| Errors | `thiserror` (lib), `anyhow` (binary glue) | Standard idiom |
| Serde | `serde`, `serde_json`, `rmp-serde` for RPC | Standard |
| BiDi | `icu_bidi` | Official Unicode reference impl, WASM-first |
| Segmenter | `icu_segmenter` | Line/word/grapheme break |
| Normalizer | `icu_normalizer` | NFC for input |
| Collation | `icu_collator` | Search, sort |
| Locale data | `icu_locale` + compiled `icu4x-datagen` blob | Trim to needed locales (`en`, `ar`, `he`, `fa`, `ur`, `zh`, `ja`, `ko`) |
| Shaping | `rustybuzz` | Pure-Rust HarfBuzz subset; complete enough for Arabic + Latin |
| Fallback shaping | `harfbuzz_rs` via Emscripten if `rustybuzz` gaps appear (escape hatch) | |
| Font parsing | `read-fonts` (Google Fonts), `swash` | Modern, WASM-ready |
| Font fallback | custom + `fontique` (Linebender) | font-kit doesn't work in WASM |
| Rasterization | `swash` rasterizer | Subpixel + hinting |
| 2D renderer | `vello` (primary, WebGPU) + custom 2D canvas fallback | Future-proof; 2D fallback for Safari |
| PDF | `pdf-writer` | Write PDF/A-1b; embed/subset fonts |
| ZIP | `rc-zip` (async-ready) or `zip` (sync) | .docx is a zip |
| XML | `quick-xml` | Streaming + DOM modes |
| SVG (icons, charts) | `usvg` + `resvg` | Pure Rust |
| Layout | **custom** | No mature .docx layout crate exists |
| .docx codec | **custom** (extend `docx-rs` as starting reference) | Existing crates incomplete |
| Logging | `tracing` + custom WASM appender → console | Standard |
| Tests | `wasm-bindgen-test` + native `cargo test` + Playwright visual diff | Two layers |

### 2.3 Six-month PoC milestones

> Goal of PoC: prove that Rust-WASM can render and edit a non-trivial Arabic .docx with print-quality output, before committing to MVP build.

| Week | Milestone | Acceptance criterion |
| --- | --- | --- |
| 1–2 | Repo skeleton: `cargo new --lib`, `wasm-pack` toolchain, GitHub Actions CI for native + headless-Chrome WASM tests, size-limit budget 5 MB initial. | `wasm-pack build` succeeds; CI green. |
| 3–4 | Hello-WASM bridge: TS calls Rust `add(a, b)`, then Rust calls JS `console.log`. | Demo page works in Chrome/Firefox/Safari. |
| 5–6 | Font loading via `swash`/`read-fonts`. Load Noto Sans + Noto Sans Arabic from CDN, render single line of plain Latin text to `<canvas>`. | Rendered text byte-equal to reference PNG (visual diff <1px). |
| 7–9 | **Arabic shaping PoC.** Render `السلام عليكم وحياكم الله` with `rustybuzz` shaping. | Glyphs joined correctly: initial/medial/final/isolated forms verified by hand. |
| 10–11 | **BiDi PoC.** Mixed paragraph: `"Hello عربي world"`. Use `icu_bidi`, render in visual order. | Latin runs LTR, Arabic run RTL, line is correct end-to-end. |
| 12–14 | Multi-line paragraph + line break: `icu_segmenter` for break opportunities, justify with Kashida. | Long Arabic paragraph wraps and justifies; selection rectangles correct. |
| 15–17 | Single-page layout: paragraphs + page margins + page break. | One A4 page renders 1:1 with reference. |
| 18–20 | Document Model + editing: insert/delete at cursor; undo/redo via `im`-based snapshot. | Type Latin+Arabic, undo, redo — model state consistent. |
| 21–22 | .docx reader (minimal): read body paragraphs + runs + simple character formatting (bold, italic, color, font size). | Open a 5-page .docx; visual diff ≥95% match. |
| 23–24 | .docx writer (round-trip): open → modify one paragraph → save. | Diff between original and saved is bounded (only edited region differs). PoC ships. |

### 2.4 PoC exit gate

PoC is APPROVED for MVP only if **all** of:
1. **WASM artifact size** ≤ 15 MB compressed / ≤ 35 MB uncompressed.
2. **Cold start to first paint** ≤ 3 s on M2-class hardware, ≤ 8 s on mid-tier Android Chrome.
3. **Arabic shaping** matches a HarfBuzz CLI reference output (`hb-shape`) for a corpus of 50 test strings.
4. **BiDi rendering** matches Unicode test suite UCD `BidiTest.txt` for sampled cases.
5. **Round-trip .docx fidelity** ≥ 95% visual on a 10-document Tier-1 corpus.
6. **Memory ceiling** ≤ 256 MB heap on 50-page mixed Arabic/English doc.

If any gate fails: stop, revisit crate choices, or pivot to vendor partnership (Collabora Online behind iframe — rejected goal, would force re-scoping).

### 2.5 Emscripten — when we still need it

Pure Rust covers 90% but **`rustybuzz` lacks some HarfBuzz features** (full Indic, some OpenType GSUB rules). Escape hatch:
- Compile upstream HarfBuzz (C++) to WASM with Emscripten, load as secondary module.
- Wrap with `harfbuzz_rs` via `extern "C"` bindings inside our worker.
- Only invoked when language tag falls outside `rustybuzz` coverage.
- Adds ~800 KB compressed. Defer until proven needed.

No other Emscripten work is required for MVP.

---

## 3. Phase 2 — Bridge & Memory (Months 4–9, overlaps PoC tail)

### 3.1 Worker isolation pattern

```ts
// main thread
const worker = new Worker(new URL('./engine.worker.ts', import.meta.url), { type: 'module' });
const engine = wrap<EngineApi>(worker); // Comlink-style typed proxy
await engine.init({ canvas: offscreenCanvas, fonts: fontList });
```

```rust
// engine crate root (compiled to WASM, loaded by engine.worker.ts)
#[wasm_bindgen]
pub struct Engine { /* opaque to JS */ }

#[wasm_bindgen]
impl Engine {
    #[wasm_bindgen(constructor)]
    pub fn new(canvas: OffscreenCanvas) -> Result<Engine, JsValue> { /* ... */ }

    pub async fn dispatch(&mut self, cmd: JsValue) -> Result<JsValue, JsValue> {
        let cmd: Command = serde_wasm_bindgen::from_value(cmd)?;
        let evt = self.apply(cmd).await?;
        Ok(serde_wasm_bindgen::to_value(&evt)?)
    }
}
```

- **One worker, one engine instance.** No sharing.
- **Typed RPC** via `tsify` — generates `.d.ts` automatically; no hand-maintained schema drift.
- **Async-first.** All `dispatch` calls return promises; main thread never blocks.
- **OffscreenCanvas transferred once** at init — worker owns the paint surface.

### 3.2 Cross-Origin isolation (required for SharedArrayBuffer)

Static server config (Caddy, nginx, or a Rust-served edge):

```
Cross-Origin-Opener-Policy: same-origin
Cross-Origin-Embedder-Policy: require-corp
Cross-Origin-Resource-Policy: same-origin
```

- Enables `SharedArrayBuffer` for zero-copy of large binary buffers (font blobs, .docx ZIP slices, image data).
- Enables `Atomics.wait` for fine-grained worker synchronization (optional).
- **All third-party assets** (fonts, images embedded in docs) require `Cross-Origin-Resource-Policy` headers or be served from same origin.

Inherited lesson from `ranuts/document`: their `docker-compose.yaml:25` uses `joseluisq/static-web-server:latest` with no header config — would block any future SAB use. We will ship a hardened static server config from day one (target image: `caddy` with explicit headers, or `static-web-server` with `[advanced.headers]` config file).

### 3.3 Memory strategy

- **No manual `_malloc`/`_free`.** Rust ownership eliminates the class of bugs that haunt `ranuts/document/lib/document-converter.ts:343` (no cleanup after conversion) and `lib/document-converter.ts:457` (old bin not released).
- **Allocator:** default first. Switch to `wee_alloc` only if size profiling shows >300 KB savings.
- **Initial memory:** 64 MB. **Max memory:** 2 GB. `--initial-memory=67108864 --maximum-memory=2147483648` via `wasm-pack` config.
- **Streaming for large docs:** read ZIP central directory first; lazily decompress entries as referenced. `rc-zip` supports this.
- **Document tree via `im`:** Persistent vectors and HashMaps give O(log n) copy-on-write. Undo stack = stack of `Arc<DocumentTree>`. 100-step undo on a 50-page doc costs ≈ 5–20 MB.
- **Glyph cache:** LRU bounded at 4096 entries (`lru` crate). Evict by glyph atlas region; rebuild on demand.
- **Crash recovery:** every command persists to an `IndexedDB`-backed event log. On WASM trap, worker restarts and replays log. Cold reload restores to last command.
- **No SharedArrayBuffer for the document tree itself** — tree lives in WASM linear memory exclusively. SAB is used only for opaque binary blobs (font files, image bytes, .docx pre-parse buffer).

### 3.4 RPC command schema (excerpt)

```rust
#[derive(Serialize, Deserialize, Tsify)]
#[tsify(into_wasm_abi, from_wasm_abi)]
#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Command {
    InsertText { at: Position, text: String, ime: bool },
    DeleteRange { range: Range },
    ApplyFormatting { range: Range, attrs: TextAttrs },
    SetSelection { range: Range, caret: Position },
    OpenDocument { bytes: Vec<u8>, format: DocFormat },
    SaveDocument { format: DocFormat },
    SetPageZoom { scale: f32 },
    RequestPaint { viewport: Rect },
    /* … */
}

#[derive(Serialize, Deserialize, Tsify)]
#[tsify(into_wasm_abi, from_wasm_abi)]
#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Event {
    Painted { dirty: Rect, version: u64 },
    SelectionChanged { range: Range, rects: Vec<Rect> },
    DocumentLoaded { meta: DocumentMeta },
    DocumentSaved { bytes: Vec<u8> },
    Error { code: ErrorCode, message: String },
    /* … */
}
```

- **All commands are idempotent at the protocol level** (replayable from event log).
- **No callbacks across the boundary.** Engine emits Events; UI subscribes.

---

## 4. Phase 3 — Canvas Rendering & RTL/Arabic Pipeline (Months 6–14)

### 4.1 Rendering target

**Primary:** `vello` over WebGPU.
- Vector pipeline with GPU compute shaders; ideal for zoom/scroll perf and crisp text at any DPI.
- WebGPU is shipped in Chrome 113+, Firefox 121+, Safari 18 (2025). By MVP date (2027–2028), coverage is ≥95%.

**Fallback:** OffscreenCanvas 2D + display list interpreter.
- Same display list (`vello::Scene`-equivalent) replayed via 2D canvas commands.
- Slower but universal; required for Safari pre-18 holdouts and locked-down enterprise envs.
- Feature detect at init; choose path.

**Print path:**
- Render at 300 DPI to PDF using `pdf-writer`.
- Embed and subset fonts via `swash` + `read-fonts` subsetter.
- PDF/A-1b for archival; PDF/UA for accessibility.
- Browser's native `window.print()` is **not used** for the main print path — its rasterization differs across browsers and breaks pixel-perfect targets. Our PDF is the source of truth.

### 4.2 Text pipeline (the heart of RTL support)

```rust
pub fn lay_out_paragraph(
    text: &str,
    runs: &[StyleRun],
    base_dir: Direction,
    width: f32,
    fonts: &FontStack,
) -> Vec<LineBox> {
    // 1. Normalize input (NFC) for consistent shaping
    let text = icu_normalizer::ComposingNormalizer::new_nfc().normalize(text);

    // 2. Run Unicode BiDi algorithm
    let bidi = icu_bidi::BidiInfo::new(&text, Some(base_dir.into()));
    let para = &bidi.paragraphs[0];
    let levels = bidi.reordered_levels(para, para.range.clone());

    // 3. Segment into shaping runs (script + style + BiDi level)
    let shaping_runs = segment(
        &text,
        &levels,
        runs,
        |script_a, script_b| script_a == script_b,
    );

    // 4. Shape each run with rustybuzz; pick font per script via fontique
    let mut shaped: Vec<ShapedRun> = vec![];
    for run in &shaping_runs {
        let font = fonts.resolve(run.script, &run.attrs);
        let mut buf = rustybuzz::UnicodeBuffer::new();
        buf.push_str(&text[run.range.clone()]);
        buf.set_direction(run.direction());
        buf.set_script(run.script);
        buf.set_language(run.language);
        let glyphs = rustybuzz::shape(&font.face(), &[], buf);
        shaped.push(ShapedRun { glyphs, font, range: run.range.clone(), level: run.level });
    }

    // 5. Line break with icu_segmenter (LineSegmenter is locale-aware)
    let segmenter = icu_segmenter::LineSegmenter::new_auto();
    let break_points: Vec<usize> = segmenter.segment_str(&text).collect();

    // 6. Greedy or Knuth-Plass line breaking; for MVP, greedy
    let mut lines = greedy_break(&shaped, &break_points, width);

    // 7. Apply Kashida elongation for Arabic justify
    for line in &mut lines {
        if line.has_arabic() && line.alignment == Alignment::Justify {
            apply_kashida(line);
        } else if line.alignment == Alignment::Justify {
            apply_space_justify(line);
        }
    }

    // 8. Reorder visual within each line per BiDi levels
    for line in &mut lines {
        line.reorder_visual(&levels);
    }

    lines
}
```

Required features for MVP-level Arabic:
- **Initial / medial / final / isolated** glyph forms (handled by `rustybuzz` GSUB).
- **Lam-Alef ligatures** (mandatory in Arabic).
- **Diacritics positioning** (GPOS marks).
- **Kashida justification** (custom; insert U+0640 or stretch idle joining glyphs).
- **Eastern Arabic / Western Arabic numeral shaping** based on locale + numeral context (UAX #44).
- **Mixed-direction line** with correct logical-to-visual mapping for cursor/selection.
- **Vertical metrics** per font; line height respects the font's `OS/2.sTypoAscender/Descender`.

Required font assets:
- **Noto Sans Arabic** (UI default Arabic).
- **Amiri** (book-quality Arabic Naskh — required for typography credibility).
- **Reem Kufi**, **Cairo**, **Tajawal** (modern Arabic sans).
- **Noto Sans / Inter** (Latin).
- All shipped via streamed CDN with `Cross-Origin-Resource-Policy: same-origin` or embedded in our origin's static fonts dir.
- License check: Noto and Amiri are SIL OFL — compatible.

### 4.3 Cursor and selection in RTL

- **Logical vs visual position** decoupled. The document model stores logical offsets; the renderer maps to visual rectangles.
- A cursor at the boundary between an RTL run and LTR run has **two visual positions** (Unicode-standard "weak" vs "strong" caret). MVP picks strong caret consistently; setting in user preferences post-MVP.
- **Selection across mixed directions** produces discontinuous visual rectangles. Standard Word/Pages behavior.
- **Keyboard navigation:**
  - Logical arrow keys (cursor moves by code point sequence): default.
  - Visual arrow keys (cursor follows screen direction): user setting.
  - Home/End behave logically per paragraph direction.

### 4.4 Print fidelity — honest scoping

Pixel-perfect against **what reference**? Three tiers:

- **Tier A (our spec):** our renderer is reproducible — same input + same fonts → same output bytes. Achievable.
- **Tier B (LibreOffice parity):** our render matches LibreOffice's render for a defined corpus. Achievable for paragraphs/tables; long-tail features need years.
- **Tier C (Word parity):** our render matches Microsoft Word's render. **Never fully achievable.** Word has 30 years of undocumented quirks. Even LibreOffice deviates.

Internal contract: target Tier A as the baseline, validate against Tier B for our test corpus, communicate to stakeholders that Tier C is a moving target.

Document corpus for fidelity testing: 200 documents covering:
- Plain prose (English, Arabic, mixed) — 40 docs
- Tables — 30 docs
- Lists (bulleted, numbered, nested) — 20 docs
- Images and floats — 20 docs
- Headers/footers, page numbers — 20 docs
- Footnotes/endnotes — 15 docs
- Comments and tracked changes — 15 docs
- Equations (OMML) — 10 docs
- SmartArt/drawings (DrawingML) — 10 docs
- Edge cases (huge tables, deeply nested lists, mixed BiDi paragraphs) — 20 docs

Each runs through a CI visual-diff job; tolerance configurable per tier.

---

## 5. Phase 4 — Headless UI Integration (Months 10–18)

### 5.1 Framework choice

**Recommend Solid.js** (or React 19 with `useSyncExternalStore`). Reasons:
- Fine-grained reactivity matches the engine's event stream model.
- Tiny runtime (<10 KB) keeps shell light against a heavy WASM core.
- No virtual-DOM reconciliation jitter near canvas.
- TS-first.

Acceptable alternatives: React 19 (largest talent pool), Vue 3.5 (good if existing team).

### 5.2 Input handling

Three layered input channels:

| Channel | Purpose | Implementation |
| --- | --- | --- |
| Pointer events on canvas | Click, drag, selection | Capture `pointerdown/move/up`; ship `{type:'POINTER', kind:'down', pos:{x,y}, modifiers}` to engine; engine returns hit-test + new selection rects |
| Keyboard on hidden `<textarea>` | Typing, shortcuts | Invisible `<textarea>` overlay focused at cursor; `keydown`/`beforeinput` shipped to engine; native browser IME composition flows through it |
| Clipboard / drag-drop | Copy / paste / drop | `clipboard.read()` / `clipboard.write()` with custom MIME for rich content; plain-text fallback |

IME considerations (CJK + Arabic typing):
- Use `compositionstart` / `compositionupdate` / `compositionend` from the hidden textarea.
- Engine receives composition deltas, renders the in-progress composition with a distinct visual style (underline), commits on `compositionend`.

Cursor caret:
- Engine emits `{caret: Rect, blink: bool}` events.
- UI overlays a `<div>` absolutely positioned at the caret rect — animation/blink in CSS, not in canvas.
- Hidden textarea is repositioned to the caret to keep IME popups anchored correctly.

### 5.3 Accessibility (built in from day one, not retrofitted)

- **Shadow DOM accessibility tree** mirrors document structure: a hidden `<div role="document">` with semantic markup rebuilt on each engine paint event.
- Screen readers walk the shadow tree; canvas is `aria-hidden`.
- Engine emits `AccessibilityTreeChanged` events with deltas; UI patches the DOM tree.
- ARIA roles for headings, lists, tables, links, images (with alt text).
- Focus moves via the textarea; selection announcements pipe through `aria-live`.

Lesson learned avoiding `ranuts/document`'s iframe: iframes inherit screen-reader anti-patterns. Our shadow-DOM accessibility tree is fully under our control.

### 5.4 State management

- **UI state in TS:** Solid signals / Zustand store. Examples: which sidebar is open, ribbon tab, dialog state.
- **Document state in WASM:** never duplicated to TS. UI subscribes to engine events for what it needs to render (selection rect, current font name, etc.).
- **Optimistic UI** for non-document interactions only (e.g. toolbar feedback). Never for document edits — every edit goes through the engine and is reflected on `Painted` event.

---

## 6. Phase 5 — Resource & Timeline (brutally honest)

### 6.1 Team composition

Assumption from user: "advanced AI engineering agents acting as Senior C++/Rust developers" + "Strong Bench." We treat AI agents as force multipliers, not replacements for human judgment on architecture and product decisions. Mapping that to FTE-equivalents:

| Track | Role | Human FTE | AI-augmentation multiplier |
| --- | --- | --- | --- |
| **A. Engine core (Rust)** | Senior Rust lead + AI-assisted impl | 1 senior + AI agents | 2–3× output |
| **B. Document codec (.docx)** | Senior OOXML expert + AI | 1 senior + AI | 2× |
| **C. Text / i18n** | Unicode/font specialist + AI | 1 senior (Arabic-native preferred) | 1.5× |
| **D. Renderer (vello/canvas)** | Senior graphics + AI | 1 senior + AI | 2× |
| **E. UI headless** | Senior TS / React-or-Solid + AI | 1 senior + AI | 2× |
| **F. Build / DevOps / WASM toolchain** | 1 senior (part-time OK after Phase 1) | 0.5 | 1.5× |
| **G. QA / fidelity** | Senior + ramp | 1 → 2 | 1.5× |
| **H. Tech lead / architect** | 1 (this role) | 1 | n/a |
| **Total** | | **7.5 humans + AI agents** | |

If "virtual strong bench" means AI agents only (no human seniors): **not advised**. AI agents are excellent at writing code, mediocre at architectural judgment under uncertainty, and unreliable on long-horizon Unicode/font edge cases. Plan assumes humans own architecture + review + RTL/font expertise; AI agents do the bulk of implementation.

### 6.2 Critical hires (non-substitutable)

- **One Arabic-native Unicode/typography engineer.** Non-negotiable. The difference between "Arabic works" and "Arabic looks right to native readers" is invisible to non-natives. They own font selection, Kashida policy, mixed-numeral behavior, vocalization (Tashkeel) handling.
- **One OOXML expert.** Someone who has shipped .docx interop. The spec is 5,000+ pages and incomplete; tribal knowledge matters.
- **One Rust + WASM principal.** Owns the bridge, the build pipeline, the memory model.

### 6.3 Timeline

```
Month  0──6──9──12─────18─────24
       │  │  │  │      │      │
PoC    ████│  │  │      │      │   ← Phase 1 (6 months)
Bridge    █████│  │      │      │   ← Phase 2 (3 months overlap)
Render       ██████████  │      │   ← Phase 3 (8 months)
UI                ███████████  │   ← Phase 4 (8 months)
Polish              ████████████   ← Hardening + fidelity loop
                                ↑
                                MVP (single-user edit + RTL, Tier A/B fidelity)
```

- **PoC exit gate:** Month 6.
- **Internal-alpha (open + view + Arabic correctly):** Month 12.
- **Internal-beta (single-user edit, round-trip .docx, basic format):** Month 18.
- **MVP (Tier-A pixel-perfect, Tier-B 90% .docx, Arabic native UX):** Month 22–24.

### 6.4 Cost ballpark

- 7.5 FTE × 24 months × $250k fully-loaded = ~$4.5M for MVP.
- AI agent infrastructure / licenses: $50k–150k/year. Negligible vs. headcount.
- WASM/canvas content delivery + fonts CDN: <$5k/month at moderate scale.
- **Total to MVP: $5M–6M.** Less than building from a 30-year C++ fork; less risky.

### 6.5 Risk register (top 10)

| # | Risk | Likelihood | Impact | Mitigation |
| --- | --- | --- | --- | --- |
| 1 | `rustybuzz` lacks needed OpenType features | High | Med | Escape hatch to HarfBuzz-WASM (Phase 2.5 contingency budgeted) |
| 2 | Layout engine fidelity gap vs. LibreOffice/Word | High | High | Tier-A scoping, golden-image regression suite from week 16 |
| 3 | WebGPU unavailable on key user browsers | Med | Med | OffscreenCanvas 2D fallback path is in scope from day one |
| 4 | iOS Safari 1.5GB memory cap blocks large docs | Med | High | Streaming .docx reader; explicit "large doc" warning UX |
| 5 | OOXML edge cases (math, SmartArt, charts) cost more than budgeted | High | Med | Defer to Phase 6 (post-MVP); render as fallback static image where possible |
| 6 | Vello not production-ready by MVP | Med | Med | Pin to a known-good Vello tag; maintain 2D fallback as primary if needed |
| 7 | `icu4x` data blob inflates WASM | Low | Low | Run `icu4x-datagen` with locale subset (8 locales × needed components ≈ 600 KB) |
| 8 | Font licensing surprises for Arabic | Low | High | Stick to SIL OFL fonts; legal review of every default font at Month 3 |
| 9 | Worker-based architecture breaks corporate proxies/CSPs | Low | Med | CSP testing in Month 6; same-origin worker source only |
| 10 | AI-agent code quality drift over 24 months | Med | High | Senior human review on every PR; architectural decision records (ADRs) reviewed by humans |

---

## 7. Lessons applied from `ranuts/document` (counterexample)

We analyzed `/home/ibrahim/Desktop/code/document` to extract what to do differently:

| Anti-pattern in `ranuts/document` | Our approach |
| --- | --- |
| WASM runs on main thread (`lib/document-converter.ts:72–83`) → UI freeze on conversion | Dedicated Web Worker; main thread never blocks |
| `iframe`-based editor via `window.DocsAPI.DocEditor` (`lib/onlyoffice-editor.ts:237`) | Direct canvas; no iframe; we own input/event chain |
| 57 MB vendored binary `x2t.wasm` with no source (`public/wasm/x2t/x2t.wasm`) | All Rust source in our repo; reproducible WASM builds in CI |
| 300 s init timeout (`lib/document-converter.ts:18`) — engine treated as a black box | 3 s target cold start; engine is ours, latencies are measurable and budgeted |
| Hand-rolled 30 s operation queue with empirical 150/250/400 ms sleeps (`lib/onlyoffice-editor.ts:29–66, 203–231`) | Proper async + command-event model; no `setTimeout` cleanup hacks |
| i18n type-system locked to en/zh (`lib/i18n.ts:12–17, 106–112`) | Locale-agnostic engine; `icu_locale` handles arbitrary BCP-47 tags |
| Locale JSONs shipped en + zh only (3 dirs under `public/web-apps/.../locale/`) | Full locale story from week one; Arabic-first PoC |
| No COOP/COEP, no SAB readiness (`vite.config.ts:1–29`, `docker-compose.yaml:25`) | Hardened static-server config from day one with `COOP/COEP/CORP` headers |
| No build pipeline for the engine — engine cannot be upgraded | Full Rust + `wasm-pack` build in CI; every commit produces a new engine artifact |
| Solo maintainer, alpha after 11 months (`v0.0.3` is latest tag) | Multi-track team with explicit fidelity QA |

---

## 8. Critical files to create (initial scaffolding)

These are the **files an implementation engineer creates first**, in dependency order:

- `Cargo.toml` — workspace with members: `engine`, `engine-wasm`, `format-docx`, `text-pipeline`, `layout`, `render`, `bridge`.
- `engine/src/lib.rs` — facade.
- `engine/src/document/mod.rs` — document tree (uses `im::Vector`, `im::HashMap`).
- `engine/src/document/command.rs` — `Command` enum with `tsify` derive.
- `engine/src/document/event.rs` — `Event` enum with `tsify` derive.
- `text-pipeline/src/bidi.rs` — `icu_bidi` wrapper.
- `text-pipeline/src/shape.rs` — `rustybuzz` wrapper + script run segmenter.
- `text-pipeline/src/line_break.rs` — `icu_segmenter` wrapper.
- `text-pipeline/src/justify.rs` — Kashida + space justification.
- `text-pipeline/src/fonts.rs` — `swash` + `fontique` font stack resolver.
- `layout/src/paragraph.rs` — paragraph box layout, calls text-pipeline.
- `layout/src/page.rs` — page master, margins, headers/footers.
- `layout/src/table.rs` — table layout (deferred to Phase 3).
- `render/src/scene.rs` — display-list builder.
- `render/src/vello_backend.rs` — Vello painter.
- `render/src/canvas2d_backend.rs` — fallback painter.
- `format-docx/src/reader.rs` — `rc-zip` + `quick-xml` streaming reader.
- `format-docx/src/writer.rs` — round-trip-preserving writer.
- `engine-wasm/src/lib.rs` — `#[wasm_bindgen]` exports + `Engine` struct.
- `ts/src/engine.worker.ts` — worker entry, loads WASM, exposes Comlink proxy.
- `ts/src/index.ts` — Solid root, mounts canvas, owns UI shell.
- `tools/visual-diff/` — Playwright-driven golden-image suite.
- `tools/docx-corpus/` — 200-document fidelity corpus + README.
- `infra/server/Caddyfile` — static server with COOP/COEP/CORP.
- `.github/workflows/ci.yml` — native `cargo test` + `wasm-pack test --headless --chrome` + `wasm-opt` size budget + visual diff.

---

## 9. Verification

How we know each phase is done:

### PoC (Month 6)
```bash
# Native tests
cargo test --workspace

# WASM tests (headless Chrome)
wasm-pack test --headless --chrome engine-wasm

# Size budget (CI fail at >15MB)
wasm-pack build --release engine-wasm && \
  ls -lh engine-wasm/pkg/*.wasm

# Arabic shaping regression
cargo run --bin shape-regression -- ./test/arabic-corpus/

# BiDi conformance
cargo run --bin bidi-regression -- ./test/bidi-test.txt
```

### Bridge (Month 9)
```bash
# COOP/COEP local server up
caddy run --config infra/server/Caddyfile

# Cross-origin isolated check
playwright test --grep "isolation"

# Worker round-trip latency budget
playwright test --grep "rpc-perf"  # target p95 < 5ms
```

### Render + RTL (Month 14)
```bash
# Visual diff against 200-doc corpus
node tools/visual-diff/run.mjs --tolerance 0.02 ./corpus/

# Print PDF round-trip
node tools/print-diff/run.mjs --reference libreoffice
```

### UI + Editing (Month 18)
```bash
# E2E editing tests
playwright test ts/e2e/

# Accessibility audit
playwright test ts/e2e/a11y.spec.ts  # axe-core integration
```

### MVP gate (Month 22–24)
- Open + edit + save round-trip on full corpus passes Tier-A/B thresholds.
- Cold-start budget hit on tier-2 hardware.
- Native-Arabic UX review by external Arabic typography expert.
- Independent security review of WASM bridge.
- 100% of P0 OOXML features supported; P1 documented.

---

## 10. Open decisions (defer to project kickoff)

These need a human call before Phase 1 starts:

1. **UI framework final choice**: Solid (recommended) vs. React vs. Vue. Drives hiring.
2. **Default Arabic font**: Noto Sans Arabic (Google-friendly) vs. Amiri (book-quality). Possibly ship both.
3. **Collaboration roadmap**: defer fully to post-MVP, or invest in Yrs (Y.js Rust port) integration during MVP to keep CRDT spine in document model? Recommend: keep `im`-based tree CRDT-ready in design, but no Yrs integration during MVP.
4. **Print pipeline**: do we ship native PDF export at MVP, or rely on browser print at MVP and add PDF in Phase 6? Recommend: ship native PDF — it's required for fidelity proof and is a 4–6 week task on top of the layout engine.
5. **Backend?** This plan is fully client-side. If we add a sync/save backend (recommended for enterprise), it's a separate workstream not covered here.
