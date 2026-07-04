/* Issue #44 — interactive image resize handles.
 *
 * When an inline image is selected (`store.selectedImage()`, set by the
 * pointer path on an image click), this overlay draws a bounding outline
 * plus 8 resize handles positioned in the page's LOCAL coordinate space
 * (same per-page overlay pattern as PageSelectionOverlay). Dragging a
 * handle resizes the image live:
 *
 *   - The overlay optimistically restyles itself each pointermove for
 *     smooth feedback, anchored at the image's top-left (matching how the
 *     engine re-lays-out an inline image — the anchor byte is fixed, the
 *     box grows right/down).
 *   - New EMU is a pure CSS-px ratio of the current extent
 *     (`newEmu = emu × newPx / oldPx`) — zoom / DPR independent, using the
 *     `width_emu`/`height_emu` the engine ships in each `ImageRect`.
 *   - Engine dispatches are coalesced to one per animation frame; the
 *     final size is flushed synchronously on pointerup.
 *
 * Corner handles keep aspect ratio; edge handles stretch a single axis. */
import { For, Show, createSignal } from 'solid-js';
import type { EngineClient } from '../engine/engine-client';
import type { EngineStore, ImageRectCss } from '../state/engine-store';

interface HandleDef {
    id: string;
    /** Fractional position within the rect (0..1). */
    fx: number;
    fy: number;
    /** How a pointer delta maps onto width/height change. */
    dw: number;
    dh: number;
    corner: boolean;
    cursor: string;
}

const HANDLES: HandleDef[] = [
    { id: 'nw', fx: 0, fy: 0, dw: -1, dh: -1, corner: true, cursor: 'nwse-resize' },
    { id: 'n', fx: 0.5, fy: 0, dw: 0, dh: -1, corner: false, cursor: 'ns-resize' },
    { id: 'ne', fx: 1, fy: 0, dw: 1, dh: -1, corner: true, cursor: 'nesw-resize' },
    { id: 'e', fx: 1, fy: 0.5, dw: 1, dh: 0, corner: false, cursor: 'ew-resize' },
    { id: 'se', fx: 1, fy: 1, dw: 1, dh: 1, corner: true, cursor: 'nwse-resize' },
    { id: 's', fx: 0.5, fy: 1, dw: 0, dh: 1, corner: false, cursor: 'ns-resize' },
    { id: 'sw', fx: 0, fy: 1, dw: -1, dh: 1, corner: true, cursor: 'nesw-resize' },
    { id: 'w', fx: 0, fy: 0.5, dw: -1, dh: 0, corner: false, cursor: 'ew-resize' },
];

const MIN_PX = 12;

export function ImageHandlesOverlay(props: {
    store: EngineStore;
    client: EngineClient;
    pageIdx: number;
}) {
    /* Optimistic size during a drag, in CSS px — null when not dragging. */
    const [liveSize, setLiveSize] = createSignal<{ w: number; h: number } | null>(null);

    /** The selected image's rect, if it exists and sits on THIS page. */
    const selected = (): ImageRectCss | undefined => {
        const sel = props.store.selectedImage();
        if (!sel) return undefined;
        const top = props.store.pageTopCss(props.pageIdx);
        const bottom = top + props.store.pageHeightCss(props.pageIdx);
        return props.store
            .imageRects()
            .find(
                (im) =>
                    im.at === sel.at &&
                    im.path.steps.length === sel.path.steps.length &&
                    im.path.steps.every((s, i) => {
                        const o = sel.path.steps[i]!;
                        if (s.kind !== o.kind) return false;
                        if (s.kind === 'BLOCK' && o.kind === 'BLOCK') return s.idx === o.idx;
                        if (s.kind === 'CELL' && o.kind === 'CELL')
                            return s.row === o.row && s.col === o.col;
                        return false;
                    }) &&
                    im.rect.y + im.rect.h > top &&
                    im.rect.y < bottom,
            );
    };

    /** Local-page CSS box for the outline + handles (optimistic during drag). */
    const box = (): { left: number; top: number; w: number; h: number } | undefined => {
        const im = selected();
        if (!im) return undefined;
        const live = liveSize();
        const w = live ? live.w : im.rect.w;
        const h = live ? live.h : im.rect.h;
        return {
            left: im.rect.x,
            top: im.rect.y - props.store.pageTopCss(props.pageIdx),
            w,
            h,
        };
    };

    const startDrag = (handle: HandleDef, e: PointerEvent): void => {
        e.preventDefault();
        e.stopPropagation();
        const im = selected();
        if (!im) return;
        /* Capture the resize address up front — a mid-drag refresh must
           not repoint the commit at a different image. */
        const addr = { path: im.path, at: im.at };
        const startX = e.clientX;
        const startY = e.clientY;
        const startW = im.rect.w;
        const startH = im.rect.h;
        const emuW = im.widthEmu;
        const emuH = im.heightEmu;
        const aspect = startH / Math.max(1, startW);
        /* Last computed target extent (EMU). The engine is dispatched
           ONCE, on pointerup — every animation frame would otherwise push
           its own UndoStack snapshot (~60/s), flooding the bounded (100)
           undo history and destroying pre-drag edits. The overlay's
           `liveSize` gives smooth feedback during the drag; the canvas
           image commits on release. */
        let targetEmuW = emuW;
        let targetEmuH = emuH;
        const target = e.currentTarget as HTMLElement;
        /* Pointer capture keeps move/up events flowing to the handle even
           when the cursor leaves it mid-drag. Guarded — a synthetic event
           (or a browser that rejects the id) must not abort the drag
           wiring below. */
        try {
            target.setPointerCapture(e.pointerId);
        } catch {
            /* non-fatal — the window listeners below still fire */
        }

        const onMove = (ev: PointerEvent): void => {
            const dx = ev.clientX - startX;
            const dy = ev.clientY - startY;
            let newW = Math.max(MIN_PX, startW + handle.dw * dx);
            let newH = Math.max(MIN_PX, startH + handle.dh * dy);
            if (handle.corner) {
                /* Aspect-locked: width drives height. */
                newH = Math.max(MIN_PX, newW * aspect);
            } else if (handle.dw === 0) {
                newW = startW; // pure vertical handle
            } else if (handle.dh === 0) {
                newH = startH; // pure horizontal handle
            }
            setLiveSize({ w: newW, h: newH });
            targetEmuW = Math.max(1, Math.round((emuW * newW) / Math.max(1, startW)));
            targetEmuH = Math.max(1, Math.round((emuH * newH) / Math.max(1, startH)));
        };
        const onUp = (ev: PointerEvent): void => {
            try {
                target.releasePointerCapture?.(ev.pointerId);
            } catch {
                /* capture may never have been granted — ignore */
            }
            window.removeEventListener('pointermove', onMove);
            window.removeEventListener('pointerup', onUp);
            setLiveSize(null);
            /* Commit the drag as ONE resize (one undo entry). Skip the
               no-op case where the size never changed. */
            if (targetEmuW !== emuW || targetEmuH !== emuH) {
                void props.client
                    .dispatch({
                        type: 'RESIZE_IMAGE',
                        path: addr.path,
                        at: addr.at,
                        width_emu: targetEmuW,
                        height_emu: targetEmuH,
                    })
                    .catch((err: unknown) => console.error('resizeImage failed', err));
            }
        };
        /* Listen on window so the drag survives the cursor leaving the
           handle, independent of whether pointer capture was granted. */
        window.addEventListener('pointermove', onMove);
        window.addEventListener('pointerup', onUp);
    };

    return (
        <Show when={box()}>
            {(b) => (
                <div
                    class="image-handles"
                    style={{
                        left: `${b().left}px`,
                        top: `${b().top}px`,
                        width: `${b().w}px`,
                        height: `${b().h}px`,
                    }}
                >
                    <For each={HANDLES}>
                        {(handle) => (
                            <div
                                class={`image-handle image-handle--${handle.id}`}
                                style={{
                                    left: `${handle.fx * 100}%`,
                                    top: `${handle.fy * 100}%`,
                                    cursor: handle.cursor,
                                }}
                                onPointerDown={(e) => startDrag(handle, e)}
                            />
                        )}
                    </For>
                </div>
            )}
        </Show>
    );
}
