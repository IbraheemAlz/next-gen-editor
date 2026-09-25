import { test, expect } from '@playwright/test';

/* D2.6 exit gate: event-log replay + sequence continuity. Flood enough
   commands to trigger a snapshot (SNAPSHOT_EVERY = 200), force a crash,
   recover, and verify the recovered worker resumes its log sequence past
   the pre-crash maximum — never restarting at 0. */
test('event log replays and sequence continuity survives a crash', async ({ page }) => {
    test.setTimeout(60_000);
    await page.goto('/');
    await page.waitForFunction(() => (window as any).__paintIdle === true, undefined, {
        timeout: 15_000,
    });

    const result = await page.evaluate(async () => {
        const dispatch = (window as any).__dispatch;
        const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

        const readLog = (): Promise<{ cmd: number[]; snap: number[] }> =>
            new Promise((resolve, reject) => {
                const open = indexedDB.open('engine-log');
                open.onsuccess = () => {
                    const db = open.result;
                    const tx = db.transaction(['commands', 'snapshots'], 'readonly');
                    const cmd = tx.objectStore('commands').getAllKeys();
                    const snap = tx.objectStore('snapshots').getAllKeys();
                    tx.oncomplete = () => {
                        db.close();
                        resolve({ cmd: cmd.result as number[], snap: snap.result as number[] });
                    };
                    tx.onerror = () => reject(tx.error);
                };
                open.onerror = () => reject(open.error);
            });

        const maxOf = (xs: number[]) => xs.reduce((m, x) => Math.max(m, x), 0);
        const waitForLog = async (
            pred: (l: { cmd: number[]; snap: number[] }) => boolean,
            label: string,
        ) => {
            for (let i = 0; i < 120; i++) {
                const l = await readLog();
                if (pred(l)) return l;
                await sleep(50);
            }
            throw new Error(`timeout: ${label}`);
        };

        /* Flood past SNAPSHOT_EVERY (200) to trigger a snapshot. */
        await Promise.all(Array.from({ length: 250 }, () => dispatch({ type: 'PING' })));
        const before = await waitForLog(
            (l) => l.cmd.length >= 250 && l.snap.length >= 1,
            'pre-crash commands + snapshot',
        );
        const preMaxSeq = maxOf(before.cmd);
        /* Issue #85 — an IDLE snapshot (1.5 s after the flood) may land
           before this read and become the newest row, so the cadence
           proof is "a snapshot at SNAPSHOT_EVERY exists", not "the newest
           snapshot is at SNAPSHOT_EVERY". */
        const snapshotSeqs = before.snap;

        /* Force a crash and wait for the UI recovery flow to finish. */
        (window as any).__engineClient.forceTrap();
        for (let i = 0; i < 300 && (window as any).__recovered !== true; i++) {
            await sleep(50);
        }
        const recovered = (window as any).__recovered === true;

        /* One command on the recovered worker, then confirm its seq is past
           the pre-crash maximum — proof the sequence did not restart at 0. */
        await dispatch({ type: 'PING' });
        const after = await waitForLog(
            (l) => maxOf(l.cmd) > preMaxSeq,
            'post-recovery seq beyond pre-crash max',
        );

        return { preMaxSeq, snapshotSeqs, recovered, postMaxSeq: maxOf(after.cmd) };
    });

    console.log(
        `[event-log] preMaxSeq=${result.preMaxSeq} snapshotSeqs=${result.snapshotSeqs.join(',')} ` +
            `postMaxSeq=${result.postMaxSeq}`,
    );

    expect(result.recovered, 'UI completed crash recovery').toBe(true);
    expect(result.snapshotSeqs, 'snapshot persisted at SNAPSHOT_EVERY').toContain(200);
    expect(result.preMaxSeq).toBeGreaterThanOrEqual(250);
    expect(
        result.postMaxSeq,
        'log sequence continued past the pre-crash max',
    ).toBeGreaterThan(result.preMaxSeq);
});

/* The boot seed `setupEngine` paints (see crash-recovery.spec.ts). */
const SEED = 'Hello world مرحبا بالعالم';

/* Issue #241 — a pruned log must never leave recovery without a
   replayable base. Flood past four cadence snapshots so `persistSnapshot`
   prunes (the oldest snapshot AND every command up to it — the boot
   `RENDER_PAGE` included — are gone), then make the NEWEST snapshot
   unreadable. Before #241 recovery only ever tried that one snapshot: it
   failed, the engine replayed the pruned tail onto a fresh document, and
   the shell re-seeded — the document was lost. Now recovery falls back
   to the next older snapshot, whose full tail the pruning invariant
   keeps. With EVERY snapshot unreadable the loss is unavoidable, and
   `RecoveryInfo.logTruncated` reports it instead of passing silently. */
for (const corrupt of ['newest', 'all'] as const) {
    test(`pruned log + ${corrupt} snapshot unreadable: ${
        corrupt === 'newest' ? 'recovery falls back, document survives' : 'loss is reported'
    }`, async ({ page }) => {
        test.setTimeout(90_000);
        await page.goto('/');
        await page.waitForFunction(() => (window as any).__paintIdle === true, undefined, {
            timeout: 15_000,
        });

        const result = await page.evaluate(async (which: 'newest' | 'all') => {
            const w = window as any;
            const dispatch = w.__dispatch as (cmd: unknown) => Promise<any>;
            const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
            const withDb = <T,>(fn: (db: IDBDatabase) => Promise<T>): Promise<T> =>
                new Promise((resolve, reject) => {
                    const open = indexedDB.open('engine-log');
                    open.onsuccess = () => {
                        const db = open.result;
                        fn(db).then(
                            (v) => {
                                db.close();
                                resolve(v);
                            },
                            (e) => {
                                db.close();
                                reject(e);
                            },
                        );
                    };
                    open.onerror = () => reject(open.error);
                });
            const readLog = () =>
                withDb(
                    (db) =>
                        new Promise<{ cmd: number[]; snap: number[]; pruned: number }>(
                            (resolve, reject) => {
                                const tx = db.transaction(
                                    ['commands', 'snapshots', 'meta'],
                                    'readonly',
                                );
                                const cmd = tx.objectStore('commands').getAllKeys();
                                const snap = tx.objectStore('snapshots').getAllKeys();
                                const pruned = tx.objectStore('meta').get('pruned');
                                tx.oncomplete = () =>
                                    resolve({
                                        cmd: cmd.result as number[],
                                        snap: snap.result as number[],
                                        pruned: (pruned.result?.through as number) ?? 0,
                                    });
                                tx.onerror = () => reject(tx.error);
                            },
                        ),
                );
            const documentText = async (): Promise<string> => {
                await dispatch({ type: 'SELECT_ALL' });
                const p = await dispatch({ type: 'GET_SELECTION_AS_CLIPBOARD' });
                return p.type === 'CLIPBOARD_PAYLOAD' ? p.plain : `<${p.type}>`;
            };

            await dispatch({ type: 'INSERT_TEXT', at: undefined, text: 'KEEP-241 ' });
            /* Four cadence snapshots (200/400/600/800): the fourth prunes. */
            for (let i = 0; i < 850; i++) await dispatch({ type: 'PING' });
            /* Let the idle snapshot (1.5 s) land so no write races the
               corruption below. */
            await sleep(2_500);
            let log = await readLog();
            for (let i = 0; i < 100 && (log.pruned === 0 || log.snap.length < 3); i++) {
                await sleep(50);
                log = await readLog();
            }
            const before = log;

            /* Make the chosen snapshot rows unreadable. */
            const victims = which === 'newest' ? [Math.max(...before.snap)] : before.snap;
            await withDb(
                (db) =>
                    new Promise<void>((resolve, reject) => {
                        const tx = db.transaction('snapshots', 'readwrite');
                        for (const seq of victims) {
                            tx.objectStore('snapshots').put({
                                seq,
                                bytes: new Uint8Array([0xde, 0xad, 0xbe, 0xef]),
                            });
                        }
                        tx.oncomplete = () => resolve();
                        tx.onerror = () => reject(tx.error);
                    }),
            );

            await w.__engineClient.armTrap(1);
            await dispatch({ type: 'PING' }).catch(() => undefined);
            for (let i = 0; i < 600 && w.__recovered !== true; i++) await sleep(50);
            if (w.__recovered !== true) return { failed: 'recovery did not complete' };
            return {
                before,
                info: w.__engineClient.lastRecovery,
                text: await documentText(),
            };
        }, corrupt);

        expect((result as any).failed, 'in-page failure').toBeUndefined();
        const r = result as any;
        console.log(
            `[event-log #241] snaps=${r.before.snap.join(',')} ` +
                `firstCmd=${Math.min(...r.before.cmd)} pruned<=${r.before.pruned} ` +
                `fallbacks=${r.info.snapshotFallbacks} restored=${r.info.restored} ` +
                `truncated=${r.info.logTruncated}`,
        );
        /* Pruning really happened: the head of the log (the boot
           RENDER_PAGE) is gone, and so is every snapshot at or before
           the pruning point — only newer ones, each with its full tail,
           remain. */
        expect(r.before.pruned).toBeGreaterThanOrEqual(200);
        expect(Math.min(...r.before.cmd)).toBe(r.before.pruned + 1);
        expect(Math.min(...r.before.snap)).toBeGreaterThan(r.before.pruned);

        if (corrupt === 'newest') {
            expect(r.info.snapshotFallbacks).toBe(1);
            expect(r.info.restored).toBe(true);
            expect(r.info.layoutRestored).toBe(true);
            expect(r.info.logTruncated).toBe(false);
            expect(r.text).toBe(`KEEP-241 ${SEED}`);
        } else {
            expect(r.info.snapshotFallbacks).toBe(r.before.snap.length);
            expect(r.info.restored).toBe(false);
            expect(r.info.logTruncated).toBe(true);
        }
    });
}
