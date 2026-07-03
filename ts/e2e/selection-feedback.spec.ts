import { test, expect } from '@playwright/test';

/* Regression guard for the SELECTION_CHANGED feedback storm (found
 * while chasing the #34 insert-latency regression).
 *
 * `Command::SetReviewIdentity` replies with a full SELECTION_CHANGED;
 * any UI effect that (a) tracks the selection signal and (b) dispatches
 * a SELECTION_CHANGED-returning command re-triggers itself forever — a
 * self-sustaining background dispatch storm that queues every real
 * command behind it. ReviewControls' identity push did exactly that
 * until its readiness gate became a boolean `createMemo` (edge-
 * triggered) instead of a plain tracked closure.
 *
 * One insert must settle to a HANDFUL of selection broadcasts, not
 * hundreds.
 */
test('one insert does not trigger a SELECTION_CHANGED feedback storm', async ({ page }) => {
    await page.goto('/');
    await page.waitForFunction(() => (window as any).__paintIdle === true, undefined, {
        timeout: 20_000,
    });

    const count = await page.evaluate(async () => {
        let n = 0;
        (window as any).__engineClient.subscribe((e: { type: string }) => {
            if (e.type === 'SELECTION_CHANGED') n++;
        });
        await (window as any).__dispatch({ type: 'INSERT_TEXT', at: undefined, text: 'x' });
        /* A storm produces dozens of events in this window; the healthy
           pipeline produces the insert's own reply plus at most a few
           follow-ups (identity push on first ready, pollers). */
        await new Promise((r) => setTimeout(r, 1_000));
        return n;
    });

    expect(count, 'SELECTION_CHANGED broadcasts within 1s of one insert').toBeLessThan(10);
});
