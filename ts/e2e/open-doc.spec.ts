import { test, expect } from '@playwright/test';
import { boot } from './helpers/editor';
import { buildDoc } from './helpers/doc';

/* Issue #567 — a legacy Word 97-2003 binary document (`.doc`) opens through
 * the REAL File menu: its hidden file input accepts `.doc`, the @nge/core
 * facade sends it as `docx` (the name never decides the format), and the
 * engine routes it by its compound-file signature to `format-doc`. Before
 * #567 the same file was refused (`InvalidDocument`). Assertions read the
 * accessibility mirror (paragraph text and `dir`, the bold run's inline
 * style) — engine truth, valid under headless Chrome — the open-warnings
 * banner (a table cell is not imported as a table: Honest UX says so), and
 * the File → Save download name (`.doc` -> `.docx`). The `.doc` is built in
 * memory (`helpers/doc.ts`); no binary fixture is committed. */

const DOC = buildDoc([
    {
        runs: [
            { text: 'Legacy ' },
            { text: 'bold', bold: true },
            { text: ' document' },
        ],
    },
    { runs: [{ text: 'مرحبا بالعالم', rtl: true }], rtl: true, center: true },
    { runs: [{ text: 'cell text' }], cell: true },
    { runs: [{ text: 'Last paragraph.' }] },
]);

test('a .doc opens through File -> Open with its text, bold run and RTL paragraph', async ({
    page,
}) => {
    test.setTimeout(60_000);
    await boot(page);

    await page.locator('.nge-fm__trigger').click();
    const menu = page.locator('.nge-fm__menu').first();
    await expect(menu).toContainText('.doc ');
    await expect(menu).not.toContainText('Engine pending');
    await page.locator('.nge-fm input[type="file"]').setInputFiles({
        name: 'legacy.doc',
        mimeType: 'application/msword',
        buffer: Buffer.from(DOC),
    });

    const paragraphs = page.locator('.a11y-mirror > p');
    await expect(paragraphs).toHaveCount(4, { timeout: 15_000 });
    await expect(paragraphs.nth(0)).toHaveText('Legacy bold document');
    await expect(paragraphs.nth(0)).toHaveAttribute('dir', 'ltr');
    /* One span per style run; only the bold run carries the weight. */
    const spans = paragraphs.nth(0).locator('span');
    await expect(spans).toHaveText(['Legacy ', 'bold', ' document']);
    await expect(spans.nth(1)).toHaveCSS('font-weight', '700');
    await expect(spans.nth(0)).not.toHaveCSS('font-weight', '700');
    await expect(paragraphs.nth(1)).toHaveText('مرحبا بالعالم');
    await expect(paragraphs.nth(1)).toHaveAttribute('dir', 'rtl');
    await expect(paragraphs.nth(2)).toHaveText('cell text');
    await expect(paragraphs.nth(3)).toHaveText('Last paragraph.');
    /* No refusal banner: the file opened. */
    await expect(page.locator('.nge-fm__error')).toHaveCount(0);

    /* Honest UX: the table cell came in as a plain paragraph, and the open
     * says so. */
    const warnings = page.locator('.nge-open-warnings');
    await expect(warnings).toBeVisible();
    await expect(warnings).toHaveAttribute('data-kinds', 'LegacyContentNotImported');
    await expect(warnings).toContainText('Word 97-2003 (.doc)');
    await expect(warnings.locator('.nge-open-warnings__item')).toContainText('tables');

    /* A save writes .docx, named after the opened file. */
    const download = page.waitForEvent('download');
    await page.locator('.nge-fm__trigger').click();
    await page.locator('.nge-fm__item[data-nge-command="SAVE_DOCUMENT"]').first().click();
    expect((await download).suggestedFilename()).toBe('legacy.docx');
});
