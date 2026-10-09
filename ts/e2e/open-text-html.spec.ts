import { test, expect, type Page } from '@playwright/test';
import { boot } from './helpers/editor';

/* Issue #339 — `OPEN_DOCUMENT` with `plain_text` / `html` used to answer
 * `Event::Error` although the engine already had the plain-text model and
 * the rich-paste HTML parser. Driven through the REAL File menu: its hidden
 * file input now accepts `.txt` / `.html`, and the @nge/core facade infers
 * the format from the file name (`docFormatForFileName`). Assertions read
 * the accessibility mirror (paragraph `dir`) and the engine's own clipboard
 * serialization (the table) — engine truth, valid under headless Chrome. */

async function openThroughFileMenu(
    page: Page,
    file: { name: string; mimeType: string; buffer: Buffer },
): Promise<void> {
    await page.locator('.nge-fm__trigger').click();
    const menu = page.locator('.nge-fm__menu').first();
    /* Honest UX: every Open format is engine-real — no pending badge. */
    await expect(menu).not.toContainText('Engine pending');
    await expect(menu).toContainText('.txt');
    await page.locator('.nge-fm input[type="file"]').setInputFiles(file);
}

async function engineHtml(page: Page): Promise<string> {
    return page.evaluate(async () => {
        const dispatch = (window as any).__dispatch;
        await dispatch({ type: 'SELECT_ALL' });
        const clip = await dispatch({ type: 'GET_SELECTION_AS_CLIPBOARD' });
        return clip.type === 'CLIPBOARD_PAYLOAD' ? (clip.html as string) : `<${clip.type}>`;
    });
}

test('a .txt with Arabic and Latin lines opens with per-paragraph directions', async ({
    page,
}) => {
    test.setTimeout(60_000);
    await boot(page);
    await openThroughFileMenu(page, {
        name: 'notes.txt',
        mimeType: 'text/plain',
        buffer: Buffer.from('Hello world\r\nمرحبا بالعالم\nSecond Latin line\n', 'utf8'),
    });

    const paragraphs = page.locator('.a11y-mirror > p');
    await expect(paragraphs).toHaveCount(3, { timeout: 15_000 });
    await expect(paragraphs.nth(0)).toHaveText('Hello world');
    await expect(paragraphs.nth(0)).toHaveAttribute('dir', 'ltr');
    await expect(paragraphs.nth(1)).toHaveText('مرحبا بالعالم');
    await expect(paragraphs.nth(1)).toHaveAttribute('dir', 'rtl');
    await expect(paragraphs.nth(2)).toHaveAttribute('dir', 'ltr');
});

test('a small .html with a table opens with the table and dir="rtl" honoured', async ({
    page,
}) => {
    test.setTimeout(60_000);
    await boot(page);
    const html =
        '<!DOCTYPE html><html><head><title>Not text</title></head><body>' +
        '<p dir="rtl">مرحبا</p>' +
        '<table><tr><td>A1</td><td>B1</td></tr><tr><td>A2</td><td>B2</td></tr></table>' +
        '<p>Tail paragraph</p></body></html>';
    await openThroughFileMenu(page, {
        name: 'page.html',
        mimeType: 'text/html',
        buffer: Buffer.from(html, 'utf8'),
    });

    const mirror = page.locator('.a11y-mirror');
    await expect(mirror).toContainText('Tail paragraph', { timeout: 15_000 });
    await expect(mirror).not.toContainText('Not text');
    await expect(mirror.locator(':scope > p').first()).toHaveAttribute('dir', 'rtl');

    const serialized = await engineHtml(page);
    expect(serialized).toContain('<table');
    for (const cell of ['A1', 'B1', 'A2', 'B2']) expect(serialized).toContain(cell);
});
