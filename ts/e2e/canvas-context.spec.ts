import { test, expect, type Page } from '@playwright/test';
import { boot } from './helpers/editor';

/* Issue #502 item 1 - a page canvas whose 2D context is lost and restored
 * comes back BLANK, and the engine's per-canvas display-list memo (#497)
 * skips an unchanged page, so without a `contextrestored` hook it stayed
 * blank until its display list changed. The worker now listens on every
 * registered `OffscreenCanvas`, and on a restore forgets that canvas's memo
 * and repaints it.
 *
 * A 2D canvas has no `WEBGL_lose_context` equivalent, so the dev-only
 * `window.__forceContextLoss(pageIndex, phase)` (installed under
 * `devHooksEnabled()` only) blanks the page's surface inside the worker and
 * dispatches synthetic `contextlost` / `contextrestored` events on the
 * `OffscreenCanvas` - the same listeners the real events reach. Paint
 * evidence is the worker-side opaque-ink probe (`probePageInk`): headless
 * Chrome never composites the transferred placeholders, so a DOM readback
 * would lie. This spec is not a race test, so it dispatches through the
 * hook rather than the `burst` helper. */

const MIN_INK = 500;

async function threePageDocument(page: Page): Promise<void> {
    await boot(page);
    const lines = Array.from(
        { length: 40 },
        (_, i) => `Line ${i + 1}: the quick brown fox jumps over the lazy dog.`,
    ).join('\n');
    for (let i = 0; i < 6; i++) {
        const pages = await page.evaluate(async (text: string) => {
            const dispatch = (window as any).__dispatch;
            await dispatch({ type: 'PASTE_PLAIN', text });
            const evt = await dispatch({
                type: 'REQUEST_PAINT',
                viewport: { x: 0, y: 0, w: 0, h: 0 },
                dirty: undefined,
            });
            return evt.type === 'PAINTED' ? evt.page_count : 0;
        }, `${lines}\n`);
        if (pages >= 3) break;
    }
    await expect
        .poll(() => page.locator('.editor-page canvas').count(), { timeout: 10_000 })
        .toBeGreaterThanOrEqual(3);
}

const inkOf = (page: Page): Promise<Record<number, number>> =>
    page.evaluate(() => (window as any).__engineClient.probePageInk());

const force = (page: Page, idx: number, phase?: 'lost' | 'restored' | 'both'): Promise<string> =>
    page.evaluate(([i, p]) => (window as any).__forceContextLoss(i, p), [idx, phase] as const);

test('a page whose 2D context is lost and restored repaints instead of staying blank (#502)', async ({
    page,
}) => {
    test.setTimeout(90_000);
    await threePageDocument(page);
    await page.keyboard.press('Control+Shift+D');
    const row = page.locator('.nge-hud__canvas-context');
    await expect(row).toHaveText('ok');

    /* Baseline: pages 0-2 all carry ink. */
    await expect
        .poll(
            async () => {
                const ink = await inkOf(page);
                return Math.min(...[0, 1, 2].map((i) => ink[i] ?? -1));
            },
            { timeout: 10_000 },
        )
        .toBeGreaterThan(MIN_INK);
    const before = await inkOf(page);

    /* The worker hears the surface's events (current Chrome has them on
       OffscreenCanvas); `polling` would mean the fallback is in force. */
    expect(await force(page, 1, 'lost')).toBe('events');

    /* Lost, restore not (yet) arrived: the page is blank and the Dev HUD
       says so; the other pages are untouched. */
    await expect(row).toHaveText('canvas context lost (page 2)');
    await expect(row).toHaveClass(/nge-hud__warn/);
    const lost = await inkOf(page);
    expect(lost[1]).toBe(0);
    expect(lost[0]).toBe(before[0]);
    expect(lost[2]).toBe(before[2]);

    /* The restore: the worker forgets that canvas's memo and repaints it,
       though its display list never changed. */
    await force(page, 1, 'restored');
    await expect.poll(async () => (await inkOf(page))[1], { timeout: 10_000 }).toBe(before[1]);
    await expect(row).toHaveText('ok (1/1 restored)');
    const after = await inkOf(page);
    expect(after[0]).toBe(before[0]);
    expect(after[2]).toBe(before[2]);

    /* A full lost + restored cycle on another page, back to back. */
    await force(page, 2, 'both');
    await expect.poll(async () => (await inkOf(page))[2], { timeout: 10_000 }).toBe(before[2]);
    await expect(row).toHaveText('ok (2/2 restored)');
});
