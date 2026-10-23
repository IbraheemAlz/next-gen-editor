import { test, expect, type Page } from '@playwright/test';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { boot, documentText } from './helpers/editor';

/* Issue #556 — a document whose `<w:bidi/>` lives only on its DEFAULT
 * paragraph style (`Normal`, `w:default="1"`) is an RTL document: Word
 * gives `Normal` to every paragraph that names no style, so its EMPTY
 * unstyled paragraph is right-to-left — before the fix the reader applied
 * only `<w:docDefaults>` and the empty line fell to the editor's LTR boot
 * base (caret at the left margin, mirrored `dir="ltr"`, Enter starting
 * LTR paragraphs).
 *
 * Fixture: `crates/format-docx/tests/fixtures/default_style_rtl.docx`
 * (`format_docx::test_fixtures::default_style_rtl_docx`): an Arabic
 * paragraph, an empty one, a Latin-first one, one naming an undefined
 * style, one in `Plain` (not based on `Normal`), one with a direct
 * `<w:bidi w:val="0"/>`.
 *
 * Everything the behaviour depends on goes through the REAL UI: the File
 * menu's file input opens the package, the hidden textarea takes the
 * arrow key, the typing and Enter. Evidence is DOM-only (the a11y mirror's
 * `dir`, the caret overlay's position) — valid under headless Chrome. */

const FIXTURE = fileURLToPath(
    new URL('../../crates/format-docx/tests/fixtures/default_style_rtl.docx', import.meta.url),
);

const mirrorDirs = (page: Page): Promise<Array<string | null>> =>
    page
        .locator('.a11y-mirror > p')
        .evaluateAll((els) => els.map((el) => el.getAttribute('dir')));

/** The caret overlay's x as a fraction of page 0's width. */
async function caretFraction(page: Page): Promise<number> {
    return page.evaluate(() => {
        const pageEl = document.querySelector<HTMLElement>('.editor-page[data-page-index="0"]');
        const caret = pageEl?.querySelector<HTMLElement>('.caret');
        if (!pageEl || !caret) throw new Error('no caret on page 0');
        return parseFloat(caret.style.left) / pageEl.getBoundingClientRect().width;
    });
}

test('a Normal-only <w:bidi/> makes the empty unstyled paragraph RTL', async ({ page }) => {
    test.setTimeout(60_000);
    await boot(page);

    /* The real File -> Open flow. */
    await page.locator('.nge-fm input[type="file"]').setInputFiles({
        name: 'default_style_rtl.docx',
        mimeType: 'application/vnd.openxmlformats-officedocument.wordprocessingml.document',
        buffer: readFileSync(FIXTURE),
    });
    await expect.poll(() => documentText(page), { timeout: 15_000 }).toContain('Dangling style id');

    const paragraphs = page.locator('.a11y-mirror > p');
    await expect(paragraphs).toHaveCount(6);
    await expect(paragraphs.nth(1)).toHaveText('');
    expect(await mirrorDirs(page)).toEqual([
        'rtl', // unstyled Arabic
        'rtl', // unstyled EMPTY — RTL only through Normal (the #556 bug: 'ltr')
        'rtl', // unstyled Latin-first — Normal's bidi, not first-strong
        'rtl', // an undefined pStyle takes Normal
        'ltr', // Plain is not based on Normal
        'ltr', // a direct <w:bidi w:val="0"/> beats Normal
    ]);

    /* Into the empty paragraph with the arrow key: its caret sits at the
       RIGHT margin, where an RTL line starts. (`documentText` selected
       everything; collapse to the document start first.) */
    const input = page.locator('textarea[data-nge-hidden-input]');
    await input.focus();
    await page.keyboard.press('Control+Home');
    await page.keyboard.press('ArrowDown');
    await expect.poll(() => caretFraction(page), { timeout: 5_000 }).toBeGreaterThan(0.6);

    /* Latin typed into it keeps the paragraph RTL, and Enter at its end
       opens another RTL paragraph (Normal again). */
    await page.keyboard.type('Latin');
    await expect(paragraphs.nth(1)).toHaveText('Latin');
    await page.keyboard.press('Enter');
    await expect(paragraphs).toHaveCount(7);
    await expect(paragraphs.nth(2)).toHaveText('');
    expect((await mirrorDirs(page)).slice(0, 3)).toEqual(['rtl', 'rtl', 'rtl']);
    await expect.poll(() => caretFraction(page), { timeout: 5_000 }).toBeGreaterThan(0.6);
});
