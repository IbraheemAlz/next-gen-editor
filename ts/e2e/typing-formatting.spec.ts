import { test, expect, type Page } from '@playwright/test';

/* Issue #276 — typing after a formatted run continues its formatting (the
 * character before the caret; at a paragraph start the one after it), and
 * the toolbar's `attrs_at_caret` reports exactly what the next keystroke
 * produces — before and after typing. Pending (sticky) formatting still
 * overrides the inherited style. */

async function boot(page: Page): Promise<void> {
    await page.goto('/');
    await page.waitForFunction(() => (window as any).__paintIdle === true, undefined, {
        timeout: 30_000,
    });
}

test('typing after a bold run is bold; pending bold-off types plain', async ({ page }) => {
    await boot(page);
    const out = await page.evaluate(async () => {
        const dispatch = (window as any).__dispatch;
        const pos = (offset: number) => ({
            path: { steps: [{ kind: 'BLOCK', idx: 0 }] },
            offset,
        });
        const select = (a: number, b: number) =>
            dispatch({ type: 'SET_SELECTION', range: { start: pos(a), end: pos(b) }, caret: pos(b) });
        const bold = (on: boolean) =>
            dispatch({ type: 'APPLY_FORMATTING', range: undefined, attrs: { bold: on } });
        /* Bold the first three characters, park the caret right after. */
        await select(0, 3);
        await bold(true);
        const before = await select(3, 3);
        await dispatch({ type: 'INSERT_TEXT', at: undefined, text: 'Z' });
        /* The typed char itself, as a one-character range. */
        const typed = await select(3, 4);
        /* Pending bold-off at the end of the (now four-char) bold run. */
        await select(4, 4);
        await bold(false);
        await dispatch({ type: 'INSERT_TEXT', at: undefined, text: 'Q' });
        const plain = await select(4, 5);
        const boldStill = await select(3, 4);
        return {
            before: before.attrs_at_caret.bold as boolean,
            typed: typed.attrs_at_caret.bold as boolean,
            plain: plain.attrs_at_caret.bold as boolean,
            boldStill: boldStill.attrs_at_caret.bold as boolean,
        };
    });
    expect(out.before, 'toolbar previews the inherited bold').toBe(true);
    expect(out.typed, 'typed text continues the bold run').toBe(true);
    expect(out.plain, 'pending bold-off wins').toBe(false);
    expect(out.boldStill).toBe(true);
});
