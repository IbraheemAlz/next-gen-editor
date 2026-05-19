/// <reference lib="webworker" />

import init, { Engine } from '../../crates/engine-wasm/pkg/engine_wasm.js';
import type { Command, Event } from '../../crates/engine-wasm/pkg/engine_wasm.js';

declare const self: DedicatedWorkerGlobalScope;

type InitMsg = { type: 'INIT'; canvas: OffscreenCanvas };

let engine: Engine | null = null;

self.onmessage = async (ev: MessageEvent<InitMsg>) => {
    const msg = ev.data;
    try {
        if (msg.type === 'INIT') {
            await init(
                new URL('../../crates/engine-wasm/pkg/engine_wasm_bg.wasm', import.meta.url),
            );
            engine = new Engine(msg.canvas);
            self.postMessage({ type: 'BOOT_OK' });

            const evt = (await engine.dispatch({ type: 'PING' } as Command)) as Event;
            self.postMessage({ type: 'PING_RESULT', event: evt });
            return;
        }
    } catch (e: unknown) {
        const error = e instanceof Error ? e.message : String(e);
        self.postMessage({ type: 'ERROR', error });
    }
};
