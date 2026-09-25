import { test, expect, type Page } from '@playwright/test';

/* Issue #99 — a trap caused by the Vello path must not crash-loop.
   Recovery re-probes the GPU (#66), which would pick Vello again and trap
   again. After VELLO_TRAP_LIMIT (2) consecutive traps on Vello the client
   boots the next generation on Canvas2D without probing, and the engine
   reports the downgrade on `Event::Recovered.renderer_downgrade`.

   CI has no WebGPU, so `?mockBackend=vello` (a DEV-only hook the worker
   honours only in a dev build) makes each generation REPORT Vello while it
   actually paints with Canvas2D — enough to drive the client policy. */

async function trapAndAwaitRecovery(page: Page): Promise<void> {
    await page.evaluate(async () => {
        const w = window as any;
        const before = w.__recoveredEvents.length;
        w.__paintIdle = false;
        await w.__engineClient.armTrap(1);
        await w.__dispatch({ type: 'PING' }).catch(() => undefined);
        for (let i = 0; i < 600; i++) {
            if (w.__recoveredEvents.length > before && w.__paintIdle === true) return;
            await new Promise((r) => setTimeout(r, 50));
        }
        throw new Error('recovery did not complete');
    });
}

test('two traps on Vello force the third generation onto Canvas2D, which stays up', async ({
    page,
}) => {
    test.setTimeout(90_000);
    await page.goto('/?mockBackend=vello');
    await page.waitForFunction(() => (window as any).__paintIdle === true, undefined, {
        timeout: 15_000,
    });
    await page.evaluate(() => {
        const w = window as any;
        w.__recoveredEvents = [];
        w.__engineClient.subscribe((e: any) => {
            if (e.type === 'RECOVERED') w.__recoveredEvents.push(e);
        });
    });
    expect(await page.evaluate(() => (window as any).__renderer)).toBe('vello');

    /* Trap 1 on Vello: below the limit — recovery re-probes, lands on
       Vello again, no downgrade. */
    await trapAndAwaitRecovery(page);
    let evts = await page.evaluate(() => (window as any).__recoveredEvents);
    expect(evts[0].renderer).toBe('vello');
    expect(evts[0].renderer_downgrade).toBeUndefined();

    /* Trap 2 on Vello: the crash loop trips — generation 3 is forced. */
    await trapAndAwaitRecovery(page);
    evts = await page.evaluate(() => (window as any).__recoveredEvents);
    expect(evts).toHaveLength(2);
    expect(evts[1].renderer).toBe('canvas2d');
    expect(evts[1].renderer_downgrade).toEqual({
        from: 'vello',
        to: 'canvas2d',
        reason: 'CRASH_LOOP',
        consecutive_traps: 2,
    });

    const status = await page.evaluate(() => {
        const w = window as any;
        return {
            generation: w.__engineClient.generation,
            clientRenderer: w.__engineClient.renderer,
            windowRenderer: w.__renderer,
            downgrade: w.__engineClient.rendererDowngrade,
            lastRecovery: w.__engineClient.lastRecovery,
        };
    });
    expect(status.generation).toBe(3);
    expect(status.clientRenderer).toBe('canvas2d');
    expect(status.windowRenderer).toBe('canvas2d');
    expect(status.downgrade?.consecutive_traps).toBe(2);
    expect(status.lastRecovery?.rendererDowngrade?.reason).toBe('CRASH_LOOP');

    /* Generation 3 stays up: it answers, edits land, and no further
       recovery happens. */
    const alive = await page.evaluate(async () => {
        const w = window as any;
        const pong = await w.__dispatch({ type: 'PING' });
        const ins = await w.__dispatch({ type: 'INSERT_TEXT', at: undefined, text: 'ok' });
        await new Promise((r) => setTimeout(r, 1_000));
        return { pong: pong.type, ins: ins.type, recoveries: w.__recoveredEvents.length };
    });
    expect(alive.pong).toBe('PONG');
    expect(alive.ins).not.toBe('ERROR');
    expect(alive.recoveries).toBe(2);
    expect(await page.evaluate(() => (window as any).__engineClient.generation)).toBe(3);

    /* The Dev HUD says why the session is on Canvas2D. */
    await page.evaluate(() => window.dispatchEvent(new Event('nge-toggle-hud')));
    const hud = page.locator('.nge-hud');
    await expect(hud).toContainText('Fallback');
    await expect(hud.locator('.nge-hud__warn')).toHaveText('vello → canvas2d (2 traps)');
});

test('without the mock, a production-shaped session never reports a downgrade', async ({
    page,
}) => {
    await page.goto('/');
    await page.waitForFunction(() => (window as any).__paintIdle === true, undefined, {
        timeout: 15_000,
    });
    const r = await page.evaluate(() => ({
        renderer: (window as any).__renderer,
        downgrade: (window as any).__engineClient.rendererDowngrade,
    }));
    expect(['vello', 'canvas2d']).toContain(r.renderer);
    expect(r.downgrade).toBeUndefined();
});

/* ------------------------------------------------------------------
   Issue #240 — the crash-loop streak persists across reloads (event-log
   `meta` row `renderer-streak`, 24 h decay).
   ------------------------------------------------------------------ */

type Streak = { renderer: string; count: number; at: number; live: boolean };

const readStreak = (page: Page): Promise<Streak | undefined> =>
    page.evaluate(
        () =>
            new Promise<Streak | undefined>((resolve, reject) => {
                const open = indexedDB.open('engine-log');
                open.onsuccess = () => {
                    const db = open.result;
                    const tx = db.transaction('meta', 'readonly');
                    const req = tx.objectStore('meta').get('renderer-streak');
                    tx.oncomplete = () => {
                        db.close();
                        const row = req.result;
                        resolve(
                            row
                                ? { renderer: row.renderer, count: row.count, at: row.at, live: row.live }
                                : undefined,
                        );
                    };
                    tx.onerror = () => reject(tx.error);
                };
                open.onerror = () => reject(open.error);
            }),
    );

/**
 * Plant a persisted streak the way a previous page lifetime would have
 * left it. The app boots once to create the event-log schema; the row is
 * then written from the `?test=` harness page, which runs no
 * EngineClient — so no `pagehide` bookkeeping of a live app can
 * overwrite it before the next boot reads it.
 */
async function plantStreak(page: Page, streak: Streak): Promise<void> {
    await page.goto('/');
    await page.waitForFunction(() => (window as any).__paintIdle === true, undefined, {
        timeout: 15_000,
    });
    await page.goto('/?test=a4-justified-mixed');
    await page.evaluate(
        (row: Streak) =>
            new Promise<void>((resolve, reject) => {
                const open = indexedDB.open('engine-log');
                open.onsuccess = () => {
                    const db = open.result;
                    const tx = db.transaction('meta', 'readwrite');
                    tx.objectStore('meta').put({ id: 'renderer-streak', ...row });
                    tx.oncomplete = () => {
                        db.close();
                        resolve();
                    };
                    tx.onerror = () => reject(tx.error);
                };
                open.onerror = () => reject(open.error);
            }),
        streak,
    );
}

async function bootMocked(page: Page): Promise<{
    renderer: string;
    probed: boolean;
    downgrade: any;
    generation: number;
}> {
    await page.goto('/?mockBackend=vello');
    await page.waitForFunction(() => (window as any).__paintIdle === true, undefined, {
        timeout: 15_000,
    });
    return page.evaluate(() => {
        const w = window as any;
        return {
            renderer: w.__renderer,
            probed: w.__engineClient.rendererProbed,
            downgrade: w.__engineClient.rendererDowngrade,
            generation: w.__engineClient.generation,
        };
    });
}

test('a persisted crash loop boots Canvas2D without probing; the HUD retry re-probes', async ({
    page,
}) => {
    test.setTimeout(90_000);
    await plantStreak(page, { renderer: 'vello', count: 2, at: Date.now() - 60_000, live: false });

    /* The mock would REPORT Vello if the worker probed at all — Canvas2D
       proves the probe was skipped. */
    const boot = await bootMocked(page);
    expect(boot.generation).toBe(1);
    expect(boot.renderer).toBe('canvas2d');
    expect(boot.probed).toBe(false);
    expect(boot.downgrade).toEqual({
        from: 'vello',
        to: 'canvas2d',
        reason: 'CRASH_LOOP',
        consecutive_traps: 2,
    });

    /* Sticky downgrade in the Dev HUD, with the retry action. */
    await page.evaluate(() => window.dispatchEvent(new Event('nge-toggle-hud')));
    const hud = page.locator('.nge-hud');
    await expect(hud.locator('.nge-hud__warn')).toHaveText('vello → canvas2d (2 traps)');
    const retry = hud.locator('.nge-hud__retry');
    await expect(retry).toHaveText('Retry vello (reload)');

    /* Retry: the record is forgotten and the reloaded page probes again. */
    await Promise.all([page.waitForEvent('load'), retry.click()]);
    await page.waitForFunction(() => (window as any).__paintIdle === true, undefined, {
        timeout: 15_000,
    });
    const after = await page.evaluate(() => {
        const w = window as any;
        return {
            renderer: w.__renderer,
            probed: w.__engineClient.rendererProbed,
            downgrade: w.__engineClient.rendererDowngrade,
        };
    });
    expect(after).toEqual({ renderer: 'vello', probed: true, downgrade: undefined });
    await expect(page.locator('.nge-hud__warn')).toHaveCount(0);
    /* The fresh Vello generation is marked live (count reset). */
    await expect.poll(() => readStreak(page)).toMatchObject({ count: 0, live: true });
});

test('a Vello generation that died with its tab counts toward the limit', async ({ page }) => {
    test.setTimeout(90_000);
    /* One earlier failure, and the generation after it still `live`: it
       never proved stable and never shut down cleanly. */
    await plantStreak(page, { renderer: 'vello', count: 1, at: Date.now() - 5_000, live: true });
    const boot = await bootMocked(page);
    expect(boot.renderer).toBe('canvas2d');
    expect(boot.probed).toBe(false);
    expect(boot.downgrade?.consecutive_traps).toBe(2);
    /* The death is folded into the record, so later boots do not count
       it again. */
    await expect.poll(() => readStreak(page)).toMatchObject({ count: 2, live: false });
});

test('a persisted streak older than 24 h is ignored', async ({ page }) => {
    test.setTimeout(90_000);
    await plantStreak(page, {
        renderer: 'vello',
        count: 5,
        at: Date.now() - 25 * 60 * 60 * 1000,
        live: false,
    });
    const boot = await bootMocked(page);
    expect(boot.renderer).toBe('vello');
    expect(boot.probed).toBe(true);
    expect(boot.downgrade).toBeUndefined();
    await expect.poll(() => readStreak(page)).toMatchObject({ count: 0, live: true });
});

test('a trap loop that spans a reload still trips the limit', async ({ page }) => {
    test.setTimeout(90_000);
    await page.goto('/?mockBackend=vello');
    await page.waitForFunction(() => (window as any).__paintIdle === true, undefined, {
        timeout: 15_000,
    });
    await page.evaluate(() => {
        const w = window as any;
        w.__recoveredEvents = [];
        w.__engineClient.subscribe((e: any) => {
            if (e.type === 'RECOVERED') w.__recoveredEvents.push(e);
        });
    });
    /* Trap 1 — below the limit, recovery stays on Vello. */
    await trapAndAwaitRecovery(page);
    await expect.poll(() => readStreak(page)).toMatchObject({ count: 1, live: true });

    /* Reload: a fresh EngineClient resumes the count (a clean pagehide
       is not a failure)… */
    await page.reload();
    await page.waitForFunction(() => (window as any).__paintIdle === true, undefined, {
        timeout: 15_000,
    });
    expect(await page.evaluate(() => (window as any).__renderer)).toBe('vello');
    await page.evaluate(() => {
        const w = window as any;
        w.__recoveredEvents = [];
        w.__engineClient.subscribe((e: any) => {
            if (e.type === 'RECOVERED') w.__recoveredEvents.push(e);
        });
    });
    /* …so trap 2 trips it: the recovered generation is forced. */
    await trapAndAwaitRecovery(page);
    const evts = await page.evaluate(() => (window as any).__recoveredEvents);
    expect(evts[0].renderer).toBe('canvas2d');
    expect(evts[0].renderer_downgrade?.consecutive_traps).toBe(2);
});
