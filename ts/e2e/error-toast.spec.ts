import { test, expect, type Page } from '@playwright/test';
import { boot, documentText } from './helpers/editor';

/* Issue #364 - a typed `Event::Error` from a keyboard path must be VISIBLE.
 *
 * #298 made unsupported tracked deletions answer a typed
 * `Event::Error { kind: TrackedDeletionRefused }`; the shell forwarded it to
 * telemetry only, so Backspace over a table boundary in review mode looked
 * like a dead key. `createEditorState().lastError` now moves on every
 * error reply and `@nge/ui`'s `ErrorToast` shows the kind's copy in a
 * transient `role="status"` region (4 s), with the Dev HUD's "Last error"
 * row recording the command + kind + count.
 *
 * Issue #365 then made that very deletion a RECORDED one (the rows are
 * marked deleted, `<w:trPr><w:del/>`): no keyboard edit reaches the
 * refusal any more (only a range whose end addresses no paragraph is
 * refused). So the first spec pins the new behaviour - Backspace across a
 * table is tracked, with no toast - and the toast / HUD specs feed a typed
 * refusal through the engine client's event stream, the exact path an
 * engine reply takes to `createEditorState`. */

const pos = (block: number, offset: number) => ({
    path: { steps: [{ kind: 'BLOCK', idx: block }] },
    offset,
});

/** `before` / table / `after`, tracking on, selection spanning the table. */
async function seedTableAndSelectAcross(page: Page): Promise<void> {
    await page.evaluate(
        async ({ start, end }) => {
            const dispatch = (cmd: unknown): Promise<any> => (window as any).__dispatch(cmd);
            await dispatch({ type: 'SELECT_ALL' });
            await dispatch({ type: 'INSERT_TEXT', at: undefined, text: 'before' });
            await dispatch({ type: 'SPLIT_PARAGRAPH', at: undefined });
            await dispatch({ type: 'INSERT_TEXT', at: undefined, text: 'after' });
            /* A 1x1 table between the two paragraphs. */
            const t = await dispatch({
                type: 'INSERT_TABLE',
                at: { steps: [{ kind: 'BLOCK', idx: 1 }] },
                rows: 1,
                cols: 1,
            });
            if (t.type === 'ERROR') throw new Error(`INSERT_TABLE: ${t.message}`);
            await dispatch({ type: 'TOGGLE_TRACK_CHANGES', enabled: true });
            await dispatch({
                type: 'SET_SELECTION',
                range: { start, end },
                caret: end,
            });
        },
        { start: pos(0, 2), end: pos(2, 2) },
    );
}

/** Deliver a typed refusal the way an engine reply reaches the shell: to
 *  every subscriber of the engine client (what `createEditorState` feeds
 *  `lastError` from). */
async function deliverRefusal(page: Page): Promise<void> {
    await page.evaluate(() => {
        const client = (window as any).__engineClient;
        const evt = {
            type: 'ERROR',
            message: 'DeleteAtCaret: the deletion range does not address a paragraph',
            kind: 'TrackedDeletionRefused',
        };
        for (const s of client.subscribers as Set<(e: unknown) => void>) s(evt);
    });
}

test('Backspace across a table in review mode is recorded, not refused - no toast (#364 x #365)', async ({
    page,
}) => {
    test.setTimeout(60_000);
    await boot(page);
    await seedTableAndSelectAcross(page);
    /* SELECT_ALL inside `documentText` would move the selection: read the
       text before, restore the selection, and compare after. */
    const before = await documentText(page);
    expect(before).toContain('before');
    expect(before).toContain('after');
    await page.evaluate(
        ({ start, end }) =>
            (window as any).__dispatch({
                type: 'SET_SELECTION',
                range: { start, end },
                caret: end,
            }),
        { start: pos(0, 2), end: pos(2, 2) },
    );

    const toast = page.locator('.nge-toast');
    await expect(toast).toHaveAttribute('role', 'status');
    await expect(toast).toHaveText('');

    /* The real key path: hidden textarea -> beforeinput -> engine. */
    await page.locator('textarea[data-nge-hidden-input]').focus();
    await page.keyboard.press('Backspace');

    /* Recorded: the text stays (struck), the table's row is a tracked row
       deletion, and nothing was refused. */
    await expect
        .poll(async () =>
            page.evaluate(async () => {
                const rows = await (window as any).__engineClient.revisionsSnapshot();
                return rows.filter((r: any) => r.row !== undefined).map((r: any) => [r.kind, r.row]);
            }),
        )
        .toEqual([['delete', 0]]);
    expect(await documentText(page)).toBe(before);
    await page.waitForTimeout(300);
    await expect(toast).toHaveText('');
    /* Accepting removes the deleted text, the table and the mark. */
    await page.evaluate(() => (window as any).__dispatch({ type: 'ACCEPT_ALL_REVISIONS' }));
    expect(await documentText(page)).toBe('beter');
});

test('a typed refusal shows the toast, and it auto-dismisses (#364)', async ({ page }) => {
    test.setTimeout(60_000);
    await boot(page);
    const toast = page.locator('.nge-toast');
    await expect(toast).toHaveAttribute('role', 'status');
    await expect(toast).toHaveText('');

    await deliverRefusal(page);
    await expect(toast).toContainText('could not be tracked');
    await expect(toast).toContainText('turn off Track Changes');
    /* The toast lives in a persistent `role="status"` live region
       (implicitly polite, announced when its text changes). */
    await expect(toast).toHaveAttribute('aria-atomic', 'true');

    /* Auto-dismisses (4 s). */
    await expect(toast).toHaveText('', { timeout: 8_000 });
});

test('the Dev HUD records the last error with its command, kind and count (#364)', async ({
    page,
}) => {
    test.setTimeout(60_000);
    await boot(page);
    await deliverRefusal(page);
    await expect(page.locator('.nge-toast')).toContainText('could not be tracked');

    await page.locator('textarea[data-nge-hidden-input]').focus();
    await page.keyboard.press('Control+Shift+D');
    const row = page.locator('.nge-hud__lasterror');
    await expect(row).toBeVisible();
    await expect(row).toContainText('DeleteAtCaret');
    await expect(row).toContainText('TrackedDeletionRefused');
    await expect(row).toContainText('#1');

    /* A second refusal moves the row and re-arms the toast. */
    await deliverRefusal(page);
    await expect(row).toContainText('#2');
});

test('an untracked delete across the same range is allowed - no toast (#364)', async ({
    page,
}) => {
    test.setTimeout(60_000);
    await boot(page);
    await seedTableAndSelectAcross(page);
    await page.evaluate(() =>
        (window as any).__dispatch({ type: 'TOGGLE_TRACK_CHANGES', enabled: false }),
    );
    await page.locator('textarea[data-nge-hidden-input]').focus();
    await page.keyboard.press('Backspace');
    await page.waitForTimeout(300);
    await expect(page.locator('.nge-toast')).toHaveText('');
});

/* Issue #427 - the remaining refusals are typed too: a field insert inside
 * a table cell answers `ErrorKind::InTableCell` and the toast says so, via
 * the real engine reply (not a delivered event). */
test('InsertField inside a table cell shows a typed toast (#427)', async ({ page }) => {
    test.setTimeout(60_000);
    await boot(page);
    const reply = await page.evaluate(async () => {
        const dispatch = (cmd: unknown): Promise<any> => (window as any).__dispatch(cmd);
        await dispatch({ type: 'SELECT_ALL' });
        await dispatch({ type: 'INSERT_TEXT', at: undefined, text: 'x' });
        const t = await dispatch({
            type: 'INSERT_TABLE',
            at: { steps: [{ kind: 'BLOCK', idx: 1 }] },
            rows: 1,
            cols: 1,
        });
        if (t.type === 'ERROR') throw new Error(`INSERT_TABLE: ${t.message}`);
        return dispatch({
            type: 'INSERT_FIELD',
            at: {
                path: {
                    steps: [
                        { kind: 'BLOCK', idx: 1 },
                        { kind: 'CELL', row: 0, col: 0 },
                        { kind: 'BLOCK', idx: 0 },
                    ],
                },
                offset: 0,
            },
            kind: 'Page',
        });
    });
    expect(reply.type).toBe('ERROR');
    expect(reply.kind).toBe('InTableCell');
    const toast = page.locator('.nge-toast');
    await expect(toast).toContainText('not supported inside a table cell');
    await expect(toast).toHaveText('', { timeout: 8_000 });
});
