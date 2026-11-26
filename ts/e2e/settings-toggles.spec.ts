import { test, expect, type Page } from '@playwright/test';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { boot } from './helpers/editor';
import { readZipEntry } from './helpers/docx';

/* Issue #354 — `word/settings.xml` `<w:trackRevisions/>` and
 * `<w:mirrorMargins/>` were never read.
 *
 * - A document Word saved with Track Changes on opened with review mode
 *   OFF: the toolbar's Track toggle was off and typing was not recorded.
 *   Now the open seeds review mode from the setting, typing records an
 *   insertion, and turning the toggle off is written back to the saved
 *   `settings.xml` (exactly that element; nothing else in the part moves).
 * - Under mirrored margins every page laid out with `w:left` on the left.
 *   Now an even page swaps the inside / outside margins: page 2's text
 *   starts at the half-inch outside margin, pages 1 and 3 at the 2-inch
 *   inside one.
 *
 * Fixtures: `crates/format-docx/tests/fixtures/{track_revisions,
 * mirror_margins}.docx` (`format_docx::test_fixtures`). Everything under
 * test goes through the REAL UI: the File menu's file input opens the
 * packages and its Save item downloads them, the Track button toggles
 * review mode, the hidden textarea takes the keys. Evidence is DOM-only
 * (the toggle's `aria-pressed`, the caret overlay's position) or the
 * downloaded file — valid under headless Chrome. */

const fixture = (name: string): Buffer =>
    readFileSync(
        fileURLToPath(new URL(`../../crates/format-docx/tests/fixtures/${name}`, import.meta.url)),
    );

const DOCX = 'application/vnd.openxmlformats-officedocument.wordprocessingml.document';

async function openDocx(page: Page, name: string, buffer: Buffer): Promise<void> {
    await page.locator('.nge-fm input[type="file"]').setInputFiles({
        name,
        mimeType: DOCX,
        buffer,
    });
}

/** File → Save, the downloaded package's bytes. */
async function saveViaMenu(page: Page): Promise<Uint8Array> {
    const download = page.waitForEvent('download');
    await page.locator('.nge-fm__trigger').click();
    await page.locator('.nge-fm__item[data-nge-command="SAVE_DOCUMENT"]').first().click();
    const path = await (await download).path();
    return new Uint8Array(readFileSync(path));
}

test('a document saved with Track Changes on opens in review mode; turning it off is saved', async ({
    page,
}) => {
    test.setTimeout(90_000);
    await boot(page);
    const track = page.getByRole('button', { name: 'Track changes', exact: true });
    await expect(track).toHaveAttribute('aria-pressed', 'false');

    const source = fixture('track_revisions.docx');
    await openDocx(page, 'track_revisions.docx', source);
    const paragraphs = page.locator('.a11y-mirror > p');
    await expect(paragraphs.nth(1)).toHaveText('Typing here is recorded as an insertion.', {
        timeout: 15_000,
    });
    /* The toggle reflects the document's setting (the #354 bug: off). */
    await expect(track).toHaveAttribute('aria-pressed', 'true');

    /* Typing is recorded as a tracked insertion. */
    const input = page.locator('textarea[data-nge-hidden-input]');
    await input.focus();
    await page.keyboard.press('Control+End');
    await page.keyboard.type(' Yes.');
    await expect(paragraphs.nth(1)).toHaveText('Typing here is recorded as an insertion. Yes.');

    /* Track off through the toolbar, then File → Save. */
    await track.click();
    await expect(track).toHaveAttribute('aria-pressed', 'false');
    const saved = await saveViaMenu(page);
    const settings = readZipEntry(saved, 'word/settings.xml');
    const sourceSettings = readZipEntry(new Uint8Array(source), 'word/settings.xml');
    expect(sourceSettings).toContain('<w:trackRevisions/>');
    /* Exactly the element goes; every other byte of the part stays. */
    expect(settings).toBe(sourceSettings.replace('<w:trackRevisions/>', ''));
    /* The insertion typed while tracking was on is a `<w:ins>`. */
    expect(readZipEntry(saved, 'word/document.xml')).toMatch(/<w:ins [^>]*>.*Yes\./s);

    /* Reopen the saved file with the toggle ON beforehand: the open sets
       review mode from the document, which now says off. */
    await track.click();
    await expect(track).toHaveAttribute('aria-pressed', 'true');
    await openDocx(page, 'saved.docx', Buffer.from(saved));
    await expect(paragraphs.nth(1)).toContainText('Yes.', { timeout: 15_000 });
    await expect(track).toHaveAttribute('aria-pressed', 'false');
});

/** The caret overlay's x on page `idx`, as a fraction of that page's
 *  width (the caret is drawn inside the page card it sits on). */
async function caretFractionOn(page: Page, idx: number): Promise<number> {
    return page.evaluate((i) => {
        const pageEl = document.querySelector<HTMLElement>(`.editor-page[data-page-index="${i}"]`);
        const caret = pageEl?.querySelector<HTMLElement>('.caret');
        if (!pageEl || !caret) throw new Error(`no caret on page ${i}`);
        return parseFloat(caret.style.left) / pageEl.getBoundingClientRect().width;
    }, idx);
}

/** How far page `idx`'s caret is from `want` (NaN while it is not there). */
const distance = (page: Page, idx: number, want: number): Promise<number> =>
    caretFractionOn(page, idx).then(
        (got) => Math.abs(got - want),
        () => Number.NaN,
    );

test('mirrored margins: an even page starts its lines at the outside margin', async ({ page }) => {
    test.setTimeout(90_000);
    await boot(page);
    await openDocx(page, 'mirror_margins.docx', fixture('mirror_margins.docx'));
    const paragraphs = page.locator('.a11y-mirror > p');
    await expect(paragraphs).toHaveCount(3, { timeout: 15_000 });
    await expect(paragraphs.nth(2)).toHaveText('Page three is odd again.');

    /* A4 is 595.3 pt wide; `w:left` (inside) is 144 pt, `w:right`
       (outside) 36 pt. */
    const inside = 144 / 595.3;
    const outside = 36 / 595.3;

    const input = page.locator('textarea[data-nge-hidden-input]');
    await input.focus();
    await page.keyboard.press('Control+Home');
    await expect
        .poll(() => distance(page, 0, inside), { timeout: 5_000 })
        .toBeLessThan(0.01);

    /* Down onto page 2, then to its line start. */
    await page.keyboard.press('ArrowDown');
    await page.keyboard.press('Home');
    await expect
        .poll(() => distance(page, 1, outside), { timeout: 5_000 })
        .toBeLessThan(0.01);

    /* Page 3 is odd again. */
    await page.keyboard.press('ArrowDown');
    await page.keyboard.press('Home');
    await expect
        .poll(() => distance(page, 2, inside), { timeout: 5_000 })
        .toBeLessThan(0.01);
});
