/**
 * createZoomAnchor — issue #312: a zoom change must not make the view
 * jump. Word and Docs keep the caret where it is when it is in view,
 * otherwise the centre of the viewport; before this the scroll offset was
 * left alone while every page card resized, so the caret flew off screen on
 * most zoom changes.
 *
 * ONE place: `createEditorCommands().setZoom` (the facade every zoom
 * control, the Fit-width mode and a host's own widget go through) calls
 * `beginZoomAnchor(engine, scale)` BEFORE it dispatches `SET_ZOOM`. A host
 * that scrolls mounts this primitive once (the reference shell's
 * `ZoomAnchor`); it then:
 *
 *  1. at `begin` — the DOM is still the old zoom's, `state.caret()` is the
 *     old caret — picks the anchor (`zoomAnchor.ts`: the caret's top when it
 *     is in view, else the viewport centre) and remembers the page card
 *     that holds it, its distance below the card at 100 %, and its offset
 *     in the viewport;
 *  2. when the zoom's repaint lands (a `PAINTED` that reports page
 *     geometry, once the zoom differs from the one the anchor was taken at)
 *     scrolls so the anchor is back at the same viewport offset. The write
 *     is repeated one frame later (a timer where rAF is paused: a hidden
 *     tab): the page cards resize on the paint, and a late-settled layout
 *     must not leave a half-applied scroll.
 *
 * No caret moves on a zoom (`SET_ZOOM` is a `VIEW` command, never
 * `reveals_caret`), so the #387 / #557 caret reveal does not fire and
 * cannot fight this scroll.
 *
 * A zoom the facade never saw (a raw `__dispatch`, the initial zoom of
 * `openDocument`) is not anchored: there is no view worth keeping on a new
 * document, and a test hook is not a user gesture.
 */
import { createEffect, on, onCleanup, untrack } from 'solid-js';
import { useEngine, type EngineHandle } from './EngineProvider';
import { createEditorState } from './createEditorState';
import type { ViewBox } from './caretReveal';
import type { Rect } from './types';
import {
    anchoredScrollTop,
    captureAnchor,
    chooseAnchorPoint,
    type PageSpan,
    type ZoomAnchor,
} from './zoomAnchor';

export interface ZoomAnchorOptions {
    /** The scrolling element; `undefined` (not mounted yet) skips. */
    viewport: () => HTMLElement | undefined;
    /** The engine caret rect (document device px) as a box in the
     *  viewport's scroll-content CSS px — the same mapping the host gives
     *  `createCaretReveal`. */
    caretBox: (caret: Rect, viewport: HTMLElement) => ViewBox | undefined;
    /** The page cards currently in the DOM, in the viewport's scroll-content
     *  CSS px (measured, not computed: the cards are what the user sees). */
    pages: (viewport: HTMLElement) => PageSpan[];
    /** Gate (default: always on). */
    enabled?: () => boolean;
}

/** One anchor, alive from the first zoom request until the zoom it asked
 *  for has painted. A chain of requests (the Fit-width mode asks twice, a
 *  held Ctrl+= asks every key repeat) shares ONE anchor: what the user
 *  was looking at before the first request is still what they want kept. */
interface Pending {
    readonly anchor: ZoomAnchor;
    /** The zoom whose page cards the DOM shows and the anchor was last
     *  placed against (the zoom the anchor was taken at, until a paint). */
    settledZoom: number;
    /** The latest zoom a request asked for. */
    target: number;
    /** The last request or paint (for the lifetime bound). */
    at: number;
}

/** A zoom request that never produces its zoom (clamped to the current
 *  one, refused, a trapped worker) leaves no anchor behind. */
const PENDING_TTL_MS = 3000;
/** Zooms closer than this are the same zoom (`createEditorState` rounds
 *  the zoom to four decimals). */
const SAME_ZOOM = 1e-4;
const same = (a: number, b: number): boolean => Math.abs(a - b) < SAME_ZOOM;

const hosts = new WeakMap<EngineHandle, (target: number) => void>();

/** Called by the facade's `setZoom` before it dispatches. A no-op for an
 *  engine no host mounted {@link createZoomAnchor} for. */
export function beginZoomAnchor(engine: EngineHandle, target: number): void {
    hosts.get(engine)?.(target);
}

const now = (): number => performance.now();

export function createZoomAnchor(opts: ZoomAnchorOptions): void {
    const state = createEditorState();
    const engine = useEngine();
    let pending: Pending | undefined;
    let frame: number | undefined;
    let fallback: ReturnType<typeof setTimeout> | undefined;

    const enabled = (): boolean => !opts.enabled || untrack(opts.enabled);

    const cancelTimers = (): void => {
        if (frame !== undefined) cancelAnimationFrame(frame);
        if (fallback !== undefined) clearTimeout(fallback);
        frame = undefined;
        fallback = undefined;
    };

    const live = (p: Pending | undefined): p is Pending =>
        p !== undefined && now() - p.at < PENDING_TTL_MS;

    const begin = (target: number): void => {
        if (!enabled()) return;
        const viewport = opts.viewport();
        if (!viewport || !(target > 0)) return;
        /* A zoom already in flight keeps ITS anchor: the user has not seen
           the intermediate state, so what they were looking at is still the
           first thing (offsets at 100 % do not depend on the zoom). */
        if (live(pending)) {
            pending.target = target;
            pending.at = now();
            return;
        }
        const zoom = untrack(state.zoom);
        if (same(target, zoom)) {
            pending = undefined;
            return;
        }
        const view = { scrollTop: viewport.scrollTop, clientHeight: viewport.clientHeight };
        const caret = untrack(state.caret);
        const box = caret ? opts.caretBox(caret, viewport) : undefined;
        const point = chooseAnchorPoint(view, box);
        const anchor = captureAnchor(point.y, point.kind, view, opts.pages(viewport), zoom);
        pending = anchor ? { anchor, settledZoom: zoom, target, at: now() } : undefined;
    };

    /** Scroll so the anchor is back at its viewport offset against the
     *  page cards of the zoom they show (`p.settledZoom`). */
    const place = (p: Pending): void => {
        const viewport = opts.viewport();
        if (!viewport) return;
        const top = anchoredScrollTop(p.anchor, opts.pages(viewport), p.settledZoom, {
            scrollHeight: viewport.scrollHeight,
            clientHeight: viewport.clientHeight,
        });
        if (top !== undefined && Math.abs(top - viewport.scrollTop) >= 0.5) {
            viewport.scrollTop = top;
        }
    };

    /** The second pass, a frame after a paint: the page cards resize on the
     *  paint, and a late-settled layout must not leave a half-applied
     *  scroll. The anchor is spent once the zoom it asked for has painted. */
    const verify = (): void => {
        cancelTimers();
        const p = pending;
        if (!p) return;
        if (!enabled()) {
            pending = undefined;
            return;
        }
        place(p);
        if (same(p.settledZoom, p.target)) pending = undefined;
    };

    /* A repaint of the SAME document keeps `PAINTED.version` (it follows
       document mutations), so the zoom's own paint is told by the page
       geometry it reports - a fresh object on every paginated paint. */
    createEffect(
        on(
            [state.paintVersion, state.pageGeometry],
            () => {
                const p = pending;
                if (!p) return;
                if (!live(p)) {
                    pending = undefined;
                    return;
                }
                /* The repaint of an earlier command, or of the zoom the DOM
                   already shows: not a new zoom's paint. */
                const zoom = untrack(state.zoom);
                if (same(zoom, p.settledZoom)) return;
                p.settledZoom = zoom;
                p.at = now();
                /* This event reaches the shell's own `PAINTED` handler too, in
                   either order: a microtask runs after every subscriber has
                   been called, so the page cards already have the new size. */
                queueMicrotask(() => {
                    if (pending === p) place(p);
                });
                if (frame === undefined) frame = requestAnimationFrame(verify);
                if (fallback === undefined) fallback = setTimeout(verify, 120);
            },
            { defer: true },
        ),
    );

    hosts.set(engine, begin);
    onCleanup(() => {
        if (hosts.get(engine) === begin) hosts.delete(engine);
        cancelTimers();
        pending = undefined;
    });
}
