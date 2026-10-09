/* Issue #332 - `recoveryNotices()` and friends (#315 / #333 / #388 / #390)
 * for every `RecoveryReport` shape the engine client can produce. Pure
 * functions: no engine, no DOM. */
import { describe, expect, it } from 'vitest';
import {
    checkpointNotices,
    previousSessionNotice,
    recoveryDegraded,
    recoveryNotices,
} from './recovery';
import type { PreviousSessionInfo, RecoveryReport } from './types';

/** A recovery that lost nothing. */
const clean: RecoveryReport = {
    restored: true,
    appliedCommands: 0,
    renderer: 'canvas2d',
    rendererDowngrade: undefined,
    rendererDowngraded: false,
    snapshotFallbacks: 0,
    logTruncated: false,
    packageFallbacks: 0,
    packageLost: false,
    pinnedBase: false,
    tailDropped: false,
    baseSnapshotAt: undefined,
};
const report = (o: Partial<RecoveryReport>): RecoveryReport => ({ ...clean, ...o });
const kinds = (r: RecoveryReport | undefined): string[] =>
    recoveryNotices(r, { formatTime: () => 'T' }).map((n) => n.kind);
const fmt = { formatTime: (ms: number): string => `t${ms}` };

describe('recoveryNotices (#315)', () => {
    it('is silent for no recovery and for a recovery that lost nothing', () => {
        expect(recoveryNotices(undefined)).toEqual([]);
        expect(recoveryNotices(clean)).toEqual([]);
        expect(recoveryDegraded(undefined)).toBe(false);
        expect(recoveryDegraded(clean)).toBe(false);
    });

    it('stays silent when only the harmless counters moved', () => {
        // a fallback to an older snapshot that replayed its full tail, a
        // package fallback that found its package, a pinned base WITH its tail
        const r = report({
            snapshotFallbacks: 2,
            packageFallbacks: 1,
            pinnedBase: true,
            appliedCommands: 40,
            baseSnapshotAt: 1,
            cause: 'engine-reload',
            journalGap: 0,
        });
        expect(recoveryNotices(r)).toEqual([]);
        expect(recoveryDegraded(r)).toBe(false);
    });

    it('log-truncated: the document is gone', () => {
        const [n, ...rest] = recoveryNotices(report({ restored: false, logTruncated: true }));
        expect(rest).toEqual([]);
        expect(n).toMatchObject({ kind: 'log-truncated' });
        expect(n?.title).toMatch(/could not be recovered/);
        expect(n?.action).toMatch(/last saved/);
    });

    it('tail-dropped: names the snapshot time when known, hedges when not', () => {
        const withTime = recoveryNotices(report({ tailDropped: true, baseSnapshotAt: 1234 }), fmt);
        expect(withTime).toHaveLength(1);
        expect(withTime[0]?.kind).toBe('tail-dropped');
        expect(withTime[0]?.title).toBe('Recovered an earlier version of your document');
        expect(withTime[0]?.detail).toContain('changes made after that were lost');
        expect(withTime[0]?.detail).toContain('as it was at t1234');
        const noTime = recoveryNotices(report({ tailDropped: true }), fmt);
        expect(noTime[0]?.detail).toContain('as it was at an earlier recovery point');
    });

    it('package-lost: points at Save As', () => {
        const [n] = recoveryNotices(report({ packageLost: true }));
        expect(n).toMatchObject({ kind: 'package-lost' });
        expect(n?.title).toBe('Parts of the original file could not be restored');
        expect(n?.action).toMatch(/Save As/);
    });

    it('journal-gap: only for a positive count, pluralised', () => {
        expect(kinds(report({ journalGap: 0 }))).toEqual([]);
        const one = recoveryNotices(report({ journalGap: 1 }));
        expect(one[0]?.kind).toBe('journal-gap');
        expect(one[0]?.detail).toContain('1 recent change ');
        const many = recoveryNotices(report({ journalGap: 3 }));
        expect(many[0]?.detail).toContain('3 recent changes ');
    });

    it('renderer-downgrade needs BOTH the flag and the descriptor', () => {
        const downgrade = {
            from: 'vello',
            to: 'canvas2d',
            reason: 'CRASH_LOOP' as const,
            consecutive_traps: 2,
        };
        // a later recovery re-sends the downgrade but did not trip it
        expect(kinds(report({ rendererDowngrade: downgrade }))).toEqual([]);
        expect(kinds(report({ rendererDowngraded: true }))).toEqual([]);
        const [n] = recoveryNotices(report({ rendererDowngraded: true, rendererDowngrade: downgrade }));
        expect(n?.kind).toBe('renderer-downgrade');
        expect(n?.detail).toContain('vello renderer crashed 2 times');
        expect(n?.detail).toContain('draws with canvas2d');
        expect(n?.detail).toContain('Your document is intact');
    });

    it('orders by severity when several apply, and every notice is complete', () => {
        const all = recoveryNotices(
            report({
                logTruncated: true,
                tailDropped: true,
                packageLost: true,
                journalGap: 2,
                rendererDowngraded: true,
                rendererDowngrade: {
                    from: 'vello',
                    to: 'canvas2d',
                    reason: 'CRASH_LOOP',
                    consecutive_traps: 3,
                },
            }),
            fmt,
        );
        expect(all.map((n) => n.kind)).toEqual([
            'log-truncated',
            'tail-dropped',
            'package-lost',
            'journal-gap',
            'renderer-downgrade',
        ]);
        for (const n of all) {
            expect(n.title).not.toBe('');
            expect(n.detail).not.toBe('');
            expect(n.action).not.toBe('');
        }
        expect(recoveryDegraded(report({ logTruncated: true }))).toBe(true);
    });

    it.each([
        ['logTruncated', { logTruncated: true }],
        ['tailDropped', { tailDropped: true }],
        ['packageLost', { packageLost: true }],
        ['journalGap', { journalGap: 1 }],
    ] as const)('%s alone yields exactly one notice and counts as degraded', (_name, o) => {
        expect(recoveryNotices(report(o))).toHaveLength(1);
        expect(recoveryDegraded(report(o))).toBe(true);
    });

    it('uses a locale time by default without throwing', () => {
        const [n] = recoveryNotices(report({ tailDropped: true, baseSnapshotAt: Date.UTC(2026, 0, 1) }));
        expect(n?.detail).toMatch(/as it was at \S+/);
    });
});

describe('checkpointNotices (#333 / #390)', () => {
    it('is silent while checkpoints and the journal are fine', () => {
        expect(checkpointNotices(false)).toEqual([]);
        expect(checkpointNotices(false, false)).toEqual([]);
    });

    it('checkpoint-failing', () => {
        const [n, ...rest] = checkpointNotices(true);
        expect(rest).toEqual([]);
        expect(n?.kind).toBe('checkpoint-failing');
        expect(n?.title).toBe('Changes are not being checkpointed');
        expect(n?.action).toMatch(/Save/);
    });

    it('a failing journal is the worse case and wins over a failing checkpoint', () => {
        for (const checkpointFailing of [true, false]) {
            const ns = checkpointNotices(checkpointFailing, true);
            expect(ns.map((n) => n.kind)).toEqual(['journal-failing']);
        }
    });
});

describe('previousSessionNotice (#388)', () => {
    const info = (o: Partial<PreviousSessionInfo>): PreviousSessionInfo => ({
        id: 'a-1',
        archivedAt: 100,
        lastEditAt: 50,
        commandCount: 3,
        ...o,
    });

    it('is silent without a waiting session', () => {
        expect(previousSessionNotice(undefined)).toEqual([]);
    });

    it('offers recovery, citing the last edit time', () => {
        const [n] = previousSessionNotice(info({}), fmt);
        expect(n?.kind).toBe('previous-session');
        expect(n?.detail).toContain('last edit around t50');
    });

    it('falls back to the archive time when the last edit is unknown', () => {
        const [n] = previousSessionNotice(info({ lastEditAt: undefined }), fmt);
        expect(n?.detail).toContain('last edit around t100');
    });
});
