import { test, expect, type Page } from '@playwright/test';
import { boot, documentText } from './helpers/editor';

/* Issue #388 - a plain reload with unsaved edits no longer loses them.
 *
 * `openEventLog` (INIT) clears the event log, so before #388 an F5 after
 * typing dropped the document. Now:
 *   - the worker keeps a `clean` marker in the log's `meta` store (false
 *     after the first document edit, true again after a successful
 *     SaveDocx / an opened document / an empty log);
 *   - `beforeunload` raises the browser prompt while the document is not
 *     clean;
 *   - the next boot sets an unclean log aside and shows a non-modal
 *     "Recover previous document? / Discard" banner; clearing happens
 *     only on Discard (or a save of the recovered document). */

const BANNER = '.nge-recovery-banner[data-kinds~="previous-session"]';

/** The persisted clean marker, read straight from IndexedDB. */
async function cleanMarker(page: Page): Promise<boolean | null> {
    return page.evaluate(
        () =>
            new Promise<boolean | null>((resolve, reject) => {
                const open = indexedDB.open('engine-log');
                open.onerror = () => reject(open.error);
                open.onsuccess = () => {
                    const db = open.result;
                    const req = db.transaction('meta', 'readonly').objectStore('meta').get('clean');
                    req.onsuccess = () => {
                        db.close();
                        const row = req.result as { clean?: boolean } | undefined;
                        resolve(typeof row?.clean === 'boolean' ? row.clean : null);
                    };
                    req.onerror = () => reject(req.error);
                };
            }),
    );
}

/** Watch for page dialogs; `beforeunload` ones are accepted (leave). */
function watchDialogs(page: Page): string[] {
    const seen: string[] = [];
    page.on('dialog', (d) => {
        seen.push(d.type());
        void d.accept();
    });
    return seen;
}

async function typeSomething(page: Page, text: string): Promise<void> {
    /* A real click + real keystrokes: the beforeunload prompt only shows
       for a page with user activation. */
    await page.locator('.editor-page').first().click({ position: { x: 40, y: 40 } });
    await page.keyboard.type(text);
    await expect.poll(() => documentText(page)).toContain(text);
    /* The marker write is journaled off the critical path. */
    await expect.poll(() => cleanMarker(page)).toBe(false);
}

async function textOrEmpty(page: Page): Promise<string> {
    try {
        return await documentText(page);
    } catch {
        return '';
    }
}

test('type, reload -> banner -> Recover brings the text back', async ({ page }) => {
    test.setTimeout(90_000);
    const dialogs = watchDialogs(page);
    await boot(page);
    await typeSomething(page, 'precious unsaved words');

    await page.reload();
    await page.waitForFunction(() => (window as any).__paintIdle === true);
    expect(dialogs, 'the unsaved-changes prompt was raised').toContain('beforeunload');

    /* The new session is a fresh document; the old one is offered back. */
    await expect(page.locator(BANNER)).toBeVisible();
    expect(await documentText(page)).not.toContain('precious unsaved words');

    await page.locator(`${BANNER} .nge-recovery-banner__recover`).click();
    await expect.poll(() => textOrEmpty(page), { timeout: 30_000 }).toContain(
        'precious unsaved words',
    );
    await expect(page.locator(BANNER)).toHaveCount(0);
    /* Still unsaved: the recovered session is not clean. */
    expect(await cleanMarker(page)).toBe(false);
});

test('Discard starts empty and the offer is gone for good', async ({ page }) => {
    test.setTimeout(90_000);
    watchDialogs(page);
    await boot(page);
    await typeSomething(page, 'throw this away');

    await page.reload();
    await page.waitForFunction(() => (window as any).__paintIdle === true);
    await expect(page.locator(BANNER)).toBeVisible();

    await page.locator(`${BANNER} .nge-recovery-banner__discard`).click();
    await expect(page.locator(BANNER)).toHaveCount(0);
    expect(await documentText(page)).not.toContain('throw this away');

    /* The fresh session is clean: a further reload offers nothing. */
    await page.reload();
    await page.waitForFunction(() => (window as any).__paintIdle === true);
    await expect(page.locator('.nge-shell')).toBeVisible();
    await expect(page.locator(BANNER)).toHaveCount(0);
});

test('an undecided offer survives another reload', async ({ page }) => {
    test.setTimeout(90_000);
    watchDialogs(page);
    await boot(page);
    await typeSomething(page, 'still here after two reloads');

    await page.reload();
    await page.waitForFunction(() => (window as any).__paintIdle === true);
    await expect(page.locator(BANNER)).toBeVisible();
    /* Ignore it (no edits in the fresh session) and reload again. */
    await page.reload();
    await page.waitForFunction(() => (window as any).__paintIdle === true);
    await expect(page.locator(BANNER)).toBeVisible();
    await page.locator(`${BANNER} .nge-recovery-banner__recover`).click();
    await expect.poll(() => textOrEmpty(page), { timeout: 30_000 }).toContain(
        'still here after two reloads',
    );
});

test('save, then reload: no prompt and no banner', async ({ page }) => {
    test.setTimeout(90_000);
    const dialogs = watchDialogs(page);
    await boot(page);
    await typeSomething(page, 'saved before leaving');

    const saved = await page.evaluate(
        async () => ((await (window as any).__dispatch({ type: 'SAVE_DOCX' })) as { type: string }).type,
    );
    expect(saved).toBe('DOCUMENT_SAVED');
    await expect.poll(() => cleanMarker(page)).toBe(true);

    await page.reload();
    await page.waitForFunction(() => (window as any).__paintIdle === true);
    expect(dialogs, 'a saved document does not prompt').not.toContain('beforeunload');
    await expect(page.locator('.nge-shell')).toBeVisible();
    await expect(page.locator(BANNER)).toHaveCount(0);
});

test('a fresh page with only the seed document is clean', async ({ page }) => {
    const dialogs = watchDialogs(page);
    await boot(page);
    expect(await cleanMarker(page)).toBe(true);
    await page.reload();
    await page.waitForFunction(() => (window as any).__paintIdle === true);
    expect(dialogs).not.toContain('beforeunload');
    await expect(page.locator(BANNER)).toHaveCount(0);
});
