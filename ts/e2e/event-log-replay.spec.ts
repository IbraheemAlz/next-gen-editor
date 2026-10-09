import { test, expect, type Page, type Worker } from '@playwright/test';
import { readFileSync } from 'node:fs';

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
                        new Promise<{
                            cmd: number[];
                            snap: number[];
                            pruned: number;
                            pinned: number | undefined;
                        }>((resolve, reject) => {
                            const tx = db.transaction(['commands', 'snapshots', 'meta'], 'readonly');
                            const cmd = tx.objectStore('commands').getAllKeys();
                            const snap = tx.objectStore('snapshots').getAllKeys();
                            const pruned = tx.objectStore('meta').get('pruned');
                            const pinned = tx.objectStore('meta').get('pinned');
                            tx.oncomplete = () =>
                                resolve({
                                    cmd: cmd.result as number[],
                                    snap: snap.result as number[],
                                    pruned: (pruned.result?.through as number) ?? 0,
                                    pinned: pinned.result?.seq as number | undefined,
                                });
                            tx.onerror = () => reject(tx.error);
                        }),
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
           remain. Issue #268 — except the pinned base, which is never
           pruned (its own tail is). */
        expect(r.before.pruned).toBeGreaterThanOrEqual(200);
        expect(Math.min(...r.before.cmd)).toBe(r.before.pruned + 1);
        const regular = r.before.snap.filter((s: number) => s !== r.before.pinned);
        expect(Math.min(...regular)).toBeGreaterThan(r.before.pruned);
        expect(r.before.snap).toContain(r.before.pinned);

        const banner = page.locator('.nge-recovery-banner');
        if (corrupt === 'newest') {
            expect(r.info.snapshotFallbacks).toBe(1);
            expect(r.info.restored).toBe(true);
            expect(r.info.layoutRestored).toBe(true);
            expect(r.info.logTruncated).toBe(false);
            expect(r.info.pinnedBase).toBe(false);
            expect(r.info.tailDropped).toBe(false);
            expect(r.text).toBe(`KEEP-241 ${SEED}`);
            /* Issue #315 — a fallback that still replayed its full tail
               lost nothing: no banner. */
            await expect(banner).toHaveCount(0);
        } else {
            expect(r.info.snapshotFallbacks).toBe(r.before.snap.length);
            expect(r.info.restored).toBe(false);
            expect(r.info.logTruncated).toBe(true);
            /* Issue #315 — the total loss is visible, with what to do. */
            await expect(banner).toBeVisible();
            await expect(banner).toHaveAttribute('role', 'alert');
            await expect(banner).toHaveAttribute('data-kinds', 'log-truncated');
        }
    });
}

/* Issue #268 — the pinned base. A document's first snapshot is pinned
   (the session's first, and the first after every OPEN_DOCUMENT): never
   pruned, so even when pruning has dropped its tail and EVERY other
   snapshot is unreadable, recovery restores it instead of losing the
   document (the #241 'all' case above corrupts the pinned row too). Its
   tail is gone, so the document comes back as of the pinned snapshot —
   the edit made after it is lost, and `RecoveryInfo.tailDropped` says so.
   The recovered engine then snapshots at the log head straight away, so
   the next recovery does not replay rows it never applied. */
const PIN_FIXTURE = new URL(
    '../../crates/format-docx/tests/fixtures/simple_text.docx',
    import.meta.url,
);
test('pruned log + all but the pinned base unreadable: the pinned base restores', async ({
    page,
}) => {
    test.setTimeout(90_000);
    await page.goto('/');
    await page.waitForFunction(() => (window as any).__paintIdle === true, undefined, {
        timeout: 15_000,
    });

    const result = await page.evaluate(async (b64: string) => {
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
        type Log = { cmd: number[]; snap: number[]; pruned: number; pinned: number | undefined };
        const readLog = () =>
            withDb(
                (db) =>
                    new Promise<Log>((resolve, reject) => {
                        const tx = db.transaction(['commands', 'snapshots', 'meta'], 'readonly');
                        const cmd = tx.objectStore('commands').getAllKeys();
                        const snap = tx.objectStore('snapshots').getAllKeys();
                        const pruned = tx.objectStore('meta').get('pruned');
                        const pinned = tx.objectStore('meta').get('pinned');
                        tx.oncomplete = () =>
                            resolve({
                                cmd: cmd.result as number[],
                                snap: snap.result as number[],
                                pruned: (pruned.result?.through as number) ?? 0,
                                pinned: pinned.result?.seq as number | undefined,
                            });
                        tx.onerror = () => reject(tx.error);
                    }),
            );
        const documentText = async (): Promise<string> => {
            await dispatch({ type: 'SELECT_ALL' });
            const p = await dispatch({ type: 'GET_SELECTION_AS_CLIPBOARD' });
            return p.type === 'CLIPBOARD_PAYLOAD' ? p.plain : `<${p.type}>`;
        };

        const awaitPin = async (after: number): Promise<Log> => {
            for (let i = 0; i < 200; i++) {
                const l = await readLog();
                if (l.pinned !== undefined && l.pinned > after) return l;
                await sleep(50);
            }
            throw new Error(`no pinned snapshot after ${after}`);
        };
        /* The session's first snapshot (the idle one after boot) is pinned;
           waiting for it leaves no idle snapshot pending. */
        const boot = await awaitPin(0);
        /* A new document: the next snapshot — the idle one after the
           insert — is ITS pinned base, holding the insert. */
        const bytes = Uint8Array.from(atob(b64), (c) => c.charCodeAt(0));
        const opened = await dispatch({ type: 'OPEN_DOCUMENT', bytes, format: 'docx', name: 'p.docx' });
        if (opened.type === 'ERROR') return { failed: `open: ${opened.message}` };
        await dispatch({ type: 'INSERT_TEXT', at: undefined, text: 'PIN-268 ' });
        let log = await awaitPin(boot.pinned!);
        const pinned = log.pinned!;
        const pinnedText = await documentText();

        /* An edit after the pin, then four cadence snapshots: pruning
           drops the pinned base's tail (the edit included). */
        await dispatch({ type: 'INSERT_TEXT', at: undefined, text: 'LOST ' });
        for (let i = 0; i < 850; i++) await dispatch({ type: 'PING' });
        await sleep(2_500);
        log = await readLog();
        for (let i = 0; i < 100 && (log.pruned <= pinned || log.snap.length < 4); i++) {
            await sleep(50);
            log = await readLog();
        }
        const before = log;
        const preMaxSeq = Math.max(...before.cmd);

        /* Every snapshot but the pinned base becomes unreadable. */
        await withDb(
            (db) =>
                new Promise<void>((resolve, reject) => {
                    const tx = db.transaction('snapshots', 'readwrite');
                    for (const seq of before.snap) {
                        if (seq === pinned) continue;
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
        const info = w.__engineClient.lastRecovery;
        const text = await documentText();
        /* The immediate re-snapshot at the log head. */
        let after = await readLog();
        for (let i = 0; i < 100 && Math.max(...after.snap) < preMaxSeq; i++) {
            await sleep(50);
            after = await readLog();
        }
        return { pinned, pinnedText, before, preMaxSeq, info, text, after };
    }, readFileSync(PIN_FIXTURE).toString('base64'));

    expect((result as any).failed, 'in-page failure').toBeUndefined();
    const r = result as any;
    console.log(
        `[event-log #268] pinned=${r.pinned} snaps=${r.before.snap.join(',')} ` +
            `pruned<=${r.before.pruned} fallbacks=${r.info.snapshotFallbacks} ` +
            `pinnedBase=${r.info.pinnedBase} tailDropped=${r.info.tailDropped} ` +
            `after=${r.after.snap.join(',')}`,
    );
    /* Set-up: the pinned base survived pruning, which passed it. */
    expect(r.before.snap).toContain(r.pinned);
    expect(r.before.pruned).toBeGreaterThan(r.pinned);
    expect(r.before.snap.length).toBeGreaterThanOrEqual(4);

    expect(r.info.restored, 'the pinned base restored').toBe(true);
    expect(r.info.pinnedBase).toBe(true);
    expect(r.info.tailDropped).toBe(true);
    expect(r.info.logTruncated).toBe(false);
    expect(r.info.layoutRestored).toBe(true);
    expect(r.info.snapshotFallbacks).toBe(r.before.snap.length - 1);
    /* The document survives, as of the pinned snapshot. */
    expect(r.pinnedText).toContain('PIN-268 ');
    expect(r.text).toBe(r.pinnedText);
    expect(r.text).not.toContain('LOST');
    /* Re-based at the log head: the next recovery replays nothing stale. */
    expect(Math.max(...r.after.snap)).toBeGreaterThanOrEqual(r.preMaxSeq);

    /* Issue #315 — the lost edits are visible: a dismissible alert names
       the snapshot time the document came back from and what to do. */
    expect(typeof r.info.baseSnapshotAt).toBe('number');
    const banner = page.locator('.nge-recovery-banner');
    await expect(banner).toBeVisible();
    await expect(banner).toHaveAttribute('role', 'alert');
    await expect(banner).toHaveAttribute('data-kinds', 'tail-dropped');
    /* The notice copy (title, snapshot time, advice) is unit-tested in
       packages/core/src/recovery.test.ts (#332); here only the wiring. */
    /* …and the Dev HUD carries the raw flags. */
    await page.evaluate(() => window.dispatchEvent(new Event('nge-toggle-hud')));
    await expect(page.locator('.nge-hud__recovery-base')).toContainText('pinned snapshot');
    await expect(page.locator('.nge-hud__recovery-losses')).toHaveText('tail dropped');
    await expect(page.locator('.nge-hud__recovery-losses')).toHaveClass(/nge-hud__recovery-losses--warn/);
    /* Dismissible. */
    await banner.getByRole('button', { name: 'Dismiss recovery notice' }).click();
    await expect(banner).toHaveCount(0);
});

/* Issue #315 — package lost. An opened `.docx` is snapshotted detached
   (#212); pruning then drops the logged `OPEN_DOCUMENT` (so the bare log
   cannot re-open the file), and the `packages` row is lost (damaged /
   evicted storage — #314 keeps a failed write from doing this). Every
   retained base restores only WITHOUT the package: the recovered session
   would save through the minimal writer, dropping the original file's
   sibling parts. That must be visible, with what to do about it. */
test('pruned log + the package row lost: the recovered loss is visible (#315)', async ({
    page,
}) => {
    test.setTimeout(90_000);
    await page.goto('/');
    await page.waitForFunction(() => (window as any).__paintIdle === true, undefined, {
        timeout: 15_000,
    });

    const result = await page.evaluate(async (b64: string) => {
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
        type Log = { snap: number[]; pruned: number; packages: string[] };
        const readLog = () =>
            withDb(
                (db) =>
                    new Promise<Log>((resolve, reject) => {
                        const tx = db.transaction(['snapshots', 'meta', 'packages'], 'readonly');
                        const snap = tx.objectStore('snapshots').getAllKeys();
                        const pruned = tx.objectStore('meta').get('pruned');
                        const packages = tx.objectStore('packages').getAllKeys();
                        tx.oncomplete = () =>
                            resolve({
                                snap: snap.result as number[],
                                pruned: (pruned.result?.through as number) ?? 0,
                                packages: packages.result as string[],
                            });
                        tx.onerror = () => reject(tx.error);
                    }),
            );
        const documentText = async (): Promise<string> => {
            await dispatch({ type: 'SELECT_ALL' });
            const p = await dispatch({ type: 'GET_SELECTION_AS_CLIPBOARD' });
            return p.type === 'CLIPBOARD_PAYLOAD' ? p.plain : `<${p.type}>`;
        };

        const bytes = Uint8Array.from(atob(b64), (c) => c.charCodeAt(0));
        const opened = await dispatch({ type: 'OPEN_DOCUMENT', bytes, format: 'docx', name: 'k.docx' });
        if (opened.type === 'ERROR') return { failed: `open: ${opened.message}` };
        await dispatch({ type: 'INSERT_TEXT', at: undefined, text: 'PKG-315 ' });
        /* Cadence snapshots past the open: pruning drops it. */
        for (let i = 0; i < 850; i++) await dispatch({ type: 'PING' });
        await sleep(2_500);
        let log = await readLog();
        for (let i = 0; i < 100 && (log.pruned < 200 || log.packages.length === 0); i++) {
            await sleep(50);
            log = await readLog();
        }
        const before = log;
        const textBefore = await documentText();

        /* The package row is lost. */
        await withDb(
            (db) =>
                new Promise<void>((resolve, reject) => {
                    const tx = db.transaction('packages', 'readwrite');
                    tx.objectStore('packages').clear();
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
            textBefore,
            info: w.__engineClient.lastRecovery,
            text: await documentText(),
        };
    }, readFileSync(PIN_FIXTURE).toString('base64'));

    expect((result as any).failed, 'in-page failure').toBeUndefined();
    const r = result as any;
    console.log(
        `[event-log #315] snaps=${r.before.snap.join(',')} pruned<=${r.before.pruned} ` +
            `packages=${r.before.packages.length} restored=${r.info.restored} ` +
            `packageLost=${r.info.packageLost} packageFallbacks=${r.info.packageFallbacks}`,
    );
    /* Set-up: pruned past the open, and the package had been stored. */
    expect(r.before.pruned).toBeGreaterThanOrEqual(200);
    expect(r.before.packages).toHaveLength(1);

    expect(r.info.restored).toBe(true);
    expect(r.info.packageLost).toBe(true);
    expect(r.info.tailDropped).toBe(false);
    expect(r.info.logTruncated).toBe(false);
    /* The text itself survived — only the original file's other parts did not. */
    expect(r.text).toBe(r.textBefore);
    expect(r.text).toContain('PKG-315 ');

    const banner = page.locator('.nge-recovery-banner');
    await expect(banner).toBeVisible();
    await expect(banner).toHaveAttribute('role', 'alert');
    await expect(banner).toHaveAttribute('data-kinds', 'package-lost');
    await page.evaluate(() => window.dispatchEvent(new Event('nge-toggle-hud')));
    await expect(page.locator('.nge-hud__recovery-losses')).toHaveText('package lost');
});

/* Issue #333 - a failed snapshot WRITE is retried on its own clock (2 s,
   4 s, 8 s), independent of new commands; after the last retry fails the
   shell says the log is not being checkpointed. The worker's
   `IDBObjectStore.put` is mocked to abort the transaction of the first N
   `snapshots` puts. */
async function snapshotSeqs(page: Page): Promise<number[]> {
    return page.evaluate(
        () =>
            new Promise<number[]>((resolve, reject) => {
                const open = indexedDB.open('engine-log');
                open.onsuccess = () => {
                    const db = open.result;
                    const tx = db.transaction(['snapshots'], 'readonly');
                    const keys = tx.objectStore('snapshots').getAllKeys();
                    tx.oncomplete = () => {
                        db.close();
                        resolve(keys.result as number[]);
                    };
                    tx.onerror = () => reject(tx.error);
                };
                open.onerror = () => reject(open.error);
            }),
    );
}

async function mockSnapshotWrites(page: Page, failFirst: number): Promise<Worker> {
    let worker: Worker | undefined;
    for (let i = 0; i < 100 && !worker; i++) {
        worker = page.workers().find((x) => x.url().includes('engine.worker'));
        if (!worker) await page.waitForTimeout(50);
    }
    if (!worker) throw new Error('engine worker not found');
    await worker.evaluate((n: number) => {
        const g = globalThis as any;
        g.__snapPuts = 0;
        g.__snapFailFirst = n;
        if (g.__snapMocked) return;
        g.__snapMocked = true;
        const realPut = IDBObjectStore.prototype.put;
        IDBObjectStore.prototype.put = function (
            this: IDBObjectStore,
            value: unknown,
            key?: IDBValidKey,
        ): IDBRequest<IDBValidKey> {
            const req = realPut.call(this, value, key);
            if (this.name === 'snapshots') {
                g.__snapPuts += 1;
                if (g.__snapPuts <= g.__snapFailFirst) this.transaction!.abort();
            }
            return req;
        };
    }, failFirst);
    return worker;
}

async function bootAndWaitForFirstSnapshot(page: Page): Promise<void> {
    await page.goto('/');
    await page.waitForFunction(() => (window as any).__paintIdle === true, undefined, {
        timeout: 15_000,
    });
    /* The boot's own idle snapshot, so the mock only sees the test's. */
    await expect.poll(() => snapshotSeqs(page), { timeout: 10_000 }).not.toEqual([]);
}

test('a failed snapshot write is retried without new commands; the third attempt lands (#333)', async ({
    page,
}) => {
    test.setTimeout(60_000);
    await bootAndWaitForFirstSnapshot(page);
    const before = Math.max(...(await snapshotSeqs(page)));
    const worker = await mockSnapshotWrites(page, 2);
    await page.evaluate(() =>
        (window as any).__dispatch({ type: 'INSERT_TEXT', at: undefined, text: 'x' }),
    );
    /* Idle snapshot (+1.5 s) fails, retry (+2 s) fails, retry (+4 s) lands:
       no further typing in between. */
    await expect
        .poll(() => worker.evaluate(() => (globalThis as any).__snapPuts as number), {
            timeout: 25_000,
        })
        .toBe(3);
    await expect
        .poll(async () => Math.max(...(await snapshotSeqs(page))), { timeout: 10_000 })
        .toBeGreaterThan(before);
    const stats = await page.evaluate(() => {
        const c = (window as any).__engineClient;
        return { failures: c.checkpointFailures, failing: c.checkpointStatus.failing };
    });
    expect(stats.failures, 'two failed writes counted').toBe(2);
    expect(stats.failing).toBe(false);
    await expect(page.locator('.nge-recovery-banner')).toHaveCount(0);
});

test('exhausted snapshot-write retries raise the "not being checkpointed" warning (#333)', async ({
    page,
}) => {
    test.setTimeout(90_000);
    await bootAndWaitForFirstSnapshot(page);
    await mockSnapshotWrites(page, Number.MAX_SAFE_INTEGER);
    await page.evaluate(() =>
        (window as any).__dispatch({ type: 'INSERT_TEXT', at: undefined, text: 'x' }),
    );
    /* +1.5 s idle, then retries at +2 s, +4 s, +8 s. */
    const banner = page.locator('.nge-recovery-banner');
    await expect(banner).toBeVisible({ timeout: 40_000 });
    await expect(banner).toHaveAttribute('role', 'alert');
    await expect(banner).toHaveAttribute('data-kinds', 'checkpoint-failing');
    expect(
        await page.evaluate(() => (window as any).__engineClient.checkpointFailures as number),
        'initial write + 3 retries',
    ).toBe(4);

    /* The store heals: the next checkpoint lands and the warning goes. */
    await mockSnapshotWrites(page, 0);
    await page.evaluate(() =>
        (window as any).__dispatch({ type: 'INSERT_TEXT', at: undefined, text: 'y' }),
    );
    await expect(banner).toHaveCount(0, { timeout: 15_000 });
});
