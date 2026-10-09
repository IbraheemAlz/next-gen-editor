/* Issue #438 - every transition of the #333 snapshot-retry and #390
 * journal-retry state machines, on a fake clock and a scripted writer. */
import { describe, expect, it } from 'vitest';
import {
    buildCheckpointState,
    createJournalRetry,
    createSnapshotRetry,
    failureText,
    type Timers,
} from './checkpoint-retry';

/** A manual clock: timers fire only when the test advances it. */
function fakeClock(): Timers & { now: number; pending(): number; advance(ms: number): void } {
    let now = 0;
    let nextId = 1;
    const timers = new Map<number, { at: number; fn: () => void }>();
    return {
        get now() {
            return now;
        },
        set(fn, ms) {
            const id = nextId++;
            timers.set(id, { at: now + ms, fn });
            return id;
        },
        clear(h) {
            timers.delete(h as number);
        },
        pending: () => timers.size,
        advance(ms) {
            const target = now + ms;
            for (;;) {
                const due = [...timers.entries()]
                    .filter(([, t]) => t.at <= target)
                    .sort((a, b) => a[1].at - b[1].at)[0];
                if (!due) break;
                timers.delete(due[0]);
                now = due[1].at;
                due[1].fn();
            }
            now = target;
        },
    };
}

describe('failureText', () => {
    it('prefers an Error message, then its name, then a string', () => {
        expect(failureText(new Error('quota'))).toBe('quota');
        const e = new Error('');
        e.name = 'AbortError';
        expect(failureText(e)).toBe('AbortError');
        expect(failureText('boom')).toBe('boom');
        expect(failureText(42)).toBe('unknown error');
    });
});

describe('buildCheckpointState (#390)', () => {
    it('is ok only when neither the snapshots nor the journal gave up', () => {
        const base = { failures: 0, lastError: undefined };
        expect(buildCheckpointState({ ...base, snapshotWarned: false, journalExhausted: false }).ok).toBe(true);
        expect(buildCheckpointState({ ...base, snapshotWarned: true, journalExhausted: false }).ok).toBe(false);
        const j = buildCheckpointState({ ...base, snapshotWarned: false, journalExhausted: true });
        expect(j.ok).toBe(false);
        expect(j.journal_failing).toBe(true);
    });

    it('carries last_error only on a failure report', () => {
        const input = { snapshotWarned: false, journalExhausted: false, lastError: 'quota' };
        expect(buildCheckpointState({ ...input, failures: 0 })).not.toHaveProperty('last_error');
        expect(buildCheckpointState({ ...input, failures: 2 })).toMatchObject({
            failures: 2,
            last_error: 'quota',
        });
        expect(
            buildCheckpointState({ ...input, failures: 2, lastError: undefined }),
        ).not.toHaveProperty('last_error');
    });
});

describe('snapshot retry (#333)', () => {
    function setup() {
        const clock = fakeClock();
        const health: number[] = [];
        const errors: string[] = [];
        let retried = 0;
        const retry = createSnapshotRetry({
            timers: clock,
            retrySnapshot: () => {
                retried += 1;
            },
            onHealth: (f) => health.push(f),
            onError: (m) => errors.push(m),
        });
        return { clock, retry, health, errors, retried: () => retried };
    }

    it('schedules retries at 2 s, 4 s and 8 s, independent of new commands', () => {
        const { clock, retry, health, retried } = setup();
        retry.noteFailed(new Error('a'));
        expect(health).toEqual([1]);
        expect(retry.timerPending).toBe(true);
        clock.advance(1999);
        expect(retried()).toBe(0);
        clock.advance(1);
        expect(retried()).toBe(1);
        retry.noteFailed('b');
        clock.advance(3999);
        expect(retried()).toBe(1);
        clock.advance(1);
        expect(retried()).toBe(2);
        retry.noteFailed('c');
        clock.advance(8000);
        expect(retried()).toBe(3);
        expect(health).toEqual([1, 2, 3]);
        expect(retry.warned).toBe(false);
    });

    it('the failure after the last delay raises the warning once and stops retrying', () => {
        const { clock, retry, health, errors, retried } = setup();
        for (let i = 0; i < 3; i++) {
            retry.noteFailed(`f${i}`);
            clock.advance(10_000);
        }
        expect(retried()).toBe(3);
        retry.noteFailed('last');
        expect(retry.warned).toBe(true);
        expect(retry.failures).toBe(4);
        expect(retry.timerPending).toBe(false);
        expect(health).toEqual([1, 2, 3, 4]);
        expect(errors.at(-1)).toBe('last');
        clock.advance(60_000);
        expect(retried()).toBe(3);
    });

    it('a success resets the run, clears the timer and reports recovery once', () => {
        const { clock, retry, health } = setup();
        retry.noteFailed('x');
        retry.noteOk();
        expect(retry.failures).toBe(0);
        expect(retry.timerPending).toBe(false);
        expect(clock.pending()).toBe(0);
        expect(health).toEqual([1, 0]);
        retry.noteOk();
        expect(health).toEqual([1, 0]);
    });

    it('a success after the warning clears it', () => {
        const { retry, health } = setup();
        for (let i = 0; i < 4; i++) retry.noteFailed('x');
        expect(retry.warned).toBe(true);
        retry.noteOk();
        expect(retry.warned).toBe(false);
        expect(health.at(-1)).toBe(0);
    });

    it('a failure while a retry is pending replaces the timer (never two)', () => {
        const { clock, retry } = setup();
        retry.noteFailed('a');
        retry.noteFailed('b');
        expect(clock.pending()).toBe(1);
    });

    it('cancel drops the pending retry without touching the counters', () => {
        const { clock, retry, retried } = setup();
        retry.noteFailed('a');
        retry.cancel();
        clock.advance(60_000);
        expect(retried()).toBe(0);
        expect(retry.failures).toBe(1);
    });
});

describe('journal retry (#390)', () => {
    function setup(opts: { backlogMax?: number } = {}) {
        const clock = fakeClock();
        const health: number[] = [];
        const errors: string[] = [];
        const written: number[] = [];
        const gaps: number[][] = [];
        let gapCleared = 0;
        /* Seqs the store refuses until `heal()`. */
        let broken = true;
        const retry = createJournalRetry<string>({
            timers: clock,
            write: async (seq) => {
                if (broken) throw new Error('quota');
                written.push(seq);
            },
            recordGap: async (seqs) => {
                gaps.push(seqs);
            },
            clearGap: async () => {
                gapCleared += 1;
            },
            onHealth: (f) => health.push(f),
            onError: (m) => errors.push(m),
            ...(opts.backlogMax !== undefined ? { backlogMax: opts.backlogMax } : {}),
        });
        return {
            clock,
            retry,
            health,
            errors,
            written,
            gaps,
            cleared: () => gapCleared,
            heal: () => {
                broken = false;
            },
        };
    }

    it('a failed row joins the backlog, records the gap and starts the 2 s clock', () => {
        const t = setup();
        t.retry.noteWriteFailed(5, 'a', new Error('quota'));
        expect(t.retry.keys()).toEqual([5]);
        expect(t.gaps).toEqual([[5]]);
        expect(t.health).toEqual([1]);
        expect(t.retry.timerPending).toBe(true);
        expect(t.errors).toEqual(['quota']);
    });

    it('a burst of failing rows is ONE round, not one per key', () => {
        const t = setup();
        for (const seq of [1, 2, 3, 4]) t.retry.noteWriteFailed(seq, 'c', 'x');
        expect(t.retry.failures).toBe(1);
        expect(t.health).toEqual([1]);
        expect(t.retry.keys()).toEqual([1, 2, 3, 4]);
        expect(t.gaps.at(-1)).toEqual([1, 2, 3, 4]);
    });

    it('the retry re-writes the backlog in order and heals fully', async () => {
        const t = setup();
        t.retry.noteWriteFailed(1, 'a', 'x');
        t.retry.noteWriteFailed(2, 'b', 'x');
        t.heal();
        t.clock.advance(2000);
        await flush();
        expect(t.written).toEqual([1, 2]);
        expect(t.retry.size).toBe(0);
        expect(t.retry.failures).toBe(0);
        expect(t.retry.exhausted).toBe(false);
        expect(t.cleared()).toBe(1);
        expect(t.health).toEqual([1, 0]);
    });

    it('a still-failing retry runs the next round on 4 s, then 8 s, then gives up', async () => {
        const t = setup();
        t.retry.noteWriteFailed(1, 'a', 'x');
        t.clock.advance(2000);
        await flush();
        expect(t.retry.failures).toBe(2);
        expect(t.retry.timerPending).toBe(true);
        t.clock.advance(3999);
        await flush();
        expect(t.retry.failures).toBe(2);
        t.clock.advance(1);
        await flush();
        expect(t.retry.failures).toBe(3);
        t.clock.advance(8000);
        await flush();
        expect(t.retry.failures).toBe(4);
        expect(t.retry.exhausted).toBe(true);
        expect(t.retry.timerPending).toBe(false);
        expect(t.health).toEqual([1, 2, 3, 4]);
        /* Exhausted: more failures only grow the backlog. */
        t.retry.noteWriteFailed(2, 'b', 'x');
        expect(t.retry.keys()).toEqual([1, 2]);
        expect(t.health).toEqual([1, 2, 3, 4]);
    });

    it('a later successful write drains an exhausted journal (the store works again)', async () => {
        const t = setup();
        t.retry.noteWriteFailed(1, 'a', 'x');
        for (const ms of [2000, 4000, 8000]) {
            t.clock.advance(ms);
            await flush();
        }
        expect(t.retry.exhausted).toBe(true);
        t.heal();
        t.retry.noteWriteOk();
        await flush();
        expect(t.written).toEqual([1]);
        expect(t.retry.exhausted).toBe(false);
        expect(t.retry.size).toBe(0);
        expect(t.health.at(-1)).toBe(0);
    });

    it('a healthy write while rows are merely waiting does not drain early', async () => {
        const t = setup();
        t.retry.noteWriteFailed(1, 'a', 'x');
        t.heal();
        t.retry.noteWriteOk();
        await flush();
        expect(t.written).toEqual([]);
        expect(t.retry.size).toBe(1);
    });

    it('drain is not re-entrant', async () => {
        const t = setup();
        t.retry.noteWriteFailed(1, 'a', 'x');
        t.heal();
        const a = t.retry.drain();
        const b = t.retry.drain();
        await Promise.all([a, b]);
        expect(t.written).toEqual([1]);
    });

    it('bounds the backlog by dropping the oldest rows', () => {
        const t = setup({ backlogMax: 3 });
        for (const seq of [1, 2, 3, 4, 5]) t.retry.noteWriteFailed(seq, 'c', 'x');
        expect(t.retry.keys()).toEqual([3, 4, 5]);
    });

    it('a partial drain keeps what did not land and fails the round', async () => {
        const clock = fakeClock();
        const written: number[] = [];
        const retry = createJournalRetry<string>({
            timers: clock,
            write: async (seq) => {
                if (seq === 2) throw new Error('bad row');
                written.push(seq);
            },
            recordGap: async () => undefined,
            clearGap: async () => undefined,
            onHealth: () => undefined,
            onError: () => undefined,
        });
        retry.noteWriteFailed(1, 'a', 'x');
        retry.noteWriteFailed(2, 'b', 'x');
        retry.noteWriteFailed(3, 'c', 'x');
        clock.advance(2000);
        await flush();
        expect(written).toEqual([1]);
        expect(retry.keys()).toEqual([2, 3]);
        expect(retry.failures).toBe(2);
    });
});

/** Let queued microtasks (the async writer chain) settle. */
async function flush(): Promise<void> {
    for (let i = 0; i < 20; i++) await Promise.resolve();
}
