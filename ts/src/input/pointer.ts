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
    selectImageByPoint,
    selectionViewForPointer,
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
    /* Bumped on every new gesture. Still needed for the DRAG path
       (extendTo), which stays two-hop: its HIT_TEST is async, so the
       EXTEND_SELECTION it feeds can land out of order; a hit-test whose
       gesture is stale by the time it resolves is dropped. Plain-click
       placement no longer needs this — PLACE_CARET_AT_POINT is a single
       synchronously-posted command ordered by the worker queue itself. */
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

    /* Issue #53 — single-hop caret placement, posted SYNCHRONOUSLY (no
       await between the pointer event and the dispatch). The engine
       hit-tests AND sets the collapsed selection inside ONE serialized
       command, so a keystroke fired <5 ms after the click still enters
       the worker queue AFTER it and inserts at the clicked position.
       The old two-hop HIT_TEST_IN_PAGE → SET_SELECTION needed the
       gesture-staleness guard here; single-hop placements are ordered
       by the queue itself (two single clicks always resolve before the
       dblclick's SELECT_WORD_AT that follows them). */
    const placeCaret = (at: Point): void => {
        void client
            .dispatch({ type: 'PLACE_CARET_AT_POINT', page: pageIdx, at })
            .catch((e: unknown) => {
                console.error('placeCaret failed', e);
            });
    };

    const extendTo = async (at: Point, g: number): Promise<void> => {
        const hit = await client.dispatch({ type: 'HIT_TEST_IN_PAGE', page: pageIdx, at });
        if (g !== gesture || hit.type !== 'HIT_RESULT') return;
        await client.dispatch({ type: 'EXTEND_SELECTION', to: hit.pos, modifier: 'None' });
    };

    const onPointerDown = (e: PointerEvent): void => {
        /* Non-primary buttons never start a drag (issue #36) — treating a
           right-click like a left-click collapsed the active TABLE_CELLS
           selection before TableContextMenu could read it, leaving "Merge
           cells" permanently disabled. Word semantics for the right
           button: a press INSIDE the active selection (or anywhere while
           a TABLE_CELLS selection is live — its rects cover only the text
           runs, so point-in-rect underreports cell coverage) preserves
           the selection for the context menu; a press OUTSIDE a linear
           selection moves the caret so menu actions never target a stale
           far-away position. Middle clicks are ignored entirely. Touch
           and pen contacts report button 0, so both still place the
           caret. */
        if (e.button !== 0) {
            if (e.button === 2) {
                const sel = selectionViewForPointer();
                if (sel.kind.kind !== 'TABLE_CELLS') {
                    const dpr = window.devicePixelRatio || 1;
                    const g = toGlobal(e);
                    const cx = g.x / dpr;
                    const cy = g.y / dpr;
                    const inside = sel.rects.some(
                        (r) =>
                            cx >= r.x &&
                            cx <= r.x + r.w &&
                            cy >= r.y &&
                            cy <= r.y + r.h,
                    );
                    if (!inside) {
                        gesture += 1;
                        placeCaret(toLocal(e));
                    }
                }
            }
            return;
        }
        /* Issue #44 — a primary press on an inline image body selects
           the image (showing resize handles) instead of placing a caret
           or starting a text drag. Presses on the handles themselves
           never reach the canvas — the overlay divs sit above it and
           consume the event. `selectImageByPoint` also CLEARS a prior
           image selection when the click misses every image, so a normal
           text click deselects. Coordinates are document-absolute CSS
           px, matching the stored image rects. */
        {
            const dpr = window.devicePixelRatio || 1;
            const gCss = toGlobal(e);
            if (selectImageByPoint(gCss.x / dpr, gCss.y / dpr)) {
                gesture += 1;
                return;
            }
        }
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
        placeCaret(toLocal(e));
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
