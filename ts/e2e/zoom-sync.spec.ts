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

/* Issue #231 — `SET_ZOOM` / `SET_DEVICE_SCALE` repaint (a fresh
   `page_tops` / `page_heights` / `document_height`) without mutating
   the document, so the worker's `document_mutation_seq` gate never fired
   a `PAINTED` broadcast for them: the overlays + page geometry stayed at
   their PRE-zoom values until the user typed a character. Both checks
   below run with ZERO edits in between the `SET_ZOOM` and the assertion —
   that's the bug: everything must already be correct. */
test('zoom rescales page geometry and the caret overlay before any edit', async ({ page }) => {
    await page.goto('/');
    await page.waitForFunction(() => (window as any).__paintIdle === true, undefined, {
        timeout: 15_000,
    });

    /* Caret overlay height at 100 % — a real DOM element positioned in
       absolute CSS px (`CaretOverlay.tsx`), independent of the still-fixed
       A4 page card size. */
    const caret = page.locator('.editor-page[data-page-index="0"] .caret');
    const before = await caret.evaluate((el) => el.getBoundingClientRect().height);
    expect(before).toBeGreaterThan(0);

    /* Install a listener BEFORE dispatching so no broadcast is missed,
       then poll its most recent reading — robust against any number of
       intervening `PAINTED`s (e.g. a stray scroll-driven `EXPAND_LAYOUT`)
       since we only care that the LATEST one already reflects 150 %. */
    await page.evaluate(() => {
        (window as any).__lastPageHeight = undefined;
        (window as any).__engineClient.subscribe((evt: any) => {
            if (evt.type === 'PAINTED' && evt.page_heights.length > 0) {
                (window as any).__lastPageHeight = evt.page_heights[0];
            }
        });
    });
    const dpr = await page.evaluate(() => window.devicePixelRatio || 1);
    /* `PAGE_H_PT` / `SCREEN_DPI_SCALE` mirror `ts/src/state/engine-store.ts` —
       A4 height in layout pt × the print-to-screen DPI factor. */
    const PAGE_H_PT = 841.9;
    const SCREEN_DPI_SCALE = 4 / 3;
    const expectedPageHeightAt150 = PAGE_H_PT * dpr * SCREEN_DPI_SCALE * 1.5;

    await page.evaluate(() => (window as any).__dispatch({ type: 'SET_ZOOM', scale: 1.5 }));

    /* Page geometry: the broadcast PAINTED already carries the 150 %
       height — no edit dispatched anywhere above. */
    await expect
        .poll(() => page.evaluate(() => (window as any).__lastPageHeight))
        .toBeGreaterThan(expectedPageHeightAt150 * 0.99);

    /* Overlay rect: the caret grew proportionally with the new scale. */
    await expect
        .poll(() => caret.evaluate((el) => el.getBoundingClientRect().height))
        .toBeGreaterThan(before * 1.49);
});
