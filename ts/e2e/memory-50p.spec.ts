import { test, expect } from '@playwright/test';

/* D2.5 exit gate: memory budget. The 50-page document parser does not exist
   yet (Phase 3+), so simulate a heavy editing session and assert via
   Command::RequestStats that the WASM heap stays within the §8.2 per-worker
   budget (< 256 MB). */
test('wasm heap stays within the 256 MB budget under load', async ({ page }) => {
    test.setTimeout(60_000);
    await page.goto('/');
    await page.waitForFunction(() => (window as any).__paintIdle === true, undefined, {
        timeout: 15_000,
    });

    const stats = await page.evaluate(async () => {
        const dispatch = (window as any).__dispatch;
        const chunk = 'lorem ipsum dolor sit am '; // 25 chars
        for (let i = 0; i < 80; i++) {
            await dispatch({ type: 'INSERT_TEXT', at: undefined, text: chunk });
        }
        return dispatch({ type: 'REQUEST_STATS' });
    });

    console.log(
        `[memory] heap=${stats.wasm_heap_bytes} B ` +
            `(${(stats.wasm_heap_bytes / 1048576).toFixed(1)} MiB) · ` +
            `tree=${stats.document_tree_bytes} B`,
    );

    expect(stats.type).toBe('STATS');
    expect(stats.wasm_heap_bytes, 'heap reading is real').toBeGreaterThan(1024 * 1024);
    expect(stats.wasm_heap_bytes, 'heap within 256 MB budget').toBeLessThan(256 * 1024 * 1024);
    expect(stats.document_tree_bytes, 'edits reached the document tree').toBeGreaterThan(0);
});
