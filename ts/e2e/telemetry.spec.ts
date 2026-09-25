import { test, expect, type Page } from '@playwright/test';
import { createHash } from 'node:crypto';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

/* Issue #86 — D5.7 real telemetry transport.
 *
 * Points the collector at `tools/telemetry-sink` (booted by
 * `playwright.config.ts`'s second `webServer` entry) via
 * `?telemetryEndpoint=`, then exercises the three acceptance items that
 * need a real browser + wasm build:
 *   - an opted-in session's flush reaches the sink,
 *   - a forced trap produces a CRASH sample that reaches the sink,
 *   - opting out sends nothing (a real network-request assertion, not just
 *     an empty receiver list).
 *
 * `window.__setTelemetryEnabled` / `window.__telemetryFlush` are debug + e2e
 * hooks (see `ts/src/index.tsx` / `ts/src/state/telemetry.ts`). */

/* Issue #205 — the sink's port is derived per-checkout so two worktrees
 * running this suite concurrently don't share one stateful sink process
 * (this file calls `POST /reset` mid-test, which would wipe another
 * worktree's in-flight samples). This spec and `playwright.config.ts` run
 * in separate processes with no shared state, so both independently
 * compute the identical port from the same algorithm + salt + range —
 * change one, change the other. */
const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..');
function stablePortFor(seed: string, rangeStart: number, rangeSize: number): number {
    const digest = createHash('sha256').update(seed).digest();
    return rangeStart + (digest.readUInt32BE(0) % rangeSize);
}
const SINK_PORT = stablePortFor(`${REPO_ROOT}::telemetry-sink`, 4200, 800);
const SINK_ORIGIN = `http://localhost:${SINK_PORT}`;

async function resetSink(page: Page): Promise<void> {
    const res = await page.request.post(`${SINK_ORIGIN}/reset`);
    expect(res.ok()).toBe(true);
}

async function received(page: Page): Promise<unknown[]> {
    const res = await page.request.get(`${SINK_ORIGIN}/received`);
    expect(res.ok()).toBe(true);
    return (await res.json()) as unknown[];
}

/** Flattened `kind.type` tags across every event in every received batch —
 *  the shape the assertions below care about. */
function sampleTypes(batches: unknown[]): string[] {
    const types: string[] = [];
    for (const batch of batches) {
        const events = (batch as { events?: { kind?: { type?: string } }[] }).events ?? [];
        for (const evt of events) {
            if (evt.kind?.type) types.push(evt.kind.type);
        }
    }
    return types;
}

async function gotoOptedIn(page: Page): Promise<void> {
    await page.goto(`/?telemetryEndpoint=${encodeURIComponent(`${SINK_ORIGIN}/telemetry`)}`);
    await page.waitForFunction(() => (window as any).__paintIdle === true, undefined, {
        timeout: 15_000,
    });
    await page.evaluate(() => (window as any).__setTelemetryEnabled(true));
}

test.describe('D5.7 telemetry transport (Issue #86)', () => {
    test('an opted-in flush reaches the sink', async ({ page }) => {
        await resetSink(page);
        await gotoOptedIn(page);

        // A real edit so ENGINE_STATS reflects a live document, then force
        // a flush instead of waiting on the 60 s interval.
        await page.evaluate(async () => {
            await (window as any).__dispatch({ type: 'INSERT_TEXT', at: undefined, text: 'x' });
            await (window as any).__telemetryFlush();
        });

        await expect
            .poll(async () => sampleTypes(await received(page)), { timeout: 10_000 })
            .toContain('ENGINE_STATS');
    });

    test('a forced trap produces a CRASH sample that reaches the sink', async ({ page }) => {
        await resetSink(page);
        await gotoOptedIn(page);

        await page.evaluate(() => (window as any).__engineClient.forceTrap());
        // Recovery is best-effort context for the sample, not a precondition
        // for it existing — the PENDING sample fires synchronously with the
        // trap (see `armCrashSample` in telemetry.ts), well before recovery
        // finishes.
        await expect
            .poll(async () => sampleTypes(await received(page)), { timeout: 10_000 })
            .toContain('CRASH');

        const outcomes = (await received(page))
            .flatMap((b) => (b as { events: { kind: { type: string; recovery_outcome?: string } }[] }).events)
            .filter((e) => e.kind.type === 'CRASH')
            .map((e) => e.kind.recovery_outcome);
        expect(outcomes.length).toBeGreaterThan(0);
        expect(outcomes.every((o) => o === 'PENDING' || o === 'RECOVERED' || o === 'FAILED')).toBe(true);
    });

    test('a batch flushes on visibilitychange, not just the 60s interval', async ({ page }) => {
        await resetSink(page);
        await gotoOptedIn(page);

        // No manual __telemetryFlush() call here — the ONLY thing that
        // should trigger a flush is the simulated tab-hide below. `flush()`
        // always samples ENGINE_STATS, so its presence alone proves a
        // flush ran.
        await page.evaluate(() => {
            Object.defineProperty(document, 'visibilityState', {
                configurable: true,
                get: () => 'hidden',
            });
            document.dispatchEvent(new Event('visibilitychange'));
        });

        await expect
            .poll(async () => sampleTypes(await received(page)), { timeout: 10_000 })
            .toContain('ENGINE_STATS');
    });

    test('opting out sends nothing (network assertion)', async ({ page }) => {
        await resetSink(page);
        await page.goto(`/?telemetryEndpoint=${encodeURIComponent(`${SINK_ORIGIN}/telemetry`)}`);
        await page.waitForFunction(() => (window as any).__paintIdle === true, undefined, {
            timeout: 15_000,
        });
        // Telemetry defaults to disabled — explicit for clarity/robustness
        // against a future default change.
        await page.evaluate(() => (window as any).__setTelemetryEnabled(false));

        let sawTelemetryRequest = false;
        page.on('request', (req) => {
            if (req.url().startsWith(SINK_ORIGIN)) sawTelemetryRequest = true;
        });

        await page.evaluate(async () => {
            await (window as any).__dispatch({ type: 'INSERT_TEXT', at: undefined, text: 'y' });
            await (window as any).__telemetryFlush();
        });
        // Give any (incorrectly-fired) beacon a moment to land before
        // asserting its absence.
        await page.waitForTimeout(1_000);

        expect(sawTelemetryRequest).toBe(false);
        expect(await received(page)).toEqual([]);
    });
});
