/// <reference lib="webworker" />

import init, { Engine, detect_backend } from '../../../crates/engine-wasm/pkg/engine_wasm.js';
import type {
    Command,
    Event,
    RendererDowngrade,
} from '../../../crates/engine-wasm/pkg/engine_wasm.js';
import { openEventLog, appendCommand, persistSnapshot } from './event-log';
import type { LoggedCommand, RecoveryCandidate } from './event-log';
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
    /** Issue #99 — DEV-only backend mock (see `probeBackend`). */
    mockBackend?: 'vello';
};
type ClientRecoverMsg = {
    id: number;
    type: 'RECOVER';
    canvas: OffscreenCanvas;
    /** Issue #241 — bases to try, newest snapshot first, ending with the
     *  snapshot-less log base (see `event-log.ts` `loadRecoveryLog`). */
    candidates: RecoveryCandidate[];
    /** Every retained logged command, ascending by seq. */
    commands: LoggedCommand[];
    lastSeq: number;
    /** Issue #241 — the log still reaches back to the first command. */
    logComplete: boolean;
    /** Issue #99 — DEV-only backend mock (see `probeBackend`). */
    mockBackend?: 'vello';
    /** Issue #99 — crash-loop fallback: boot this generation on Canvas2D
     *  without re-probing the GPU backend that kept trapping. */
    forceRenderer?: 'canvas2d';
    /** Issue #99 — the downgrade record, echoed by the engine on
     *  `Event::Recovered.renderer_downgrade`. */
    rendererDowngrade?: RendererDowngrade;
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
/* Issue #85 — fault injection (test hook). After `after_commands` more
   LOGGED commands have been applied and acknowledged, the worker flushes
   its in-flight event-log writes and traps the wasm instance on purpose
   (`Engine.debug_force_trap`), exercising the REAL path — `RuntimeError`
   → `{ trap: true }` → `self.close()` → respawn → `RECOVER` — instead of
   `EngineClient.forceTrap()`'s worker.terminate() shortcut. */
type ArmTrapMsg = { id: number; type: 'ARM_TRAP'; after_commands: number };
/* Issue #96 — DEV-only test hook: per-page opaque-ink counts read back
   from the surfaces this worker holds (see the handler). */
type ProbePageInkMsg = { id: number; type: 'PROBE_PAGE_INK' };

type Msg =
    | InitMsg
    | CommandMsg
    | ClientInitMsg
    | ClientRecoverMsg
    | ClientCommandMsg
    | GetCommentsMsg
    | GetRevisionsMsg
    | RegisterPageCanvasMsg
    | ArmTrapMsg
    | ProbePageInkMsg;

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
/* Issue #85 — idle snapshot cadence: this long after the last logged
   command, with commands outstanding since the last snapshot, a snapshot
   task is queued behind whatever is in flight. Together with
   SNAPSHOT_EVERY this bounds the replay tail to ≤ 200 commands during a
   typing burst and to ~0 once the user pauses. */
const SNAPSHOT_IDLE_MS = 1500;
let idleSnapshotTimer: ReturnType<typeof setTimeout> | undefined;
/* Issue #85 — every in-flight event-log write, chained so the fault-
   injection hook can flush before trapping. A REAL trap loses whatever
   is still in flight at that instant — that is the inherent window of
   logging off the critical path (D2.8), bounded by the idle snapshot. */
let pendingLogWrites: Promise<unknown> = Promise.resolve();
/* Issue #85 — fault-injection countdown; `null` = disarmed. */
let trapAfterCommands: number | null = null;

/* Issue #96 — every OffscreenCanvas this worker generation was handed, by
   page index (0 = the INIT / RECOVER surface). Only the DEV paint probe
   reads it; the engine owns the contexts. A fresh worker starts empty, so
   a page the shell failed to re-register after a trap is visibly absent. */
const pageSurfaces = new Map<number, OffscreenCanvas>();

/**
 * Issue #99 — pick the backend for a fresh surface. `mock === 'vello'` is
 * a DEV-only test hook (`?mockBackend=vello`, forwarded by EngineClient):
 * the generation REPORTS Vello — to the client, on the INIT reply and on
 * `Event::Recovered.renderer` — while actually painting with Canvas2D, so
 * the crash-loop fallback can be exercised on GPU-less CI. A production
 * build ignores it.
 */
async function probeBackend(
    mock: 'vello' | undefined,
): Promise<{ renderer: string; mocked: boolean }> {
    if (import.meta.env.DEV && mock === 'vello') {
        return { renderer: 'vello', mocked: true };
    }
    return { renderer: await detect_backend(), mocked: false };
}

async function constructEngine(
    canvas: OffscreenCanvas,
    probe: { renderer: string; mocked: boolean },
): Promise<Engine> {
    return probe.renderer === 'vello' && !probe.mocked
        ? await Engine.with_vello(canvas)
        : new Engine(canvas);
}

function countOpaqueInk(surface: OffscreenCanvas): number {
    /* Same context type the engine took → the SAME context back; a
       surface Vello claimed for WebGPU answers `null`. */
    const ctx = surface.getContext('2d') as OffscreenCanvasRenderingContext2D | null;
    if (!ctx || surface.width === 0 || surface.height === 0) return -1;
    const d = ctx.getImageData(0, 0, surface.width, surface.height).data;
    let ink = 0;
    for (let p = 0; p < d.length; p += 4) {
        const opaque = (d[p + 3] ?? 0) > 200;
        const white = (d[p] ?? 255) >= 250 && (d[p + 1] ?? 255) >= 250 && (d[p + 2] ?? 255) >= 250;
        if (opaque && !white) ink++;
    }
    return ink;
}

/* Issue #194 — the engine `document_mutation_seq` the last accessibility
   delta was broadcast for. A fresh engine (INIT, crash recovery — a new
   worker every time) starts at 0, matching this. */
let broadcastMutationSeq = 0;

/* Highest `version` seen on a real `Painted` event — synthetic paint-dims
   broadcasts reuse it so `paintVersion` consumers never see a reset to 0. */
let lastPaintVersion = 0;

function notePaintVersion(evt: Event): void {
    if (evt.type === 'PAINTED' && evt.version > lastPaintVersion) {
        lastPaintVersion = evt.version;
    }
}

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

/* Issue #51 — one repaint per REGISTER_PAGE_CANVAS burst, not one per
   page. The setTimeout(0) hop lets every registration message already
   sitting in the event loop enqueue first; the flag resets when the
   repaint task STARTS, so a registration landing after that point
   schedules a fresh repaint and never misses its canvas. */
let pageCanvasRepaintQueued = false;
function schedulePageCanvasRepaint(): void {
    if (pageCanvasRepaintQueued) return;
    pageCanvasRepaintQueued = true;
    setTimeout(() => {
        void enqueue(async () => {
            pageCanvasRepaintQueued = false;
            try {
                const evt = await dispatch({
                    type: 'REQUEST_PAINT',
                    viewport: { x: 0, y: 0, w: 0, h: 0 },
                    dirty: undefined,
                });
                notePaintVersion(evt);
                self.postMessage({ evt });
            } catch (e: unknown) {
                console.error('[worker] page-canvas repaint failed', e);
            }
        });
    }, 0);
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
            /* Issue #53 made `at: undefined` = "at the live caret", and
               LOAD_DOCX seats the caret at (0,0) — an implicit-append
               here would silently PREPEND (caught as a 0.211% golden
               drift). Say what the case means: place the caret at the
               end of the reloaded text, then insert. */
            const endOfText = new TextEncoder().encode('افتح، عدِّل، احفظ.').length;
            const endPos = {
                path: { steps: [{ kind: 'BLOCK', idx: 0 }] },
                offset: endOfText,
            };
            await dispatch({
                type: 'SET_SELECTION',
                range: { start: endPos, end: endPos },
                caret: endPos,
            } as Command);
            paintEvt = await dispatch({
                type: 'INSERT_TEXT',
                at: undefined,
                text: ' تم التعديل',
            } as Command);
            break;
        }

        case 'table-merges': {
            /* Tables epic — a 3×3 grid with a 2×2 rectangular merge
               (gridSpan + vMerge in Word's collapsed shape) plus text in
               the merged cell, a neighbour, and a below-merge cell, so
               the golden pins span widths, continuation-row skipping,
               and border/edge alignment around merges. */
            const cellPos = (r: number, c: number) => ({
                path: {
                    steps: [
                        { kind: 'BLOCK', idx: 1 },
                        { kind: 'CELL', row: r, col: c },
                        { kind: 'BLOCK', idx: 0 },
                    ],
                },
                offset: 0,
            });
            await dispatch({
                type: 'RENDER_PAGE',
                text: 'Merged tables',
                font_id: LATIN_ID,
                base_direction: 'LTR',
                px_size: 18,
                line_height: 26,
                align: 'START',
            } as Command);
            await dispatch({
                type: 'INSERT_TABLE',
                at: { steps: [{ kind: 'BLOCK', idx: 1 }] },
                rows: 3,
                cols: 3,
            } as Command);
            await dispatch({
                type: 'MERGE_CELLS',
                table_path: { steps: [{ kind: 'BLOCK', idx: 1 }] },
                from_row: 0,
                from_col: 0,
                to_row: 1,
                to_col: 1,
            } as Command);
            /* The engine owns the selection: after the first insert the
               live caret wins over the `at` hint, so each cell hop needs
               an explicit SET_SELECTION (the invariant the interactive
               shell relies on — CLAUDE.md Phase 4). */
            const selectCell = async (r: number, c: number) => {
                await dispatch({
                    type: 'SET_SELECTION',
                    range: { start: cellPos(r, c), end: cellPos(r, c) },
                    caret: cellPos(r, c),
                } as Command);
            };
            await selectCell(0, 0);
            await dispatch({
                type: 'INSERT_TEXT',
                at: cellPos(0, 0),
                text: 'merged 2×2',
            } as Command);
            await selectCell(0, 1);
            await dispatch({
                type: 'INSERT_TEXT',
                at: cellPos(0, 1),
                text: 'C3',
            } as Command);
            await selectCell(2, 1);
            paintEvt = await dispatch({
                type: 'INSERT_TEXT',
                at: cellPos(2, 1),
                text: 'below',
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

        case 'list-bullet-numbered': {
            /* Issue #50 — interactive-path lists: TOGGLE_LIST must produce a
               VISIBLE marker in the hanging gutter plus the stock level
               indent (start 36 pt / hanging 18 pt). Two bullet items (the
               second long enough to wrap, pinning the wrapped-lines-hug-
               the-start-indent rule) and two numbered items pin both
               synthesis kinds. */
            const pos = (idx: number, offset: number) => ({
                path: { steps: [{ kind: 'BLOCK', idx }] },
                offset,
            });
            await dispatch({
                type: 'RENDER_PAGE',
                text: 'First bullet item',
                font_id: LATIN_ID,
                base_direction: 'LTR',
                px_size: 18,
                line_height: 26,
                align: 'START',
            } as Command);
            await dispatch({ type: 'SPLIT_PARAGRAPH', at: pos(0, 17) } as Command);
            await dispatch({
                type: 'INSERT_TEXT',
                at: pos(1, 0),
                text:
                    'Second bullet item stretched with enough trailing words ' +
                    'that the line wraps and proves wrapped lines hug the ' +
                    'start indent instead of the gutter',
            } as Command);
            await dispatch({ type: 'SPLIT_PARAGRAPH', at: pos(1, 143) } as Command);
            await dispatch({
                type: 'INSERT_TEXT',
                at: pos(2, 0),
                text: 'Numbered item one',
            } as Command);
            await dispatch({ type: 'SPLIT_PARAGRAPH', at: pos(2, 17) } as Command);
            await dispatch({
                type: 'INSERT_TEXT',
                at: pos(3, 0),
                text: 'Numbered item two',
            } as Command);
            await dispatch({
                type: 'TOGGLE_LIST',
                range: { start: pos(0, 0), end: pos(1, 0) },
                kind: 'Bullet',
            } as Command);
            paintEvt = await dispatch({
                type: 'TOGGLE_LIST',
                range: { start: pos(2, 0), end: pos(3, 0) },
                kind: 'Number',
            } as Command);
            break;
        }

        case 'rich-text-caps': {
            /* Issue #37 — <w:caps>/<w:smallCaps> display transform: the
               middle word renders ALL-CAPS, the last word renders in
               0.8x small caps; the leading word stays untouched as the
               control. The document model keeps the original text. */
            await dispatch({
                type: 'RENDER_PAGE',
                text: 'plain allcaps smallcaps',
                font_id: LATIN_ID,
                base_direction: 'LTR',
                px_size: 26,
                line_height: 40,
                align: 'START',
            } as Command);
            await dispatch({
                type: 'APPLY_FORMATTING',
                range: {
                    start: { path: { steps: [{ kind: 'BLOCK', idx: 0 }] }, offset: 6 },
                    end: { path: { steps: [{ kind: 'BLOCK', idx: 0 }] }, offset: 13 },
                },
                attrs: { caps: true },
            } as Command);
            paintEvt = await dispatch({
                type: 'APPLY_FORMATTING',
                range: {
                    start: { path: { steps: [{ kind: 'BLOCK', idx: 0 }] }, offset: 14 },
                    end: { path: { steps: [{ kind: 'BLOCK', idx: 0 }] }, offset: 23 },
                },
                attrs: { small_caps: true },
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
                stops: [{ position_pt: 250, kind: 'Center', leader: undefined }],
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
                stops: [{ position_pt: 300, kind: 'Right', leader: undefined }],
            } as Command);
            break;
        }

        case 'autofit-long-url-overflow': {
            /* L2.2 (#7) — single-cell autofit table containing a pure-
               letter token (no break opportunities). The column floor
               anchors at the token's min_content, so the table
               overflows the right page margin rather than wrapping
               the token mid-character. */
            await dispatch({
                type: 'RENDER_PAGE',
                text: '',
                font_id: LATIN_ID,
                base_direction: 'LTR',
                px_size: 18,
                line_height: 26,
                align: 'START',
            } as Command);
            await dispatch({
                type: 'INSERT_TABLE',
                at: { steps: [{ kind: 'BLOCK', idx: 1 }] },
                rows: 1,
                cols: 1,
            } as Command);
            const cellStart = {
                path: {
                    steps: [
                        { kind: 'BLOCK', idx: 1 },
                        { kind: 'CELL', row: 0, col: 0 },
                        { kind: 'BLOCK', idx: 0 },
                    ],
                },
                offset: 0,
            };
            paintEvt = await dispatch({
                type: 'INSERT_TEXT',
                at: cellStart,
                text: 'a'.repeat(100),
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
                stops: [{ position_pt: 250, kind: 'Decimal', leader: undefined }],
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
        const probe = await probeBackend(msg.mockBackend);
        const renderer = probe.renderer;
        engine = await constructEngine(msg.canvas, probe);
        pageSurfaces.set(0, msg.canvas);
        await openEventLog(msg.documentId);
        /* Issue #43 — inject today's date so DATE fields resolve at
           layout time (Word updates DATE on open/print). Single
           injection site: the engine core never reads a wall clock, so
           native tests and byte-stable exports stay deterministic. */
        const now = new Date();
        await dispatch({
            type: 'SET_RENDER_DATE',
            year: now.getFullYear(),
            month: now.getMonth() + 1,
            day: now.getDate(),
            /* Issue #77 — the clock half TIME fields resolve against. */
            hour: now.getHours(),
            minute: now.getMinutes(),
        } as Command);
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
        /* Issue #66 — re-probe the backend exactly as INIT does. The fresh
           canvas has taken no context yet, so Vello is available again
           whenever the GPU is. The engine reports what it ACTUALLY paints
           with on `Event::Recovered.renderer`; that value — never a
           remembered INIT-time one — is what the reply and the shell's
           `__renderer` carry. */
        /* Issue #99 — after a crash loop on the GPU backend the client
           forces Canvas2D: no probe, so a failing WebGPU driver / shader
           path cannot be re-selected and trap this generation too. */
        const probe =
            msg.forceRenderer === 'canvas2d'
                ? { renderer: 'canvas2d', mocked: false }
                : await probeBackend(msg.mockBackend);
        const probed = probe.renderer;
        engine = await constructEngine(msg.canvas, probe);
        pageSurfaces.set(0, msg.canvas);
        /* Resume the event-log sequence past what was already persisted, so
           post-recovery appends don't collide with or shadow prior rows. */
        logSequence = msg.lastSeq;
        trapAfterCommands = null;
        /* Issue #85 — base snapshot + replayed tail, inside the engine.
           Issue #241 — the candidates are tried newest first: a snapshot
           that does not restore (unreadable row) falls back to the next
           older one with its longer tail — every retained snapshot keeps
           its full tail (`persistSnapshot`'s pruning invariant) — and
           finally to the bare log. `Command::Recover` starts from a reset
           engine every time, so a failed attempt leaves nothing behind. */
        let evt: Event | undefined;
        let base: RecoveryCandidate | undefined;
        let snapshotFallbacks = 0;
        for (const candidate of msg.candidates) {
            const tail = msg.commands.filter((c) => c.seq > candidate.seq).map((c) => c.cmd);
            evt = await dispatch({
                type: 'RECOVER',
                snapshot: candidate.snapshot,
                log_tail: tail,
                ...(msg.rendererDowngrade ? { renderer_downgrade: msg.rendererDowngrade } : {}),
            });
            base = candidate;
            const usable =
                evt.type === 'RECOVERED' &&
                (evt.snapshot_restored || candidate.snapshot.length === 0);
            if (usable) break;
            snapshotFallbacks += 1;
            console.warn(
                `[worker] recovery: snapshot @${candidate.seq} did not restore; ` +
                    'falling back to the next older base',
            );
        }
        if (!evt) throw new Error('recovery: no base to recover from');
        /* The replay tail of the NEXT recovery starts after the base this
           one actually restored (0 = none: the next logged command then
           takes a snapshot straight away — `seq - 0 ≥ SNAPSHOT_EVERY`
           once the log is long). */
        lastSnapshotAt =
            evt.type === 'RECOVERED' && evt.snapshot_restored ? (base?.seq ?? 0) : 0;
        /* Issue #43 — a recovered engine needs the render date again.
           Dispatched AFTER `RECOVER`: its session reset wipes the clock
           half (TIME fields), so an injection ahead of it was lost. */
        const now = new Date();
        await dispatch({
            type: 'SET_RENDER_DATE',
            year: now.getFullYear(),
            month: now.getMonth() + 1,
            day: now.getDate(),
            hour: now.getHours(),
            minute: now.getMinutes(),
        });
        const recovered = evt.type === 'RECOVERED' ? evt : undefined;
        /* DEV mock (issue #99): the engine truthfully says `canvas2d`;
           the mocked generation must keep reporting the backend it
           pretends to run, or the crash-loop policy never sees Vello. */
        if (recovered && probe.mocked) recovered.renderer = probe.renderer;
        const renderer = recovered?.renderer ?? probed;
        const restored = recovered?.snapshot_restored === true;
        /* Issue #97 — the engine holds a live session when a snapshot was
           restored OR the replayed tail re-seeded one (it carried the boot
           RENDER_PAGE → a layout config, reported as `device_scale`). The
           shell then keeps it instead of re-seeding, so it needs the same
           media re-decode + a11y rebuild as a snapshot restore. */
        const sessionRestored = restored || recovered?.device_scale !== undefined;
        /* Phase 7 — a restored document may carry inline images whose
           bitmaps died with the old worker; decode them again. */
        if (sessionRestored) {
            await decodeAndRegisterMedia();
        }
        self.postMessage({
            id: msg.id,
            ok: true,
            evt,
            renderer,
            restored,
            appliedCommands: recovered?.applied_commands ?? 0,
            snapshotFallbacks,
            logComplete: msg.logComplete,
        });
        /* §10 — the recovered engine has no a11y cache, so this delta is a
           full `Replace`: the mirror DOM rebuilds from the restored tree
           instead of narrating 200 replayed edits. */
        if (sessionRestored) {
            const delta = await dispatch({ type: 'REQUEST_ACCESSIBILITY_DELTA' });
            if (delta.type === 'ACCESSIBILITY_TREE_DELTA') {
                self.postMessage({ evt: delta });
                /* Issue #194 — the replace covers every replayed edit. */
                broadcastMutationSeq = engine.document_mutation_seq();
            }
        }
    } catch (e: unknown) {
        replyError(msg.id, e);
    }
}

/**
 * Whether a successfully dispatched command belongs in the durable event
 * log. Recovery replays the tail through `dispatch`, so anything that moves
 * engine state a later logged command depends on must be kept — document
 * mutations, selection/caret moves (caret-relative
 * edits like `INSERT_TEXT` at `undefined` replay wrong without them),
 * composition, font loads, and view state. Pure read-back queries are
 * skipped: they replay as no-ops, and the per-pointermove `HIT_TEST` alone
 * would grow the commands store without bound. `PING` stays logged — the
 * D2.6 exit gate (e2e/event-log-replay.spec.ts) drives the sequence with it.
 */
function shouldLogCommand(cmd: Command): boolean {
    switch (cmd.type) {
        case 'HIT_TEST':
        case 'HIT_TEST_IN_PAGE':
        case 'REQUEST_PAINT':
        case 'REQUEST_ACCESSIBILITY_DELTA':
        case 'GET_SELECTION_AS_CLIPBOARD':
        case 'REQUEST_STATS':
        case 'SAVE_DOCX':
        case 'SAVE_DOCUMENT':
        case 'EXPORT_PDF':
        /* Issue #85 — the recovery primitives are never part of the
           history they persist / restore. */
        case 'SNAPSHOT':
        case 'RECOVER':
            return false;
        default:
            return true;
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
        notePaintVersion(evt);
        self.postMessage({ id: msg.id, ok: true, evt, elapsed });
        /* D2.8 backpressure (PHASE_2_BRIDGE_MEMORY.md §12 risk 5): persist to
           the event log OFF the critical path. The RPC response is already
           sent, so event-log latency never throttles command throughput. */
        if (shouldLogCommand(msg.cmd)) {
            const seq = logCommand(msg.cmd);
            /* Issue #85 — cadence snapshot, taken HERE (still inside this
               command's queue task, reply already posted) so its bytes
               describe exactly the state after `seq`: a command queued
               behind us cannot slip in between and get replayed twice. */
            if (seq - lastSnapshotAt >= SNAPSHOT_EVERY) {
                await takeSnapshot(seq);
            }
            armIdleSnapshot();
            if (trapAfterCommands !== null && --trapAfterCommands <= 0) {
                trapAfterCommands = null;
                await pendingLogWrites;
                /* Throws `RuntimeError: unreachable` → the catch below
                   flags `trap: true` and closes the worker. */
                engine.debug_force_trap();
            }
        }
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
           the paragraphs that changed.

           Issue #194 — "was this a mutation?" is the ENGINE's answer, not a
           hand-kept list of command types (that list drifted: styles, lists,
           header/footer + note edits, fields, sections, table properties,
           image moves, TOC… never refreshed the mirror). The engine bumps
           `document_mutation_seq` once per command that changed the
           document; comparing it to the last value we broadcast for also
           catches worker-internal dispatches (boot, render date) that
           restamped the document between client commands. */
        const mutationSeq = engine.document_mutation_seq();
        if (mutationSeq !== broadcastMutationSeq) {
            broadcastMutationSeq = mutationSeq;
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
        const dims = engine.paint_dims() as {
            document_height: number;
            page_count: number;
            estimated_document_height: number;
            is_full_layout: boolean;
            page_tops: number[];
            page_heights: number[];
            image_count: number;
            page_margin_tops: number[];
            page_margin_bottoms: number[];
            page_content_tops: number[];
            page_content_bottoms: number[];
            layout_degraded: { reason: string; page: number | undefined }[];
            /** Issue #86 — real cost (ms) of the last actual paint, replayed
             *  here since this side-channel doesn't repaint. */
            paint_ms: number;
            /** Issue #194 — the engine's document-mutation counter. */
            mutation_seq: number;
        };
        self.postMessage({
            evt: {
                type: 'PAINTED',
                dirty: { x: 0, y: 0, w: 0, h: 0 },
                version: lastPaintVersion,
                paint_ms: dims.paint_ms,
                document_height: dims.document_height,
                page_count: dims.page_count,
                is_full_layout: dims.is_full_layout,
                estimated_document_height: dims.estimated_document_height,
                page_tops: dims.page_tops,
                page_heights: dims.page_heights,
                image_count: dims.image_count,
                /* Phase 3 (#39) — the double-click header/footer zone
                gate reads per-page margins; BOTH Painted producers (the
                real paint and this synthetic side-channel) must carry
                them (the #44 image_count gate relearned this the hard
                way). */
                page_margin_tops: dims.page_margin_tops,
                page_margin_bottoms: dims.page_margin_bottoms,
                /* Issue #71 — effective body extents: the zone gate's
                truth once a band intrudes past its margin. */
                page_content_tops: dims.page_content_tops,
                page_content_bottoms: dims.page_content_bottoms,
                /* Issue #87 — the last real paint's degradation notes
                ride the synthetic side-channel too, so a consumer never
                sees a degraded paint "heal" on the next mutation. */
                layout_degraded: dims.layout_degraded ?? [],
                /* Issue #194 — same counter the real paint carries. */
                mutation_seq: dims.mutation_seq,
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
 * though the IndexedDB writes settle asynchronously. Returns the row's seq.
 */
function logCommand(cmd: Command): number {
    const seq = ++logSequence;
    const write = appendCommand(seq, cmd).catch((e: unknown) =>
        console.warn('[worker] event-log append failed', e),
    );
    pendingLogWrites = pendingLogWrites.then(() => write);
    return seq;
}

/**
 * Issue #85 — take an engine snapshot at log position `seq` and persist it
 * (`persistSnapshot` prunes to the newest 3 and drops the command rows
 * they make unreachable). Runs on the serial queue with no command in
 * flight — callers are either a command task after its reply, or the
 * queued idle task — so the bytes describe exactly the state after
 * command `seq` and recovery's replay tail starts at `seq + 1`. The
 * `SNAPSHOT` dispatch (≈ one document serialization) is the only
 * synchronous cost; the IndexedDB write settles off the critical path.
 */
async function takeSnapshot(seq: number): Promise<void> {
    if (!engine || seq <= lastSnapshotAt) return;
    try {
        const evt = await dispatch({ type: 'SNAPSHOT', seq });
        if (evt.type !== 'SNAPSHOT') {
            console.warn('[worker] engine snapshot failed', evt);
            return;
        }
        lastSnapshotAt = seq;
        const write = persistSnapshot(seq, evt.bytes).catch((e: unknown) =>
            console.warn('[worker] event-log snapshot failed', e),
        );
        pendingLogWrites = pendingLogWrites.then(() => write);
    } catch (e: unknown) {
        console.warn('[worker] snapshot dispatch failed', e);
    }
}

/** Issue #85 — (re)arm the idle snapshot timer after a logged command. */
function armIdleSnapshot(): void {
    if (idleSnapshotTimer !== undefined) clearTimeout(idleSnapshotTimer);
    idleSnapshotTimer = setTimeout(() => {
        idleSnapshotTimer = undefined;
        /* Read `logSequence` INSIDE the queued task: commands already
           queued ahead of it advance the sequence before it runs, and the
           snapshot must be stamped with the position it really captures. */
        void enqueue(() => takeSnapshot(logSequence));
    }, SNAPSHOT_IDLE_MS);
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
       is read-only document metadata, not an event-log mutation).
       Issue #51 — routed through the serial queue: even a `&self` wasm
       call aliases the engine while an in-flight `&mut self` dispatch
       future is parked at an await, and wasm-bindgen panics with
       "recursive use of an object". */
    /* Issue #96 — DEV-only paint probe (test hook). Headless Chrome never
       composites a transferred placeholder `<canvas>` for the full app,
       so a main-thread `drawImage` readback cannot tell a painted page
       from a blank one there. Read the pixels where they actually land —
       the OffscreenCanvas surfaces this worker was handed — and count
       OPAQUE non-white ink per page (an unpainted surface is transparent
       black; alpha > 200 keeps it from counting as ink). `-1` = the
       surface has no 2d context (Vello page 0) or is not registered. */
    if (msg.type === 'PROBE_PAGE_INK') {
        void enqueue(async () => {
            if (!import.meta.env.DEV) {
                self.postMessage({ id: msg.id, ok: false, error: 'PROBE_PAGE_INK is dev-only' });
                return;
            }
            const ink: Record<number, number> = {};
            for (const [idx, surface] of pageSurfaces) {
                ink[idx] = countOpaqueInk(surface);
            }
            self.postMessage({ id: msg.id, ok: true, ink });
        });
        return;
    }

    if (msg.type === 'GET_COMMENTS') {
        void enqueue(async () => {
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
        });
        return;
    }

    /* Phase 6c — multi-canvas registration. Engine's `set_page_canvas`
       transfers the OffscreenCanvas to a per-page slot; the next paint
       fills it.

       Issue #51 — two hardening moves over the original shape:
       1. The registration AND its follow-up repaint ride the serial
          queue (the old `void dispatch(...)` bypassed it — overlapping
          an in-flight command triggers wasm-bindgen's "recursive use
          of an object" panic, and a catch-less rejection wedged the
          worker silently).
       2. The repaint is COALESCED: opening a multi-page document
          mounts one canvas per page, and one full repaint per
          registration was O(pages²) page paints. A macrotask-deferred
          flag folds each registration burst into a single repaint. */
    if (msg.type === 'REGISTER_PAGE_CANVAS') {
        void enqueue(async () => {
            if (!engine) {
                self.postMessage({ id: msg.id, ok: false, error: 'engine not initialized' });
                return;
            }
            try {
                /* Issue #96 — a respawned worker receives registrations for
                   pages its restored layout already knows (the shell
                   remounts every page canvas after a trap): the slot is
                   simply (re)filled with the fresh surface. */
                engine.set_page_canvas(msg.idx, msg.canvas);
                pageSurfaces.set(msg.idx, msg.canvas);
                self.postMessage({ id: msg.id, ok: true });
            } catch (e: unknown) {
                replyError(msg.id, e);
                return;
            }
            schedulePageCanvasRepaint();
        });
        return;
    }

    /* Phase 8b — revisions side-channel mirror of `GET_COMMENTS`;
       queued for the same aliasing reason. */
    if (msg.type === 'GET_REVISIONS') {
        void enqueue(async () => {
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
        });
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

    /* Issue #85 — fault injection. Not queued: arming must take effect
       before the commands posted right after it. */
    if (msg.type === 'ARM_TRAP') {
        trapAfterCommands = Math.max(1, Math.floor(msg.after_commands));
        self.postMessage({ id: msg.id, ok: true });
        return;
    }

    if (msg.type === 'COMMAND') {
        void enqueue(() => handleCommand(msg));
    }
};
