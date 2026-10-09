/* Issue #438 - `takeSnapshot`'s decision logic (issues #85 / #212 / #268 /
 * #314 / #333 / #390), pulled out of `engine.worker.ts` so a unit test can
 * script the engine reply and the IndexedDB write. The worker owns the
 * state object and injects the two effects:
 *
 *   - `engineSnapshot(seq, known)` - the `SNAPSHOT` dispatch;
 *   - `persist(seq, bytes, pkg, { pin })` - `persistSnapshot`.
 *
 * What lives here, and nowhere else:
 *   - skip a position already snapshotted (`seq <= lastSnapshotAt`);
 *   - which package hash the engine may skip shipping (`known`): a
 *     COMMITTED one, or one whose bytes the worker still holds (#314);
 *   - which package rides in THIS write: the bytes the engine shipped, the
 *     in-flight bytes for a hash that has not committed yet, or a bare hash
 *     the store is verified to hold;
 *   - the pin flag (#268) and its restore when the write fails;
 *   - the rewind of `lastSnapshotAt` on a failed write (#333) so the
 *     position is snapshotted again rather than skipped as "taken";
 *   - forgetting the package when its write failed (#314).
 */
import type { SnapshotPackage } from './event-log';

/** The worker-owned snapshot bookkeeping. Mutated in place by
 *  [`takeSnapshotFlow`] and by the worker on recovery / new documents. */
export interface SnapshotState {
    /** Log position of the newest snapshot taken (0 = none). */
    lastSnapshotAt: number;
    /** Issue #212 / #314 - content key of the package the event log's
     *  `packages` store is KNOWN to hold (its write committed). */
    committedPackageHash: string | undefined;
    /** Issue #314 - package bytes the engine shipped whose write has not
     *  committed yet. */
    pendingPackage: { hash: string; bytes: Uint8Array } | undefined;
    /** Issue #268 - the next persisted snapshot becomes the pinned base. */
    pinNextSnapshot: boolean;
}

export function newSnapshotState(): SnapshotState {
    return {
        lastSnapshotAt: 0,
        committedPackageHash: undefined,
        pendingPackage: undefined,
        pinNextSnapshot: false,
    };
}

/** The slice of the engine's reply this flow reads. */
export interface SnapshotReply {
    type: string;
    message?: string;
    bytes?: Uint8Array;
    package_hash?: string | undefined;
    package?: Uint8Array | undefined;
}

export interface SnapshotFlowDeps {
    /** `false` while no engine is constructed. */
    hasEngine(): boolean;
    /** The `SNAPSHOT` dispatch (detached package, `known_package_hash`). */
    engineSnapshot(seq: number, known: string | undefined): Promise<SnapshotReply>;
    /** `persistSnapshot`; rejects when the IndexedDB write fails. */
    persist(
        seq: number,
        bytes: Uint8Array,
        pkg: SnapshotPackage | undefined,
        opts: { pin: boolean },
    ): Promise<void>;
    /** The snapshot retry machine (`createSnapshotRetry`). */
    retry: { noteFailed(reason: unknown): void; noteOk(): void };
    /** Chain the write so a flush can await every in-flight log write. */
    track(write: Promise<void>): void;
    warn?(message: string, detail?: unknown): void;
}

/** What one call did (the worker ignores it; the tests read it). */
export type SnapshotOutcome =
    | 'skipped'
    | 'engine-failed'
    | 'dispatch-threw'
    | 'persisting';

/** Which package a write carries, given what the engine shipped and what
 *  the worker knows is stored (issue #314). Pure. */
export function planSnapshotPackage(
    state: Pick<SnapshotState, 'committedPackageHash' | 'pendingPackage'>,
    reply: Pick<SnapshotReply, 'package_hash' | 'package'>,
): { pkg: SnapshotPackage | undefined; pendingPackage: SnapshotState['pendingPackage'] } {
    const hash = reply.package_hash;
    if (hash === undefined) return { pkg: undefined, pendingPackage: state.pendingPackage };
    if (reply.package) {
        return {
            pkg: { hash, bytes: reply.package },
            pendingPackage: { hash, bytes: reply.package },
        };
    }
    if (hash !== state.committedPackageHash && state.pendingPackage?.hash === hash) {
        /* The engine skipped the bytes because an earlier snapshot shipped
           them, but that write has not committed: carry them, so this row
           cannot outlive a failure of that one. */
        return {
            pkg: { hash, bytes: state.pendingPackage.bytes },
            pendingPackage: state.pendingPackage,
        };
    }
    /* Committed: `persistSnapshot` verifies the store still holds it and
       aborts the write otherwise. */
    return { pkg: { hash }, pendingPackage: state.pendingPackage };
}

/** Take an engine snapshot at log position `seq` and persist it. Must run
 *  on the serial queue with no command in flight (see the worker). */
export async function takeSnapshotFlow(
    seq: number,
    state: SnapshotState,
    deps: SnapshotFlowDeps,
): Promise<SnapshotOutcome> {
    if (!deps.hasEngine() || seq <= state.lastSnapshotAt) return 'skipped';
    const warn = deps.warn ?? (() => undefined);
    try {
        const known = state.committedPackageHash ?? state.pendingPackage?.hash;
        const evt = await deps.engineSnapshot(seq, known);
        if (evt.type !== 'SNAPSHOT' || evt.bytes === undefined) {
            warn('[worker] engine snapshot failed', evt);
            /* Issue #390 - retried on the same clock as a failed write
               (the replay tail keeps growing until a snapshot lands). */
            deps.retry.noteFailed(
                evt.type === 'ERROR' ? (evt.message ?? 'error') : `unexpected ${evt.type}`,
            );
            return 'engine-failed';
        }
        const previousSnapshotAt = state.lastSnapshotAt;
        state.lastSnapshotAt = seq;
        const hash = evt.package_hash;
        const plan = planSnapshotPackage(state, evt);
        state.pendingPackage = plan.pendingPackage;
        /* Issue #268 - the document's first snapshot is its pinned base. */
        const pin = state.pinNextSnapshot;
        state.pinNextSnapshot = false;
        const write = deps.persist(seq, evt.bytes, plan.pkg, { pin }).then(
            () => {
                deps.retry.noteOk();
                if (hash === undefined) return;
                /* Issue #314 - only now is the package known stored. */
                state.committedPackageHash = hash;
                if (state.pendingPackage?.hash === hash) state.pendingPackage = undefined;
            },
            (e: unknown) => {
                warn('[worker] event-log snapshot failed', e);
                /* Issue #333 - nothing was checkpointed at `seq`: let a
                   retry (or the next command) snapshot this position again
                   instead of skipping it as "already taken". */
                if (state.lastSnapshotAt === seq) state.lastSnapshotAt = previousSnapshotAt;
                deps.retry.noteFailed(e);
                /* Issue #314 - the package may not be stored (this write
                   carried it, or the store lost it): forget it, so the next
                   snapshot re-ships the bytes. */
                if (hash !== undefined) {
                    if (state.committedPackageHash === hash) state.committedPackageHash = undefined;
                    if (state.pendingPackage?.hash === hash) state.pendingPackage = undefined;
                }
                /* Nor the pin: pin the next one instead. */
                if (pin) state.pinNextSnapshot = true;
            },
        );
        deps.track(write);
        return 'persisting';
    } catch (e: unknown) {
        warn('[worker] snapshot dispatch failed', e);
        deps.retry.noteFailed(e);
        return 'dispatch-threw';
    }
}
