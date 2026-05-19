/// <reference lib="webworker" />

import init, { Engine } from '../../crates/engine-wasm/pkg/engine_wasm.js';
import type { Command, Event } from '../../crates/engine-wasm/pkg/engine_wasm.js';
/* Fonts are imported as Vite `?url` assets, NOT fetched from absolute
   `/fonts/...` paths. Absolute paths break under a deploy subpath (e.g.
   GitHub Pages /next-gen-editor/); `?url` imports are hashed + base-aware. */
import LATIN_URL from '../fonts/LiberationSans-Regular.ttf?url';
import ARABIC_URL from '../fonts/NotoNaskhArabic-Regular.ttf?url';
import DUAL_URL from '../fonts/Amiri-Regular.ttf?url';

declare const self: DedicatedWorkerGlobalScope;

type InitMsg = { type: 'INIT'; canvas: OffscreenCanvas; testCase: string };
type CommandMsg = { type: 'COMMAND'; id: number; cmd: Command };
type Msg = InitMsg | CommandMsg;

const LATIN_ID = 'liberation-sans';
const ARABIC_ID = 'noto-naskh-arabic';
/* Amiri is a book-quality Naskh face that ALSO ships Latin glyphs, so the
   interactive editor can render mixed Arabic/English without engine-side
   font fallback (that lands in Phase 3). The visual-diff test cases keep
   their original single-script fonts so their goldens stay valid. */
const DUAL_ID = 'amiri';

const A4_TEXT =
    'هذا نص تجريبي مكتوب باللغة العربية لاختبار خوارزمية تخطيط الصفحة. ' +
    'This paragraph mixes Arabic and English text to validate BiDi run resolution, ' +
    'greedy line breaking via icu_segmenter, and basic Kashida elongation. ' +
    'الكلمات العربية يجب أن تظهر بالشكل الصحيح مع الربط بين الحروف. ' +
    'The justify alignment should stretch each non-final line to reach both margins.';

let engine: Engine | null = null;

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
            '../../crates/engine-wasm/pkg/engine_wasm_bg.wasm',
            import.meta.url,
        ),
    });
    engine = new Engine(msg.canvas);
    self.postMessage({ type: 'BOOT_OK' });

    const pong = await dispatch({ type: 'PING' } as Command);
    self.postMessage({ type: 'PING_RESULT', event: pong });

    /* Default (no ?test= param) is the interactive A4 editor. */
    const testCase = msg.testCase || 'interactive';

    /* Per-case font loading. Interactive uses the dual-script Amiri so
       mixed Arabic/English renders; the test cases keep their original
       single-script fonts so their committed goldens stay valid. */
    if (testCase === 'interactive') {
        const e = await dispatch({
            type: 'LOAD_FONT',
            id: DUAL_ID,
            bytes: await fetchBytes(DUAL_URL),
        } as Command);
        self.postMessage({ type: 'FONT_LOADED_RESULT', event: e });
    } else if (
        testCase === 'a4-justified-mixed' ||
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

self.onmessage = (ev: MessageEvent<Msg>): void => {
    const msg = ev.data;
    if (msg.type === 'INIT') {
        enqueue(() => handleInit(msg)).catch((e: unknown) => {
            const error = e instanceof Error ? e.message : String(e);
            self.postMessage({ type: 'ERROR', error });
        });
    } else if (msg.type === 'COMMAND') {
        void enqueue(() => handleCommand(msg));
    }
};
