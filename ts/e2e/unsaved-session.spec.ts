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
async function cleanMarker(page: Page, dbName = 'engine-log'): Promise<boolean | null> {
    return page.evaluate(
        (name) =>
            new Promise<boolean | null>((resolve, reject) => {
                const open = indexedDB.open(name);
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
        dbName,
    );
}

/** Issue #426 - the event-log database this tab's page logs into. */
async function logDbOf(page: Page): Promise<string> {
    return page.evaluate(
        () => (JSON.parse(sessionStorage.getItem('nge.tab') ?? '{}') as { db?: string }).db ?? '',
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
    await expect.poll(async () => cleanMarker(page, await logDbOf(page))).toBe(false);
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

/* ---- Issue #426: per-tab sessions + the archive ring ------------------ */

const ROWS = `${BANNER} .nge-recovery-banner__choices`;

async function typeInto(page: Page, text: string): Promise<void> {
    await typeSomething(page, text);
    await expect.poll(async () => cleanMarker(page, await logDbOf(page))).toBe(false);
}

async function reloaded(page: Page): Promise<void> {
    await page.reload();
    await page.waitForFunction(() => (window as any).__paintIdle === true);
}

test("#426: a second tab never archives or clears the first tab's live session", async ({
    context,
}) => {
    test.setTimeout(120_000);
    const a = await context.newPage();
    watchDialogs(a);
    await boot(a);
    await typeInto(a, 'alpha tab words');
    expect(await logDbOf(a)).toBe('engine-log');

    const b = await context.newPage();
    watchDialogs(b);
    await boot(b);
    /* B is a fresh session: no offer, and it logs into its OWN database. */
    await expect(b.locator(BANNER)).toHaveCount(0);
    expect(await logDbOf(b)).toMatch(/^engine-log-.+/);
    expect(await documentText(b)).not.toContain('alpha tab words');

    /* A kept its text, and its log is still marked unsaved. */
    expect(await documentText(a)).toContain('alpha tab words');
    expect(await cleanMarker(a)).toBe(false);

    await typeInto(b, 'beta tab words');
    expect(await documentText(a)).not.toContain('beta tab words');
    expect(await documentText(b)).not.toContain('alpha tab words');

    /* Reloading A offers only A's own session back (never B's). */
    await reloaded(a);
    await expect(a.locator(BANNER)).toBeVisible();
    await expect(a.locator(ROWS)).toHaveCount(1);
    await a.locator(`${BANNER} .nge-recovery-banner__recover`).click();
    await expect.poll(() => textOrEmpty(a), { timeout: 30_000 }).toContain('alpha tab words');
    expect(await documentText(a)).not.toContain('beta tab words');
    /* ... and B is untouched by all of it. */
    expect(await documentText(b)).toContain('beta tab words');
});

test("#426: a closed tab's unsaved session is offered to the next tab that boots", async ({
    context,
}) => {
    test.setTimeout(120_000);
    const a = await context.newPage();
    watchDialogs(a);
    await boot(a);
    const b = await context.newPage();
    watchDialogs(b);
    await boot(b);
    await typeInto(b, 'orphaned tab words');
    const dbB = await logDbOf(b);
    expect(dbB).not.toBe('engine-log');
    await b.close({ runBeforeUnload: false });

    /* B's heartbeat is left behind; age it past the reload grace instead
       of waiting seconds for it. */
    await a.evaluate(() => {
        const mine = (JSON.parse(sessionStorage.getItem('nge.tab') ?? '{}') as { token?: string })
            .token;
        for (let i = 0; i < localStorage.length; i++) {
            const k = localStorage.key(i);
            if (!k || !k.startsWith('nge.tab-hb.') || k.endsWith(String(mine))) continue;
            const v = JSON.parse(localStorage.getItem(k) ?? '{}');
            localStorage.setItem(k, JSON.stringify({ ...v, at: 0 }));
        }
    });

    const c = await context.newPage();
    watchDialogs(c);
    await c.goto('/');
    await c.waitForFunction(() => (window as any).__paintIdle === true, undefined, {
        timeout: 20_000,
    });
    await expect(c.locator(BANNER)).toBeVisible();
    await expect(c.locator(ROWS)).toHaveCount(1);
    /* A is still live, so C logs on its own; B's database is gone. */
    expect(await logDbOf(c)).toMatch(/^engine-log-.+/);
    expect(
        await c.evaluate(async () => (await indexedDB.databases()).map((d) => d.name)),
    ).not.toContain(dbB);
    await c.locator(`${BANNER} .nge-recovery-banner__recover`).click();
    await expect.poll(() => textOrEmpty(c), { timeout: 30_000 }).toContain('orphaned tab words');
    /* A never noticed. */
    expect(await documentText(a)).not.toContain('orphaned tab words');
});

test('#426: an undecided offer survives a newer unsaved session (ring of 3)', async ({ page }) => {
    test.setTimeout(120_000);
    watchDialogs(page);
    await boot(page);
    await typeInto(page, 'first unsaved session');
    await reloaded(page);
    await expect(page.locator(ROWS)).toHaveCount(1);
    /* Not decided: type in the fresh session and reload again. */
    await typeInto(page, 'second unsaved session');
    await reloaded(page);
    await expect(page.locator(ROWS)).toHaveCount(2);

    /* A third and fourth session: the ring keeps the newest three. */
    await typeInto(page, 'third unsaved session');
    await reloaded(page);
    await expect(page.locator(ROWS)).toHaveCount(3);
    await typeInto(page, 'fourth unsaved session');
    await reloaded(page);
    await expect(page.locator(ROWS)).toHaveCount(3);

    /* Rows are newest first: the last row is the oldest survivor, "second". */
    await page.locator(ROWS).last().locator('.nge-recovery-banner__recover').click();
    await expect.poll(() => textOrEmpty(page), { timeout: 30_000 }).toContain(
        'second unsaved session',
    );
    expect(await documentText(page)).not.toContain('first unsaved session');
    /* The other two stay offered. */
    await expect(page.locator(ROWS)).toHaveCount(2);
});

test('#426: a dismissed (seen) entry is evicted before an undecided one', async ({ page }) => {
    test.setTimeout(120_000);
    watchDialogs(page);
    await boot(page);
    for (const t of ['s one', 's two']) {
        await typeInto(page, t);
        await reloaded(page);
    }
    await expect(page.locator(ROWS)).toHaveCount(2);
    /* Dismiss the offer: both entries become "seen". */
    await page.locator('.nge-recovery-banner__dismiss').click();
    await expect(page.locator(BANNER)).toHaveCount(0);
    await typeInto(page, 's three');
    await reloaded(page);
    await typeInto(page, 's four');
    await reloaded(page);
    /* Ring of 3, four sessions: a SEEN entry ("s one", the oldest) went, not
       the undecided "s three". Rows are newest first: four, three, two. */
    await expect(page.locator(ROWS)).toHaveCount(3);
    await page.locator(ROWS).last().locator('.nge-recovery-banner__recover').click();
    await expect.poll(() => textOrEmpty(page), { timeout: 30_000 }).toContain('s two');
});
