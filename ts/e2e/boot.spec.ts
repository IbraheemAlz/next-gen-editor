import { test, expect } from '@playwright/test';

/* D2.1 exit gate: the engine worker boots cold (worker spawn + WASM load +
   engine construction) in under 500 ms. `index.ts` records the duration of
   `EngineClient.init()` into `window.__bootMs`. */
test('engine worker boots cold under 500 ms', async ({ page }) => {
    await page.goto('/');
    await page.waitForFunction(() => typeof (window as any).__bootMs === 'number', undefined, {
        timeout: 15_000,
    });

    const bootMs = await page.evaluate(() => (window as any).__bootMs as number);
    console.log(`[boot] cold boot: ${bootMs.toFixed(1)} ms`);

    expect(bootMs).toBeLessThan(500);
});
