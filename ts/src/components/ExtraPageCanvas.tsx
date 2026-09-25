/* Phase 6c — multi-canvas DOM page card.
 *
 * One `<canvas>` per paginated page beyond the first. Mounts a fresh
 * `OffscreenCanvas` (~595 × 842 layout pt → 794 × 1123 CSS px = well
 * inside Safari's 4096 px and Chrome's 32 k canvas size limits) and
 * transfers it to the worker via `EngineClient.registerPageCanvas`.
 * The engine paints page `idx` into THIS canvas at the next render.
 *
 * Page 0's canvas is the boot canvas (handled by `EditorCanvas` so the
 * one-time `client.init()` flow stays linear). Pages 1+ instantiate
 * this component. DevTools picks each one out as a distinct DOM
 * element — clicking a page in the inspector highlights only that
 * page, never the full document. */
import { onCleanup, onMount } from 'solid-js';
import type { EngineClient } from '../engine/engine-client';
import { deviceRatio, type EngineStore } from '../state/engine-store';
import { attachPointer } from '../input/pointer';
import { PageSelectionOverlay } from './PageSelectionOverlay';
import { CaretOverlay } from './CaretOverlay';
import { ImageHandlesOverlay } from './ImageHandlesOverlay';
import { StoryModeOverlay } from './StoryModeOverlay';

export interface ExtraPageCanvasProps {
    client: EngineClient;
    store: EngineStore;
    pageIdx: number;
}

export function ExtraPageCanvas(props: ExtraPageCanvasProps) {
    let canvasRef: HTMLCanvasElement | undefined;
    let detach: (() => void) | undefined;

    onMount(async () => {
        const canvas = canvasRef!;
        /* Pre-sized to the card (already at the engine's page geometry +
           zoom, issue #280) so the transferred backing store matches the
           engine's first paint. The worker resizes per-page if the
           section's geometry differs (Phase 6 section support). */
        const dpr = deviceRatio();
        canvas.width = Math.max(1, Math.round(canvas.clientWidth * dpr));
        canvas.height = Math.max(1, Math.round(canvas.clientHeight * dpr));
        const offscreen = canvas.transferControlToOffscreen();
        detach = attachPointer(canvas, props.client, props.pageIdx);
        /* Issue #96 — this element is one-shot: a trap unmounts it (App's
           `booting` boundary) and recovery mounts a FRESH one that
           registers with the respawned worker. A registration that loses
           a race with a trap is therefore not retried here — the
           remount after recovery is the retry. */
        try {
            await props.client.registerPageCanvas(props.pageIdx, offscreen);
        } catch (e: unknown) {
            console.warn(`[page ${props.pageIdx}] canvas registration failed`, e);
        }
    });

    onCleanup(() => detach?.());

    return (
        <div
            class="editor-page"
            data-page-index={props.pageIdx}
            style={{
                /* Issue #280 — sized from the engine's page geometry, so
                   zoom (and a landscape section) resizes the card. */
                width: `${props.store.pageCardCss(props.pageIdx).w}px`,
                height: `${props.store.pageCardCss(props.pageIdx).h}px`,
            }}
        >
            <canvas ref={canvasRef} class="editor-canvas" />
            <PageSelectionOverlay store={props.store} pageIdx={props.pageIdx} />
            <CaretOverlay store={props.store} pageIdx={props.pageIdx} />
            <ImageHandlesOverlay
                store={props.store}
                client={props.client}
                pageIdx={props.pageIdx}
            />
            <StoryModeOverlay store={props.store} pageIdx={props.pageIdx} />
        </div>
    );
}
