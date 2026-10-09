/* Issue #438 - the #268 candidate ranking and the #390 journal-gap count. */
import { describe, expect, it } from 'vitest';
import {
    countJournalGap,
    pickRecoveryBase,
    recoveryRank,
    recoveryRankCeiling,
    type RecoverAttempt,
} from './recovery-plan';
import type { RecoveryCandidate } from './event-log';

const snap = (seq: number, extra: Partial<RecoveryCandidate> = {}): RecoveryCandidate => ({
    seq,
    snapshot: new Uint8Array([1, 2, 3]),
    ...extra,
});
/** The snapshot-less log base (seq 0, empty bytes). */
const logBase = (extra: Partial<RecoveryCandidate> = {}): RecoveryCandidate => ({
    seq: 0,
    snapshot: new Uint8Array(),
    ...extra,
});
const restored = (extra: Partial<RecoverAttempt> = {}): RecoverAttempt => ({
    type: 'RECOVERED',
    snapshot_restored: true,
    package_lost: false,
    ...extra,
});

describe('recoveryRank (#268)', () => {
    it('ranks a full-tail snapshot with its package 4, without it 3', () => {
        expect(recoveryRank(snap(5), restored())).toBe(4);
        expect(recoveryRank(snap(5), restored({ package_lost: true }))).toBe(3);
    });

    it('ranks a pinned base restored without its tail 2 (package) or 1 (lost)', () => {
        const pinned = snap(1, { pinned: true, tailComplete: false });
        expect(recoveryRank(pinned, restored())).toBe(2);
        expect(recoveryRank(pinned, restored({ package_lost: true }))).toBe(1);
    });

    it('ranks the complete bare log 4 and a truncated one 0', () => {
        const evt = restored({ snapshot_restored: false });
        expect(recoveryRank(logBase(), evt)).toBe(4);
        expect(recoveryRank(logBase({ tailComplete: false }), evt)).toBe(0);
    });

    it('ranks a snapshot that did not restore, or a non-RECOVERED reply, -1', () => {
        expect(recoveryRank(snap(5), restored({ snapshot_restored: false }))).toBe(-1);
        expect(recoveryRank(snap(5), { type: 'ERROR' })).toBe(-1);
        expect(recoveryRank(logBase(), { type: 'ERROR' })).toBe(-1);
    });

    it('orders: full tail > package-lost full tail > pinned alone > pinned lost > truncated log', () => {
        const order = [
            recoveryRank(snap(9), restored()),
            recoveryRank(snap(9), restored({ package_lost: true })),
            recoveryRank(snap(1, { tailComplete: false }), restored()),
            recoveryRank(snap(1, { tailComplete: false }), restored({ package_lost: true })),
            recoveryRank(logBase({ tailComplete: false }), restored({ snapshot_restored: false })),
        ];
        expect(order).toEqual([4, 3, 2, 1, 0]);
    });
});

describe('recoveryRankCeiling (#268)', () => {
    it('a package the store no longer holds is predictably lost', () => {
        expect(recoveryRankCeiling(snap(5))).toBe(4);
        expect(recoveryRankCeiling(snap(5, { packageHash: 'h' }))).toBe(3);
        expect(recoveryRankCeiling(snap(5, { packageHash: 'h', package: new Uint8Array(1) }))).toBe(4);
        expect(recoveryRankCeiling(snap(5, { packageHash: 'h', tailComplete: false }))).toBe(1);
        expect(recoveryRankCeiling(snap(5, { tailComplete: false }))).toBe(2);
        expect(recoveryRankCeiling(logBase())).toBe(4);
        expect(recoveryRankCeiling(logBase({ tailComplete: false }))).toBe(0);
    });
});

describe('pickRecoveryBase (#241 / #268)', () => {
    /** Script the engine: a reply per candidate seq. */
    const script =
        (replies: Record<number, RecoverAttempt>, calls: number[] = []) =>
        async (c: RecoveryCandidate): Promise<RecoverAttempt> => {
            calls.push(c.seq);
            const r = replies[c.seq];
            if (!r) throw new Error(`no scripted reply for ${c.seq}`);
            return r;
        };

    it('takes the newest snapshot when it restores perfectly, and tries nothing else', async () => {
        const calls: number[] = [];
        const choice = await pickRecoveryBase(
            [snap(30), snap(20), logBase()],
            script({ 30: restored() }, calls),
        );
        expect(choice.base.seq).toBe(30);
        expect(choice.attempted).toEqual([30]);
        expect(calls).toEqual([30]);
        expect(choice.snapshotFallbacks).toBe(0);
    });

    it('falls back to an older snapshot when the newest does not restore', async () => {
        const warnings: string[] = [];
        const choice = await pickRecoveryBase(
            [snap(30), snap(20), logBase()],
            script({ 30: restored({ snapshot_restored: false }), 20: restored() }),
            (m) => warnings.push(m),
        );
        expect(choice.base.seq).toBe(20);
        expect(choice.snapshotFallbacks).toBe(1);
        expect(choice.pinnedFailed).toBe(false);
        expect(warnings).toHaveLength(1);
        expect(warnings[0]).toContain('@30');
    });

    it('reports a pinned base that failed to restore', async () => {
        const choice = await pickRecoveryBase(
            [snap(30, { pinned: true }), logBase()],
            script({ 30: { type: 'ERROR' }, 0: restored({ snapshot_restored: false }) }),
        );
        expect(choice.pinnedFailed).toBe(true);
        expect(choice.base.seq).toBe(0);
    });

    it('prefers an older base that still has its package over a newer one that lost it', async () => {
        const choice = await pickRecoveryBase(
            [snap(30, { packageHash: 'h' }), snap(20, { packageHash: 'h', package: new Uint8Array(1) })],
            script({ 30: restored({ package_lost: true }), 20: restored() }),
        );
        expect(choice.base.seq).toBe(20);
        expect(choice.evt.package_lost).toBe(false);
        expect(choice.packageFallbacks).toBe(1);
    });

    it('re-dispatches the winner when a later, worse attempt left the engine in its state', async () => {
        const calls: number[] = [];
        /* 30 restores without a package (rank 3, ceiling 4 so it IS tried);
           20 is tried next (ceiling 4 > 3) and loses (rank 3 is not > 3). */
        const choice = await pickRecoveryBase(
            [snap(30), snap(20)],
            script({ 30: restored({ package_lost: true }), 20: restored({ package_lost: true }) }, calls),
        );
        expect(choice.base.seq).toBe(30);
        expect(calls).toEqual([30, 20, 30]);
        expect(choice.attempted).toEqual([30, 20, 30]);
        /* Neither found a package: nothing was passed over for one. */
        expect(choice.packageFallbacks).toBe(0);
    });

    it('skips a candidate whose ceiling cannot beat the best so far', async () => {
        const calls: number[] = [];
        /* After 30 (rank 3), an older base whose package is missing has
           ceiling 3: not tried at all. */
        const choice = await pickRecoveryBase(
            [snap(30), snap(20, { packageHash: 'h' }), snap(10, { packageHash: 'h' })],
            script({ 30: restored({ package_lost: true }) }, calls),
        );
        expect(calls).toEqual([30]);
        expect(choice.base.seq).toBe(30);
    });

    it('a truncated bare log is the last resort (rank 0) behind any restored snapshot', async () => {
        const choice = await pickRecoveryBase(
            [snap(5, { tailComplete: false, packageHash: 'h' }), logBase({ tailComplete: false })],
            script({ 5: restored({ package_lost: true }), 0: restored({ snapshot_restored: false }) }),
        );
        expect(choice.base.seq).toBe(5);
    });

    it('throws when there is nothing to try', async () => {
        await expect(pickRecoveryBase([], script({}))).rejects.toThrow('no base to recover from');
    });

    it('hands back the (failed) last attempt when nothing restored: the caller reports it', async () => {
        const choice = await pickRecoveryBase(
            [snap(5)],
            script({ 5: { type: 'ERROR' } }),
        );
        expect(choice.evt.type).toBe('ERROR');
        expect(choice.snapshotFallbacks).toBe(1);
    });
});

describe('countJournalGap (#390)', () => {
    const rows = (...seqs: number[]) => seqs.map((seq) => ({ seq }));

    it('is zero for a gapless tail with no recorded gap', () => {
        expect(countJournalGap({ seq: 10 }, rows(11, 12, 13), [])).toBe(0);
        expect(countJournalGap({ seq: 10 }, rows(11, 12, 13), undefined)).toBe(0);
    });

    it('counts recorded seqs after the base that are still absent', () => {
        expect(countJournalGap({ seq: 10 }, rows(11, 13), [12])).toBe(1);
    });

    it('does not count a recorded seq that landed later, or one at/before the base', () => {
        expect(countJournalGap({ seq: 10 }, rows(11, 12), [12, 5, 10])).toBe(0);
    });

    it('counts holes inside the retained tail without a record', () => {
        expect(countJournalGap({ seq: 10 }, rows(11, 15), undefined)).toBe(3);
    });

    it('does not double count a hole that is also recorded', () => {
        expect(countJournalGap({ seq: 10 }, rows(11, 14), [12, 13])).toBe(2);
    });

    it('ignores rows at or before the base', () => {
        expect(countJournalGap({ seq: 10 }, rows(3, 4, 11), undefined)).toBe(0);
    });

    it('a base restored without its pruned tail has no journal gap to report', () => {
        expect(countJournalGap({ seq: 10, tailComplete: false }, rows(20), [15])).toBe(0);
    });

    it('bounds a hole this wide (pruning, not a journal failure)', () => {
        const n = countJournalGap({ seq: 0 }, rows(1, 50_000), undefined);
        expect(n).toBe(10_000);
    });
});
