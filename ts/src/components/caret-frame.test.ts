import { describe, expect, it } from 'vitest';
import {
    CARET_FRAME_FALLBACK_MS,
    applyCaretAtOnce,
    createCaretFrameScheduler,
    isVerticalMove,
    type CaretFrameClock,
} from './caret-frame';

/** A hand-driven clock: frames and timers run only when the test says so. */
function fakeClock() {
    let next = 1;
    const frames = new Map<number, () => void>();
    const timers = new Map<number, { cb: () => void; ms: number }>();
    const clock: CaretFrameClock = {
        requestFrame: (cb) => {
            frames.set(next, cb);
            return next++;
        },
        cancelFrame: (h) => void frames.delete(h),
        setTimer: (cb, ms) => {
            timers.set(next, { cb, ms });
            return next++;
        },
        clearTimer: (h) => void timers.delete(h),
    };
    return {
        clock,
        pendingFrames: () => frames.size,
        pendingTimers: () => timers.size,
        timerDelays: () => [...timers.values()].map((t) => t.ms),
        /** The browser's next rendering opportunity: run the frame callbacks. */
        runFrame: () => {
            const cbs = [...frames.values()];
            frames.clear();
            cbs.forEach((cb) => cb());
        },
        /** The fallback timers firing (a hidden tab: no frames). */
        runTimers: () => {
            const cbs = [...timers.values()].map((t) => t.cb);
            timers.clear();
            cbs.forEach((cb) => cb());
        },
    };
}

function setup() {
    const applied: number[] = [];
    const t = fakeClock();
    const sched = createCaretFrameScheduler<number>((v) => applied.push(v), t.clock);
    return { applied, t, sched };
}

describe('isVerticalMove (issue #559: which replies apply at once)', () => {
    it('is true for Up / Down / PageUp / PageDown only', () => {
        for (const d of ['Up', 'Down', 'PageUp', 'PageDown'] as const) {
            expect(isVerticalMove(d), d).toBe(true);
        }
        for (const d of [
            'Left',
            'Right',
            'NextCell',
            'PrevCell',
            'WordLeft',
            'WordRight',
            'LineHome',
            'LineEnd',
            'DocHome',
            'DocEnd',
        ] as const) {
            expect(isVerticalMove(d), d).toBe(false);
        }
    });
});

describe('applyCaretAtOnce (issue #559: which updates skip the frame)', () => {
    const none = { vertical: false, overlayHidden: false, storyChanged: false };

    it('coalesces an ordinary caret move', () => {
        expect(applyCaretAtOnce(none)).toBe(false);
    });

    it('applies a vertical move, a hidden overlay and a story change at once', () => {
        expect(applyCaretAtOnce({ ...none, vertical: true })).toBe(true);
        expect(applyCaretAtOnce({ ...none, overlayHidden: true })).toBe(true);
        expect(applyCaretAtOnce({ ...none, storyChanged: true })).toBe(true);
    });
});

describe('createCaretFrameScheduler (issue #559: one caret write per frame)', () => {
    it('applies nothing until the frame, then only the newest value, once', () => {
        const { applied, t, sched } = setup();
        for (const v of [1, 2, 3, 4, 5, 6]) sched.push(v);
        expect(applied).toEqual([]);
        expect(t.pendingFrames()).toBe(1);
        t.runFrame();
        expect(applied).toEqual([6]);
        /* The frame ran: nothing is left to fire twice. */
        expect(t.pendingFrames()).toBe(0);
        expect(t.pendingTimers()).toBe(0);
        t.runFrame();
        t.runTimers();
        expect(applied).toEqual([6]);
    });

    it('applies one value per frame across frames', () => {
        const { applied, t, sched } = setup();
        sched.push(1);
        sched.push(2);
        t.runFrame();
        sched.push(3);
        sched.push(4);
        sched.push(5);
        t.runFrame();
        expect(applied).toEqual([2, 5]);
    });

    it('applies a vertical push at once and drops what waited for the frame', () => {
        const { applied, t, sched } = setup();
        sched.push(1);
        sched.push(2);
        sched.push(3, true);
        expect(applied).toEqual([3]);
        /* The older waiting values are superseded; the frame has nothing. */
        expect(t.pendingFrames()).toBe(0);
        expect(t.pendingTimers()).toBe(0);
        t.runFrame();
        expect(applied).toEqual([3]);
    });

    it('a horizontal push after an immediate one waits for the next frame', () => {
        const { applied, t, sched } = setup();
        sched.push(1, true);
        sched.push(2);
        expect(applied).toEqual([1]);
        t.runFrame();
        expect(applied).toEqual([1, 2]);
    });

    it('a hidden tab (no frames) is served by the timer, once', () => {
        const { applied, t, sched } = setup();
        sched.push(1);
        sched.push(2);
        expect(t.timerDelays()).toEqual([CARET_FRAME_FALLBACK_MS]);
        t.runTimers();
        expect(applied).toEqual([2]);
        /* The frame the timer overtook is cancelled, not run late. */
        expect(t.pendingFrames()).toBe(0);
        t.runFrame();
        expect(applied).toEqual([2]);
    });

    it('the frame cancels its fallback timer', () => {
        const { t, sched } = setup();
        sched.push(1);
        expect(t.pendingTimers()).toBe(1);
        t.runFrame();
        expect(t.pendingTimers()).toBe(0);
    });

    it('dispose drops the waiting value and every wake-up', () => {
        const { applied, t, sched } = setup();
        sched.push(1);
        sched.dispose();
        t.runFrame();
        t.runTimers();
        expect(applied).toEqual([]);
        expect(t.pendingFrames()).toBe(0);
        expect(t.pendingTimers()).toBe(0);
    });

    it('an apply that pushes again schedules a fresh frame', () => {
        const t = fakeClock();
        const applied: number[] = [];
        const sched = createCaretFrameScheduler<number>((v) => {
            applied.push(v);
            if (v === 1) sched.push(2);
        }, t.clock);
        sched.push(1);
        t.runFrame();
        expect(applied).toEqual([1]);
        expect(t.pendingFrames()).toBe(1);
        t.runFrame();
        expect(applied).toEqual([1, 2]);
    });
});
