import { test, expect, type Page } from '@playwright/test';

/* Issue #52 — the footer mounts TWO zoom widgets (the StatusBar's embedded
   `ZoomControls` and the standalone one). Both must mirror the ENGINE's
   zoom (`SELECTION_CHANGED.zoom`), never a per-widget local signal: a
   change through either — mouse, keyboard, the +/- buttons, or a raw
   `SET_ZOOM` from outside the UI — shows up in both. */

const zoomValues = (page: Page): Promise<string[]> =>
    page.$$eval('.nge-zoom__select', (els) =>
        els.map((el) => (el as HTMLSelectElement).value),
    );

async function expectBoth(page: Page, value: string): Promise<void> {
    await expect.poll(() => zoomValues(page)).toEqual([value, value]);
}

test('both zoom selects show the engine zoom, whichever control changed it', async ({
    page,
}) => {
    await page.goto('/');
    await page.waitForFunction(() => (window as any).__paintIdle === true, undefined, {
        timeout: 15_000,
    });
    const selects = page.locator('.nge-zoom__select');
    await expect(selects).toHaveCount(2);
    await expectBoth(page, '1');

    /* Mouse-style pick on the SECOND widget → the first follows. */
    await selects.nth(1).selectOption('1.25');
    await expectBoth(page, '1.25');

    /* Keyboard on the FIRST widget (focus + ArrowDown = next preset). */
    await selects.nth(0).focus();
    await page.keyboard.press('ArrowDown');
    await expectBoth(page, '1.5');

    /* The step buttons land off-preset; both display the same value.
       (The D2.5 EngineStats debug strip overlays the footer's right edge.) */
    await page.evaluate(() => document.getElementById('stats')?.remove());
    await page.locator('.nge-zoom').nth(1).getByRole('button', { name: 'Zoom in' }).click();
    await expectBoth(page, '1.6');
    await expect(selects.nth(0).locator('option:checked')).toHaveText('160%');

    /* A zoom the UI never issued (raw dispatch) still re-syncs both —
       the engine, not the widget, is the source of truth. */
    await page.evaluate(() => (window as any).__dispatch({ type: 'SET_ZOOM', scale: 0.75 }));
    await expectBoth(page, '0.75');

    /* Out-of-range requests show the engine's CLAMPED value. */
    await page.evaluate(() => (window as any).__dispatch({ type: 'SET_ZOOM', scale: 9 }));
    await expectBoth(page, '4');
});
