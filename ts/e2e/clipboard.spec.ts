import { test, expect, type Page } from '@playwright/test';

/* Issue #48 — silent clipboard copy failure.
 *
 * copy/cut await a worker round-trip (GET_SELECTION_AS_CLIPBOARD) before
 * touching `navigator.clipboard`, so the write lands outside the trusted
 * copy-event window and can be blocked (NotAllowedError "Document is not
 * focused"). The fix hardens the write ladder (rich → plain → refocus +
 * retry), surfaces total failure in a visible `role="alert"` banner, and
 * pins the write-before-delete invariant: a blocked cut deletes NOTHING. */

const SEED = 'clipboard-spec payload ';

async function boot(page: Page): Promise<void> {
    await page.goto('/');
    await page.waitForFunction(() => (window as any).__paintIdle === true, undefined, {
        timeout: 30_000,
    });
}

/** Insert a known payload at the caret, then select the whole document. */
async function seedAndSelectAll(page: Page): Promise<void> {
    await page.evaluate(async (seed) => {
        const dispatch = (window as any).__dispatch;
        await dispatch({ type: 'INSERT_TEXT', at: undefined, text: seed });
        await dispatch({ type: 'SELECT_ALL' });
    }, SEED);
}

/** Focus the hidden textarea (the OS text-input surface) so Ctrl+C/X fire
 *  the native copy/cut events HiddenInput listens for. */
async function focusHiddenInput(page: Page): Promise<void> {
    await page.locator('textarea[data-nge-hidden-input]').focus();
}

/** Every unhandled promise rejection recorded since `BLOCK_CLIPBOARD_INIT`
 *  (or `RECORD_REJECTIONS_INIT`) installed its listener. Issue #55 fixed
 *  the one pre-existing source of boot-time noise ("engine not
 *  initialized" from `ReviewControls`'s pre-INIT dispatch) — this no
 *  longer needs to filter down to clipboard-only reasons; ANY rejection
 *  here is a bug. */
async function unhandledRejections(page: Page): Promise<string[]> {
    return page.evaluate(() => (window as any).__unhandledRejections as string[]);
}

/** Install just the `unhandledrejection` recorder, without stubbing the
 *  clipboard — for specs that assert boot cleanliness on its own. */
const RECORD_REJECTIONS_INIT = (): void => {
    (window as any).__unhandledRejections = [];
    window.addEventListener('unhandledrejection', (e) => {
        (window as any).__unhandledRejections.push(String(e.reason));
    });
};

/** Stub both clipboard write tiers to reject like a focus-lapsed document,
 *  and record any unhandled promise rejections to a window global. Must run
 *  as an init script (before the app boots). */
const BLOCK_CLIPBOARD_INIT = (): void => {
    (window as any).__unhandledRejections = [];
    window.addEventListener('unhandledrejection', (e) => {
        (window as any).__unhandledRejections.push(String(e.reason));
    });
    const blocked = (): Promise<never> =>
        Promise.reject(new DOMException('Document is not focused.', 'NotAllowedError'));
    Object.defineProperty(navigator.clipboard, 'write', {
        value: blocked,
        configurable: true,
    });
    Object.defineProperty(navigator.clipboard, 'writeText', {
        value: blocked,
        configurable: true,
    });
};

test('copy places the selection on the system clipboard', async ({ page, context }) => {
    await context.grantPermissions(['clipboard-read', 'clipboard-write']);
    await boot(page);
    await seedAndSelectAll(page);
    await focusHiddenInput(page);

    await page.keyboard.press('Control+C');

    /* The write is async (worker round-trip → navigator.clipboard) — poll
       until it lands. The clipboard carries the seeded payload plus the
       boot seed text (SELECT_ALL covers the whole document). */
    await expect
        .poll(() => page.evaluate(() => navigator.clipboard.readText()), {
            timeout: 10_000,
        })
        .toContain(SEED.trim());
});

test('a blocked clipboard write shows a visible error banner and no unhandled rejection', async ({
    page,
}) => {
    await page.addInitScript(BLOCK_CLIPBOARD_INIT);
    await boot(page);
    await seedAndSelectAll(page);
    await focusHiddenInput(page);

    await page.keyboard.press('Control+C');

    const banner = page.locator('.ui-error-banner[role="alert"]');
    await expect(banner).toBeVisible();
    await expect(banner).toContainText('Copy failed');

    /* The rejection must be OBSERVED (caught + surfaced), never silent. */
    expect(await unhandledRejections(page)).toEqual([]);
});

test('cut with a blocked clipboard write deletes nothing (write-before-delete)', async ({
    page,
}) => {
    await page.addInitScript(BLOCK_CLIPBOARD_INIT);
    await boot(page);
    await seedAndSelectAll(page);
    await focusHiddenInput(page);

    await page.keyboard.press('Control+X');

    const banner = page.locator('.ui-error-banner[role="alert"]');
    await expect(banner).toBeVisible();
    await expect(banner).toContainText('Cut failed');

    /* The document text must be UNCHANGED — the delete only dispatches
       after a successful write. Read it back through a pure worker
       round-trip (no system clipboard involved). */
    const plain = await page.evaluate(async () => {
        const dispatch = (window as any).__dispatch;
        await dispatch({ type: 'SELECT_ALL' });
        const evt = await dispatch({ type: 'GET_SELECTION_AS_CLIPBOARD' });
        return evt.plain as string;
    });
    expect(plain).toContain(SEED.trim());
    /* The boot seed survived too — nothing in the selection was lost. */
    expect(plain).toContain('Hello world');

    expect(await unhandledRejections(page)).toEqual([]);
});

/* Issue #55 — a pre-INIT dispatch (`ReviewControls`'s review-identity
 * effect, which mounts unconditionally in the always-on toolbar row)
 * rejected with "engine not initialized" before the worker finished
 * `Command::Init`, and the rejection was void-discarded — an unhandled
 * promise rejection on every cold boot, unrelated to clipboard. */
test('booting the editor records zero unhandled promise rejections', async ({ page }) => {
    await page.addInitScript(RECORD_REJECTIONS_INIT);
    await boot(page);
    /* Settle past the boot sequence's own async tail (font loads, the
       first SELECTION_CHANGED, stats polling's first tick) — `__paintIdle`
       flips before every post-INIT effect has necessarily run. */
    await page.waitForTimeout(2_000);

    expect(await unhandledRejections(page)).toEqual([]);
});
