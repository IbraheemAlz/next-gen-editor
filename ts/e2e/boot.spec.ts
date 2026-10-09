import { test, expect } from '@playwright/test';

/* D2.1 boot SMOKE (issue #425): the engine worker boots and reports it.
   `index.ts` records the duration of `EngineClient.init()` into
   `window.__bootMs`; the figure is logged, not asserted against a wall clock.
   The < 500 ms budget is a measurement, not a correctness property - it
   lives in `tools/perf/run.mjs --strict` (`workerBootMs`), because a fixed
   wall-clock bound in the blocking suite fails under machine load (717 ms at
   load 43-58) with the code unchanged. This spec still fails if the worker
   never reports ready. */
test('engine worker boots and reports ready', async ({ page }) => {
    await page.goto('/');
    await page.waitForFunction(() => typeof (window as any).__bootMs === 'number', undefined, {
        timeout: 15_000,
    });
    await page.waitForFunction(() => (window as any).__engineReady === true, undefined, {
        timeout: 15_000,
    });

    const bootMs = await page.evaluate(() => (window as any).__bootMs as number);
    console.log(`[boot] cold boot: ${bootMs.toFixed(1)} ms (budget asserted in tools/perf, not here)`);

    expect(Number.isFinite(bootMs)).toBe(true);
    expect(bootMs).toBeGreaterThan(0);
});
