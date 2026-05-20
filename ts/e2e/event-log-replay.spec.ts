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
        const snapshotSeq = maxOf(before.snap);

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

        return { preMaxSeq, snapshotSeq, recovered, postMaxSeq: maxOf(after.cmd) };
    });

    console.log(
        `[event-log] preMaxSeq=${result.preMaxSeq} snapshotSeq=${result.snapshotSeq} ` +
            `postMaxSeq=${result.postMaxSeq}`,
    );

    expect(result.recovered, 'UI completed crash recovery').toBe(true);
    expect(result.snapshotSeq, 'snapshot persisted at SNAPSHOT_EVERY').toBe(200);
    expect(result.preMaxSeq).toBeGreaterThanOrEqual(250);
    expect(
        result.postMaxSeq,
        'log sequence continued past the pre-crash max',
    ).toBeGreaterThan(result.preMaxSeq);
});
