/* Issue #438 - the worker's recovery DECISIONS (issues #241 / #268 / #390),
 * pulled out of `engine.worker.ts` so they run under vitest: how good a
 * recovery base is (`recoveryRank`), the best it could possibly be before
 * trying it (`recoveryRankCeiling`), the walk over the candidates that picks
 * one (`pickRecoveryBase`, with the engine `RECOVER` attempt injected), and
 * the journal-gap count (`countJournalGap`). The worker keeps the effects:
 * dispatching `RECOVER`, the post-recovery bookkeeping, `SET_RENDER_DATE`. */
import type { LoggedCommand, RecoveryCandidate } from './event-log';

/** The slice of an `Event` a recovery attempt's outcome is judged by. */
export interface RecoverAttempt {
    type: string;
    snapshot_restored?: boolean;
    package_lost?: boolean;
}

/** Issue #268 - how good a recovery base is, best first:
 *  4 - full tail, package present (or none needed);
 *  3 - full tail, but the snapshot's source package was lost (the session
 *      saves through the minimal writer - sibling parts gone);
 *  2 - a pinned base whose tail pruning has passed (restored alone: edits
 *      after it are lost), package present;
 *  1 - the same, package lost;
 *  0 - nothing restored and the log is truncated (the document is lost);
 *  -1 - the snapshot did not restore at all. */
export function recoveryRank(candidate: RecoveryCandidate, evt: RecoverAttempt): number {
    if (evt.type !== 'RECOVERED') return -1;
    const isLogBase = candidate.snapshot.length === 0;
    if (!evt.snapshot_restored && !isLogBase) return -1;
    const tailComplete = candidate.tailComplete !== false;
    if (isLogBase) return tailComplete ? 4 : 0;
    return (tailComplete ? 3 : 1) + (evt.package_lost ? 0 : 1);
}

/** Issue #268 - the best rank a candidate could reach, before trying it:
 *  a package the store no longer holds is predictably lost. */
export function recoveryRankCeiling(candidate: RecoveryCandidate): number {
    const tailComplete = candidate.tailComplete !== false;
    if (candidate.snapshot.length === 0) return tailComplete ? 4 : 0;
    const packageMissing = candidate.packageHash !== undefined && !candidate.package;
    return (tailComplete ? 3 : 1) + (packageMissing ? 0 : 1);
}

export interface RecoveryChoice<E extends RecoverAttempt> {
    /** The winning attempt's reply (re-dispatched when a later, worse
     *  attempt left the engine in its state). */
    evt: E;
    base: RecoveryCandidate;
    /** Candidates that did not restore at all. */
    snapshotFallbacks: number;
    /** The pinned base was among the ones that did not restore. */
    pinnedFailed: boolean;
    /** Readable snapshots passed over for an older base with its package. */
    packageFallbacks: number;
    /** Candidates `attempt` was actually called for, in order (+ the final
     *  re-dispatch of the winner, when one was needed). */
    attempted: number[];
}

/** Walk `candidates` (newest snapshot first, ending with the snapshot-less
 *  log base), SCORE each by [`recoveryRank`] and keep the best; stop at a
 *  perfect 4; skip a candidate whose ceiling cannot beat the best so far.
 *  Every `attempt` starts the engine from a reset, so when the last attempt
 *  was not the winner the winner is attempted once more. */
export async function pickRecoveryBase<E extends RecoverAttempt>(
    candidates: readonly RecoveryCandidate[],
    attempt: (candidate: RecoveryCandidate) => Promise<E>,
    warn: (message: string) => void = () => undefined,
): Promise<RecoveryChoice<E>> {
    let evt: E | undefined;
    let base: RecoveryCandidate | undefined;
    let bestRank = -2;
    let current: RecoveryCandidate | undefined;
    let snapshotFallbacks = 0;
    let pinnedFailed = false;
    const packageLostAttempts: RecoveryCandidate[] = [];
    const attempted: number[] = [];
    for (const candidate of candidates) {
        if (bestRank >= 4) break;
        if (recoveryRankCeiling(candidate) <= bestRank) continue;
        const result = await attempt(candidate);
        attempted.push(candidate.seq);
        current = candidate;
        const rank = recoveryRank(candidate, result);
        if (rank < 0) {
            snapshotFallbacks += 1;
            if (candidate.pinned) pinnedFailed = true;
            warn(
                `[worker] recovery: snapshot @${candidate.seq} did not restore; ` +
                    'falling back to the next older base',
            );
        } else if (result.type === 'RECOVERED' && result.package_lost) {
            packageLostAttempts.push(candidate);
            warn(
                `[worker] recovery: snapshot @${candidate.seq} restores without its ` +
                    'source package; looking for an older base that has it',
            );
        }
        if (rank > bestRank) {
            bestRank = rank;
            evt = result;
            base = candidate;
        }
    }
    if (!evt || !base) throw new Error('recovery: no base to recover from');
    if (current !== base) {
        /* A later, worse attempt holds the engine: restore the best. */
        evt = await attempt(base);
        attempted.push(base.seq);
    }
    const chosen = base;
    const packageFallbacks =
        evt.type === 'RECOVERED' && !evt.package_lost
            ? packageLostAttempts.filter((c) => c !== chosen).length
            : 0;
    return { evt, base, snapshotFallbacks, pinnedFailed, packageFallbacks, attempted };
}

/** Issue #390 - how many logged commands after `base` could not be
 *  replayed because their row was never written: seqs the best-effort
 *  `journal-gap` record names that are still absent, plus holes inside the
 *  retained tail. A base restored WITHOUT its pruned tail replays nothing,
 *  so it has no journal gap to report (its loss is `tailDropped`). */
export function countJournalGap(
    base: Pick<RecoveryCandidate, 'seq' | 'tailComplete'>,
    commands: readonly Pick<LoggedCommand, 'seq'>[],
    journalGapSeqs: readonly number[] | undefined,
): number {
    if (base.tailComplete === false) return 0;
    const present = new Set(commands.map((c) => c.seq));
    const missing = new Set<number>();
    for (const seq of journalGapSeqs ?? []) {
        if (seq > base.seq && !present.has(seq)) missing.add(seq);
    }
    let prev = base.seq;
    for (const c of commands) {
        if (c.seq <= base.seq) continue;
        /* Bounded: a hole this wide is a different failure (pruning). */
        for (let s = prev + 1; s < c.seq && s - prev <= 10_000; s++) missing.add(s);
        prev = c.seq;
    }
    return missing.size;
}
