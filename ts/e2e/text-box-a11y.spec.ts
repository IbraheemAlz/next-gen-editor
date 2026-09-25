import { test, expect } from '@playwright/test';

/* Issue #165 — text-box stories in the screen-reader mirror, end to end
 * through the REAL worker + WASM engine + a11y delta stream:
 *
 *   body text → insert a text box → its `role="group"` region appears
 *   right after the anchor paragraph, marked `aria-current` while its
 *   story is edited → typing in the box patches ONLY that region (the
 *   body `<p>` keeps its DOM identity — a delta, not a full rebuild) →
 *   leaving the story clears `aria-current`.
 *
 * DOM-only assertions: the mirror is plain DOM, valid under headless
 * Chrome (the canvas is never screenshotted, see CLAUDE.md). */

test('text box story is mirrored as a live group region', async ({ page }) => {
    test.setTimeout(60_000);
    await page.goto('/');
    await page.waitForFunction(() => (window as any).__paintIdle === true, undefined, {
        timeout: 20_000,
    });

    await page.evaluate(async () => {
        const dispatch = (cmd: unknown): Promise<any> => (window as any).__dispatch(cmd);
        const top = (idx: number, offset: number) => ({
            path: { steps: [{ kind: 'BLOCK', idx }] },
            offset,
        });
        await dispatch({ type: 'SELECT_ALL' });
        await dispatch({ type: 'INSERT_TEXT', at: undefined, text: 'Alpha body text' });
        await dispatch({
            type: 'SET_SELECTION',
            range: { start: top(0, 5), end: top(0, 5) },
            caret: top(0, 5),
        });
    });

    const mirror = page.locator('.a11y-mirror');
    const region = mirror.locator(':scope > [role="group"]');
    await expect(region).toHaveCount(0);

    /* Insert through the real toolbar button (enters the box's story). */
    await page.getByRole('button', { name: 'Insert text box' }).click();
    await expect(page.getByRole('button', { name: 'Close text box editing' })).toBeVisible();

    await expect(region).toHaveCount(1);
    await expect(region).toHaveAttribute('aria-label', 'Text box');
    await expect(region).toHaveAttribute('data-story-id', '0@5');
    await expect(region).toHaveAttribute('aria-current', 'true');
    /* Placed right after its anchor paragraph, body-shaped inside. */
    const order = await mirror.evaluate((root) =>
        Array.from(root.children).map((c) => c.getAttribute('role') ?? c.tagName),
    );
    expect(order.slice(0, 2)).toEqual(['P', 'group']);
    await expect(region.locator('p')).toHaveCount(1);

    /* Tag the body paragraph's DOM node: a delta must leave it alone. */
    await mirror.evaluate((root) => {
        (root.children[0] as any).__nge165 = true;
    });

    await page.evaluate(async () => {
        const dispatch = (cmd: unknown): Promise<any> => (window as any).__dispatch(cmd);
        await dispatch({ type: 'INSERT_TEXT', at: undefined, text: 'boxed words' });
    });
    await expect(region.locator('p')).toHaveText('boxed words');
    await expect(region.locator('p')).toHaveAttribute('dir', /^(ltr|rtl)$/);
    await expect(region.locator('p > span').first()).toHaveText('boxed words');
    const kept = await mirror.evaluate((root) => (root.children[0] as any).__nge165 === true);
    expect(kept, 'the body <p> was not rebuilt').toBe(true);
    await expect(mirror.locator(':scope > p').first()).toContainText('Alpha');

    /* Leaving the story clears the active marker; the region stays. */
    await page.evaluate(async () => {
        const dispatch = (cmd: unknown): Promise<any> => (window as any).__dispatch(cmd);
        await dispatch({ type: 'EXIT_HEADER_FOOTER' });
    });
    await expect(region).not.toHaveAttribute('aria-current', 'true');
    await expect(region.locator('p')).toHaveText('boxed words');
});
