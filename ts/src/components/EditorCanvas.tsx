/* Phase 4 §5 — EditorCanvas: the canvas mount.
 *
 * Owns the single <canvas>, transfers its control to the engine worker once
 * (`transferControlToOffscreen()` is one-shot per element), and forwards
 * viewport resizes. The engine never touches this element again — it draws
 * into the transferred OffscreenCanvas from the worker.
 *
 * Phase 6b — the engine reports a paginated `document_height` on every
 * `Painted` event. The mount reads it from `store.documentHeight` and
 * sets the element's CSS height so the browser scrollbar exposes every
 * page. The OffscreenCanvas backing store is resized worker-side. */
import { createEffect, onCleanup, onMount } from 'solid-js';
import { attachPointer } from '../input/pointer';
import type { EngineClient } from '../engine/engine-client';
import type { EngineStore } from '../state/engine-store';

export interface EditorCanvasProps {
    client: EngineClient;
    store: EngineStore;
    /**
     * `0` on first boot; bumped by App on every crash so Solid remounts a
     * fresh <canvas> — a consumed OffscreenCanvas cannot be re-transferred.
     * `0` → `client.init()`, any other value → `client.recover()`.
     */
    generation: number;
    /** Run once the engine holds the surface; receives the init/recover ms. */
    onReady: (generation: number, initMs: number) => Promise<void>;
}

export function EditorCanvas(props: EditorCanvasProps) {
    let canvasRef: HTMLCanvasElement | undefined;
    let resizeObserver: ResizeObserver | undefined;
    let detachPointer: (() => void) | undefined;

    onMount(async () => {
        const canvas = canvasRef!;

        /* Size the backing store in device pixels so text stays crisp on
           high-DPI displays; CSS holds the element at viewport size. */
        const dpr = window.devicePixelRatio || 1;
        canvas.width = Math.max(1, Math.round(canvas.clientWidth * dpr));
        canvas.height = Math.max(1, Math.round(canvas.clientHeight * dpr));

        const offscreen = canvas.transferControlToOffscreen();

        const t0 = performance.now();
        if (props.generation === 0) {
            await props.client.init(offscreen);
        } else {
            await props.client.recover(offscreen);
        }
        const initMs = performance.now() - t0;

        await props.onReady(props.generation, initMs);

        /* Viewport resize → engine. `SET_VIEWPORT` is still an engine-side
           stub (engine-wasm `phase3_stub`); the dispatch lands with the
           scroll/viewport work. The observer is wired now so the plumbing
           is in place. */
        resizeObserver = new ResizeObserver(() => {
            /* TODO: dispatch SET_VIEWPORT once the engine implements it. */
        });
        resizeObserver.observe(canvas);

        /* §7 — pointer → engine hit-testing. Pointer events still fire on a
           canvas whose drawing surface has been transferred to the worker.
           Phase 6c — page 0; clicks dispatch `HIT_TEST_IN_PAGE { page: 0 }`. */
        detachPointer = attachPointer(canvas, props.client, 0);
    });

    /* Phase 6c — multi-canvas DOM: each `.editor-page` element keeps its
       fixed A4 dimensions (CSS `width: 794px; min-height: 1123px`),
       and the canvas inside fills it. No more growing `min-height`
       per paginated page — multi-page documents grow by mounting
       more `<canvas>` elements (see `ExtraPageCanvas`), not by
       stretching one. The `documentHeight` signal is now informational
       only (telemetry, scroll bookkeeping). */
    createEffect(() => {
        /* read-only subscription so the canvas mounts react to layout
           reconfigurations triggered by section changes. No-op body. */
        void props.store.documentHeight();
    });

    onCleanup(() => {
        resizeObserver?.disconnect();
        detachPointer?.();
    });

    /* No `tabindex` — a focusable canvas would steal focus from the hidden
       textarea on click, killing keyboard input. The textarea owns focus. */
    return <canvas ref={canvasRef} class="editor-canvas" />;
}
