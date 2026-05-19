/// <reference lib="webworker" />

import init, { Engine } from '../../crates/engine-wasm/pkg/engine_wasm.js';
import type { Command, Event } from '../../crates/engine-wasm/pkg/engine_wasm.js';

declare const self: DedicatedWorkerGlobalScope;

type InitMsg = { type: 'INIT'; canvas: OffscreenCanvas };

const FONT_URL = '/fonts/LiberationSans-Regular.ttf';
const FONT_ID = 'liberation-sans';
const PX_SIZE = 128;
const TEST_CHAR = 'A';

let engine: Engine | null = null;

self.onmessage = async (ev: MessageEvent<InitMsg>) => {
    const msg = ev.data;
    try {
        if (msg.type === 'INIT') {
            await init({
                module_or_path: new URL(
                    '../../crates/engine-wasm/pkg/engine_wasm_bg.wasm',
                    import.meta.url,
                ),
            });
            engine = new Engine(msg.canvas);
            self.postMessage({ type: 'BOOT_OK' });

            /* Ping/pong sanity. */
            const pong = (await engine.dispatch({ type: 'PING' } as Command)) as Event;
            self.postMessage({ type: 'PING_RESULT', event: pong });

            /* Week 5: fetch font bytes and load. */
            const fontResp = await fetch(FONT_URL);
            if (!fontResp.ok) throw new Error(`fetch font: HTTP ${fontResp.status}`);
            const fontBytes = new Uint8Array(await fontResp.arrayBuffer());

            const loadEvt = (await engine.dispatch({
                type: 'LOAD_FONT',
                id: FONT_ID,
                bytes: fontBytes,
            } as Command)) as Event;
            self.postMessage({ type: 'FONT_LOADED_RESULT', event: loadEvt });

            /* Week 6: rasterize 'A' and paint via swash + putImageData. */
            const rasterEvt = (await engine.dispatch({
                type: 'RASTERIZE_GLYPH',
                font_id: FONT_ID,
                ch: TEST_CHAR,
                px_size: PX_SIZE,
            } as Command)) as Event;
            self.postMessage({ type: 'GLYPH_RESULT', event: rasterEvt });

            self.postMessage({ type: 'IDLE' });
            return;
        }
    } catch (e: unknown) {
        const error = e instanceof Error ? e.message : String(e);
        self.postMessage({ type: 'ERROR', error });
    }
};
