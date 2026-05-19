# Phase 2 — Worker Bridge & Memory Architecture

> **Parent:** [`MASTER_PLAN.md`](./MASTER_PLAN.md) §3.
> **Owning tracks:** A (Engine), E (UI), F (DevOps).
> **Calendar:** Months 4–9 (overlaps tail of Phase 1).
> **Exit gate:** §11.

---

## 1. Objective

Promote the PoC bridge (small, ad-hoc) to production-grade:

1. **Dedicated Web Worker** isolation for the WASM engine.
2. **Type-safe RPC** via `tsify` — generated `.d.ts` matches Rust types byte-for-byte.
3. **Cross-origin isolation** + `SharedArrayBuffer` for zero-copy bulk transfers.
4. **Memory architecture** with explicit budgets, allocator choice, glyph + image caches.
5. **Event log + crash recovery** via IndexedDB.
6. **Backpressure** to sustain 1 000+ commands/s without dropped paint frames.

---

## 2. Deliverables

| ID | Deliverable | Acceptance signal |
| --- | --- | --- |
| D2.1 | Worker bootstrap | `engine.worker.ts` loads pkg, posts `Ready` in <500 ms |
| D2.2 | Typed RPC | `pnpm tsc --noEmit` clean; bridge `.d.ts` generated from Rust |
| D2.3 | Cross-origin isolated | `self.crossOriginIsolated === true` in dev + prod |
| D2.4 | `SharedArrayBuffer` paths | Font + image bytes transferred zero-copy |
| D2.5 | Memory budget enforced | `--initial-memory=64MB --max-memory=2GB`; CI memory tests pass |
| D2.6 | Event log | All commands persisted to IndexedDB; replay verified |
| D2.7 | Crash recovery | Forced WASM trap recovers within 2 s |
| D2.8 | Backpressure | 1 000 cmds/s sustained; paint p95 <16 ms |

---

## 3. Architecture

```
Main thread                                      Engine Worker
─────────────                                    ──────────────
Solid components                                 wasm-bindgen entry
        │                                                │
        │ user input                                     │
        ▼                                                ▼
EngineClient (typed RPC)                          Command dispatcher
        │                                                │
        │ postMessage({id, cmd})                         │
        │ + Transferable[]                               ▼
        ├─────────────────────────────────────►   Engine.dispatch(cmd) ──► Event
        │                                                │
        │ ◄─────────────────────────────────────  postMessage({id, evt})
        │ + Transferable[ImageBitmap]                    │
        │                                                ▼
        ▼                                          OffscreenCanvas paint
   pending.resolve(evt)                                  │
        │                                                ▼
        ▼                                          IndexedDB event-log
   subscribers.forEach(s ⇒ s(evt))
```

Key invariants:

- **One worker, one engine.** No engine sharing across documents (multi-doc opens multiple workers).
- **Engine state is opaque** to main thread; all access via typed commands.
- **OffscreenCanvas transferred at init only.** Never re-transferred.
- **All Vec\<u8\> bulk payloads** travel via `SharedArrayBuffer` or `Transferable ArrayBuffer`.

---

## 4. Command schema (canonical, full)

`crates/bridge/src/command.rs`:

```rust
use serde::{Serialize, Deserialize};
use tsify::Tsify;

#[derive(Serialize, Deserialize, Tsify, Clone, Debug)]
#[tsify(into_wasm_abi, from_wasm_abi)]
#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Command {
    /* Lifecycle */
    Init { canvas_id: u32, dpi: f32, locale: String, capabilities: ClientCapabilities },
    Recover { snapshot: Vec<u8>, log_tail: Vec<Command> },
    Dispose,
    Tick { now_ms: f64 },
    Ping,

    /* Document I/O */
    OpenDocument { bytes: Vec<u8>, format: DocFormat, name: Option<String> },
    SaveDocument { format: DocFormat },
    ExportPdf { conformance: PdfConformance },
    CloseDocument,

    /* Editing */
    InsertText { at: LogicalPos, text: String, ime: bool },
    DeleteRange { range: LogicalRange },
    ReplaceRange { range: LogicalRange, text: String },
    ApplyFormatting { range: LogicalRange, attrs: TextAttrsPatch },
    SplitParagraph { at: LogicalPos },
    MergeParagraph { left: ParagraphId, right: ParagraphId },
    InsertImage { at: LogicalPos, image: ImageBlob, fit: ImageFit },

    /* Selection */
    SetSelection { range: LogicalRange, caret: LogicalPos },
    ExtendSelection { to: LogicalPos, modifier: SelectionModifier },
    SelectAll,

    /* IME */
    BeginComposition { at: LogicalPos },
    UpdateComposition { text: String, target_range: Option<LogicalRange> },
    EndComposition { commit: bool },

    /* View */
    SetViewport { rect: Rect },
    SetZoom { scale: f32 },
    RequestPaint { viewport: Rect, dirty: Option<Rect> },

    /* Undo */
    Undo,
    Redo,

    /* Fonts / resources */
    LoadFont { id: String, bytes: Vec<u8>, fallback_rank: i32 },
    UnloadFont { id: String },

    /* Telemetry */
    RequestStats,
}

#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug)]
#[serde(rename_all = "snake_case")]
pub enum DocFormat { Docx, Pdf, PlainText, Html }

#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug)]
pub enum PdfConformance { A1b, A2u, X3 }

#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug)]
pub struct LogicalPos { pub para: u32, pub offset: u32 }

#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug)]
pub struct LogicalRange { pub start: LogicalPos, pub end: LogicalPos }

#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug)]
pub struct Rect { pub x: f32, pub y: f32, pub w: f32, pub h: f32 }

#[derive(Serialize, Deserialize, Tsify, Clone, Debug)]
pub struct TextAttrsPatch {
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub underline: Option<UnderlineStyle>,
    pub strike: Option<bool>,
    pub font_family: Option<String>,
    pub font_size: Option<f32>,
    pub color: Option<Color>,
    pub bg_color: Option<Color>,
    pub script: Option<VerticalScript>,
    pub language: Option<String>,
}

#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug)]
pub enum SelectionModifier { None, Shift, Alt, ShiftAlt }
```

---

## 5. Event schema (canonical, full)

`crates/bridge/src/event.rs`:

```rust
#[derive(Serialize, Deserialize, Tsify, Clone, Debug)]
#[tsify(into_wasm_abi, from_wasm_abi)]
#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Event {
    /* Lifecycle */
    Ready { version: String, capabilities: EngineCapabilities },
    Recovered { applied_commands: u32 },
    Pong,

    /* Document */
    DocumentLoaded { meta: DocumentMeta, paragraphs: u32, pages: u32 },
    DocumentSaved { bytes: Vec<u8>, format: DocFormat },
    DocumentClosed,
    PdfExported { bytes: Vec<u8>, pages: u32 },

    /* Rendering */
    Painted { dirty: Rect, version: u64, paint_ms: f32 },

    /* Selection */
    SelectionChanged {
        range: LogicalRange,
        caret: Rect,
        direction: Direction,
        rects: Vec<Rect>,
        attrs_at_caret: TextAttrs,
    },

    /* IME */
    CompositionUpdated { at: LogicalPos, text: String, target_range: Option<LogicalRange> },

    /* Editing feedback */
    UndoStateChanged { can_undo: bool, can_redo: bool },
    FormattingChanged { range: LogicalRange, attrs: TextAttrs },

    /* Accessibility */
    AccessibilityTreeChanged { delta: A11yDelta },

    /* Telemetry */
    Stats(EngineStats),

    /* Resource events */
    FontLoaded { id: String },
    FontMissing { script: Script, requested: String },

    /* Errors */
    Error { code: ErrorCode, message: String, recoverable: bool },
    Trap { stack: String },          /* fatal; worker about to die */
}

#[derive(Serialize, Deserialize, Tsify, Clone, Debug)]
pub struct EngineStats {
    pub wasm_heap_bytes: u32,
    pub document_tree_bytes: u32,
    pub glyph_cache_entries: u32,
    pub undo_stack_depth: u32,
    pub fonts_resident: u32,
    pub last_paint_ms: f32,
    pub last_command_ms: f32,
}

#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug)]
pub enum ErrorCode {
    InvalidCommand, IoError, ParseError, FontMissing,
    DocumentTooLarge, OutOfMemory, FeatureUnsupported,
}
```

---

## 6. Worker entry (`ts/src/engine.worker.ts`)

```ts
import init, { Engine } from '../../crates/engine-wasm/pkg/engine_wasm';
import { openEventLog, appendCommand, persistSnapshot } from './event-log';
import type { Command } from '../../crates/bridge/pkg/types';

let engine: Engine | null = null;
let logSequence = 0;
let lastSnapshotAt = 0;
const SNAPSHOT_EVERY = 200;

self.onmessage = async (ev: MessageEvent) => {
    const { id, type } = ev.data;

    try {
        if (type === 'INIT') {
            await init();
            engine = new Engine(ev.data.canvas);
            await openEventLog(ev.data.documentId);
            postMessage({ id, ok: true });
            return;
        }
        if (type === 'RECOVER') {
            await init();
            engine = new Engine(ev.data.canvas);
            const evt = await engine.dispatch({ type: 'RECOVER', snapshot: ev.data.snapshot, log_tail: ev.data.log });
            postMessage({ id, ok: true, evt });
            return;
        }

        if (!engine) throw new Error('engine not initialized');

        const cmd: Command = ev.data.cmd;
        const t0 = performance.now();
        const evt = await engine.dispatch(cmd);
        const elapsed = performance.now() - t0;

        await appendCommand(++logSequence, cmd);
        if (logSequence - lastSnapshotAt >= SNAPSHOT_EVERY) {
            const snap = engine.snapshot();
            await persistSnapshot(logSequence, snap);
            lastSnapshotAt = logSequence;
        }

        postMessage({ id, ok: true, evt, elapsed });
    } catch (e: any) {
        const error = e?.message ?? String(e);
        if (/RuntimeError|unreachable/.test(error)) {
            postMessage({ id, ok: false, error, trap: true });
            self.close();
        } else {
            postMessage({ id, ok: false, error });
        }
    }
};
```

---

## 7. Engine client (`ts/src/engine-client.ts`)

```ts
import type { Command, Event } from '../../crates/bridge/pkg/types';

type Resolver = (v: { ok: boolean; evt?: Event; error?: string; trap?: boolean }) => void;

export class EngineClient {
    private worker!: Worker;
    private nextId = 1;
    private pending = new Map<number, Resolver>();
    private subscribers = new Set<(e: Event) => void>();
    private documentId: string;

    constructor(documentId: string) {
        this.documentId = documentId;
        this.spawn();
    }

    private spawn() {
        this.worker = new Worker(new URL('./engine.worker.ts', import.meta.url), { type: 'module' });
        this.worker.onmessage = (ev) => this.handle(ev.data);
        this.worker.onerror = (e) => this.onWorkerError(e);
    }

    async init(canvas: OffscreenCanvas): Promise<void> {
        await this.send({ type: 'INIT', canvas, documentId: this.documentId }, [canvas]);
    }

    async recover(canvas: OffscreenCanvas, snapshot: Uint8Array, log: Command[]): Promise<void> {
        await this.send({ type: 'RECOVER', canvas, snapshot, log }, [canvas, snapshot.buffer]);
    }

    async dispatch(cmd: Command, transfer: Transferable[] = []): Promise<Event> {
        const r = await this.send({ cmd }, transfer);
        if (!r.ok) throw new Error(r.error);
        return r.evt!;
    }

    subscribe(fn: (e: Event) => void): () => void {
        this.subscribers.add(fn);
        return () => { this.subscribers.delete(fn); };
    }

    private send(payload: any, transfer: Transferable[] = []) {
        return new Promise<{ ok: boolean; evt?: Event; error?: string; trap?: boolean }>((resolve) => {
            const id = this.nextId++;
            this.pending.set(id, resolve);
            this.worker.postMessage({ id, ...payload }, transfer);
        });
    }

    private handle(msg: any) {
        const cb = this.pending.get(msg.id);
        if (cb) { this.pending.delete(msg.id); cb(msg); }
        if (msg.evt) this.subscribers.forEach((s) => s(msg.evt));
        if (msg.trap) this.onTrap();
    }

    private onWorkerError(e: ErrorEvent) {
        console.error('worker error', e);
        this.onTrap();
    }

    private async onTrap() {
        /* Phase 2: recovery flow */
        const { snapshot, log } = await loadLatestEventLog(this.documentId);
        this.spawn();
        const canvas = newOffscreenForTrapRecovery();
        await this.recover(canvas, snapshot, log);
    }
}
```

---

## 8. Memory architecture

### 8.1 Allocator decision matrix

| Allocator | Size delta | Throughput | Decision |
| --- | --- | --- | --- |
| Default `dlmalloc` | baseline | best | **PoC + MVP** |
| `wee_alloc` | −200 KB | −15 % | rejected: large-doc throughput regression |
| `mimalloc-wasm` | +50 KB | +5 % | revisit post-MVP |

### 8.2 Memory budget (per worker)

| Allocation pool | Cap | Tracking |
| --- | --- | --- |
| WASM linear memory | 64 MB initial / 2 GB max | linker flags + `Engine::stats()` |
| Document tree (`im` snapshots) | 64 MB | per-snapshot byte estimate |
| Undo stack | 32 snapshots × ≤16 MB | LRU eviction below |
| Glyph atlas | 64 MB | LRU 4 096 entries |
| Image cache | 128 MB | LRU bytes-bounded |
| Font bytes | 32 MB total | refcount via `Arc<[u8]>` |
| Per-command scratch | 8 MB | `bumpalo::Bump` reset per command |
| Total soft budget | ≤256 MB on 50-page doc | enforced by E2E test |

### 8.3 `Engine::stats()` surface

Re-emitted every 5 s and on demand via `Command::RequestStats`. Used by telemetry and CI memory tests.

### 8.4 Glyph cache

```rust
use lru::LruCache;
pub struct GlyphAtlas {
    cache: LruCache<GlyphKey, GlyphEntry>,
    bytes: usize,
    cap_bytes: usize,
}

#[derive(Hash, PartialEq, Eq, Clone, Copy)]
pub struct GlyphKey {
    pub font_id: FontId,
    pub glyph_id: u16,
    pub px_size: u16,   /* fixed-point: pt × 100 */
    pub subpixel_x: u8, /* 0..3 */
    pub subpixel_y: u8,
}
```

### 8.5 Streaming `.docx` reader

```rust
pub struct StreamingDocxReader<'a> { /* rc_zip::ArchiveReader */ }

impl<'a> StreamingDocxReader<'a> {
    pub fn read_entry(&mut self, name: &str) -> Result<EntryReader, DocxError> { /* … */ }
    pub fn iter_xml<'b>(&mut self, name: &str) -> Result<XmlIter<'b>, DocxError> { /* … */ }
}
```

Never holds whole ZIP in memory. Layout traverses XML stream once; lazy load images on first use.

---

## 9. Cross-origin isolation

`infra/server/Caddyfile`:

```
{
    auto_https off
    admin off
}

:8080 {
    root * /srv
    file_server

    header {
        Cross-Origin-Opener-Policy "same-origin"
        Cross-Origin-Embedder-Policy "require-corp"
        Cross-Origin-Resource-Policy "same-origin"
        Cache-Control "public, max-age=31536000, immutable"
        X-Content-Type-Options "nosniff"
        Referrer-Policy "strict-origin-when-cross-origin"
        Content-Security-Policy "default-src 'self'; worker-src 'self'; connect-src 'self'; img-src 'self' data: blob:; style-src 'self' 'unsafe-inline'; font-src 'self'"
    }

    @wasm path *.wasm
    header @wasm Content-Type "application/wasm"

    @fonts path /fonts/*
    header @fonts Cross-Origin-Resource-Policy "cross-origin"
    header @fonts Cache-Control "public, max-age=31536000, immutable"
}
```

Validation at engine init:

```ts
if (!self.crossOriginIsolated) {
    throw new Error('COOP/COEP misconfigured — SAB unavailable');
}
if (typeof SharedArrayBuffer !== 'function') {
    throw new Error('SAB not supported');
}
```

---

## 10. Event log + crash recovery

### 10.1 Storage

IndexedDB schema:

```ts
const db = await openDB('engine-log', 1, {
    upgrade(db) {
        db.createObjectStore('commands', { keyPath: 'seq' });
        db.createObjectStore('snapshots', { keyPath: 'seq' });
        db.createObjectStore('meta', { keyPath: 'id' });
    },
});
```

### 10.2 Append + snapshot

```ts
export async function appendCommand(seq: number, cmd: Command) {
    await db.put('commands', { seq, cmd, at: Date.now() });
}

export async function persistSnapshot(seq: number, bytes: Uint8Array) {
    await db.put('snapshots', { seq, bytes });
    /* prune older snapshots; keep last 3 */
    const all = await db.getAllKeys('snapshots');
    for (const k of all.slice(0, -3)) await db.delete('snapshots', k);
}
```

### 10.3 Recovery flow on trap

1. Worker posts `{ trap: true, stack }`, calls `self.close()`.
2. Main thread:
   - Notify UI ("Recovering…").
   - Read latest snapshot from IndexedDB.
   - Read commands with `seq > snapshot.seq`.
   - Spawn new worker; send `Command::Recover { snapshot, log_tail }`.
3. Engine in new worker:
   - Decode snapshot into document tree.
   - Apply log tail commands in order (idempotent).
   - Emit `Event::Recovered { applied_commands }`.
4. UI: hide recovery overlay, continue.

Budget: ≤2 s for recover-from-trap on a 50-page doc.

---

## 11. Exit gate (Phase 2)

```bash
# 1. Worker boots cold in <500 ms (M2 baseline)
playwright test ts/e2e/boot.spec.ts

# 2. Type-safety end-to-end
pnpm tsc --noEmit

# 3. Cross-origin isolation
playwright test ts/e2e/isolation.spec.ts

# 4. SAB transfer of 50 MB blob round-trip <50 ms
playwright test ts/e2e/sab-transfer.spec.ts

# 5. Memory budget on 50-page doc
playwright test ts/e2e/memory-50p.spec.ts          # asserts <256 MB total

# 6. Event log replay
playwright test ts/e2e/event-log-replay.spec.ts

# 7. Crash recovery under 2 s
playwright test ts/e2e/crash-recovery.spec.ts

# 8. Backpressure 1000 cmds/s
playwright test ts/e2e/rpc-throughput.spec.ts
```

---

## 12. Risk register (Phase 2 specific)

| # | Risk | Likelihood | Detection | Mitigation |
| --- | --- | --- | --- | --- |
| 1 | Browser dropping COEP support / breaking SAB | Low | CI matrix | Fallback to `ArrayBuffer` `Transferable`; lose zero-copy but keep correctness |
| 2 | `tsify` generated types lag Rust schema | Med | `pnpm tsc` in CI | Tag bridge crate version; bump TS package together |
| 3 | IndexedDB quota exhaustion on long sessions | Med | E2E quota test | Prune commands older than last snapshot; per-document cap |
| 4 | Worker hang (infinite loop in WASM) | Med | Heartbeat via `Command::Ping` | Watchdog terminates worker after 5 s no-pong, triggers recover |
| 5 | RPC throughput regresses with growing event log | Low | Bench in CI | Log writes batched + off-critical-path |

---

## 13. Hand-off into Phase 3

Phase 2 hands Phase 3 a stable bridge + memory contract. Phase 3 work is engine-internal:

- Layout engine implementation behind `Engine::dispatch`.
- Renderer backends behind the display list.
- All without changing the schema in this document.

The schema in §4–§5 is **frozen** at end of Phase 2; subsequent additions are append-only with `#[serde(default)]` for backward compat.
