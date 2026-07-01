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
import { PAGE_H_PT, SCREEN_DPI_SCALE } from '../state/engine-store';

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
    let detachViewport: (() => void) | undefined;

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

        /* Audit gap C.H1 — feed the engine's lazy-layout band. Scroll +
           viewport-resize ticks are rAF-coalesced into a `SET_VIEWPORT`
           carrying the visible rect in document-relative device px
           (origin = this page-0 canvas's top-left). When the visible
           bottom nears the laid-out tail, `EXPAND_LAYOUT` forces a paint
           that extends the band — its `target_y` is layout pt at
           scale = 1 (the engine multiplies by its own scale). */
        const scroller = canvas.closest('.editor-viewport');
        let rafId = 0;
        let expandedToPt = 0;
        const pushViewport = (): void => {
            rafId = 0;
            if (!scroller) return;
            const dpr = window.devicePixelRatio || 1;
            const r = canvas.getBoundingClientRect();
            const v = scroller.getBoundingClientRect();
            const rect = {
                x: (v.left - r.left) * dpr,
                y: (v.top - r.top) * dpr,
                w: scroller.clientWidth * dpr,
                h: scroller.clientHeight * dpr,
            };
            void props.client.dispatch({ type: 'SET_VIEWPORT', rect });
            const scale = dpr * SCREEN_DPI_SCALE;
            const laidOutPt = props.store.documentHeight() / scale;
            const bottomPt = (rect.y + rect.h) / scale;
            const targetPt = bottomPt + 2 * PAGE_H_PT;
            /* Skip when the paginator already exhausted every block, laid
               out past the buffered target, or an equal-or-deeper expand
               is in flight — a redundant EXPAND_LAYOUT still repaints. */
            if (
                !props.store.isFullLayout() &&
                bottomPt + PAGE_H_PT > laidOutPt &&
                targetPt > laidOutPt &&
                targetPt > expandedToPt
            ) {
                expandedToPt = targetPt;
                void props.client.dispatch({ type: 'EXPAND_LAYOUT', target_y: targetPt });
            }
        };
        const schedule = (): void => {
            if (rafId === 0) rafId = requestAnimationFrame(pushViewport);
        };
        scroller?.addEventListener('scroll', schedule, { passive: true });
        resizeObserver = new ResizeObserver(schedule);
        resizeObserver.observe(scroller ?? canvas);
        schedule();
        detachViewport = () => {
            if (rafId !== 0) cancelAnimationFrame(rafId);
            scroller?.removeEventListener('scroll', schedule);
        };

        /* §7 — pointer → engine hit-testing. Pointer events still fire on a
           canvas whose drawing surface has been transferred to the worker.
           Phase 6c — page 0; clicks dispatch
           `HIT_TEST_IN_PAGE { page: 0, at: local }`. */
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
        detachViewport?.();
        detachPointer?.();
    });

    /* No `tabindex` — a focusable canvas would steal focus from the hidden
       textarea on click, killing keyboard input. The textarea owns focus. */
    return <canvas ref={canvasRef} class="editor-canvas" />;
}
