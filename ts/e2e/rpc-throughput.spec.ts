import { test, expect } from '@playwright/test';

/* D2.8 exit gate: backpressure. Flood the engine with 1 000 commands and
   assert every one resolves — no dropped messages, no deadlock — within 1 s.
   Proven against the off-critical-path event log (the worker posts each RPC
   reply before persisting, so log latency never throttles throughput). */
test('1000 commands resolve within 1 s', async ({ page }) => {
    await page.goto('/');
    await page.waitForFunction(() => (window as any).__paintIdle === true, undefined, {
        timeout: 15_000,
    });

    const result = await page.evaluate(async () => {
        const dispatch = (window as any).__dispatch as (c: unknown) => Promise<{ type: string }>;
        const t0 = performance.now();
        const events = await Promise.all(
            Array.from({ length: 1000 }, () => dispatch({ type: 'PING' })),
        );
        return {
            elapsedMs: performance.now() - t0,
            count: events.length,
            allPong: events.every((e) => e && e.type === 'PONG'),
        };
    });

    console.log(`[throughput] 1000 commands in ${result.elapsedMs.toFixed(1)} ms`);

    expect(result.count).toBe(1000);
    expect(result.allPong, 'every command resolved with PONG').toBe(true);
    expect(result.elapsedMs).toBeLessThan(1000);
});
