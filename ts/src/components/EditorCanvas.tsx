/* Phase 4 §5 — EditorCanvas: the canvas mount.
 *
 * Owns the single <canvas>, transfers its control to the engine worker once
 * (`transferControlToOffscreen()` is one-shot per element), and forwards
 * viewport resizes. The engine never touches this element again — it draws
 * into the transferred OffscreenCanvas from the worker. */
import { onCleanup, onMount } from 'solid-js';
import { attachPointer } from '../input/pointer';
import type { EngineClient } from '../engine/engine-client';

export interface EditorCanvasProps {
    client: EngineClient;
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
           canvas whose drawing surface has been transferred to the worker. */
        detachPointer = attachPointer(canvas, props.client);
    });

    onCleanup(() => {
        resizeObserver?.disconnect();
        detachPointer?.();
    });

    return <canvas ref={canvasRef} class="editor-canvas" tabindex="-1" />;
}
