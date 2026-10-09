/* Issue #332 - unit tests for the IndexedDB event log, run against
 * `fake-indexeddb` (no browser, no worker). Covers what the Playwright
 * `event-log-replay` spec used to prove through mocked rows: the #268
 * recovery-candidate shape (order, tail completeness, pinned base,
 * package attachment), pruning (#241) and package GC (#212), the #314
 * write-confirm contract, the #388 clean marker + archive, and the #390
 * journal-gap record. */
import { IDBFactory, IDBKeyRange } from 'fake-indexeddb';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { Command } from '../../../crates/engine-wasm/pkg/engine_wasm.js';

type Log = typeof import('./event-log');

/** A pristine database AND a pristine module (the connection is cached at
 *  module scope). */
async function freshLog(): Promise<Log> {
    vi.stubGlobal('indexedDB', new IDBFactory());
    vi.stubGlobal('IDBKeyRange', IDBKeyRange);
    vi.resetModules();
    return import('./event-log');
}

const bytes = (...n: number[]): Uint8Array => new Uint8Array(n);
const insert = (text: string): Command => ({ type: 'INSERT_TEXT', text }) as Command;

let log: Log;
beforeEach(async () => {
    log = await freshLog();
    await log.openEventLog('doc-1');
});

describe('recovery candidates (#268 / #241)', () => {
    it('an empty log yields only the snapshot-less base', async () => {
        const r = await log.loadRecoveryLog();
        expect(r.candidates).toEqual([{ seq: 0, snapshot: new Uint8Array(0), tailComplete: true }]);
        expect(r.commands).toEqual([]);
        expect(r.lastSeq).toBe(0);
        expect(r.logComplete).toBe(true);
        expect(r.journalGapSeqs).toEqual([]);
    });

    it('lists snapshots newest first, then the base; lastSeq spans commands and snapshots', async () => {
        for (let s = 1; s <= 5; s++) await log.appendCommand(s, insert(`c${s}`));
        await log.persistSnapshot(2, bytes(2));
        await log.persistSnapshot(4, bytes(4));
        const r = await log.loadRecoveryLog();
        expect(r.candidates.map((c) => c.seq)).toEqual([4, 2, 0]);
        expect(r.candidates.every((c) => c.tailComplete)).toBe(true);
        expect(r.commands.map((c) => c.seq)).toEqual([1, 2, 3, 4, 5]);
        expect(r.lastSeq).toBe(5);
        expect(r.logComplete).toBe(true);
        expect(r.candidates[0]?.takenAt).toBeTypeOf('number');
    });

    it('lastSeq never restarts below a snapshot taken past the last command row', async () => {
        await log.appendCommand(1, insert('a'));
        await log.persistSnapshot(9, bytes(9));
        expect((await log.loadRecoveryLog()).lastSeq).toBe(9);
    });

    it('keeps only the newest 3 snapshots and prunes commands strictly before the oldest retained (#241)', async () => {
        for (let s = 1; s <= 8; s++) await log.appendCommand(s, insert(`c${s}`));
        for (const s of [1, 2, 3, 4]) await log.persistSnapshot(s, bytes(s));
        const r = await log.loadRecoveryLog();
        expect(r.candidates.map((c) => c.seq)).toEqual([4, 3, 2, 0]);
        // snapshot 1 pruned -> commands <= 1 gone; every retained snapshot keeps its tail
        expect(r.commands.map((c) => c.seq)).toEqual([2, 3, 4, 5, 6, 7, 8]);
        expect(r.logComplete).toBe(false);
        // the snapshot-less base can no longer rebuild the document
        expect(r.candidates.at(-1)).toMatchObject({ seq: 0, tailComplete: false });
        expect(r.candidates.slice(0, 3).every((c) => c.tailComplete === true)).toBe(true);
    });

    it('never prunes commands while no snapshot exists', async () => {
        for (let s = 1; s <= 50; s++) await log.appendCommand(s, insert('x'));
        const r = await log.loadRecoveryLog();
        expect(r.commands).toHaveLength(50);
        expect(r.logComplete).toBe(true);
    });

    it('the pinned base survives pruning, is flagged, and loses its tail (#268)', async () => {
        for (let s = 1; s <= 10; s++) await log.appendCommand(s, insert(`c${s}`));
        await log.persistSnapshot(1, bytes(1), undefined, { pin: true });
        for (const s of [3, 5, 7, 9]) await log.persistSnapshot(s, bytes(s));
        const r = await log.loadRecoveryLog();
        // pinned (1) + the 3 newest ordinary ones
        expect(r.candidates.map((c) => c.seq)).toEqual([9, 7, 5, 1, 0]);
        const pinned = r.candidates.find((c) => c.seq === 1);
        expect(pinned?.pinned).toBe(true);
        // pruning passed the pinned base: it restores ALONE
        expect(pinned?.tailComplete).toBe(false);
        expect(r.candidates.filter((c) => c.pinned)).toHaveLength(1);
        expect(r.candidates.find((c) => c.seq === 5)?.tailComplete).toBe(true);
    });

    it('re-pinning hands the old pinned row back to ordinary pruning', async () => {
        await log.persistSnapshot(1, bytes(1), undefined, { pin: true });
        await log.persistSnapshot(2, bytes(2), undefined, { pin: true });
        for (const s of [3, 4, 5, 6]) await log.persistSnapshot(s, bytes(s));
        const r = await log.loadRecoveryLog();
        expect(r.candidates.map((c) => c.seq)).toEqual([6, 5, 4, 2, 0]);
        expect(r.candidates.find((c) => c.seq === 2)?.pinned).toBe(true);
    });

    it('openEventLog clears a previous session entirely, pin and journal gap included', async () => {
        await log.appendCommand(1, insert('a'));
        await log.persistSnapshot(1, bytes(1), { hash: 'h', bytes: bytes(7) }, { pin: true });
        await log.writeJournalGap([2, 3]);
        await log.openEventLog('doc-2');
        const r = await log.loadRecoveryLog();
        expect(r.candidates).toHaveLength(1);
        expect(r.commands).toEqual([]);
        expect(r.journalGapSeqs).toEqual([]);
    });
});

describe('detached source package (#212 / #314)', () => {
    it('attaches package bytes to the candidate that names them', async () => {
        await log.persistSnapshot(1, bytes(1), { hash: 'sha256-a', bytes: bytes(9, 9) });
        const [c] = (await log.loadRecoveryLog()).candidates;
        expect(c?.packageHash).toBe('sha256-a');
        expect(c?.package).toEqual(bytes(9, 9));
    });

    it('a later snapshot may name the stored package without re-shipping its bytes', async () => {
        await log.persistSnapshot(1, bytes(1), { hash: 'sha256-a', bytes: bytes(9) });
        await log.persistSnapshot(2, bytes(2), { hash: 'sha256-a' });
        const r = await log.loadRecoveryLog();
        expect(r.candidates.map((c) => c.package?.[0])).toEqual([9, 9, undefined]);
    });

    it('does not rewrite a package the store already holds', async () => {
        await log.persistSnapshot(1, bytes(1), { hash: 'sha256-a', bytes: bytes(1) });
        // different bytes under the same content key: the stored ones win
        await log.persistSnapshot(2, bytes(2), { hash: 'sha256-a', bytes: bytes(2) });
        const [c] = (await log.loadRecoveryLog()).candidates;
        expect(c?.package).toEqual(bytes(1));
    });

    it('#314: a bytes-less package reference that the store lacks aborts and writes NOTHING', async () => {
        await log.appendCommand(1, insert('a'));
        const err = await log
            .persistSnapshot(1, bytes(1), { hash: 'sha256-missing' })
            .catch((e: unknown) => e);
        expect(err).toBeInstanceOf(log.PackageMissingError);
        expect((err as InstanceType<Log['PackageMissingError']>).hash).toBe('sha256-missing');
        const r = await log.loadRecoveryLog();
        expect(r.candidates.map((c) => c.seq)).toEqual([0]);
        expect(r.commands.map((c) => c.seq)).toEqual([1]);
    });

    it('#314: after PackageMissingError, re-sending the bytes lands the row with its package', async () => {
        await expect(
            log.persistSnapshot(1, bytes(1), { hash: 'sha256-a' }),
        ).rejects.toBeInstanceOf(log.PackageMissingError);
        await log.persistSnapshot(1, bytes(1), { hash: 'sha256-a', bytes: bytes(5) });
        const [c] = (await log.loadRecoveryLog()).candidates;
        expect(c).toMatchObject({ seq: 1, packageHash: 'sha256-a', package: bytes(5) });
    });

    it('#314: a missing-package abort does not disturb earlier snapshots or pruning bookkeeping', async () => {
        for (const s of [1, 2, 3]) await log.persistSnapshot(s, bytes(s));
        await expect(log.persistSnapshot(4, bytes(4), { hash: 'nope' })).rejects.toBeTruthy();
        const r = await log.loadRecoveryLog();
        expect(r.candidates.map((c) => c.seq)).toEqual([3, 2, 1, 0]);
        expect(r.logComplete).toBe(true);
    });

    it('GC: a package no retained snapshot names is dropped with the pruning', async () => {
        await log.persistSnapshot(1, bytes(1), { hash: 'old', bytes: bytes(1) });
        await log.persistSnapshot(2, bytes(2), { hash: 'new', bytes: bytes(2) });
        for (const s of [3, 4, 5]) await log.persistSnapshot(s, bytes(s), { hash: 'new' });
        const r = await log.loadRecoveryLog();
        expect(r.candidates.map((c) => c.seq)).toEqual([5, 4, 3, 0]);
        expect(r.candidates[0]?.package).toEqual(bytes(2));
        // 'old' is unreferenced after the pruning -> collected: naming it
        // without bytes is now a missing-package abort
        await expect(
            log.persistSnapshot(6, bytes(6), { hash: 'old' }),
        ).rejects.toBeInstanceOf(log.PackageMissingError);
    });

    it('GC keeps a package a pinned snapshot still names', async () => {
        await log.persistSnapshot(1, bytes(1), { hash: 'base', bytes: bytes(1) }, { pin: true });
        for (const s of [2, 3, 4, 5]) await log.persistSnapshot(s, bytes(s));
        const r = await log.loadRecoveryLog();
        const pinned = r.candidates.find((c) => c.pinned);
        expect(pinned?.package).toEqual(bytes(1));
    });

    it('a snapshot whose package row is gone reports the hash without bytes', async () => {
        // simulate pre-#314 loss: delete the packages row behind the log's back
        await log.persistSnapshot(1, bytes(1), { hash: 'h', bytes: bytes(1) });
        await new Promise<void>((resolve, reject) => {
            const open = indexedDB.open('engine-log');
            open.onsuccess = () => {
                const db = open.result;
                const tx = db.transaction('packages', 'readwrite');
                tx.objectStore('packages').delete('h');
                tx.oncomplete = () => {
                    db.close();
                    resolve();
                };
                tx.onerror = () => reject(tx.error);
            };
        });
        const [c] = (await log.loadRecoveryLog()).candidates;
        expect(c?.packageHash).toBe('h');
        expect(c?.package).toBeUndefined();
    });
});

describe('clean marker and archived session (#388)', () => {
    it('a freshly opened log is clean; the marker round-trips', async () => {
        expect(await log.loadCleanMarker()).toBe(true);
        await log.writeCleanMarker(false);
        expect(await log.loadCleanMarker()).toBe(false);
        await log.writeCleanMarker(true);
        expect(await log.loadCleanMarker()).toBe(true);
    });

    it('inspectActiveLog reports marker, content and time without touching the log', async () => {
        let s = await log.inspectActiveLog();
        expect(s).toMatchObject({ clean: true, hasContent: false });
        expect(s.at).toBeTypeOf('number');
        await log.appendCommand(1, insert('a'));
        await log.writeCleanMarker(false);
        s = await log.inspectActiveLog();
        expect(s).toMatchObject({ clean: false, hasContent: true });
        expect((await log.loadRecoveryLog()).commands).toHaveLength(1);
    });

    it('a snapshot alone counts as content', async () => {
        await log.persistSnapshot(1, bytes(1));
        expect((await log.inspectActiveLog()).hasContent).toBe(true);
    });

    it('no marker and no log reads as unknown (nothing to offer)', async () => {
        const fresh = await freshLog();
        expect(await fresh.loadCleanMarker()).toBeUndefined();
        expect(await fresh.inspectActiveLog()).toEqual({
            clean: undefined,
            hasContent: false,
            at: undefined,
        });
        expect(await fresh.loadArchiveInfo()).toBeUndefined();
    });

    async function dirtySession(): Promise<void> {
        await log.appendCommand(1, insert('one'));
        await log.appendCommand(2, insert('two'));
        await log.persistSnapshot(1, bytes(1), { hash: 'pkg', bytes: bytes(4) }, { pin: true });
        await log.writeCleanMarker(false);
    }

    it('archive survives the next openEventLog, which then starts clean and empty', async () => {
        await dirtySession();
        await log.archiveActiveLog();
        await log.openEventLog('doc-2');
        expect(await log.loadCleanMarker()).toBe(true);
        expect((await log.loadRecoveryLog()).commands).toEqual([]);
        const info = await log.loadArchiveInfo();
        expect(info?.commandCount).toBe(2);
        expect(info?.archivedAt).toBeTypeOf('number');
        expect(info?.lastEditAt).toBeTypeOf('number');
    });

    it('restoreArchive swaps the session back (commands, snapshots, package, pin), marks it unclean and consumes the archive', async () => {
        await dirtySession();
        await log.archiveActiveLog();
        await log.openEventLog('doc-2');
        await log.appendCommand(1, insert('other session'));
        expect(await log.restoreArchive()).toBe(true);
        const r = await log.loadRecoveryLog();
        expect(r.commands.map((c) => c.cmd)).toEqual([insert('one'), insert('two')]);
        expect(r.candidates[0]).toMatchObject({ seq: 1, pinned: true, package: bytes(4) });
        expect(await log.loadCleanMarker()).toBe(false);
        expect(await log.loadArchiveInfo()).toBeUndefined();
        expect(await log.restoreArchive()).toBe(false);
    });

    it('discardArchive removes it; a second archive replaces an older one', async () => {
        await dirtySession();
        await log.archiveActiveLog();
        await log.openEventLog('doc-2');
        await log.appendCommand(1, insert('x'));
        await log.archiveActiveLog();
        expect((await log.loadArchiveInfo())?.commandCount).toBe(1);
        await log.discardArchive();
        expect(await log.loadArchiveInfo()).toBeUndefined();
    });

    it('restoreArchive with no archive leaves the active log alone', async () => {
        await log.appendCommand(1, insert('keep'));
        expect(await log.restoreArchive()).toBe(false);
        expect((await log.loadRecoveryLog()).commands).toHaveLength(1);
        expect(await log.loadCleanMarker()).toBe(true);
    });
});

describe('journal gap record (#390)', () => {
    it('is reported by the recovery log and cleared once the rows landed', async () => {
        await log.writeJournalGap([4, 5, 6]);
        expect((await log.loadRecoveryLog()).journalGapSeqs).toEqual([4, 5, 6]);
        await log.clearJournalGap();
        expect((await log.loadRecoveryLog()).journalGapSeqs).toEqual([]);
    });

    it('a rewrite replaces the previous gap', async () => {
        await log.writeJournalGap([1]);
        await log.writeJournalGap([1, 2]);
        expect((await log.loadRecoveryLog()).journalGapSeqs).toEqual([1, 2]);
    });
});

describe('renderer streak (#240)', () => {
    it('round-trips and survives openEventLog until cleared', async () => {
        expect(await log.loadRendererStreak()).toBeUndefined();
        await log.saveRendererStreak({ renderer: 'vello', count: 2, at: 5, live: true, token: 't' });
        await log.openEventLog('doc-2');
        expect(await log.loadRendererStreak()).toEqual({
            renderer: 'vello',
            count: 2,
            at: 5,
            live: true,
            token: 't',
        });
        await log.clearRendererStreak();
        expect(await log.loadRendererStreak()).toBeUndefined();
    });
});
