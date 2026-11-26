import { test, expect, type Page } from '@playwright/test';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { boot, documentText } from './helpers/editor';

/* Issue #558 — the DEFAULT CHARACTER style (`DefaultParagraphFont`,
 * `w:type="character" w:default="1"`) formats every run that names no
 * character style (ECMA-376 §17.7.4.17). The reader used to record it and
 * apply nothing: a document whose Default Paragraph Font is bold 16 pt
 * showed its plain runs regular 11 pt (the docDefaults), and the toolbar
 * said so.
 *
 * Fixture: `crates/format-docx/tests/fixtures/default_char_style.docx`
 * (`format_docx::test_fixtures::default_char_style_docx`): a plain run, a
 * run in `Quiet` (a character style NOT based on it: italic), one in `Loud`
 * (based on it), one with a direct `<w:b w:val="0"/>`, an empty paragraph,
 * one in the `Aside` paragraph style.
 *
 * Everything goes through the REAL UI: the File menu's file input opens
 * the package, the hidden textarea takes the arrow keys and the typing,
 * the toolbar's Bold / Italic buttons and font-size box report the caret's
 * formatting (DOM-only evidence, valid under headless Chrome). */

const FIXTURE = fileURLToPath(
    new URL('../../crates/format-docx/tests/fixtures/default_char_style.docx', import.meta.url),
);

interface Toolbar {
    bold: string | null;
    italic: string | null;
    size: string;
}

async function toolbar(page: Page): Promise<Toolbar> {
    return {
        bold: await page.locator('.nge-tfmt__btn--bold').getAttribute('aria-pressed'),
        italic: await page.locator('.nge-tfmt__btn--italic').getAttribute('aria-pressed'),
        size: await page.locator('input[aria-label="Font size (pt)"]').inputValue(),
    };
}

/** Put the caret one character into paragraph `i` (Ctrl+Home, i x Down,
 *  Right) and wait for the toolbar to report `want`. */
async function expectAt(page: Page, i: number, want: Toolbar): Promise<void> {
    await page.keyboard.press('Control+Home');
    for (let k = 0; k < i; k++) await page.keyboard.press('ArrowDown');
    await page.keyboard.press('ArrowRight');
    await expect.poll(() => toolbar(page), { timeout: 5_000 }).toEqual(want);
}

test('runs naming no character style take the default one', async ({ page }) => {
    test.setTimeout(60_000);
    await boot(page);

    await page.locator('.nge-fm input[type="file"]').setInputFiles({
        name: 'default_char_style.docx',
        mimeType: 'application/vnd.openxmlformats-officedocument.wordprocessingml.document',
        buffer: readFileSync(FIXTURE),
    });
    await expect.poll(() => documentText(page), { timeout: 15_000 }).toContain('An aside over it');
    await expect(page.locator('.a11y-mirror > p')).toHaveCount(6);

    const input = page.locator('textarea[data-nge-hidden-input]');
    await input.focus();
    /* The plain run: bold 16 pt from Default Paragraph Font (the #558 bug:
       not bold, 11 pt). */
    await expectAt(page, 0, { bold: 'true', italic: 'false', size: '16' });
    /* Quiet is not based on it: italic, the docDefaults' 11 pt. */
    await expectAt(page, 1, { bold: 'false', italic: 'true', size: '11' });
    /* Loud is based on it. */
    await expectAt(page, 2, { bold: 'true', italic: 'false', size: '16' });
    /* A direct <w:b w:val="0"/> wins over it; its size still applies. */
    await expectAt(page, 3, { bold: 'false', italic: 'false', size: '16' });
    /* Above the Aside paragraph style's italic runs. */
    await expectAt(page, 5, { bold: 'true', italic: 'true', size: '16' });

    /* Enter at the end of the plain paragraph and typing: the new run is
       in the default character style too. */
    await page.keyboard.press('Control+Home');
    await page.keyboard.press('End');
    await page.keyboard.press('Enter');
    await page.keyboard.type('New run');
    await expect(page.locator('.a11y-mirror > p').nth(1)).toHaveText('New run');
    await page.keyboard.press('ArrowLeft');
    await expect
        .poll(() => toolbar(page), { timeout: 5_000 })
        .toEqual({ bold: 'true', italic: 'false', size: '16' });
});
