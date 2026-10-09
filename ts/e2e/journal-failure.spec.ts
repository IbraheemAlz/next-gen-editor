import { test, expect, type Page, type Worker } from '@playwright/test';

/* Issue #390 - the command journal (`appendCommand`) is no longer a silent
 * `console.warn`.
 *
 * A failed command-row write is retried on the same 2/4/8 s clock as a
 * failed snapshot write; after the last retry the worker broadcasts the
 * typed `Event::CheckpointState { ok: false, journal_failing: true }` and
 * the recovery banner says commands are not being recorded. A recovery
 * after such a failure reports the gap in `RecoveryInfo.journalGap`.
 *
 * The worker's IndexedDB is mocked from the test: `IDBObjectStore.put` on
 * the `commands` store aborts its transaction for the first N rows. */

const BANNER = '.nge-recovery-banner';

async function mockCommandWrites(page: Page, failFirst: number): Promise<Worker> {
    let worker: Worker | undefined;
    for (let i = 0; i < 100 && !worker; i++) {
        worker = page.workers().find((x) => x.url().includes('engine.worker'));
        if (!worker) await page.waitForTimeout(50);
    }
    if (!worker) throw new Error('engine worker not found');
    await worker.evaluate((n: number) => {
        const g = globalThis as any;
        g.__cmdPuts = 0;
        g.__cmdFailFirst = n;
        if (g.__cmdMocked) return;
        g.__cmdMocked = true;
        const realPut = IDBObjectStore.prototype.put;
        IDBObjectStore.prototype.put = function (
            this: IDBObjectStore,
            value: unknown,
            key?: IDBValidKey,
        ): IDBRequest<IDBValidKey> {
            const req = realPut.call(this, value, key);
            if (this.name === 'commands') {
                g.__cmdPuts += 1;
                if (g.__cmdPuts <= g.__cmdFailFirst) this.transaction!.abort();
            }
            return req;
        };
    }, failFirst);
    return worker;
}

async function bootIdle(page: Page): Promise<void> {
    await page.goto('/');
    await page.waitForFunction(() => (window as any).__paintIdle === true, undefined, {
        timeout: 15_000,
    });
    /* The boot's own idle snapshot lands first, so the mock only sees the
       test's commands and the recovery base is the boot snapshot. */
    await expect
        .poll(
            () =>
                page.evaluate(
                    () =>
                        new Promise<number>((resolve, reject) => {
                            const open = indexedDB.open('engine-log');
                            open.onsuccess = () => {
                                const db = open.result;
                                const req = db
                                    .transaction('snapshots', 'readonly')
                                    .objectStore('snapshots')
                                    .count();
                                req.onsuccess = () => {
                                    db.close();
                                    resolve(req.result);
                                };
                                req.onerror = () => reject(req.error);
                            };
                            open.onerror = () => reject(open.error);
                        }),
                ),
            { timeout: 10_000 },
        )
        .toBeGreaterThan(0);
}

test('exhausted command-journal retries raise the "edits are not being recorded" warning (#390)', async ({
    page,
}) => {
    test.setTimeout(90_000);
    await bootIdle(page);
    await mockCommandWrites(page, Number.MAX_SAFE_INTEGER);
    await page.evaluate(() =>
        (window as any).__dispatch({ type: 'INSERT_TEXT', at: undefined, text: 'x' }),
    );
    /* First failure at once, retries at +2 s, +4 s, +8 s. */
    const banner = page.locator(BANNER);
    await expect(banner).toBeVisible({ timeout: 40_000 });
    await expect(banner).toHaveAttribute('role', 'alert');
    await expect(banner).toHaveAttribute('data-kinds', 'journal-failing');
    await expect(banner).toContainText('Your edits are not being recorded');
    await expect(banner).toContainText('Save your work');
    const health = await page.evaluate(() => {
        const c = (window as any).__engineClient;
        return { ...c.checkpointStatus, failureCount: c.checkpointFailures as number };
    });
    expect(health.failing).toBe(true);
    expect(health.journalFailing).toBe(true);
    expect(typeof health.lastError).toBe('string');
    expect(health.failureCount, 'first attempt + 3 retries').toBe(4);

    /* The store heals: the next row lands, the backlog drains, the warning goes. */
    await mockCommandWrites(page, 0);
    await page.evaluate(() =>
        (window as any).__dispatch({ type: 'INSERT_TEXT', at: undefined, text: 'y' }),
    );
    await expect(banner).toHaveCount(0, { timeout: 15_000 });
    expect(
        await page.evaluate(() => (window as any).__engineClient.checkpointStatus.journalFailing),
    ).toBe(false);
});

test('a journal failure that heals within the retries never raises the warning (#390)', async ({
    page,
}) => {
    test.setTimeout(60_000);
    await bootIdle(page);
    const worker = await mockCommandWrites(page, 1);
    await page.evaluate(() =>
        (window as any).__dispatch({ type: 'INSERT_TEXT', at: undefined, text: 'x' }),
    );
    /* Row fails once; the +2 s retry re-writes it. */
    await expect
        .poll(() => worker.evaluate(() => (globalThis as any).__cmdPuts as number), {
            timeout: 15_000,
        })
        .toBeGreaterThanOrEqual(2);
    await expect
        .poll(() => page.evaluate(() => (window as any).__engineClient.checkpointFailures as number))
        .toBe(1);
    expect(
        await page.evaluate(() => (window as any).__engineClient.checkpointStatus.failing),
    ).toBe(false);
    await expect(page.locator(BANNER)).toHaveCount(0);
});

test('a recovery after a journal failure reports the gap in RecoveryInfo (#390)', async ({
    page,
}) => {
    test.setTimeout(90_000);
    await bootIdle(page);
    await mockCommandWrites(page, Number.MAX_SAFE_INTEGER);
    await page.evaluate(() =>
        (window as any).__dispatch({ type: 'INSERT_TEXT', at: undefined, text: 'lost-edit' }),
    );
    /* The gap record is a best-effort `meta` write: wait for it, then crash
       before the 1.5 s idle snapshot would cover the failed rows. */
    const gapRecorded = (): Promise<boolean> =>
        page.evaluate(
            () =>
                new Promise<boolean>((resolve, reject) => {
                    const open = indexedDB.open('engine-log');
                    open.onsuccess = () => {
                        const db = open.result;
                        const req = db
                            .transaction('meta', 'readonly')
                            .objectStore('meta')
                            .get('journal-gap');
                        req.onsuccess = () => {
                            db.close();
                            resolve(req.result !== undefined);
                        };
                        req.onerror = () => reject(req.error);
                    };
                    open.onerror = () => reject(open.error);
                }),
        );
    await expect.poll(gapRecorded, { timeout: 5_000 }).toBe(true);
    await page.evaluate(() => (window as any).__engineClient.forceTrap());
    await page.waitForFunction(() => (window as any).__recovered === true, undefined, {
        timeout: 30_000,
    });
    const info = await page.evaluate(() => (window as any).__engineClient.lastRecovery);
    expect(info.journalGap, 'the unjournaled commands are reported').toBeGreaterThan(0);
    const banner = page.locator(BANNER);
    await expect(banner).toHaveAttribute('data-kinds', /journal-gap/);
    await expect(banner).toContainText('Some of your latest edits could not be restored');
});
