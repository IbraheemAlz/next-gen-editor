import { test, expect } from '@playwright/test';
import { boot } from './helpers/editor';

/* Issue #429 - `theme.css` is imported after the component sheets, so the
 * plain `.nge-btn` rule used to beat `.nge-btn--primary` and every dialog's
 * Apply button rendered as a plain button. Assert the computed style, not
 * the class name. */
test('#429: the Font dialog primary button renders with the primary colour', async ({ page }) => {
    await boot(page);
    await page.locator('button.nge-font__more').click();
    await expect(page.locator('.nge-font-dialog')).toBeVisible();

    const apply = page.locator('.nge-dialog__footer button.nge-btn--primary', { hasText: 'Apply' });
    await expect(apply).toBeVisible();
    const probe = await page.evaluate(() => {
        const el = document.querySelector('.nge-dialog__footer button.nge-btn--primary') as HTMLElement;
        const plain = document.querySelector(
            '.nge-dialog__footer button.nge-btn:not(.nge-btn--primary)',
        ) as HTMLElement | null;
        const root = getComputedStyle(document.documentElement);
        const resolve = (v: string) => {
            const t = document.createElement('i');
            t.style.background = root.getPropertyValue(v);
            document.body.appendChild(t);
            const c = getComputedStyle(t).backgroundColor;
            t.remove();
            return c;
        };
        return {
            bg: getComputedStyle(el).backgroundColor,
            color: getComputedStyle(el).color,
            primary: resolve('--nge-color-primary'),
            plainBg: plain ? getComputedStyle(plain).backgroundColor : null,
        };
    });
    expect(probe.bg).toBe(probe.primary);
    expect(probe.color).toBe('rgb(255, 255, 255)');
    if (probe.plainBg) expect(probe.plainBg).not.toBe(probe.bg);
});
