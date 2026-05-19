/// <reference lib="webworker" />

import init, { Engine } from '../../crates/engine-wasm/pkg/engine_wasm.js';
import type { Command, Event } from '../../crates/engine-wasm/pkg/engine_wasm.js';

declare const self: DedicatedWorkerGlobalScope;

type InitMsg = { type: 'INIT'; canvas: OffscreenCanvas; testCase: string };

const LATIN_URL = '/fonts/LiberationSans-Regular.ttf';
const LATIN_ID = 'liberation-sans';
const ARABIC_URL = '/fonts/NotoNaskhArabic-Regular.ttf';
const ARABIC_ID = 'noto-naskh-arabic';

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

self.onmessage = async (ev: MessageEvent<InitMsg>) => {
    const msg = ev.data;
    try {
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

        /* Load only what each case needs. */
        if (testCase === 'hello-arabic') {
            const arabicBytes = await fetchBytes(ARABIC_URL);
            const e = await dispatch({
                type: 'LOAD_FONT',
                id: ARABIC_ID,
                bytes: arabicBytes,
            } as Command);
            self.postMessage({ type: 'FONT_LOADED_RESULT', event: e });
        } else {
            const latinBytes = await fetchBytes(LATIN_URL);
            const e = await dispatch({
                type: 'LOAD_FONT',
                id: LATIN_ID,
                bytes: latinBytes,
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
