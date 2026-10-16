/**
 * createCaretReveal — issue #387: keep the caret visible after keyboard
 * navigation and typing.
 *
 * Follows `createEditorState().caretRevealSeq` (the engine stamps
 * `SELECTION_CHANGED.reveal_caret` from the command's `bridge::meta`
 * classification: navigation, typing, edits, undo — never a pointer
 * gesture, Ctrl+A or a zoom) and scrolls the host's viewport minimally so
 * the caret stays inside it, with a margin. The mapping from the engine's
 * caret rect (absolute document device px) to the viewport's scroll
 * content is the host's — it owns the page layout and the zoom.
 *
 * Issue #486 — the reveal is a *consequence* of an earlier input and must
 * never undo a later scroll of the user's. So:
 *
 *  - when the host can place the caret at the moment the selection event
 *    arrives (`caretBox` answers), the scroll happens THERE, in the same
 *    task — there is no gap left for a user scroll to fall into. A second,
 *    cheap pass one frame later (with a timer fallback for a hidden tab)
 *    re-checks only when the layout settled late (a repaint that grew the
 *    document resized the host's DOM, or the first write was clamped);
 *  - when it cannot place the caret yet, that frame pass places it;
 *  - either way the reveal stands down if a user scroll intent arrived
 *    after the command was sent (`revealIntent.ts`): `wheel`, `touchmove`,
 *    a press on the scrollbar, a key pressed on the scroller itself, or a
 *    `scroll` event that is not the echo of the reveal's own write.
 */
import { createEffect, on, onCleanup, untrack } from 'solid-js';
import { useEngine } from './EngineProvider';
import { createEditorState } from './createEditorState';
import { CARET_REVEAL_MARGIN_PX, keepBoxVisible, type ViewBox } from './caretReveal';
import {
    ScrollIntentLedger,
    decideSync,
    decideVerify,
    isScrollbarPress,
    type RevealRequest,
} from './revealIntent';
import type { Rect } from './types';

export interface CaretRevealOptions {
    /** The scrolling element; `undefined` (not mounted yet) skips. */
    viewport: () => HTMLElement | undefined;
    /** The engine caret rect (document device px) as a box in the
     *  viewport's scroll-content CSS px — zoom applied; `undefined` when it
     *  cannot be placed yet. */
    caretBox: (caret: Rect, viewport: HTMLElement) => ViewBox | undefined;
    /** Breathing room in CSS px (default {@link CARET_REVEAL_MARGIN_PX}). */
    margin?: number;
    /** Gate (default: always on). */
    enabled?: () => boolean;
}

const now = (): number => performance.now();

export function createCaretReveal(opts: CaretRevealOptions): void {
    const state = createEditorState();
    const engine = useEngine();
    const ledger = new ScrollIntentLedger();
    let frame: number | undefined;
    let fallback: ReturnType<typeof setTimeout> | undefined;
    /** The reveal waiting for its verification pass (the newest event wins:
     *  a burst of keystrokes is one pass per frame). */
    let pending: RevealRequest | undefined;

    /* ---- user scroll intent: listeners on the current viewport ---- */
    let watched: HTMLElement | undefined;
    let unwatch: (() => void) | undefined;
    const watch = (viewport: HTMLElement | undefined): void => {
        if (viewport === watched) return;
        unwatch?.();
        unwatch = undefined;
        watched = viewport;
        if (!viewport) return;
        const intent = (): void => ledger.noteIntent(now());
        const onScroll = (): void => {
            ledger.noteScroll(viewport.scrollTop, viewport.scrollLeft, now());
        };
        const onPointerDown = (e: PointerEvent): void => {
            if (e.target !== viewport) return;
            const r = viewport.getBoundingClientRect();
            if (
                isScrollbarPress({
                    clientX: e.clientX,
                    clientY: e.clientY,
                    rect: r,
                    clientLeft: viewport.clientLeft,
                    clientTop: viewport.clientTop,
                    clientWidth: viewport.clientWidth,
                    clientHeight: viewport.clientHeight,
                })
            ) {
                intent();
            }
        };
        /* Only a key pressed ON the scroller (Space / PageDown / arrows on a
           focused scroll container) scrolls it; keys typed into the hidden
           textarea bubble through here as a different target. */
        const onKeyDown = (e: KeyboardEvent): void => {
            if (e.target === viewport) intent();
        };
        const passive = { passive: true } as const;
        viewport.addEventListener('wheel', intent, passive);
        viewport.addEventListener('touchmove', intent, passive);
        viewport.addEventListener('pointerdown', onPointerDown, passive);
        viewport.addEventListener('keydown', onKeyDown, passive);
        viewport.addEventListener('scroll', onScroll, passive);
        unwatch = () => {
            viewport.removeEventListener('wheel', intent);
            viewport.removeEventListener('touchmove', intent);
            viewport.removeEventListener('pointerdown', onPointerDown);
            viewport.removeEventListener('keydown', onKeyDown);
            viewport.removeEventListener('scroll', onScroll);
        };
    };

    const cancelTimers = (): void => {
        if (frame !== undefined) cancelAnimationFrame(frame);
        if (fallback !== undefined) clearTimeout(fallback);
        frame = undefined;
        fallback = undefined;
    };

    /** Scroll `box` into view, marking the write as the reveal's own so
     *  its `scroll` event is not mistaken for the user's. */
    const apply = (viewport: HTMLElement, box: ViewBox): void => {
        const beforeTop = viewport.scrollTop;
        const beforeLeft = viewport.scrollLeft;
        if (!keepBoxVisible(viewport, box, opts.margin ?? CARET_REVEAL_MARGIN_PX)) return;
        if (viewport.scrollTop !== beforeTop || viewport.scrollLeft !== beforeLeft) {
            ledger.noteOwnWrite(viewport.scrollTop, viewport.scrollLeft);
        }
    };

    const boxFor = (viewport: HTMLElement): ViewBox | undefined => {
        const caret = untrack(state.caret);
        return caret ? opts.caretBox(caret, viewport) : undefined;
    };

    const enabled = (): boolean => !opts.enabled || untrack(opts.enabled);

    /* The frame / timer pass. */
    const verify = (): void => {
        const req = pending;
        cancelTimers();
        pending = undefined;
        if (!req || !enabled()) return;
        const viewport = opts.viewport();
        watch(viewport);
        if (!viewport) return;
        const box = boxFor(viewport);
        if (decideVerify(ledger, req, box !== undefined) === 'reveal' && box) apply(viewport, box);
    };

    createEffect(
        on(
            state.caretRevealSeq,
            () => {
                if (!enabled()) return;
                const eventAt = now();
                /* The newest selection-moving command the client sent: any
                   scroll the user made after it is their last word. A client
                   that does not say falls back to "the reply arrived". */
                const sentAt = Math.min(engine.lastSelectionMoveSentAt ?? eventAt, eventAt);
                const req: RevealRequest = { sentAt, eventAt };
                const viewport = opts.viewport();
                watch(viewport);
                if (!viewport) return;
                const box = boxFor(viewport);
                const verdict = decideSync(ledger, req, box !== undefined);
                if (verdict === 'superseded') {
                    pending = undefined;
                    cancelTimers();
                    return;
                }
                if (verdict === 'now' && box) apply(viewport, box);
                /* Verification: coalesces a burst into one pass per frame;
                   the timeout covers a hidden tab, where rAF is paused. */
                pending = req;
                if (frame === undefined) frame = requestAnimationFrame(verify);
                if (fallback === undefined) fallback = setTimeout(verify, 120);
            },
            { defer: true },
        ),
    );
    onCleanup(() => {
        cancelTimers();
        pending = undefined;
        unwatch?.();
        unwatch = undefined;
        watched = undefined;
    });
}
