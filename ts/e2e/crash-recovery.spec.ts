import { test, expect } from '@playwright/test';

/* D2.7 exit gate: crash recovery. Force a worker trap, then verify the UI
   shell handles the crash callback — swaps in a fresh <canvas>, respawns the
   worker via recover() — and that the respawned engine answers RPC again. */
test('UI recovers from a worker crash', async ({ page }) => {
    await page.goto('/');
    await page.waitForFunction(() => (window as any).__paintIdle === true, undefined, {
        timeout: 15_000,
    });

    /* Trigger the crash: forceTrap() terminates the worker and runs onTrap. */
    await page.evaluate(() => (window as any).__engineClient.forceTrap());

    /* The UI shell's onCrash callback swaps the canvas and calls recover();
       `index.ts` sets __recovered once the respawned engine is set up. */
    await page.waitForFunction(() => (window as any).__recovered === true, undefined, {
        timeout: 15_000,
    });

    /* The respawned worker answers a Ping. */
    const evt = await page.evaluate(
        () => (window as any).__dispatch({ type: 'PING' }) as Promise<{ type: string }>,
    );
    expect(evt.type).toBe('PONG');
});
