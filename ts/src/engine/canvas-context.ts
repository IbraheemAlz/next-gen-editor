/* Issue #502 - the worker's 2D-context bookkeeping, pulled out of
 * `engine.worker.ts` (which imports the wasm engine and cannot be loaded
 * by a unit test).
 *
 * A 2D canvas whose context is LOST and later RESTORED comes back blank.
 * The engine remembers the display list each page canvas last painted and
 * skips an identical one (#497), so without a hook a restored page would
 * stay blank until its display list happened to change. The worker owns
 * every `OffscreenCanvas`, so it listens here:
 *
 * - `contextlost`     -> record the loss (Dev HUD row, telemetry);
 * - `contextrestored` -> tell the caller to forget that canvas's memo and
 *                        repaint it.
 *
 * The events exist on `OffscreenCanvas` in current Chrome (the HTML
 * spec's `oncontextlost` / `oncontextrestored` attributes). A surface
 * that lacks them (an older engine, a non-Chromium worker) is polled with
 * `isContextLost()` on ONE shared interval instead - a slower, coarser
 * signal, so `mode()` reports which one is in force.
 *
 * Nothing here knows about `postMessage` or the engine: the watcher
 * reports through `onHealth` (the lost-set / counters changed) and
 * `onRestored` (repaint this page), and the worker wires those to the
 * `Event::CanvasContext` broadcast and `Engine.forget_canvas_memo`. */
import { realTimers, type Timers } from './checkpoint-retry';

/** The wire shape of `Event::CanvasContext` (minus its `type` tag). */
export interface CanvasContextHealth {
    /** Page indices whose context is lost right now, ascending. */
    lostPages: number[];
    /** Losses seen by this worker generation. */
    losses: number;
    /** Restorations seen by this worker generation. */
    restores: number;
}

/** What the watcher needs of an `OffscreenCanvas` (kept structural so a
 *  unit test hands it a plain `EventTarget`). */
export interface WatchedSurface extends EventTarget {
    getContext?(kind: '2d'): { isContextLost?(): boolean } | null;
}

/** How a surface's context state reaches the watcher. */
export type WatchMode = 'events' | 'polling' | 'none';

/** What the tracker decided about an observation. */
export interface ContextChange {
    /** The counters or the lost-set changed (broadcast the health). */
    changed: boolean;
    /** This canvas must forget its memo and repaint. */
    repaint: boolean;
}

const IGNORED: ContextChange = { changed: false, repaint: false };

interface CanvasRecord {
    token: number;
    lost: boolean;
}

/**
 * Pure per-canvas state: which canvases are lost and how many losses /
 * restorations this generation has seen. A canvas is identified by its
 * page index plus the token its registration returned, so a late event
 * from a surface that was since replaced (the shell remounts a page
 * canvas) is ignored instead of repainting - or marking lost - the new
 * one.
 */
export class CanvasContextTracker {
    private readonly canvases = new Map<number, CanvasRecord>();
    private nextToken = 1;
    private losses = 0;
    private restores = 0;

    /** A surface was registered at `idx`: it starts healthy (a fresh
     *  surface has nothing painted and is repainted by its own
     *  registration), and any record of the surface it replaced goes.
     *  Returns the token that surface's events must carry. */
    register(idx: number): number {
        const token = this.nextToken++;
        this.canvases.set(idx, { token, lost: false });
        return token;
    }

    /** The surface at `idx` is gone. */
    unregister(idx: number, token: number): ContextChange {
        const rec = this.canvases.get(idx);
        if (!rec || rec.token !== token) return IGNORED;
        this.canvases.delete(idx);
        return { changed: rec.lost, repaint: false };
    }

    /** `contextlost` fired. A repeat while already lost counts once. */
    noteLost(idx: number, token: number): ContextChange {
        const rec = this.canvases.get(idx);
        if (!rec || rec.token !== token || rec.lost) return IGNORED;
        rec.lost = true;
        this.losses += 1;
        return { changed: true, repaint: false };
    }

    /** `contextrestored` fired. The canvas repaints even when its loss was
     *  never observed (a restored context is blank either way). */
    noteRestored(idx: number, token: number): ContextChange {
        const rec = this.canvases.get(idx);
        if (!rec || rec.token !== token) return IGNORED;
        rec.lost = false;
        this.restores += 1;
        return { changed: true, repaint: true };
    }

    /** The polling fallback: fold the context's current `isContextLost()`
     *  into the same two transitions. No transition, no change. */
    observe(idx: number, token: number, isLost: boolean): ContextChange {
        const rec = this.canvases.get(idx);
        if (!rec || rec.token !== token || rec.lost === isLost) return IGNORED;
        return isLost ? this.noteLost(idx, token) : this.noteRestored(idx, token);
    }

    /** Page indices that are lost now, ascending. */
    lostPages(): number[] {
        return [...this.canvases]
            .filter(([, rec]) => rec.lost)
            .map(([idx]) => idx)
            .sort((a, b) => a - b);
    }

    health(): CanvasContextHealth {
        return { lostPages: this.lostPages(), losses: this.losses, restores: this.restores };
    }
}

export interface CanvasContextHooks {
    /** The lost-set or a counter changed: broadcast `Event::CanvasContext`. */
    onHealth(health: CanvasContextHealth): void;
    /** A lost context came back: forget page `idx`'s memo and repaint it. */
    onRestored(idx: number): void;
}

interface Watch {
    idx: number;
    token: number;
    surface: WatchedSurface;
    mode: WatchMode;
    detach: () => void;
}

/** The default poll period of the `isContextLost()` fallback. */
export const CONTEXT_POLL_MS = 1000;

/** `true` when the surface exposes the context-loss events. */
export function hasContextEvents(surface: WatchedSurface): boolean {
    return (
        typeof surface.addEventListener === 'function' &&
        'oncontextlost' in surface &&
        'oncontextrestored' in surface
    );
}

function readLost(surface: WatchedSurface): boolean | undefined {
    try {
        const ctx = surface.getContext?.('2d');
        return typeof ctx?.isContextLost === 'function' ? ctx.isContextLost() : undefined;
    } catch {
        return undefined;
    }
}

/**
 * Watches every registered surface: events where the surface has them,
 * one shared `isContextLost()` poll for the rest.
 */
export class CanvasContextWatcher {
    private readonly tracker = new CanvasContextTracker();
    private readonly watches = new Map<number, Watch>();
    private pollHandle: unknown;
    private polling = false;

    constructor(
        private readonly hooks: CanvasContextHooks,
        private readonly timers: Timers = realTimers,
        private readonly pollMs: number = CONTEXT_POLL_MS,
        /** Overridable so a test can pin the polling fallback on a surface
         *  that does have the events. */
        private readonly detectEvents: (s: WatchedSurface) => boolean = hasContextEvents,
    ) {}

    /** Start watching `surface` as page `idx` (replacing whatever was
     *  watched there). Returns the mode in force for it. */
    watch(idx: number, surface: WatchedSurface): WatchMode {
        this.unwatch(idx);
        const token = this.tracker.register(idx);
        const apply = (c: ContextChange): void => {
            if (c.changed) this.hooks.onHealth(this.tracker.health());
            if (c.repaint) this.hooks.onRestored(idx);
        };
        let mode: WatchMode;
        let detach = (): void => undefined;
        if (this.detectEvents(surface)) {
            const lost = (): void => apply(this.tracker.noteLost(idx, token));
            const restored = (): void => apply(this.tracker.noteRestored(idx, token));
            surface.addEventListener('contextlost', lost);
            surface.addEventListener('contextrestored', restored);
            detach = () => {
                surface.removeEventListener('contextlost', lost);
                surface.removeEventListener('contextrestored', restored);
            };
            mode = 'events';
        } else if (readLost(surface) !== undefined) {
            mode = 'polling';
        } else {
            /* A surface without a 2D context (a WebGPU / Vello boot
               surface) has no 2D context to lose. */
            mode = 'none';
        }
        this.watches.set(idx, { idx, token, surface, mode, detach });
        this.syncPoll();
        return mode;
    }

    /** Stop watching page `idx`. */
    unwatch(idx: number): void {
        const w = this.watches.get(idx);
        if (!w) return;
        w.detach();
        this.watches.delete(idx);
        const c = this.tracker.unregister(idx, w.token);
        this.syncPoll();
        if (c.changed) this.hooks.onHealth(this.tracker.health());
    }

    /** One polling pass (the interval calls it; a test may too). */
    pollOnce(): void {
        for (const w of this.watches.values()) {
            if (w.mode !== 'polling') continue;
            const lost = readLost(w.surface);
            if (lost === undefined) continue;
            const c = this.tracker.observe(w.idx, w.token, lost);
            if (c.changed) this.hooks.onHealth(this.tracker.health());
            if (c.repaint) this.hooks.onRestored(w.idx);
        }
    }

    /** `'events'` when every watched context-bearing surface reports by
     *  event, `'polling'` when any needs the poll, `'none'` when there is
     *  nothing to watch. */
    mode(): WatchMode {
        let mode: WatchMode = 'none';
        for (const w of this.watches.values()) {
            if (w.mode === 'polling') return 'polling';
            if (w.mode === 'events') mode = 'events';
        }
        return mode;
    }

    health(): CanvasContextHealth {
        return this.tracker.health();
    }

    /** Every surface unwatched, the poll stopped. */
    dispose(): void {
        for (const idx of [...this.watches.keys()]) this.unwatch(idx);
    }

    private syncPoll(): void {
        const need = [...this.watches.values()].some((w) => w.mode === 'polling');
        if (need && !this.polling) {
            this.polling = true;
            const tick = (): void => {
                this.pollOnce();
                if (this.polling) this.pollHandle = this.timers.set(tick, this.pollMs);
            };
            this.pollHandle = this.timers.set(tick, this.pollMs);
        } else if (!need && this.polling) {
            this.polling = false;
            this.timers.clear(this.pollHandle);
            this.pollHandle = undefined;
        }
    }
}

/** The worker-side test hook behind the dev-only `__forceContextLoss`:
 *  which synthetic events to dispatch on a surface, in order. */
export type ForcedPhase = 'lost' | 'restored' | 'both';

export function forcedEvents(phase: ForcedPhase): Array<'contextlost' | 'contextrestored'> {
    return phase === 'lost'
        ? ['contextlost']
        : phase === 'restored'
          ? ['contextrestored']
          : ['contextlost', 'contextrestored'];
}
