/* Phase 4 §7 — pointer input.
 *
 * The engine owns hit-testing; the UI only forwards device-pixel coordinates.
 * pointerdown places the caret, drag extends the selection, double-click
 * selects a word. Each gesture round-trips through HIT_TEST then a selection
 * command; the engine answers with SELECTION_CHANGED, which engine-store
 * fans out to the overlays. */
import type { EngineClient } from '../engine/engine-client';
import type { Point } from '../engine/types';

/**
 * Wire pointer listeners on `canvas` for one page in the multi-canvas DOM
 * architecture (Phase 6c). Returns a teardown that removes them. Pointer
 * events still fire on a `<canvas>` whose drawing surface has been
 * transferred to the worker, so this is safe to attach post-transfer.
 *
 * `pageIdx` identifies which page-card canvas this is. Clicks dispatch
 * `HIT_TEST_IN_PAGE` with that index + the canvas-local device-pixel
 * point — the engine adds the page's accumulated top offset, no
 * TS-side offset math.
 */
export function attachPointer(
    canvas: HTMLCanvasElement,
    client: EngineClient,
    pageIdx: number = 0,
): () => void {
    let dragging = false;
    /* Bumped on every new gesture. HIT_TEST is async, so the SET_SELECTION
       it feeds can land out of order — a double-click's two single-click
       SET_SELECTIONs would otherwise resolve after the SELECT_WORD_AT and
       clobber the word selection. A hit-test whose gesture is stale by the
       time it resolves is dropped. */
    let gesture = 0;

    /* Client coords → canvas device pixels. The engine paints the page 1:1
       into the device-pixel backing store, so device px are page points. */
    const toCanvas = (e: PointerEvent | MouseEvent): Point => {
        const r = canvas.getBoundingClientRect();
        const dpr = window.devicePixelRatio || 1;
        return { x: (e.clientX - r.left) * dpr, y: (e.clientY - r.top) * dpr };
    };

    const placeCaret = async (at: Point, g: number): Promise<void> => {
        const hit = await client.dispatch({ type: 'HIT_TEST_IN_PAGE', page: pageIdx, at });
        if (g !== gesture || hit.type !== 'HIT_RESULT') return;
        await client.dispatch({
            type: 'SET_SELECTION',
            range: { start: hit.pos, end: hit.pos },
            caret: hit.pos,
        });
    };

    const extendTo = async (at: Point, g: number): Promise<void> => {
        const hit = await client.dispatch({ type: 'HIT_TEST_IN_PAGE', page: pageIdx, at });
        if (g !== gesture || hit.type !== 'HIT_RESULT') return;
        await client.dispatch({ type: 'EXTEND_SELECTION', to: hit.pos, modifier: 'None' });
    };

    const onPointerDown = (e: PointerEvent): void => {
        canvas.setPointerCapture(e.pointerId);
        dragging = true;
        gesture += 1;
        void placeCaret(toCanvas(e), gesture);
    };

    const onPointerMove = (e: PointerEvent): void => {
        if (!dragging) return;
        void extendTo(toCanvas(e), gesture);
    };

    const onPointerUp = (e: PointerEvent): void => {
        dragging = false;
        if (canvas.hasPointerCapture(e.pointerId)) {
            canvas.releasePointerCapture(e.pointerId);
        }
    };

    const onDblClick = (e: MouseEvent): void => {
        /* Bump past the two single-clicks' in-flight hit-tests so their
           SET_SELECTIONs are dropped and the word selection survives. */
        gesture += 1;
        void client.dispatch({ type: 'SELECT_WORD_AT', at: toCanvas(e) });
    };

    /* Triple-click — select the whole paragraph (Backlog #14). The DOM has no
       native event for this; the third click in a chain carries detail === 3.
       Bump `gesture` so the third pointerdown's hit-test is dropped, same as
       dblclick does for the second. */
    const onClick = (e: MouseEvent): void => {
        if (e.detail === 3) {
            gesture += 1;
            void client.dispatch({ type: 'SELECT_PARAGRAPH_AT', at: toCanvas(e) });
        }
    };

    canvas.addEventListener('pointerdown', onPointerDown);
    canvas.addEventListener('pointermove', onPointerMove);
    canvas.addEventListener('pointerup', onPointerUp);
    canvas.addEventListener('dblclick', onDblClick);
    canvas.addEventListener('click', onClick);

    return () => {
        canvas.removeEventListener('pointerdown', onPointerDown);
        canvas.removeEventListener('pointermove', onPointerMove);
        canvas.removeEventListener('pointerup', onPointerUp);
        canvas.removeEventListener('dblclick', onDblClick);
        canvas.removeEventListener('click', onClick);
    };
}
