/* Issue #559 (clean-room memo §11, last sentence: "caret updates are
 * coalesced to the next paint; vertical moves force an immediate update") —
 * the caret overlay's frame scheduler.
 *
 * Pure logic, no DOM at import time (unit-tested in `caret-frame.test.ts`);
 * the engine store owns ONE scheduler and the `CaretOverlay`s of every page
 * read the value it applies.
 *
 * During a key repeat or a typing burst several `SELECTION_CHANGED` replies
 * can land inside one frame. Each used to move the overlay (one style write
 * per reply); only the newest rect can ever be seen, so the scheduler keeps
 * the newest and applies it ONCE per animation frame — in the frame's rAF
 * callback, i.e. before that frame paints, so a coalesced caret is never a
 * frame late. Two exceptions apply at once:
 *
 *  - a vertical move (Up / Down / PageUp / PageDown), whose reply must never
 *    leave the caret lagging a jump of whole lines;
 *  - a hidden tab, where rAF is paused: a timer applies the newest value
 *    (the same fallback `createCaretReveal` uses).
 *
 * Whichever of {frame, timer, immediate push} comes first applies the newest
 * value and cancels the others, so every value is applied at most once and
 * the overlay's blink restart (one per applied update) cannot double-fire. */
import type { MoveDirection } from '../engine/types';

/** The hidden-tab fallback delay (ms): a frame is 16 ms, so by 100 ms a
 *  visible tab has long since run its rAF callback. */
export const CARET_FRAME_FALLBACK_MS = 100;

/** `MOVE_CARET` directions whose reply is applied immediately (memo §11:
 *  "vertical moves force an immediate update"). */
export function isVerticalMove(direction: MoveDirection): boolean {
    return (
        direction === 'Up' ||
        direction === 'Down' ||
        direction === 'PageUp' ||
        direction === 'PageDown'
    );
}

/** What the store knows when a `SELECTION_CHANGED` arrives. */
export interface CaretUpdateContext {
    /** The reply answers a vertical `MOVE_CARET` ({@link isVerticalMove}). */
    vertical: boolean;
    /** No caret is being drawn: a picture is object-selected, or a key that
     *  left that selection holds the overlay hidden until this reply lands
     *  (#506 - its release runs a microtask AFTER the fan-out, and must not
     *  reveal the previous rect for a frame). Nothing is visible to coalesce. */
    overlayHidden: boolean;
    /** The caret entered / left a header, footer or other story: the band
     *  overlay switches in this very task, and the caret must jump with it. */
    storyChanged: boolean;
}

/** `true` when the newest caret applies in the task that delivers it rather
 *  than at the next frame - the ONE policy the store consults. */
export function applyCaretAtOnce(ctx: CaretUpdateContext): boolean {
    return ctx.vertical || ctx.overlayHidden || ctx.storyChanged;
}

/** The scheduler's clock — injectable so the unit test drives frames and
 *  timers by hand. */
export interface CaretFrameClock {
    requestFrame(cb: () => void): number;
    cancelFrame(handle: number): void;
    setTimer(cb: () => void, ms: number): number;
    clearTimer(handle: number): void;
}

/** The browser clock (resolved lazily so importing this module under node
 *  never touches `window`). */
export function browserCaretFrameClock(): CaretFrameClock {
    return {
        requestFrame: (cb) => requestAnimationFrame(() => cb()),
        cancelFrame: (h) => cancelAnimationFrame(h),
        setTimer: (cb, ms) => window.setTimeout(cb, ms),
        clearTimer: (h) => window.clearTimeout(h),
    };
}

export interface CaretFrameScheduler<T> {
    /** Offer the newest value. `immediate` applies it now (a vertical move);
     *  otherwise it is applied once, in the next animation frame, replacing
     *  any value still waiting. */
    push(value: T, immediate?: boolean): void;
    /** Drop anything still waiting (the owner is going away). */
    dispose(): void;
}

export function createCaretFrameScheduler<T>(
    apply: (value: T) => void,
    clock: CaretFrameClock = browserCaretFrameClock(),
    fallbackMs: number = CARET_FRAME_FALLBACK_MS,
): CaretFrameScheduler<T> {
    let waiting: { value: T } | undefined;
    let frame: number | undefined;
    let timer: number | undefined;

    const cancel = (): void => {
        if (frame !== undefined) clock.cancelFrame(frame);
        if (timer !== undefined) clock.clearTimer(timer);
        frame = undefined;
        timer = undefined;
    };

    /** Apply the waiting value (if any) and stand every pending wake-up down. */
    const flush = (): void => {
        const next = waiting;
        waiting = undefined;
        cancel();
        if (next) apply(next.value);
    };

    return {
        push(value, immediate = false) {
            if (immediate) {
                /* The newest value supersedes whatever waited for the frame. */
                waiting = undefined;
                cancel();
                apply(value);
                return;
            }
            waiting = { value };
            if (frame === undefined) frame = clock.requestFrame(flush);
            if (timer === undefined) timer = clock.setTimer(flush, fallbackMs);
        },
        dispose() {
            waiting = undefined;
            cancel();
        },
    };
}
