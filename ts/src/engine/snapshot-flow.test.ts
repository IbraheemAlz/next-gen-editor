/* Issue #438 - `takeSnapshot`'s decisions (#85 / #212 / #268 / #314 / #333),
 * with the engine reply and the IndexedDB write scripted. */
import { describe, expect, it } from 'vitest';
import {
    newSnapshotState,
    planSnapshotPackage,
    takeSnapshotFlow,
    type SnapshotFlowDeps,
    type SnapshotReply,
    type SnapshotState,
} from './snapshot-flow';
import type { SnapshotPackage } from './event-log';

const bytes = (n: number): Uint8Array => new Uint8Array([n]);

interface Persisted {
    seq: number;
    bytes: Uint8Array;
    pkg: SnapshotPackage | undefined;
    pin: boolean;
}

function harness(opts: { reply?: (seq: number, known: string | undefined) => SnapshotReply | Error } = {}) {
    const state: SnapshotState = newSnapshotState();
    const persisted: Persisted[] = [];
    const known: (string | undefined)[] = [];
    const failures: unknown[] = [];
    let oks = 0;
    const writes: Promise<void>[] = [];
    let engine = true;
    /* Per-write control: resolve or reject on demand. */
    const settle: { resolve: () => void; reject: (e: unknown) => void }[] = [];
    let autoFail: unknown;
    const deps: SnapshotFlowDeps = {
        hasEngine: () => engine,
        engineSnapshot: async (seq, k) => {
            known.push(k);
            const r = opts.reply?.(seq, k) ?? { type: 'SNAPSHOT', bytes: bytes(seq) };
            if (r instanceof Error) throw r;
            return r;
        },
        persist: (seq, b, pkg, o) => {
            persisted.push({ seq, bytes: b, pkg, pin: o.pin });
            if (autoFail !== undefined) return Promise.reject(autoFail);
            return new Promise<void>((resolve, reject) => settle.push({ resolve, reject }));
        },
        retry: {
            noteFailed: (r) => failures.push(r),
            noteOk: () => {
                oks += 1;
            },
        },
        track: (w) => writes.push(w),
    };
    return {
        state,
        deps,
        persisted,
        known,
        failures,
        oks: () => oks,
        writes,
        settle,
        noEngine: () => {
            engine = false;
        },
        failWrites: (e: unknown) => {
            autoFail = e;
        },
        flush: () => Promise.all(writes),
    };
}

describe('planSnapshotPackage (#314)', () => {
    it('ships nothing when the snapshot names no package', () => {
        const s = { committedPackageHash: undefined, pendingPackage: undefined };
        expect(planSnapshotPackage(s, {}).pkg).toBeUndefined();
    });

    it('carries the bytes the engine shipped and remembers them as in flight', () => {
        const s = { committedPackageHash: undefined, pendingPackage: undefined };
        const plan = planSnapshotPackage(s, { package_hash: 'h1', package: bytes(9) });
        expect(plan.pkg).toEqual({ hash: 'h1', bytes: bytes(9) });
        expect(plan.pendingPackage).toEqual({ hash: 'h1', bytes: bytes(9) });
    });

    it('re-carries the in-flight bytes when the engine skipped them but the write has not committed', () => {
        const s = { committedPackageHash: undefined, pendingPackage: { hash: 'h1', bytes: bytes(9) } };
        const plan = planSnapshotPackage(s, { package_hash: 'h1' });
        expect(plan.pkg).toEqual({ hash: 'h1', bytes: bytes(9) });
    });

    it('a committed hash goes bare (persistSnapshot verifies the store holds it)', () => {
        const s = { committedPackageHash: 'h1', pendingPackage: undefined };
        expect(planSnapshotPackage(s, { package_hash: 'h1' }).pkg).toEqual({ hash: 'h1' });
    });
});

describe('takeSnapshotFlow', () => {
    it('skips a position already snapshotted, and when there is no engine', async () => {
        const h = harness();
        h.state.lastSnapshotAt = 10;
        expect(await takeSnapshotFlow(10, h.state, h.deps)).toBe('skipped');
        expect(await takeSnapshotFlow(3, h.state, h.deps)).toBe('skipped');
        h.noEngine();
        expect(await takeSnapshotFlow(50, h.state, h.deps)).toBe('skipped');
        expect(h.persisted).toEqual([]);
        expect(h.known).toEqual([]);
    });

    it('persists the engine bytes at seq and advances lastSnapshotAt immediately', async () => {
        const h = harness();
        expect(await takeSnapshotFlow(7, h.state, h.deps)).toBe('persisting');
        expect(h.state.lastSnapshotAt).toBe(7);
        expect(h.persisted).toEqual([{ seq: 7, bytes: bytes(7), pkg: undefined, pin: false }]);
        h.settle[0]!.resolve();
        await h.flush();
        expect(h.oks()).toBe(1);
        expect(h.failures).toEqual([]);
    });

    it('passes the committed hash, else the in-flight one, as known_package_hash', async () => {
        const h = harness();
        await takeSnapshotFlow(1, h.state, h.deps);
        h.state.pendingPackage = { hash: 'p', bytes: bytes(1) };
        await takeSnapshotFlow(2, h.state, h.deps);
        h.state.committedPackageHash = 'c';
        await takeSnapshotFlow(3, h.state, h.deps);
        expect(h.known).toEqual([undefined, 'p', 'c']);
    });

    it('an engine ERROR reply is a retryable failure and does not move lastSnapshotAt', async () => {
        const h = harness({ reply: () => ({ type: 'ERROR', message: 'nope' }) });
        expect(await takeSnapshotFlow(4, h.state, h.deps)).toBe('engine-failed');
        expect(h.failures).toEqual(['nope']);
        expect(h.state.lastSnapshotAt).toBe(0);
        expect(h.persisted).toEqual([]);
    });

    it('an unexpected reply type is a failure too', async () => {
        const h = harness({ reply: () => ({ type: 'PAINTED' }) });
        await takeSnapshotFlow(4, h.state, h.deps);
        expect(h.failures).toEqual(['unexpected PAINTED']);
    });

    it('a throwing dispatch is a failure (not an unhandled rejection)', async () => {
        const h = harness({ reply: () => new Error('wasm trap') });
        expect(await takeSnapshotFlow(4, h.state, h.deps)).toBe('dispatch-threw');
        expect(h.failures).toHaveLength(1);
        expect(h.state.lastSnapshotAt).toBe(0);
    });

    it('a failed write rewinds lastSnapshotAt so the position is retried, not skipped (#333)', async () => {
        const h = harness();
        h.state.lastSnapshotAt = 2;
        await takeSnapshotFlow(9, h.state, h.deps);
        expect(h.state.lastSnapshotAt).toBe(9);
        h.settle[0]!.reject(new Error('quota'));
        await h.flush();
        expect(h.state.lastSnapshotAt).toBe(2);
        expect(h.failures).toHaveLength(1);
        /* ... and the same position can now be snapshotted again. */
        expect(await takeSnapshotFlow(9, h.state, h.deps)).toBe('persisting');
    });

    it('does not rewind when a newer snapshot already moved lastSnapshotAt', async () => {
        const h = harness();
        await takeSnapshotFlow(5, h.state, h.deps);
        await takeSnapshotFlow(8, h.state, h.deps);
        h.settle[1]!.resolve();
        h.settle[0]!.reject(new Error('late failure of the older write'));
        await Promise.allSettled(h.writes);
        expect(h.state.lastSnapshotAt).toBe(8);
    });

    it('pins the first snapshot of a document and re-arms the pin when that write fails (#268)', async () => {
        const h = harness();
        h.state.pinNextSnapshot = true;
        await takeSnapshotFlow(1, h.state, h.deps);
        expect(h.persisted[0]!.pin).toBe(true);
        expect(h.state.pinNextSnapshot).toBe(false);
        await takeSnapshotFlow(2, h.state, h.deps);
        expect(h.persisted[1]!.pin).toBe(false);
        h.settle[1]!.resolve();
        h.settle[0]!.reject(new Error('x'));
        await Promise.allSettled(h.writes);
        expect(h.state.pinNextSnapshot).toBe(true);
    });

    it('a pin that landed stays consumed', async () => {
        const h = harness();
        h.state.pinNextSnapshot = true;
        await takeSnapshotFlow(1, h.state, h.deps);
        h.settle[0]!.resolve();
        await h.flush();
        expect(h.state.pinNextSnapshot).toBe(false);
    });

    describe('package bookkeeping (#212 / #314)', () => {
        const withPackage = (hash: string, shipped: boolean) =>
            harness({
                reply: (seq) => ({
                    type: 'SNAPSHOT',
                    bytes: bytes(seq),
                    package_hash: hash,
                    ...(shipped ? { package: bytes(99) } : {}),
                }),
            });

        it('commits the hash only once the write landed', async () => {
            const h = withPackage('h1', true);
            await takeSnapshotFlow(1, h.state, h.deps);
            expect(h.state.committedPackageHash).toBeUndefined();
            expect(h.state.pendingPackage?.hash).toBe('h1');
            expect(h.persisted[0]!.pkg).toEqual({ hash: 'h1', bytes: bytes(99) });
            h.settle[0]!.resolve();
            await h.flush();
            expect(h.state.committedPackageHash).toBe('h1');
            expect(h.state.pendingPackage).toBeUndefined();
        });

        it('a second snapshot while the first is in flight carries the bytes itself', async () => {
            const h = harness({
                reply: (seq, known) => ({
                    type: 'SNAPSHOT',
                    bytes: bytes(seq),
                    package_hash: 'h1',
                    /* The engine skips what the worker says it knows. */
                    ...(known === 'h1' ? {} : { package: bytes(99) }),
                }),
            });
            await takeSnapshotFlow(1, h.state, h.deps);
            await takeSnapshotFlow(2, h.state, h.deps);
            expect(h.known).toEqual([undefined, 'h1']);
            expect(h.persisted[1]!.pkg).toEqual({ hash: 'h1', bytes: bytes(99) });
        });

        it('a failed write forgets the package so the next snapshot re-ships it', async () => {
            const h = withPackage('h1', true);
            h.state.committedPackageHash = 'h1';
            await takeSnapshotFlow(1, h.state, h.deps);
            h.settle[0]!.reject(new Error('PackageMissingError'));
            await Promise.allSettled(h.writes);
            expect(h.state.committedPackageHash).toBeUndefined();
            expect(h.state.pendingPackage).toBeUndefined();
        });

        it('a committed package is referenced by hash alone', async () => {
            const h = withPackage('h1', false);
            h.state.committedPackageHash = 'h1';
            await takeSnapshotFlow(1, h.state, h.deps);
            expect(h.persisted[0]!.pkg).toEqual({ hash: 'h1' });
        });
    });
});
