import { test, expect } from '@playwright/test';

/* Issue #357 — `<w:bdo>` / `<w:dir>` wrappers live in the engine's text as
 * the UAX #9 controls (RLO / LRO / RLE / LRE … PDF). The screen-reader
 * mirror turns each into a `<bdo dir>` (override) / `<span dir>`
 * (embedding) around the runs it covers and never mirrors the controls as
 * text. Real worker + WASM engine, DOM-only assertions (the mirror is plain
 * DOM, valid under headless Chrome). */

test('bidi wrappers mirror as bdo / span dir elements', async ({ page }) => {
    test.setTimeout(60_000);
    await page.goto('/');
    await page.waitForFunction(() => (window as any).__paintIdle === true, undefined, {
        timeout: 20_000,
    });

    await page.evaluate(async () => {
        const w = window as any;
        const dispatch = (cmd: unknown): Promise<any> => w.__dispatch(cmd);
        await dispatch({ type: 'SELECT_ALL' });
        await dispatch({
            type: 'INSERT_TEXT',
            at: undefined,
            text: 'Forced ‮ABC def‬ and ‫embedded‬ end',
        });
        await dispatch({ type: 'PING' });
    });

    const p = page.locator('.a11y-mirror > p').first();
    const bdo = p.locator('bdo');
    await expect(bdo).toHaveCount(1);
    await expect(bdo).toHaveAttribute('dir', 'rtl');
    await expect(bdo).toHaveText('ABC def');
    const embedding = p.locator(':scope > span[dir="rtl"]');
    await expect(embedding).toHaveCount(1);
    await expect(embedding).toHaveText('embedded');
    /* The paragraph reads its words, never a control character. */
    const text = (await p.textContent()) ?? '';
    expect(text).toBe('Forced ABC def and embedded end');
    expect(/[‪-‮]/.test(text)).toBe(false);
});
