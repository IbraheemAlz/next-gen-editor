/* Phase 4 §7 — pointer input.
 *
 * The engine owns hit-testing; the UI only forwards device-pixel coordinates.
 * pointerdown places the caret, drag extends the selection, double-click
 * selects a word. Each gesture round-trips through HIT_TEST then a selection
 * command; the engine answers with SELECTION_CHANGED, which engine-store
 * fans out to the overlays. */
import type { EngineClient } from '../engine/engine-client';
import type { Point } from '../engine/types';

/* Phase 6c — multi-canvas drag math constants. The pointer handler
   converts canvas-local pointer coords to document-absolute engine
   device-px so a drag that leaves the originally-clicked canvas (the
   `setPointerCapture` target) still resolves to the right glyph on
   whichever page the cursor visits. The conversion adds this canvas's
   page-top offset in engine device px, computed from the engine's
   constants below. */
const PAGE_H_PT = 841.9; // A4 height in layout pt (engine `PageGeometry::a4()`).
const PAGE_GAP_PT = 48; // Engine `render::scene::PAGE_GAP_PT`.
const SCREEN_DPI_SCALE = 4 / 3; // 96 / 72 — `App.tsx::SCREEN_DPI_SCALE`.

/**
 * Wire pointer listeners on `canvas` for one page in the multi-canvas DOM
 * architecture (Phase 6c). Returns a teardown that removes them. Pointer
 * events still fire on a `<canvas>` whose drawing surface has been
 * transferred to the worker, so this is safe to attach post-transfer.
 *
 * `pageIdx` is the canvas's page index — used to convert
 * canvas-local pointer coords to document-absolute engine device-px,
 * so `setPointerCapture` drags that cross page boundaries still
 * resolve to the right glyph on whichever page the cursor visits.
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

    /* Client coords → DOCUMENT-ABSOLUTE engine device pixels. Adding
       this canvas's page-top offset means a `setPointerCapture` drag
       that wanders onto a neighbouring canvas (or even past the last
       page) still resolves the correct glyph — `e.clientY - r.top`
       legitimately exceeds the canvas height when the cursor is below
       this canvas, and that's exactly the engine-space Y of the line
       below. Multiplied by `dpr` to lift into engine device px;
       `pageIdx × (page_h + gap) × scale` shifts to absolute. */
    const toGlobal = (e: PointerEvent | MouseEvent): Point => {
        const r = canvas.getBoundingClientRect();
        const dpr = window.devicePixelRatio || 1;
        const scale = dpr * SCREEN_DPI_SCALE;
        const pageOffsetY = pageIdx * (PAGE_H_PT + PAGE_GAP_PT) * scale;
        return {
            x: (e.clientX - r.left) * dpr,
            y: (e.clientY - r.top) * dpr + pageOffsetY,
        };
    };

    const placeCaret = async (at: Point, g: number): Promise<void> => {
        const hit = await client.dispatch({ type: 'HIT_TEST', at });
        if (g !== gesture || hit.type !== 'HIT_RESULT') return;
        await client.dispatch({
            type: 'SET_SELECTION',
            range: { start: hit.pos, end: hit.pos },
            caret: hit.pos,
        });
    };

    const extendTo = async (at: Point, g: number): Promise<void> => {
        const hit = await client.dispatch({ type: 'HIT_TEST', at });
        if (g !== gesture || hit.type !== 'HIT_RESULT') return;
        await client.dispatch({ type: 'EXTEND_SELECTION', to: hit.pos, modifier: 'None' });
    };

    const onPointerDown = (e: PointerEvent): void => {
        canvas.setPointerCapture(e.pointerId);
        dragging = true;
        gesture += 1;
        /* Shift+Click extends the existing selection (anchor stays
           put, caret jumps to the hit position) instead of resetting
           it. UX_BEHAVIOR_SPEC §IV.7. */
        if (e.shiftKey) {
            void extendTo(toGlobal(e), gesture);
            return;
        }
        void placeCaret(toGlobal(e), gesture);
    };

    const onPointerMove = (e: PointerEvent): void => {
        if (!dragging) return;
        void extendTo(toGlobal(e), gesture);
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
        void client.dispatch({ type: 'SELECT_WORD_AT', at: toGlobal(e) });
    };

    /* Triple-click — whole paragraph. Quadruple-click — whole cell
       (when inside a table) or whole document (UX_BEHAVIOR_SPEC §I.3).
       DOM has no native triple/quadruple events; `e.detail` carries
       the click count. Bump `gesture` on each so the pending
       hit-tests from the inner clicks get dropped (mirrors dblclick's
       defence against the two single-clicks). `SELECT_CELL_AT`
       resolves the click's owning cell engine-side and falls back to
       `SELECT_ALL` when the hit lands outside any cell. */
    const onClick = (e: MouseEvent): void => {
        if (e.detail === 3) {
            gesture += 1;
            void client.dispatch({ type: 'SELECT_PARAGRAPH_AT', at: toGlobal(e) });
        } else if (e.detail >= 4) {
            gesture += 1;
            void client.dispatch({ type: 'SELECT_CELL_AT', at: toGlobal(e) });
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
