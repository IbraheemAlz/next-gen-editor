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

async function boot(page: Page, query = ''): Promise<void> {
    await page.goto(`/${query}`);
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

/* The #48 async-path specs below pin the CACHE-MISS path, so they boot
   with the issue-#57 prefetch disabled — a warm cache would (correctly)
   serve the copy synchronously and never touch the stubbed async API. */
test('a blocked clipboard write shows a visible error banner and no unhandled rejection', async ({
    page,
}) => {
    await page.addInitScript(BLOCK_CLIPBOARD_INIT);
    await boot(page, '?clipboardPrefetch=0');
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
    await boot(page, '?clipboardPrefetch=0');
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

/* ------------------------------------------------------------------------
 * Issue #57 — synchronous clipboard payload cache.
 *
 * A debounced GET_SELECTION_AS_CLIPBOARD { include_docx: false } prefetch
 * keeps the live selection's plain + HTML payload warm, so copy/cut call
 * `e.clipboardData.setData` synchronously inside the trusted event. The
 * async `navigator.clipboard` API is stubbed to REJECT in the warm specs:
 * they can only pass through the synchronous path.
 * ---------------------------------------------------------------------- */

/** Record what the copy/cut handlers put on the event's DataTransfer (read
 *  in a window bubble listener, i.e. after HiddenInput's own handler), then
 *  drop document focus right away — the "focus lapsed mid-flight" case
 *  that used to break the async path. */
const RECORD_SYNC_WRITES_INIT = (): void => {
    (window as any).__syncWrites = [];
    const record = (e: ClipboardEvent): void => {
        (window as any).__syncWrites.push({
            type: e.type,
            plain: e.clipboardData?.getData('text/plain') ?? '',
            html: e.clipboardData?.getData('text/html') ?? '',
            defaultPrevented: e.defaultPrevented,
        });
        (document.activeElement as HTMLElement | null)?.blur();
        document.hasFocus = () => false;
    };
    window.addEventListener('copy', record);
    window.addEventListener('cut', record);
};

type SyncWrite = { type: string; plain: string; html: string; defaultPrevented: boolean };

async function syncWrites(page: Page): Promise<SyncWrite[]> {
    return page.evaluate(() => (window as any).__syncWrites as SyncWrite[]);
}

async function prefetchStats(page: Page): Promise<any> {
    return page.evaluate(() => (window as any).__clipboardPrefetch.stats());
}

async function waitWarm(page: Page): Promise<void> {
    await expect
        .poll(async () => (await prefetchStats(page)).warm, { timeout: 10_000 })
        .toBe(true);
}

test('copy with a warm cache writes synchronously even with the async clipboard blocked and focus lost', async ({
    page,
}) => {
    await page.addInitScript(BLOCK_CLIPBOARD_INIT);
    await page.addInitScript(RECORD_SYNC_WRITES_INIT);
    await boot(page);
    await seedAndSelectAll(page);
    await waitWarm(page);
    await focusHiddenInput(page);

    await page.keyboard.press('Control+C');

    const writes = await syncWrites(page);
    expect(writes).toHaveLength(1);
    expect(writes[0]!.type).toBe('copy');
    expect(writes[0]!.defaultPrevented).toBe(true);
    expect(writes[0]!.plain).toContain(SEED.trim());
    expect(writes[0]!.plain).toContain('Hello world');
    expect(writes[0]!.html).toContain(SEED.trim());

    const stats = await prefetchStats(page);
    expect(stats.hits).toBe(1);
    expect(stats.allSkippedDocx).toBe(true);

    /* The async path (stubbed to reject) never ran: no banner, nothing
       unhandled. Give a would-be async failure time to surface. */
    await page.waitForTimeout(500);
    await expect(page.locator('.ui-error-banner[role="alert"]')).toHaveCount(0);
    expect(await unhandledRejections(page)).toEqual([]);
});

test('cut with a warm cache writes synchronously, then deletes the selection', async ({
    page,
}) => {
    await page.addInitScript(BLOCK_CLIPBOARD_INIT);
    await page.addInitScript(RECORD_SYNC_WRITES_INIT);
    await boot(page);
    await seedAndSelectAll(page);
    await waitWarm(page);
    await focusHiddenInput(page);

    await page.keyboard.press('Control+X');

    const writes = await syncWrites(page);
    expect(writes).toHaveLength(1);
    expect(writes[0]!.type).toBe('cut');
    expect(writes[0]!.plain).toContain(SEED.trim());

    /* Write-before-delete: the synchronous write landed, so the delete
       follows — the document no longer holds the seeded payload. */
    await expect
        .poll(
            () =>
                page.evaluate(async () => {
                    const dispatch = (window as any).__dispatch;
                    await dispatch({ type: 'SELECT_ALL' });
                    const evt = await dispatch({ type: 'GET_SELECTION_AS_CLIPBOARD' });
                    return evt.plain as string;
                }),
            { timeout: 10_000 },
        )
        .not.toContain(SEED.trim());
    await expect(page.locator('.ui-error-banner[role="alert"]')).toHaveCount(0);
    expect(await unhandledRejections(page)).toEqual([]);
});

test('a cold-cache copy still succeeds through the async path', async ({ page, context }) => {
    await context.grantPermissions(['clipboard-read', 'clipboard-write']);
    await page.addInitScript(RECORD_REJECTIONS_INIT);
    await boot(page);
    await seedAndSelectAll(page);
    /* Force a miss: drop the entry and cancel the pending prefetch. */
    await page.evaluate(() => (window as any).__clipboardPrefetch.invalidate());
    await focusHiddenInput(page);

    await page.keyboard.press('Control+C');

    await expect
        .poll(() => page.evaluate(() => navigator.clipboard.readText()), {
            timeout: 10_000,
        })
        .toContain(SEED.trim());
    const stats = await prefetchStats(page);
    expect(stats.hits).toBe(0);
    expect(stats.misses).toBeGreaterThanOrEqual(1);
    expect(await unhandledRejections(page)).toEqual([]);
});

test('drag-selection traffic prefetches at most at the debounce rate', async ({ page }) => {
    await boot(page);
    const before = (await prefetchStats(page)).prefetches as number;

    /* 40 selection extensions ~5 ms apart — a drag in progress. */
    await page.evaluate(async () => {
        const dispatch = (window as any).__dispatch;
        const pos = (offset: number) => ({
            path: { steps: [{ kind: 'BLOCK', idx: 0 }] },
            offset,
        });
        await dispatch({
            type: 'SET_SELECTION',
            range: { start: pos(0), end: pos(0) },
            caret: pos(0),
        });
        for (let i = 1; i <= 40; i += 1) {
            await dispatch({ type: 'EXTEND_SELECTION', to: pos(1 + (i % 10)), modifier: 'None' });
            await new Promise((r) => setTimeout(r, 5));
        }
    });
    /* Let the trailing debounce fire and the prefetch reply land. A fixed
       wait was too short on a loaded machine (the round-trip can exceed
       600 ms while other builds run), so poll for the warm entry like the
       warm-copy specs above do; no further prefetch can run without a new
       selection change, so polling longer cannot inflate `during`. */
    await expect
        .poll(async () => (await prefetchStats(page)).warm, { timeout: 10_000 })
        .toBe(true);

    const stats = await prefetchStats(page);
    const during = (stats.prefetches as number) - before;
    expect(during, 'prefetches for one settled drag').toBeGreaterThanOrEqual(1);
    expect(during, 'prefetches for one settled drag').toBeLessThanOrEqual(2);
    expect(stats.allSkippedDocx).toBe(true);
    expect(stats.warm).toBe(true);
});
