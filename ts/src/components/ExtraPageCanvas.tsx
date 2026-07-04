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
import type { EngineStore } from '../state/engine-store';
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
        const dpr = window.devicePixelRatio || 1;
        /* Pre-sized to the A4-at-96-DPI page so the transferred backing
           store matches the engine's first paint. The worker resizes
           per-page if the section's geometry differs (Phase 6 section
           support). */
        canvas.width = Math.max(1, Math.round(canvas.clientWidth * dpr));
        canvas.height = Math.max(1, Math.round(canvas.clientHeight * dpr));
        const offscreen = canvas.transferControlToOffscreen();
        await props.client.registerPageCanvas(props.pageIdx, offscreen);
        detach = attachPointer(canvas, props.client, props.pageIdx);
    });

    onCleanup(() => detach?.());

    return (
        <div class="editor-page" data-page-index={props.pageIdx}>
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
