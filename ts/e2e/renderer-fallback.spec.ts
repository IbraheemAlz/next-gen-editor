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
