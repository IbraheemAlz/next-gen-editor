/* Issue #502 - the worker's 2D-context bookkeeping: per-canvas lost /
 * restored counts, which canvas to repaint, stale-surface handling, and
 * the polling fallback. */
import { describe, expect, it } from 'vitest';
import {
    CanvasContextTracker,
    CanvasContextWatcher,
    forcedEvents,
    hasContextEvents,
    type CanvasContextHealth,
    type WatchedSurface,
} from './canvas-context';
import type { Timers } from './checkpoint-retry';

/** A surface with the context-loss events, like a current-Chrome
 *  `OffscreenCanvas`. */
class EventSurface extends EventTarget implements WatchedSurface {
    oncontextlost: unknown = null;
    oncontextrestored: unknown = null;
    fire(type: 'contextlost' | 'contextrestored'): void {
        this.dispatchEvent(new Event(type));
    }
}

/** A surface WITHOUT the events but with a 2D context to poll. */
class PollSurface extends EventTarget implements WatchedSurface {
    lost = false;
    getContext(): { isContextLost(): boolean } {
        return { isContextLost: () => this.lost };
    }
}

/** A surface with neither (a WebGPU boot surface: no 2D context). */
class BareSurface extends EventTarget implements WatchedSurface {
    getContext(): null {
        return null;
    }
}

/** A manual clock: `tick(ms)` runs what is due. */
function fakeTimers(): Timers & { tick(): void; active(): number } {
    let next = 1;
    const pending = new Map<number, () => void>();
    return {
        set(fn) {
            const id = next++;
            pending.set(id, fn);
            return id;
        },
        clear(h) {
            pending.delete(h as number);
        },
        tick() {
            const due = [...pending.entries()];
            pending.clear();
            for (const [, fn] of due) fn();
        },
        active: () => pending.size,
    };
}

function harness(opts: { timers?: Timers; detectEvents?: (s: WatchedSurface) => boolean } = {}) {
    const health: CanvasContextHealth[] = [];
    const restored: number[] = [];
    const watcher = new CanvasContextWatcher(
        {
            onHealth: (h) => health.push(h),
            onRestored: (idx) => restored.push(idx),
        },
        opts.timers ?? fakeTimers(),
        1000,
        opts.detectEvents ?? hasContextEvents,
    );
    return { watcher, health, restored };
}

describe('CanvasContextTracker', () => {
    it('counts losses and restorations per canvas and repaints only the restored one', () => {
        const t = new CanvasContextTracker();
        const a = t.register(0);
        const b = t.register(3);
        expect(t.health()).toEqual({ lostPages: [], losses: 0, restores: 0 });

        expect(t.noteLost(3, b)).toEqual({ changed: true, repaint: false });
        expect(t.noteLost(0, a)).toEqual({ changed: true, repaint: false });
        expect(t.health()).toEqual({ lostPages: [0, 3], losses: 2, restores: 0 });

        /* Page 3 comes back: only page 3 is asked to repaint. */
        expect(t.noteRestored(3, b)).toEqual({ changed: true, repaint: true });
        expect(t.health()).toEqual({ lostPages: [0], losses: 2, restores: 1 });
    });

    it('counts a repeated contextlost once', () => {
        const t = new CanvasContextTracker();
        const a = t.register(1);
        expect(t.noteLost(1, a).changed).toBe(true);
        expect(t.noteLost(1, a)).toEqual({ changed: false, repaint: false });
        expect(t.health().losses).toBe(1);
    });

    it('repaints a restored canvas whose loss was never observed', () => {
        const t = new CanvasContextTracker();
        const a = t.register(2);
        expect(t.noteRestored(2, a)).toEqual({ changed: true, repaint: true });
        expect(t.health()).toEqual({ lostPages: [], losses: 0, restores: 1 });
    });

    it('ignores an event from a surface that was since replaced', () => {
        const t = new CanvasContextTracker();
        const stale = t.register(1);
        const fresh = t.register(1);
        expect(t.noteLost(1, stale)).toEqual({ changed: false, repaint: false });
        expect(t.noteRestored(1, stale)).toEqual({ changed: false, repaint: false });
        expect(t.health()).toEqual({ lostPages: [], losses: 0, restores: 0 });
        expect(t.noteLost(1, fresh).changed).toBe(true);
    });

    it('re-registering a lost page clears its lost mark but keeps the totals', () => {
        const t = new CanvasContextTracker();
        const old = t.register(1);
        t.noteLost(1, old);
        t.register(1);
        expect(t.health()).toEqual({ lostPages: [], losses: 1, restores: 0 });
    });

    it('unregistering a lost canvas drops it from the lost set', () => {
        const t = new CanvasContextTracker();
        const a = t.register(4);
        t.noteLost(4, a);
        expect(t.unregister(4, a)).toEqual({ changed: true, repaint: false });
        expect(t.health().lostPages).toEqual([]);
        expect(t.unregister(4, a).changed).toBe(false);
    });

    it('folds the polled state into the same transitions', () => {
        const t = new CanvasContextTracker();
        const a = t.register(0);
        expect(t.observe(0, a, false)).toEqual({ changed: false, repaint: false });
        expect(t.observe(0, a, true)).toEqual({ changed: true, repaint: false });
        expect(t.observe(0, a, true)).toEqual({ changed: false, repaint: false });
        expect(t.observe(0, a, false)).toEqual({ changed: true, repaint: true });
        expect(t.health()).toEqual({ lostPages: [], losses: 1, restores: 1 });
    });
});

describe('CanvasContextWatcher (events)', () => {
    it('reports a loss, then repaints exactly the restored page', () => {
        const { watcher, health, restored } = harness();
        const p0 = new EventSurface();
        const p2 = new EventSurface();
        expect(watcher.watch(0, p0)).toBe('events');
        expect(watcher.watch(2, p2)).toBe('events');
        expect(watcher.mode()).toBe('events');

        p2.fire('contextlost');
        expect(health.at(-1)).toEqual({ lostPages: [2], losses: 1, restores: 0 });
        expect(restored).toEqual([]);

        p2.fire('contextrestored');
        expect(health.at(-1)).toEqual({ lostPages: [], losses: 1, restores: 1 });
        expect(restored).toEqual([2]);
    });

    it('a contextrestored that never arrives stays visible as a lost page', () => {
        const { watcher, health, restored } = harness();
        const p1 = new EventSurface();
        watcher.watch(1, p1);
        p1.fire('contextlost');
        expect(watcher.health().lostPages).toEqual([1]);
        expect(health.at(-1)?.lostPages).toEqual([1]);
        expect(restored).toEqual([]);
    });

    it('stops listening to a replaced surface', () => {
        const { watcher, health, restored } = harness();
        const old = new EventSurface();
        const fresh = new EventSurface();
        watcher.watch(1, old);
        watcher.watch(1, fresh);
        old.fire('contextlost');
        old.fire('contextrestored');
        expect(health).toEqual([]);
        expect(restored).toEqual([]);
        fresh.fire('contextlost');
        expect(watcher.health().lostPages).toEqual([1]);
    });

    it('unwatching a lost page clears it from the broadcast health', () => {
        const { watcher, health } = harness();
        const p = new EventSurface();
        watcher.watch(5, p);
        p.fire('contextlost');
        watcher.unwatch(5);
        expect(health.at(-1)).toEqual({ lostPages: [], losses: 1, restores: 0 });
    });

    it('starts no timer when every surface has the events', () => {
        const timers = fakeTimers();
        const { watcher } = harness({ timers });
        watcher.watch(0, new EventSurface());
        expect(timers.active()).toBe(0);
    });
});

describe('CanvasContextWatcher (polling fallback)', () => {
    it('detects the events by the oncontext* attributes', () => {
        expect(hasContextEvents(new EventSurface())).toBe(true);
        expect(hasContextEvents(new PollSurface())).toBe(false);
    });

    it('polls isContextLost() on one shared timer when the events are missing', () => {
        const timers = fakeTimers();
        const { watcher, health, restored } = harness({ timers });
        const a = new PollSurface();
        const b = new PollSurface();
        expect(watcher.watch(0, a)).toBe('polling');
        expect(watcher.watch(1, b)).toBe('polling');
        expect(watcher.mode()).toBe('polling');
        expect(timers.active()).toBe(1);

        timers.tick();
        expect(health).toEqual([]);

        b.lost = true;
        timers.tick();
        expect(health.at(-1)).toEqual({ lostPages: [1], losses: 1, restores: 0 });
        expect(restored).toEqual([]);

        b.lost = false;
        timers.tick();
        expect(health.at(-1)).toEqual({ lostPages: [], losses: 1, restores: 1 });
        expect(restored).toEqual([1]);
    });

    it('stops the timer once nothing needs polling', () => {
        const timers = fakeTimers();
        const { watcher } = harness({ timers });
        watcher.watch(0, new PollSurface());
        expect(timers.active()).toBe(1);
        watcher.unwatch(0);
        expect(timers.active()).toBe(0);
    });

    it('a surface with no 2D context is not watched at all', () => {
        const timers = fakeTimers();
        const { watcher } = harness({ timers });
        expect(watcher.watch(0, new BareSurface())).toBe('none');
        expect(watcher.mode()).toBe('none');
        expect(timers.active()).toBe(0);
    });

    it('can be pinned onto a surface that has the events', () => {
        const timers = fakeTimers();
        const { watcher } = harness({ timers, detectEvents: () => false });
        const s = new EventSurface();
        Object.assign(s, { getContext: () => ({ isContextLost: () => false }) });
        expect(watcher.watch(0, s)).toBe('polling');
    });
});

describe('forcedEvents', () => {
    it('maps the dev-hook phase to the synthetic events', () => {
        expect(forcedEvents('lost')).toEqual(['contextlost']);
        expect(forcedEvents('restored')).toEqual(['contextrestored']);
        expect(forcedEvents('both')).toEqual(['contextlost', 'contextrestored']);
    });
});
