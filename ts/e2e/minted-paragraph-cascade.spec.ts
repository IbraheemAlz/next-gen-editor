import { test, expect, type Page } from '@playwright/test';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { boot, documentText, settle } from './helpers/editor';

/* Issue #560 — paragraphs the ENGINE creates (an inserted table's cells
 * and the paragraph after it) take the document's style cascade when they
 * are made: `<w:docDefaults>` + the default paragraph style (`Normal`).
 * They used to be bare paragraphs that laid out WITHOUT Normal's spacing
 * and line rule until the document was saved and re-opened — so a table
 * grew taller across a save.
 *
 * Fixture: `crates/format-docx/tests/fixtures/default_style_rtl.docx`
 * (`Normal`: RTL, 6 pt before / 12 pt after, 1.5 lines).
 *
 * Everything goes through the REAL UI: File -> Open, the toolbar's
 * "Insert 3x3 table" button, the keyboard on the hidden textarea, and
 * File -> Save, whose download is opened again through File -> Open. The
 * evidence is the caret overlay (plain DOM, valid under headless Chrome):
 * the table's height — from its first row to the paragraph after it — is
 * the same before and after the save + reopen. */

const FIXTURE = fileURLToPath(
    new URL('../../crates/format-docx/tests/fixtures/default_style_rtl.docx', import.meta.url),
);
const INPUT = 'textarea[data-nge-hidden-input]';
const DOCX = 'application/vnd.openxmlformats-officedocument.wordprocessingml.document';

/** The caret overlay's top in the viewport's SCROLL content (CSS px). */
async function caretY(page: Page): Promise<number> {
    await settle(page);
    return page.evaluate(() => {
        const c = document.querySelector<HTMLElement>('.caret');
        const v = document.querySelector<HTMLElement>('.editor-viewport');
        if (!c || !v) throw new Error('no caret overlay / viewport');
        return c.getBoundingClientRect().top - v.getBoundingClientRect().top + v.scrollTop;
    });
}

async function open(page: Page, name: string, buffer: Buffer): Promise<void> {
    await page.locator('.nge-fm input[type="file"]').setInputFiles({ name, mimeType: DOCX, buffer });
}

test('an inserted table lays out the same before and after save + reopen', async ({ page }) => {
    test.setTimeout(90_000);
    await boot(page);
    await open(page, 'default_style_rtl.docx', readFileSync(FIXTURE));
    await expect.poll(() => documentText(page), { timeout: 15_000 }).toContain('Direct LTR override');
    const mirror = page.locator('.a11y-mirror');
    await expect(mirror.locator(':scope > p')).toHaveCount(6);

    /* The table goes after the last paragraph; the caret parks in its
       first cell, and the engine adds the paragraph after it. */
    const input = page.locator(INPUT);
    await input.focus();
    await page.keyboard.press('Control+End');
    await page.getByRole('button', { name: 'Insert 3×3 table' }).click();
    await expect(mirror.locator('td[role="gridcell"]')).toHaveCount(9);
    await input.focus();
    const firstRow = await caretY(page);
    await page.keyboard.press('Control+End');
    const after = await caretY(page);
    const height = after - firstRow;
    expect(height, 'the paragraph after the table is below it').toBeGreaterThan(0);

    /* File -> Save, then open the very bytes that were saved. */
    const download = page.waitForEvent('download');
    await page.locator('.nge-fm__trigger').click();
    await page.locator('.nge-fm__item[data-nge-command="SAVE_DOCUMENT"]').first().click();
    const saved = await (await download).path();
    await open(page, 'reopened.docx', readFileSync(saved));
    await expect.poll(() => mirror.locator('td[role="gridcell"]').count(), { timeout: 15_000 }).toBe(9);

    await input.focus();
    await page.keyboard.press('Control+End');
    const afterAgain = await caretY(page);
    for (let row = 0; row < 3; row++) await page.keyboard.press('ArrowUp');
    const firstRowAgain = await caretY(page);
    expect(
        Math.abs(afterAgain - firstRowAgain - height),
        `table height ${height} in the session vs ${afterAgain - firstRowAgain} reopened`,
    ).toBeLessThan(0.5);
});
