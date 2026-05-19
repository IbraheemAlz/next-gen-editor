/// <reference lib="webworker" />

import init, { Engine } from '../../crates/engine-wasm/pkg/engine_wasm.js';
import type { Command, Event } from '../../crates/engine-wasm/pkg/engine_wasm.js';

declare const self: DedicatedWorkerGlobalScope;

type InitMsg = { type: 'INIT'; canvas: OffscreenCanvas; testCase: string };
type CommandMsg = { type: 'COMMAND'; id: number; cmd: Command };
type Msg = InitMsg | CommandMsg;

const LATIN_URL = '/fonts/LiberationSans-Regular.ttf';
const LATIN_ID = 'liberation-sans';
const ARABIC_URL = '/fonts/NotoNaskhArabic-Regular.ttf';
const ARABIC_ID = 'noto-naskh-arabic';

const A4_TEXT =
    'هذا نص تجريبي مكتوب باللغة العربية لاختبار خوارزمية تخطيط الصفحة. ' +
    'This paragraph mixes Arabic and English text to validate BiDi run resolution, ' +
    'greedy line breaking via icu_segmenter, and basic Kashida elongation. ' +
    'الكلمات العربية يجب أن تظهر بالشكل الصحيح مع الربط بين الحروف. ' +
    'The justify alignment should stretch each non-final line to reach both margins.';

let engine: Engine | null = null;

async function fetchBytes(url: string): Promise<Uint8Array> {
    const r = await fetch(url);
    if (!r.ok) throw new Error(`fetch ${url}: HTTP ${r.status}`);
    return new Uint8Array(await r.arrayBuffer());
}

async function dispatch(cmd: Command): Promise<Event> {
    if (!engine) throw new Error('engine not initialized');
    return (await engine.dispatch(cmd)) as Event;
}

self.onmessage = async (ev: MessageEvent<Msg>) => {
    const msg = ev.data;
    try {
        if (msg.type === 'COMMAND') {
            const result = await dispatch(msg.cmd);
            self.postMessage({ type: 'COMMAND_RESULT', id: msg.id, event: result });
            return;
        }
        if (msg.type !== 'INIT') return;

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

        const testCase = msg.testCase || 'glyph-a';

        /* Pre-test font loading. */
        if (testCase === 'a4-justified-mixed' || testCase === 'hello-arabic' ||
            testCase === 'editing-arabic' || testCase === 'docx-round-trip') {
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
                /* Phase 1 W15-18 demo:
                   1. RenderPage seeds the document + caches layout config.
                   2. Two InsertText calls each auto-repaint.
                   3. Final canvas = "السلام عليكم ورحمة الله".
                   undo/redo roundtrip verified via console events. */
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
                /* Demonstrate undo + redo (final state = post-redo, same as e2). */
                const u1 = await dispatch({ type: 'UNDO' } as Command);
                self.postMessage({ type: 'EDIT_RESULT', step: 'undo', event: u1 });
                const r1 = await dispatch({ type: 'REDO' } as Command);
                self.postMessage({ type: 'EDIT_RESULT', step: 'redo', event: r1 });
                paintEvt = r1;
                break;
            }

            case 'docx-round-trip': {
                /* Phase 1 W19-24 demo:
                   1. SaveDocx on the current (empty) doc — produces minimal .docx.
                   2. LoadDocx round-trips it.
                   3. Insert + save again. */
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
                const inserted = await dispatch({
                    type: 'INSERT_TEXT',
                    at: undefined,
                    text: ' تم التعديل',
                } as Command);
                paintEvt = inserted;
                break;
            }

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
    } catch (e: unknown) {
        const error = e instanceof Error ? e.message : String(e);
        self.postMessage({ type: 'ERROR', error });
    }
};
