/// <reference lib="webworker" />

import init, { Engine } from '../../../crates/engine-wasm/pkg/engine_wasm.js';
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
type InitMsg = { type: 'INIT'; canvas: OffscreenCanvas; testCase: string };
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

type Msg = InitMsg | CommandMsg | ClientInitMsg | ClientRecoverMsg | ClientCommandMsg;

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
    engine = new Engine(msg.canvas);
    self.postMessage({ type: 'BOOT_OK' });

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
                range: { start: { para: 0, offset: 0 }, end: { para: 0, offset: 14 } },
                attrs: { color: { r: 200, g: 30, b: 30, a: 255 } },
            } as Command);
            await dispatch({
                type: 'APPLY_FORMATTING',
                range: { start: { para: 0, offset: 14 }, end: { para: 0, offset: 35 } },
                attrs: { color: { r: 30, g: 70, b: 200, a: 255 } },
            } as Command);
            paintEvt = await dispatch({
                type: 'APPLY_FORMATTING',
                range: { start: { para: 0, offset: 3 }, end: { para: 0, offset: 29 } },
                attrs: { font_size: 44 },
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
        engine = new Engine(msg.canvas);
        await openEventLog(msg.documentId);
        /* Report worker-context cross-origin isolation (D2.3) in the reply. */
        self.postMessage({
            id: msg.id,
            ok: true,
            crossOriginIsolated: self.crossOriginIsolated,
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
        case 'SPLIT_PARAGRAPH':
        case 'APPLY_FORMATTING':
        case 'END_COMPOSITION':
        case 'UNDO':
        case 'REDO':
        case 'RENDER_PAGE':
        case 'LOAD_DOCX':
        case 'OPEN_DOCUMENT':
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
        /* §10: after a document mutation, broadcast a fresh accessibility
           tree. The message carries no `id`, so EngineClient fans it out to
           subscribers — the screen-reader shadow DOM stays synced. */
        if (mutatesDocument(msg.cmd)) {
            const tree = await dispatch({ type: 'REQUEST_ACCESSIBILITY_TREE' } as Command);
            self.postMessage({ evt: tree });
        }
    } catch (e: unknown) {
        replyError(msg.id, e);
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
