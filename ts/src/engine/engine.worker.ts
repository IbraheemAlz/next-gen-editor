/// <reference lib="webworker" />

import init, { Engine, detect_backend } from '../../../crates/engine-wasm/pkg/engine_wasm.js';
import type { Command, Event } from '../../../crates/engine-wasm/pkg/engine_wasm.js';
import { openEventLog, appendCommand, persistSnapshot } from './event-log';
/* Fonts are imported as Vite `?url` assets, NOT fetched from absolute
   `/fonts/...` paths. Absolute paths break under a deploy subpath (e.g.
   GitHub Pages /next-gen-editor/); `?url` imports are hashed + base-aware. */
import LATIN_URL from '../../fonts/LiberationSans-Regular.ttf?url';
import ARABIC_URL from '../../fonts/NotoNaskhArabic-Regular.ttf?url';
import DUAL_URL from '../../fonts/Amiri-Regular.ttf?url';

declare const self: DedicatedWorkerGlobalScope;

/* Phase 1 PoC harness envelope — used only by the visual-diff harness
   (ts/src/harness/visual-diff.ts) for the `?test=` golden cases. The
   interactive editor drives the engine through the EngineClient path below. */
type InitMsg = {
    type: 'INIT';
    canvas: OffscreenCanvas;
    testCase: string;
    /* PR #19 cherry-pick: explicit renderer override for the dual-tier
       golden suite. `'vello'` opts the harness into Vello (and the
       `golden/vello/` corpus); absent or `'canvas2d'` keeps the
       Canvas2D default that the root `golden/` corpus is calibrated
       against. Never auto-detected here — auto-detection produced
       non-deterministic CI failures in PR #19. */
    renderer?: 'canvas2d' | 'vello';
};
type CommandMsg = { type: 'COMMAND'; id: number; cmd: Command };

/* Phase 2 §6/§7 — EngineClient envelope. Every request carries a numeric
   `id`; INIT carries a `documentId` (vs. the harness `testCase`), and a bare
   command request has no `type` field at all. */
type ClientInitMsg = {
    id: number;
    type: 'INIT';
    canvas: OffscreenCanvas;
    documentId: string;
};
type ClientRecoverMsg = {
    id: number;
    type: 'RECOVER';
    canvas: OffscreenCanvas;
    snapshot: Uint8Array;
    log: Command[];
    snapshotSeq: number;
    lastSeq: number;
};
type ClientCommandMsg = { id: number; cmd: Command };
/* Phase 8a — side-channel snapshot request. The reply carries the array
   the engine's `comments_snapshot()` wasm method returns; the TS shell
   feeds it into the comments sidebar signal. */
type GetCommentsMsg = { id: number; type: 'GET_COMMENTS' };
/* Phase 8b — same shape for `revisions_snapshot()`. */
type GetRevisionsMsg = { id: number; type: 'GET_REVISIONS' };
/* Phase 6c — multi-canvas: hand the worker an OffscreenCanvas for page
   `idx`. The worker dispatches into `engine.set_page_canvas`, which
   registers the surface so subsequent paints fill that page's
   <canvas> in the DOM. */
type RegisterPageCanvasMsg = { id: number; type: 'REGISTER_PAGE_CANVAS'; idx: number; canvas: OffscreenCanvas };

type Msg =
    | InitMsg
    | CommandMsg
    | ClientInitMsg
    | ClientRecoverMsg
    | ClientCommandMsg
    | GetCommentsMsg
    | GetRevisionsMsg
    | RegisterPageCanvasMsg;

const LATIN_ID = 'liberation-sans';
const ARABIC_ID = 'noto-naskh-arabic';
/* Amiri is a book-quality Naskh face that ALSO ships Latin glyphs, so the
   interactive editor renders mixed Arabic/English from a single face.
   `a4-justified-mixed` instead exercises the engine's §13.A FontStack with
   two single-script faces; the other test cases stay single-script. */
const DUAL_ID = 'amiri';

const A4_TEXT =
    'هذا نص تجريبي مكتوب باللغة العربية لاختبار خوارزمية تخطيط الصفحة. ' +
    'This paragraph mixes Arabic and English text to validate BiDi run resolution, ' +
    'greedy line breaking via icu_segmenter, and basic Kashida elongation. ' +
    'الكلمات العربية يجب أن تظهر بالشكل الصحيح مع الربط بين الحروف. ' +
    'The justify alignment should stretch each non-final line to reach both margins.';

let engine: Engine | null = null;

/* Phase 2 §6 — event-log sequencing for the EngineClient command path. */
let logSequence = 0;
let lastSnapshotAt = 0;
const SNAPSHOT_EVERY = 200;

/**
 * Serial command queue.
 *
 * `engine.dispatch` is a `&mut self` async method. wasm-bindgen forbids
 * aliasing a `&mut` borrow — if two `dispatch` futures overlap (which a
 * plain `async onmessage` allows, since it yields at every `await`),
 * wasm-bindgen panics with "recursive use of an object" and the command
 * is lost. Fast typing reproduced this as dropped characters.
 *
 * Every INIT and COMMAND is chained onto `queue` so exactly one engine
 * call is ever in flight.
 */
let queue: Promise<unknown> = Promise.resolve();
function enqueue<T>(task: () => Promise<T>): Promise<T> {
    const run = queue.then(task, task);
    queue = run.then(
        () => undefined,
        () => undefined,
    );
    return run;
}

async function fetchBytes(url: string): Promise<Uint8Array> {
    const r = await fetch(url);
    if (!r.ok) throw new Error(`fetch ${url}: HTTP ${r.status}`);
    return new Uint8Array(await r.arrayBuffer());
}

async function dispatch(cmd: Command): Promise<Event> {
    if (!engine) throw new Error('engine not initialized');
    return (await engine.dispatch(cmd)) as Event;
}

async function handleInit(msg: InitMsg): Promise<void> {
    await init({
        module_or_path: new URL(
            '../../../crates/engine-wasm/pkg/engine_wasm_bg.wasm',
            import.meta.url,
        ),
    });
    /* PR #19 cherry-pick: harness path stays Canvas2D-locked by default
       so the root `golden/` corpus is reproducible across machines.
       Opt-in to Vello via the `renderer` field on the INIT envelope
       (visual-diff.ts sets it from `?renderer=vello`); falls back to
       Canvas2D when Vello requested but no WebGPU adapter — that way
       the harness fails closed (mismatch shows up clearly in
       `golden/vello/`) instead of silently emitting Canvas2D pixels
       against Vello goldens. */
    const wantVello = msg.renderer === 'vello';
    if (wantVello && (await detect_backend()) === 'vello') {
        engine = await Engine.with_vello(msg.canvas);
        self.postMessage({ type: 'BOOT_OK', renderer: 'vello' });
    } else {
        engine = new Engine(msg.canvas);
        self.postMessage({ type: 'BOOT_OK', renderer: 'canvas2d' });
    }

    const pong = await dispatch({ type: 'PING' } as Command);
    self.postMessage({ type: 'PING_RESULT', event: pong });

    /* Default (no ?test= param) is the interactive A4 editor. */
    const testCase = msg.testCase || 'interactive';

    /* Per-case font loading. Interactive uses the dual-script Amiri so mixed
       Arabic/English renders. `a4-justified-mixed` loads both single-script
       faces so the engine's FontStack falls back per script (§13.A); the
       remaining test cases are single-script. */
    if (testCase === 'interactive') {
        const e = await dispatch({
            type: 'LOAD_FONT',
            id: DUAL_ID,
            bytes: await fetchBytes(DUAL_URL),
        } as Command);
        self.postMessage({ type: 'FONT_LOADED_RESULT', event: e });
    } else if (testCase === 'a4-justified-mixed' || testCase === 'rich-text') {
        const arabic = await dispatch({
            type: 'LOAD_FONT',
            id: ARABIC_ID,
            bytes: await fetchBytes(ARABIC_URL),
        } as Command);
        self.postMessage({ type: 'FONT_LOADED_RESULT', event: arabic });
        const latin = await dispatch({
            type: 'LOAD_FONT',
            id: LATIN_ID,
            bytes: await fetchBytes(LATIN_URL),
        } as Command);
        self.postMessage({ type: 'FONT_LOADED_RESULT', event: latin });
    } else if (
        testCase === 'hello-arabic' ||
        testCase === 'editing-arabic' ||
        testCase === 'docx-round-trip'
    ) {
        const e = await dispatch({
            type: 'LOAD_FONT',
            id: ARABIC_ID,
            bytes: await fetchBytes(ARABIC_URL),
        } as Command);
        self.postMessage({ type: 'FONT_LOADED_RESULT', event: e });
    } else {
        const e = await dispatch({
            type: 'LOAD_FONT',
            id: LATIN_ID,
            bytes: await fetchBytes(LATIN_URL),
        } as Command);
        self.postMessage({ type: 'FONT_LOADED_RESULT', event: e });
    }

    let paintEvt: Event;
    switch (testCase) {
        case 'hello-latin':
            paintEvt = await dispatch({
                type: 'SHAPE_AND_RASTERIZE',
                text: 'hello',
                font_id: LATIN_ID,
                direction: 'LTR',
                px_size: 96,
            } as Command);
            break;

        case 'hello-arabic':
            paintEvt = await dispatch({
                type: 'SHAPE_AND_RASTERIZE',
                text: 'السلام',
                font_id: ARABIC_ID,
                direction: 'RTL',
                px_size: 96,
            } as Command);
            break;

        case 'a4-justified-mixed':
            paintEvt = await dispatch({
                type: 'RENDER_PAGE',
                text: A4_TEXT,
                font_id: ARABIC_ID,
                base_direction: 'RTL',
                px_size: 18,
                line_height: 26,
                align: 'JUSTIFY',
            } as Command);
            break;

        case 'editing-arabic': {
            await dispatch({
                type: 'RENDER_PAGE',
                text: 'السلام',
                font_id: ARABIC_ID,
                base_direction: 'RTL',
                px_size: 28,
                line_height: 42,
                align: 'START',
            } as Command);
            const e1 = await dispatch({
                type: 'INSERT_TEXT',
                at: undefined,
                text: ' عليكم',
            } as Command);
            self.postMessage({ type: 'EDIT_RESULT', step: 'insert-1', event: e1 });
            const e2 = await dispatch({
                type: 'INSERT_TEXT',
                at: undefined,
                text: ' ورحمة الله',
            } as Command);
            self.postMessage({ type: 'EDIT_RESULT', step: 'insert-2', event: e2 });
            const u1 = await dispatch({ type: 'UNDO' } as Command);
            self.postMessage({ type: 'EDIT_RESULT', step: 'undo', event: u1 });
            const r1 = await dispatch({ type: 'REDO' } as Command);
            self.postMessage({ type: 'EDIT_RESULT', step: 'redo', event: r1 });
            paintEvt = r1;
            break;
        }

        case 'docx-round-trip': {
            await dispatch({
                type: 'RENDER_PAGE',
                text: 'افتح، عدِّل، احفظ.',
                font_id: ARABIC_ID,
                base_direction: 'RTL',
                px_size: 28,
                line_height: 42,
                align: 'START',
            } as Command);
            const saved = await dispatch({ type: 'SAVE_DOCX' } as Command);
            self.postMessage({ type: 'DOCX_RESULT', step: 'save', event: saved });
            if (saved.type === 'DOCUMENT_SAVED') {
                const reloaded = await dispatch({
                    type: 'LOAD_DOCX',
                    bytes: saved.bytes,
                } as Command);
                self.postMessage({ type: 'DOCX_RESULT', step: 'load', event: reloaded });
            }
            paintEvt = await dispatch({
                type: 'INSERT_TEXT',
                at: undefined,
                text: ' تم التعديل',
            } as Command);
            break;
        }

        case 'rich-text': {
            /* Rich text: a plain RenderPage, then ApplyFormatting spans of
               colour + size over mixed Arabic/English. The 44px size span
               starts at offset 3 — mid-"Hello" — so it splits a shaping run
               purely on a style change; it also overlaps both colour spans. */
            await dispatch({
                type: 'RENDER_PAGE',
                text: 'Hello أهلا world عالم done.',
                font_id: LATIN_ID,
                base_direction: 'LTR',
                px_size: 26,
                line_height: 56,
                align: 'START',
            } as Command);
            await dispatch({
                type: 'APPLY_FORMATTING',
                range: {
                    start: { path: { steps: [{ kind: 'BLOCK', idx: 0 }] }, offset: 0 },
                    end: { path: { steps: [{ kind: 'BLOCK', idx: 0 }] }, offset: 14 },
                },
                attrs: { color: { r: 200, g: 30, b: 30, a: 255 } },
            } as Command);
            await dispatch({
                type: 'APPLY_FORMATTING',
                range: {
                    start: { path: { steps: [{ kind: 'BLOCK', idx: 0 }] }, offset: 14 },
                    end: { path: { steps: [{ kind: 'BLOCK', idx: 0 }] }, offset: 35 },
                },
                attrs: { color: { r: 30, g: 70, b: 200, a: 255 } },
            } as Command);
            await dispatch({
                type: 'APPLY_FORMATTING',
                range: {
                    start: { path: { steps: [{ kind: 'BLOCK', idx: 0 }] }, offset: 3 },
                    end: { path: { steps: [{ kind: 'BLOCK', idx: 0 }] }, offset: 29 },
                },
                attrs: { font_size: 44 },
            } as Command);
            /* Sprint 6 — exercise the decoration + highlight render paths:
               underline + strikethrough strokes and a background colour. */
            await dispatch({
                type: 'APPLY_FORMATTING',
                range: {
                    start: { path: { steps: [{ kind: 'BLOCK', idx: 0 }] }, offset: 15 },
                    end: { path: { steps: [{ kind: 'BLOCK', idx: 0 }] }, offset: 20 },
                },
                attrs: { underline: 'Single' },
            } as Command);
            await dispatch({
                type: 'APPLY_FORMATTING',
                range: {
                    start: { path: { steps: [{ kind: 'BLOCK', idx: 0 }] }, offset: 30 },
                    end: { path: { steps: [{ kind: 'BLOCK', idx: 0 }] }, offset: 35 },
                },
                attrs: { strike: true },
            } as Command);
            paintEvt = await dispatch({
                type: 'APPLY_FORMATTING',
                range: {
                    start: { path: { steps: [{ kind: 'BLOCK', idx: 0 }] }, offset: 6 },
                    end: { path: { steps: [{ kind: 'BLOCK', idx: 0 }] }, offset: 14 },
                },
                attrs: { bg_color: { r: 255, g: 235, b: 120, a: 255 } },
            } as Command);
            break;
        }

        case 'tab-stops-center-kind-ltr': {
            /* L2.1 (#6) — Center tab kind at 250 pt. Segment "City"
               sits to the right of a TAB; its midpoint lands at
               column 250 in paragraph-content coords. */
            await dispatch({
                type: 'RENDER_PAGE',
                text: 'Name\tCity',
                font_id: LATIN_ID,
                base_direction: 'LTR',
                px_size: 18,
                line_height: 26,
                align: 'START',
            } as Command);
            const wholeRange = {
                start: { path: { steps: [{ kind: 'BLOCK', idx: 0 }] }, offset: 0 },
                end: { path: { steps: [{ kind: 'BLOCK', idx: 0 }] }, offset: 0 },
            };
            paintEvt = await dispatch({
                type: 'SET_TAB_STOPS',
                range: wholeRange,
                stops: [{ position_pt: 250, kind: 'Center' }],
            } as Command);
            break;
        }

        case 'tab-stops-right-kind-ltr': {
            /* L2.1 (#6) — Right tab kind at 300 pt. Segment "Total"
               right-edge lands at column 300. */
            await dispatch({
                type: 'RENDER_PAGE',
                text: 'Year\tTotal',
                font_id: LATIN_ID,
                base_direction: 'LTR',
                px_size: 18,
                line_height: 26,
                align: 'START',
            } as Command);
            const wholeRange = {
                start: { path: { steps: [{ kind: 'BLOCK', idx: 0 }] }, offset: 0 },
                end: { path: { steps: [{ kind: 'BLOCK', idx: 0 }] }, offset: 0 },
            };
            paintEvt = await dispatch({
                type: 'SET_TAB_STOPS',
                range: wholeRange,
                stops: [{ position_pt: 300, kind: 'Right' }],
            } as Command);
            break;
        }

        case 'tab-stops-decimal-kind-ltr': {
            /* L2.1 (#6) — Decimal tab kind at 250 pt. Segment "12.50"
               aligns its `.` separator at column 250. */
            await dispatch({
                type: 'RENDER_PAGE',
                text: 'Price\t12.50',
                font_id: LATIN_ID,
                base_direction: 'LTR',
                px_size: 18,
                line_height: 26,
                align: 'START',
            } as Command);
            const wholeRange = {
                start: { path: { steps: [{ kind: 'BLOCK', idx: 0 }] }, offset: 0 },
                end: { path: { steps: [{ kind: 'BLOCK', idx: 0 }] }, offset: 0 },
            };
            paintEvt = await dispatch({
                type: 'SET_TAB_STOPS',
                range: wholeRange,
                stops: [{ position_pt: 250, kind: 'Decimal' }],
            } as Command);
            break;
        }

        case 'interactive':
            /* Blank A4 page seeded with one empty paragraph. RenderPage
               caches the layout config so subsequent InsertText / Undo /
               Redo commands from the textarea auto-repaint. */
            paintEvt = await dispatch({
                type: 'RENDER_PAGE',
                text: '',
                font_id: DUAL_ID,
                base_direction: 'RTL',
                px_size: 24,
                line_height: 36,
                align: 'START',
            } as Command);
            break;

        case 'glyph-a':
        default:
            paintEvt = await dispatch({
                type: 'RASTERIZE_GLYPH',
                font_id: LATIN_ID,
                ch: 'A',
                px_size: 128,
            } as Command);
            break;
    }
    self.postMessage({ type: 'PAINT_RESULT', event: paintEvt });
    self.postMessage({ type: 'IDLE' });
}

async function handleCommand(msg: CommandMsg): Promise<void> {
    try {
        const result = await dispatch(msg.cmd);
        self.postMessage({ type: 'COMMAND_RESULT', id: msg.id, event: result });
    } catch (e: unknown) {
        const error = e instanceof Error ? e.message : String(e);
        self.postMessage({
            type: 'COMMAND_RESULT',
            id: msg.id,
            event: { type: 'ERROR', message: error } as Event,
        });
    }
}

/* ===================================================================
   Phase 2 §6/§7 — EngineClient (id-routed) message path.

   This runs ALONGSIDE the Phase 1 test-harness path above. A given worker
   instance only ever receives one protocol (the visual-diff harness uses the
   harness path; `EngineClient` uses this path), so they coexist without
   interfering.
   =================================================================== */

/**
 * Reply to an id-routed request with a failure. A WASM trap/panic
 * (`RuntimeError` / `unreachable`) is fatal: flag it and close the worker so
 * the client can spin up a fresh one and recover.
 */
function replyError(id: number, e: unknown): void {
    const error = e instanceof Error ? e.message : String(e);
    if (/RuntimeError|unreachable/.test(error)) {
        self.postMessage({ id, ok: false, error, trap: true });
        self.close();
    } else {
        self.postMessage({ id, ok: false, error });
    }
}

async function handleClientInit(msg: ClientInitMsg): Promise<void> {
    try {
        await init({
            module_or_path: new URL(
                '../../../crates/engine-wasm/pkg/engine_wasm_bg.wasm',
                import.meta.url,
            ),
        });
        /* Pick the renderer once, before the canvas takes a context: Vello
           (WebGPU) when a GPU device is available, else the Canvas2D fallback.
           transferControlToOffscreen is one-shot, so this choice is permanent
           for the canvas (Backlog #4). */
        const renderer = await detect_backend();
        engine =
            renderer === 'vello'
                ? await Engine.with_vello(msg.canvas)
                : new Engine(msg.canvas);
        await openEventLog(msg.documentId);
        /* Report worker-context cross-origin isolation (D2.3) + the chosen
           renderer (Backlog #4) in the reply. */
        self.postMessage({
            id: msg.id,
            ok: true,
            crossOriginIsolated: self.crossOriginIsolated,
            renderer,
        });
    } catch (e: unknown) {
        replyError(msg.id, e);
    }
}

async function handleClientRecover(msg: ClientRecoverMsg): Promise<void> {
    try {
        await init({
            module_or_path: new URL(
                '../../../crates/engine-wasm/pkg/engine_wasm_bg.wasm',
                import.meta.url,
            ),
        });
        engine = new Engine(msg.canvas);
        /* Resume the event-log sequence past what was already persisted, so
           post-recovery appends don't collide with or shadow prior rows. */
        logSequence = msg.lastSeq;
        lastSnapshotAt = msg.snapshotSeq;
        const evt = await dispatch({
            type: 'RECOVER',
            snapshot: msg.snapshot,
            log_tail: msg.log,
        } as Command);
        self.postMessage({ id: msg.id, ok: true, evt });
    } catch (e: unknown) {
        replyError(msg.id, e);
    }
}

/**
 * Commands whose dispatch changes document content — they invalidate the
 * accessibility tree, so the worker re-broadcasts it afterward (§10).
 */
function mutatesDocument(cmd: Command): boolean {
    switch (cmd.type) {
        case 'INSERT_TEXT':
        case 'DELETE_RANGE':
        case 'DELETE_AT_CARET':
        case 'REPLACE_RANGE':
        case 'SPLIT_PARAGRAPH':
        case 'MERGE_PARAGRAPH':
        case 'APPLY_FORMATTING':
        case 'SET_PARAGRAPH_ALIGN':
        case 'INSERT_IMAGE':
        case 'PASTE_PLAIN':
        case 'PASTE_HTML':
        case 'END_COMPOSITION':
        case 'UNDO':
        case 'REDO':
        case 'RENDER_PAGE':
        case 'LOAD_DOCX':
        case 'OPEN_DOCUMENT':
        /* Phase 5 PR 3 table mutations. Each one flips a Table's
           `dirty` bit; the worker must broadcast a fresh a11y delta so
           the TablePanel's `tables` signal re-renders and the mirror
           DOM exposes the new structure to screen readers. */
        case 'INSERT_TABLE':
        case 'DELETE_TABLE':
        case 'INSERT_ROW':
        case 'DELETE_ROW':
        case 'INSERT_COLUMN':
        case 'DELETE_COLUMN':
        case 'MERGE_CELLS':
        case 'SPLIT_CELL':
        case 'SET_CELL_SHADING':
        case 'SET_CELL_BORDERS':
            return true;
        default:
            return false;
    }
}

async function handleClientCommand(msg: ClientCommandMsg): Promise<void> {
    if (!engine) {
        self.postMessage({ id: msg.id, ok: false, error: 'engine not initialized' });
        return;
    }
    try {
        const t0 = performance.now();
        const evt = await dispatch(msg.cmd);
        const elapsed = performance.now() - t0;
        self.postMessage({ id: msg.id, ok: true, evt, elapsed });
        /* D2.8 backpressure (PHASE_2_BRIDGE_MEMORY.md §12 risk 5): persist to
           the event log OFF the critical path. The RPC response is already
           sent, so event-log latency never throttles command throughput. */
        logCommand(msg.cmd);
        /* Phase 7 — after `OPEN_DOCUMENT`, enumerate the engine's parsed
           inline-image media blobs, decode each into an `ImageBitmap` via
           the browser's native pipeline, and register the result back on
           the engine. The first paint after this picks the bitmaps up. */
        if (msg.cmd.type === 'OPEN_DOCUMENT') {
            await decodeAndRegisterMedia();
        }
        /* §10 / Backlog #10: after a document mutation, broadcast an
           accessibility delta. The message carries no `id`, so EngineClient
           fans it out to subscribers — the mirror DOM reconciler patches only
           the paragraphs that changed. */
        if (mutatesDocument(msg.cmd)) {
            const delta = await dispatch({ type: 'REQUEST_ACCESSIBILITY_DELTA' } as Command);
            self.postMessage({ evt: delta });
            /* Bug-fix sprint after Phase 8b — every mutating command rendered
               the document but never emitted `Painted`, so the TS shell's
               `documentHeight` signal stuck at 0 and a multi-page document
               got vertically squashed into the original A4 CSS box. Synthesize
               a `Painted` side-channel here from the engine's cached last-
               render dimensions so the canvas grows when the paginator
               emits more pages. */
            broadcastPaintDims();
        }
        /* Sprint 10 — drain queued aria-live announcements (queued by
           the engine's mutation handlers via `Engine::announce`). Each
           one rides the subscribe path (no `id`) so the
           `Announcements.tsx` aria-live region narrates engine
           actions in dispatch order. */
        broadcastAnnouncements();
    } catch (e: unknown) {
        replyError(msg.id, e);
    }
}

/**
 * Side-channel: post a synthetic `Painted` event built from the engine's
 * cached `last_paint_dims`. Called after every mutating command so the
 * TS shell's `documentHeight` signal stays in sync with the paginator's
 * actual page count.
 */
function broadcastPaintDims(): void {
    if (!engine) return;
    try {
        const dims = engine.paint_dims() as { document_height: number; page_count: number };
        self.postMessage({
            evt: {
                type: 'PAINTED',
                dirty: { x: 0, y: 0, w: 0, h: 0 },
                version: 0,
                paint_ms: 0,
                document_height: dims.document_height,
                page_count: dims.page_count,
            },
        });
    } catch (e: unknown) {
        console.warn('[worker] paint_dims broadcast failed', e);
    }
}

/**
 * Sprint 10 — drain every queued `Event::Announcement` from the engine
 * and broadcast each one through the subscribe path so the TS shell's
 * `Announcements.tsx` `aria-live` region narrates it. No-op when the
 * engine's queue is empty.
 */
function broadcastAnnouncements(): void {
    if (!engine) return;
    try {
        const drained = engine.drain_announcements() as Event[];
        for (const evt of drained) {
            self.postMessage({ evt });
        }
    } catch (e: unknown) {
        console.warn('[worker] drain_announcements failed', e);
    }
}

/**
 * Phase 7 — decode every inline-image media blob the engine parsed into an
 * `ImageBitmap` and install it back on the engine. Run after `OPEN_DOCUMENT`
 * succeeds; on decode failure the bitmap is skipped and the renderer falls
 * back to the placeholder rectangle for that image.
 */
async function decodeAndRegisterMedia(): Promise<void> {
    if (!engine) return;
    const entries = engine.media_entries() as Array<{
        rel_id: string;
        mime: string;
        bytes: Uint8Array;
    }>;
    for (const entry of entries) {
        try {
            const blob = new Blob([entry.bytes as BlobPart], { type: entry.mime });
            const bitmap = await createImageBitmap(blob);
            engine.register_image(entry.rel_id, bitmap);
        } catch (e: unknown) {
            console.warn(`[worker] image decode failed (${entry.rel_id}):`, e);
        }
    }
}

/**
 * Append a command to the durable log without blocking the RPC response.
 * `logSequence` increments synchronously so sequence order is preserved even
 * though the IndexedDB writes settle asynchronously.
 */
function logCommand(cmd: Command): void {
    const seq = ++logSequence;
    void appendCommand(seq, cmd).catch((e: unknown) =>
        console.warn('[worker] event-log append failed', e),
    );
    if (seq - lastSnapshotAt >= SNAPSHOT_EVERY) {
        lastSnapshotAt = seq;
        /* TODO Phase 2 D2.6: persist engine.snapshot() once that API exists. */
        void persistSnapshot(seq, new Uint8Array(0)).catch((e: unknown) =>
            console.warn('[worker] event-log snapshot failed', e),
        );
    }
}

self.onmessage = (ev: MessageEvent<Msg>): void => {
    const msg = ev.data;

    /* Phase 2 §7: a bare command request — `{ id, cmd }` — has no `type`. */
    if (!('type' in msg)) {
        void enqueue(() => handleClientCommand(msg));
        return;
    }

    /* Phase 8a — `GET_COMMENTS`: side-channel call into the engine's
       `comments_snapshot()` wasm method. Not a `Command` (the snapshot
       is read-only document metadata, not an event-log mutation). */
    if (msg.type === 'GET_COMMENTS') {
        if (!engine) {
            self.postMessage({ id: msg.id, ok: false, error: 'engine not initialized' });
            return;
        }
        try {
            const snapshot = engine.comments_snapshot();
            self.postMessage({ id: msg.id, ok: true, comments: snapshot });
        } catch (e: unknown) {
            replyError(msg.id, e);
        }
        return;
    }

    /* Phase 6c — multi-canvas registration. Engine's `set_page_canvas`
       transfers the OffscreenCanvas to a per-page slot; the next paint
       fills it. */
    if (msg.type === 'REGISTER_PAGE_CANVAS') {
        if (!engine) {
            self.postMessage({ id: msg.id, ok: false, error: 'engine not initialized' });
            return;
        }
        try {
            engine.set_page_canvas(msg.idx, msg.canvas);
            self.postMessage({ id: msg.id, ok: true });
            void dispatch({
                type: 'REQUEST_PAINT',
                viewport: { x: 0, y: 0, w: 0, h: 0 },
                dirty: undefined,
            }).then((evt) => self.postMessage({ evt }));
        } catch (e: unknown) {
            replyError(msg.id, e);
        }
        return;
    }

    /* Phase 8b — revisions side-channel mirror of `GET_COMMENTS`. */
    if (msg.type === 'GET_REVISIONS') {
        if (!engine) {
            self.postMessage({ id: msg.id, ok: false, error: 'engine not initialized' });
            return;
        }
        try {
            const snapshot = engine.revisions_snapshot();
            self.postMessage({ id: msg.id, ok: true, revisions: snapshot });
        } catch (e: unknown) {
            replyError(msg.id, e);
        }
        return;
    }

    if (msg.type === 'INIT') {
        /* `documentId` distinguishes the EngineClient INIT from the Phase 1
           harness INIT (which carries `testCase`). */
        if ('documentId' in msg) {
            void enqueue(() => handleClientInit(msg));
        } else {
            enqueue(() => handleInit(msg)).catch((e: unknown) => {
                const error = e instanceof Error ? e.message : String(e);
                self.postMessage({ type: 'ERROR', error });
            });
        }
        return;
    }

    if (msg.type === 'RECOVER') {
        void enqueue(() => handleClientRecover(msg));
        return;
    }

    if (msg.type === 'COMMAND') {
        void enqueue(() => handleCommand(msg));
    }
};
