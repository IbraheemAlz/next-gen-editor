/* Issue #438 - the event-log retry state machines, pulled out of
 * `engine.worker.ts` (which imports the wasm engine and cannot be loaded by
 * a unit test). Both take an injectable clock (`Timers`) and writer, so a
 * test drives every transition with fake timers and a scripted writer; the
 * worker passes `setTimeout` / `clearTimeout` and the real IndexedDB
 * functions.
 *
 * - `createSnapshotRetry` - issue #333 / #390: a failed snapshot (the
 *   engine-side `SNAPSHOT` dispatch or its IndexedDB write) is retried on
 *   the bounded 2/4/8 s clock, independent of new commands; the failure
 *   after the last delay raises the "not being checkpointed" warning; a
 *   success resets everything.
 * - `createJournalRetry` - issue #390: a failed command-journal row waits
 *   in a backlog and is re-written on the same clock; a burst of failing
 *   keystrokes is ONE round, not one per key; after the last delay the
 *   journal is `exhausted` and a recovery would miss commands.
 *
 * Neither knows about postMessage: they report through `onHealth(failures)`
 * and the worker folds both machines into `buildCheckpointState`. */
import { nextRetry } from './retry-schedule';

/** An injectable clock: `setTimeout` / `clearTimeout` in the worker. */
export interface Timers {
    set(fn: () => void, ms: number): unknown;
    clear(handle: unknown): void;
}

/** The real clock. */
export const realTimers: Timers = {
    set: (fn, ms) => setTimeout(fn, ms),
    clear: (h) => clearTimeout(h as ReturnType<typeof setTimeout>),
};

/** A human-readable failure message (no document content: IndexedDB /
 *  engine error text only). */
export function failureText(e: unknown): string {
    if (e instanceof Error) return e.message || e.name;
    return typeof e === 'string' ? e : 'unknown error';
}

/** The wire shape of `Event::CheckpointState` (issue #390): `ok` is false
 *  once the snapshot retries are exhausted OR the journal is. Every event
 *  with `failures > 0` reports exactly one failed attempt. */
export function buildCheckpointState(input: {
    snapshotWarned: boolean;
    journalExhausted: boolean;
    failures: number;
    lastError: string | undefined;
}): {
    type: 'CHECKPOINT_STATE';
    ok: boolean;
    failures: number;
    journal_failing: boolean;
    last_error?: string;
} {
    return {
        type: 'CHECKPOINT_STATE',
        ok: !input.snapshotWarned && !input.journalExhausted,
        failures: input.failures,
        journal_failing: input.journalExhausted,
        ...(input.lastError !== undefined && input.failures > 0
            ? { last_error: input.lastError }
            : {}),
    };
}

/* ------------------------------------------------------------------ */
/* Snapshot retry (issue #333 / #390)                                  */
/* ------------------------------------------------------------------ */

export interface SnapshotRetryDeps {
    timers: Timers;
    /** Queue a snapshot attempt at the log head (the worker enqueues
     *  `takeSnapshot(logSequence)` behind in-flight commands). */
    retrySnapshot(): void;
    /** A health change worth telling the shell about (`failures` = the
     *  failed attempts so far in this run; `0` = recovered). */
    onHealth(failures: number): void;
    /** The latest failure's message. */
    onError(message: string): void;
}

export interface SnapshotRetry {
    /** A snapshot attempt failed. */
    noteFailed(reason: unknown): void;
    /** A snapshot landed: the failure run is over. */
    noteOk(): void;
    /** Drop a pending retry timer (planned retirement). */
    cancel(): void;
    readonly failures: number;
    /** Retries exhausted: the user has been told the log is not
     *  checkpointing. */
    readonly warned: boolean;
    readonly timerPending: boolean;
}

export function createSnapshotRetry(deps: SnapshotRetryDeps): SnapshotRetry {
    let failures = 0;
    let warned = false;
    let timer: unknown;
    let pending = false;
    const clear = (): void => {
        if (pending) {
            deps.timers.clear(timer);
            pending = false;
            timer = undefined;
        }
    };
    return {
        noteFailed(reason) {
            failures += 1;
            deps.onError(failureText(reason));
            const decision = nextRetry(failures);
            if (decision.retry) {
                deps.onHealth(failures);
                clear();
                pending = true;
                timer = deps.timers.set(() => {
                    pending = false;
                    timer = undefined;
                    deps.retrySnapshot();
                }, decision.delayMs);
                return;
            }
            /* Retries exhausted. A later command-driven snapshot may still
               land (and reset); until then the user must know the log is
               not checkpointing. The exhausting failure is ONE event. */
            warned = true;
            deps.onHealth(failures);
        },
        noteOk() {
            const had = failures > 0;
            failures = 0;
            clear();
            if (had || warned) {
                warned = false;
                deps.onHealth(0);
            }
        },
        cancel: clear,
        get failures() {
            return failures;
        },
        get warned() {
            return warned;
        },
        get timerPending() {
            return pending;
        },
    };
}

/* ------------------------------------------------------------------ */
/* Journal retry (issue #390)                                          */
/* ------------------------------------------------------------------ */

export const JOURNAL_BACKLOG_MAX = 5000;

export interface JournalRetryDeps<C> {
    timers: Timers;
    /** Re-write one journal row (`appendCommand`). */
    write(seq: number, cmd: C): Promise<void>;
    /** Best effort, on a store that may not be the broken one: lets a later
     *  recovery report the gap. */
    recordGap(seqs: number[]): Promise<void>;
    clearGap(): Promise<void>;
    onHealth(failures: number): void;
    onError(message: string): void;
    backlogMax?: number;
}

export interface JournalRetry<C> {
    /** A first-time row write landed (a healthy store again: drain). */
    noteWriteOk(): void;
    /** A first-time row write failed: it joins the backlog. */
    noteWriteFailed(seq: number, cmd: C, e: unknown): void;
    /** Re-write every backlogged row; all landed = healthy again, any
     *  failure = the next round. */
    drain(): Promise<void>;
    readonly size: number;
    readonly failures: number;
    readonly exhausted: boolean;
    readonly draining: boolean;
    readonly timerPending: boolean;
    /** Backlogged seqs, ascending insertion order. */
    keys(): number[];
}

export function createJournalRetry<C>(deps: JournalRetryDeps<C>): JournalRetry<C> {
    const max = deps.backlogMax ?? JOURNAL_BACKLOG_MAX;
    const backlog = new Map<number, C>();
    let failures = 0;
    let exhausted = false;
    let draining = false;
    let timer: unknown;
    let pending = false;
    const clearTimer = (): void => {
        if (pending) {
            deps.timers.clear(timer);
            pending = false;
            timer = undefined;
        }
    };

    /* One round of attempts failed: retry later, or give up loudly. */
    const failRound = (): void => {
        if (exhausted) return;
        failures += 1;
        const decision = nextRetry(failures);
        if (decision.retry) {
            deps.onHealth(failures);
            pending = true;
            timer = deps.timers.set(() => {
                pending = false;
                timer = undefined;
                void drain();
            }, decision.delayMs);
            return;
        }
        exhausted = true;
        deps.onHealth(failures);
    };

    async function drain(): Promise<void> {
        if (draining) return;
        draining = true;
        try {
            clearTimer();
            for (const [seq, cmd] of [...backlog]) {
                try {
                    await deps.write(seq, cmd);
                    backlog.delete(seq);
                } catch (e: unknown) {
                    deps.onError(failureText(e));
                    failRound();
                    return;
                }
            }
            const wasFailing = failures > 0 || exhausted;
            failures = 0;
            exhausted = false;
            void deps.clearGap().catch(() => undefined);
            if (wasFailing) deps.onHealth(0);
        } finally {
            draining = false;
        }
    }

    return {
        noteWriteOk() {
            if (exhausted && backlog.size > 0 && !draining) void drain();
        },
        noteWriteFailed(seq, cmd, e) {
            backlog.set(seq, cmd);
            if (backlog.size > max) {
                const oldest = backlog.keys().next();
                if (!oldest.done) backlog.delete(oldest.value);
            }
            deps.onError(failureText(e));
            void deps.recordGap([...backlog.keys()]).catch(() => undefined);
            if (exhausted || pending || draining) return;
            failRound();
        },
        drain,
        get size() {
            return backlog.size;
        },
        get failures() {
            return failures;
        },
        get exhausted() {
            return exhausted;
        },
        get draining() {
            return draining;
        },
        get timerPending() {
            return pending;
        },
        keys: () => [...backlog.keys()],
    };
}
