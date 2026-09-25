import { test, expect } from '@playwright/test';

/* Issue #262 — the toolbar's "Accept all" / "Reject all" are ONE engine
 * command each (`ACCEPT_ALL_REVISIONS` / `REJECT_ALL_REVISIONS`), not a
 * loop of per-revision accepts: exactly one new undo snapshot per click,
 * and every tracked change resolved. Assertions ride command replies
 * (`SELECTION_CHANGED.undo_depth`, the clipboard payload's plain text) —
 * engine truth, valid under headless Chrome.
 */

declare global {
    interface Window {
        __dispatch: (cmd: unknown) => Promise<any>;
        __paintIdle?: boolean;
    }
}

async function boot(page: import('@playwright/test').Page) {
    await page.goto('/');
    await page.waitForFunction(() => (window as any).__paintIdle === true, undefined, {
        timeout: 20_000,
    });
}

/** Type `base`, then `tracked` with track changes on; returns the undo
 *  depth before the toolbar click. */
async function seedTrackedEdit(
    page: import('@playwright/test').Page,
    base: string,
    tracked: string,
): Promise<number> {
    return page.evaluate(
        async ({ base, tracked }) => {
            const dispatch = (cmd: unknown): Promise<any> => (window as any).__dispatch(cmd);
            await dispatch({ type: 'SELECT_ALL' });
            await dispatch({ type: 'INSERT_TEXT', at: undefined, text: base });
            await dispatch({ type: 'TOGGLE_TRACK_CHANGES', enabled: true });
            await dispatch({ type: 'INSERT_TEXT', at: undefined, text: tracked });
            const off = await dispatch({ type: 'TOGGLE_TRACK_CHANGES', enabled: false });
            return off.undo_depth as number;
        },
        { base, tracked },
    );
}

async function state(page: import('@playwright/test').Page) {
    return page.evaluate(async () => {
        const dispatch = (cmd: unknown): Promise<any> => (window as any).__dispatch(cmd);
        const sel = await dispatch({ type: 'SELECT_ALL' });
        const clip = await dispatch({ type: 'GET_SELECTION_AS_CLIPBOARD', include_docx: false });
        return { depth: sel.undo_depth as number, plain: clip.plain as string };
    });
}

test('Reject all is one engine command: one undo step, tracked text gone', async ({ page }) => {
    await boot(page);
    const before = await seedTrackedEdit(page, 'alpha', ' beta');
    expect((await state(page)).plain).toBe('alpha beta');
    await page.getByRole('button', { name: 'Reject all revisions' }).click();
    await expect.poll(async () => (await state(page)).plain).toBe('alpha');
    expect((await state(page)).depth).toBe(before + 1);
});

test('Accept all is one engine command: one undo step, tracked text kept', async ({ page }) => {
    await boot(page);
    const before = await seedTrackedEdit(page, 'alpha', ' beta');
    await page.getByRole('button', { name: 'Accept all revisions' }).click();
    await expect.poll(async () => (await state(page)).depth).toBe(before + 1);
    expect((await state(page)).plain).toBe('alpha beta');
    /* Nothing left to resolve: a second click pushes no undo step. */
    await page.getByRole('button', { name: 'Accept all revisions' }).click();
    await page.getByRole('button', { name: 'Reject all revisions' }).click();
    await page.waitForTimeout(200);
    const after = await state(page);
    expect(after.depth).toBe(before + 1);
    expect(after.plain).toBe('alpha beta');
});
