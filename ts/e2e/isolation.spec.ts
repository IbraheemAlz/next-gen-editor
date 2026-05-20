import { test, expect } from '@playwright/test';

/* D2.3 exit gate: cross-origin isolation must hold on the main thread AND
   inside the engine worker (SharedArrayBuffer depends on it). The worker
   reports its `self.crossOriginIsolated` in the INIT reply; EngineClient
   exposes it via `crossOriginIsolated`. */
test('main thread and worker are cross-origin isolated', async ({ page }) => {
    await page.goto('/');
    await page.waitForFunction(() => (window as any).__paintIdle === true, undefined, {
        timeout: 15_000,
    });

    const mainIsolated = await page.evaluate(() => self.crossOriginIsolated);
    expect(mainIsolated, 'main thread crossOriginIsolated').toBe(true);

    const workerIsolated = await page.evaluate(
        () => (window as any).__engineClient.crossOriginIsolated as boolean,
    );
    expect(workerIsolated, 'worker context crossOriginIsolated').toBe(true);
});
