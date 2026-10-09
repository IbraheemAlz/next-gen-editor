import { test, expect } from '@playwright/test';
import { boot, burst, documentText, settle } from './helpers/editor';

/* Issue #428 - the Settings menu's renderer switch is an IN-PLACE switch
 * (the #270 retire-respawn path), not a `?renderer=` reload; and the unload
 * guard comes back when the crash overlay's "Reload page" never reloads.
 *
 * CI has no WebGPU: `?mockBackend=vello` (dev hooks only, #428) makes a
 * generation REPORT Vello while painting with Canvas2D - enough to see the
 * client policy switch backends. */

async function renderer(page: import('@playwright/test').Page): Promise<string> {
    return page.evaluate(() => (window as any).__engineClient.renderer as string);
}

test('#428: Settings switch to Canvas2D and back restarts in place and keeps the document', async ({
    page,
}) => {
    test.setTimeout(90_000);
    await page.goto('/?mockBackend=vello');
    await page.waitForFunction(() => (window as any).__paintIdle === true, undefined, {
        timeout: 20_000,
    });
    await burst(page, ['kept across the switch']);
    await settle(page);
    /* A marker only a SAME page survives - a reload would drop it. */
    await page.evaluate(() => ((window as any).__sameDocument = 'yes'));
    expect(await renderer(page)).toBe('vello');

    await page.getByRole('button', { name: 'Settings' }).click();
    await page.getByRole('button', { name: /Switch to Canvas2D/ }).click();
    await expect.poll(() => renderer(page), { timeout: 20_000 }).toBe('canvas2d');
    await page.waitForFunction(() => (window as any).__paintIdle === true);

    expect(await page.evaluate(() => (window as any).__sameDocument)).toBe('yes');
    await expect.poll(() => documentText(page)).toContain('kept across the switch');

    /* ... and back: lifts the pin and probes again. */
    /* The menu stays open after a switch: only open it when closed. */
    if (!(await page.locator('.nge-settings__menu').isVisible())) {
        await page.getByRole('button', { name: 'Settings' }).click();
    }
    await page.getByRole('button', { name: /Switch to Vello/ }).click();
    await expect.poll(() => renderer(page), { timeout: 20_000 }).toBe('vello');
    expect(await page.evaluate(() => (window as any).__sameDocument)).toBe('yes');
    await expect.poll(() => documentText(page)).toContain('kept across the switch');
});

test('#428: a prepared reload that never happens re-arms the unload guard', async ({ page }) => {
    test.setTimeout(60_000);
    await boot(page);
    await burst(page, ['unsaved words']);
    await settle(page);
    const state = () =>
        page.evaluate(() => ({
            unsaved: (window as any).__engineClient.hasUnsavedChanges as boolean,
            token: sessionStorage.getItem('nge.carry-over'),
        }));
    expect((await state()).unsaved).toBe(true);

    await page.evaluate(() => (window as any).__engineClient.prepareCarryOver());
    /* Carry-over prepared: the guard is muted, the token is armed. */
    expect(await state()).toMatchObject({ unsaved: false });
    expect((await state()).token).not.toBeNull();

    /* The page comes back to the foreground with no `pagehide` in between:
       the reload did not happen. */
    await page.evaluate(() => {
        Object.defineProperty(document, 'visibilityState', { value: 'visible', configurable: true });
        document.dispatchEvent(new Event('visibilitychange'));
    });
    await expect.poll(state).toMatchObject({ unsaved: true, token: null });
});

test('#428: a prepared reload that really starts (pagehide) keeps the guard muted', async ({
    page,
}) => {
    test.setTimeout(60_000);
    await boot(page);
    await burst(page, ['unsaved words']);
    await settle(page);
    await page.evaluate(() => (window as any).__engineClient.prepareCarryOver());
    await page.evaluate(() => {
        window.dispatchEvent(new Event('pagehide'));
        Object.defineProperty(document, 'visibilityState', { value: 'visible', configurable: true });
        document.dispatchEvent(new Event('visibilitychange'));
    });
    expect(await page.evaluate(() => (window as any).__engineClient.hasUnsavedChanges)).toBe(false);
});
