import { test, expect } from '@playwright/test';
import { boot, settle } from './helpers/editor';

/* Issue #500 (item 3) - the layout caches are observable through the REAL
 * shell + worker + WASM engine: `EngineStats` carries the paragraph / table
 * cache bytes, entries and lookup counters, and the Dev HUD's "Layout
 * caches" row words them.
 *
 * Assertions read engine truth (the `REQUEST_STATS` reply) and the HUD's DOM
 * - never a canvas screenshot (CLAUDE.md: headless Chrome does not composite
 * the interactive canvas). */

interface CacheStats {
    paragraph_cache_bytes: number;
    paragraph_cache_entries: number;
    paragraph_cache_hits: number;
    paragraph_cache_misses: number;
    table_cache_bytes: number;
    table_cache_entries: number;
    table_cache_hits: number;
    table_cache_misses: number;
}

const requestStats = (page: import('@playwright/test').Page): Promise<CacheStats> =>
    page.evaluate(async () => {
        const reply = await (window as any).__dispatch({ type: 'REQUEST_STATS' });
        return reply as CacheStats;
    });

test('#500: the Dev HUD shows the layout caches and their hit count grows with another paint', async ({
    page,
}) => {
    test.setTimeout(45_000);
    await boot(page);

    /* Five paragraphs, so a later edit of one leaves four to be served from
       the paragraph cache. */
    await page.evaluate(async () => {
        const dispatch = (window as any).__dispatch;
        for (const text of ['alpha', 'beta', 'gamma', 'delta']) {
            await dispatch({ type: 'INSERT_TEXT', at: undefined, text });
            await dispatch({ type: 'SPLIT_PARAGRAPH', at: undefined });
        }
        await dispatch({ type: 'INSERT_TEXT', at: undefined, text: 'omega' });
    });
    await settle(page);

    await page.keyboard.press('Control+Shift+D');
    const row = page.locator('.nge-hud__layout-caches');
    await expect(row).toBeVisible();
    await expect(row).toHaveText(
        /^para \d+(\.\d)? (B|KiB|MiB) \/ [\d,]+ · (\d+ %|–) hit · table \d+(\.\d)? (B|KiB|MiB) \/ [\d,]+ · (\d+ %|–) hit$/,
    );

    const before = await requestStats(page);
    expect(before.paragraph_cache_entries).toBeGreaterThan(0);
    expect(before.paragraph_cache_bytes).toBeGreaterThan(0);
    expect(before.paragraph_cache_misses).toBeGreaterThan(0);

    /* A second paint: one more keystroke repaints the document; the four
       untouched paragraphs are cache hits. */
    await page.evaluate(() =>
        (window as any).__dispatch({ type: 'INSERT_TEXT', at: undefined, text: '!' }),
    );
    await settle(page);
    const after = await requestStats(page);
    expect(after.paragraph_cache_hits).toBeGreaterThan(before.paragraph_cache_hits);

    /* The HUD polls once a second: its tooltip carries the raw counts. */
    await expect
        .poll(
            async () => {
                const title = (await row.getAttribute('title')) ?? '';
                const m = /paragraph cache: ([\d,]+) hits/.exec(title);
                return m ? Number((m[1] ?? '0').replace(/,/g, '')) : 0;
            },
            { timeout: 10_000 },
        )
        .toBeGreaterThanOrEqual(after.paragraph_cache_hits);
});
