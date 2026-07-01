/* Phase 4 §7 — pointer input.
 *
 * The engine owns hit-testing; the UI only forwards device-pixel coordinates.
 * pointerdown places the caret, drag extends the selection, double-click
 * selects a word. Each gesture round-trips through HIT_TEST_IN_PAGE then a
 * selection command; the engine answers with SELECTION_CHANGED, which
 * engine-store fans out to the overlays. */
import type { EngineClient } from '../engine/engine-client';
import type { Point } from '../engine/types';
import {
    PAGE_GAP_PT,
    PAGE_H_PT,
    SCREEN_DPI_SCALE,
    enginePageTopDevice,
} from '../state/engine-store';

/**
 * Wire pointer listeners on `canvas` for one page in the multi-canvas DOM
 * architecture (Phase 6c). Returns a teardown that removes them. Pointer
 * events still fire on a `<canvas>` whose drawing surface has been
 * transferred to the worker, so this is safe to attach post-transfer.
 *
 * `pageIdx` is the canvas's page index. Hit-tests ship the click in this
 * page's LOCAL device-pixel coords via `HIT_TEST_IN_PAGE`; the engine adds
 * the page's REAL accumulated top offset (per-section landscape / custom
 * page sizes included), so the TS side does no page-height math at all.
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

    /* Client coords → this page's LOCAL engine device pixels (origin at
       the page's top-left). `HIT_TEST_IN_PAGE` adds the page's real
       accumulated top offset engine-side. `e.clientY - r.top`
       legitimately exceeds the canvas height (or goes negative) when a
       `setPointerCapture` drag wanders onto a neighbouring canvas — the
       overshoot maps onto the adjacent page because the CSS inter-page
       gap mirrors the engine's `PAGE_GAP_PT`. */
    const toLocal = (e: PointerEvent | MouseEvent): Point => {
        const r = canvas.getBoundingClientRect();
        const dpr = window.devicePixelRatio || 1;
        return {
            x: (e.clientX - r.left) * dpr,
            y: (e.clientY - r.top) * dpr,
        };
    };

    /* Client coords → DOCUMENT-ABSOLUTE engine device pixels, only for
       the gesture commands that have no page-local bridge variant
       (`SELECT_WORD_AT` / `SELECT_PARAGRAPH_AT` / `SELECT_CELL_AT`).
       The page-top offset is the shared uniform-A4 approximation from
       engine-store — exact for portrait-A4 documents; per-section page
       heights are tracked separately. */
    const toGlobal = (e: PointerEvent | MouseEvent): Point => {
        const local = toLocal(e);
        const dpr = window.devicePixelRatio || 1;
        /* Engine-reported page top (device px, exact under mixed
           orientations — issue #26); uniform-A4 fallback pre-paint. */
        const pageOffsetY =
            enginePageTopDevice(pageIdx) ??
            pageIdx * (PAGE_H_PT + PAGE_GAP_PT) * dpr * SCREEN_DPI_SCALE;
        return { x: local.x, y: local.y + pageOffsetY };
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
        /* Shift+Click extends the existing selection (anchor stays
           put, caret jumps to the hit position) instead of resetting
           it. UX_BEHAVIOR_SPEC §IV.7. */
        if (e.shiftKey) {
            void extendTo(toLocal(e), gesture);
            return;
        }
        void placeCaret(toLocal(e), gesture);
    };

    const onPointerMove = (e: PointerEvent): void => {
        if (!dragging) return;
        void extendTo(toLocal(e), gesture);
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
