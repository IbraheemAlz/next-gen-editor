import { test, expect, type Page } from '@playwright/test';
import { boot, burst, settle } from './helpers/editor';

/* Issue #442 — a structural edit made through the UI with review mode on
 * is tracked: "Insert row below" from the table context menu records an
 * inserted row (`<w:trPr><w:ins/>`), the review pane lists it, and the
 * toolbar's "Accept all revisions" clears it (the row stays).
 *
 * Issue #283 — typing before a tracked insertion keeps the revision on its
 * own text: the review pane's range moves with the text and the bytes it
 * names are still the inserted ones. The typing goes through the real
 * hidden input in one synchronous `burst` (CLAUDE.md, issue #310).
 *
 * Assertions ride engine truth (command replies, `revisions_snapshot`
 * through the sidebar), valid under headless Chrome.
 */

/** Every command in `cmds` in order; returns the last reply. */
async function run(page: Page, cmds: unknown[]): Promise<any> {
    return page.evaluate(async (cmds) => {
        let last: any;
        for (const cmd of cmds) last = await (window as any).__dispatch(cmd);
        return last;
    }, cmds);
}

const inCell = (row: number, col: number, offset: number) => ({
    path: {
        steps: [
            { kind: 'BLOCK', idx: 1 },
            { kind: 'CELL', row, col },
            { kind: 'BLOCK', idx: 0 },
        ],
    },
    offset,
});
const caretIn = (row: number, col: number) => ({
    type: 'SET_SELECTION',
    range: { start: inCell(row, col, 0), end: inCell(row, col, 0) },
    caret: inCell(row, col, 0),
});
const pos = (block: number, offset: number) => ({
    path: { steps: [{ kind: 'BLOCK', idx: block }] },
    offset,
});
const caret = (block: number, offset: number) => ({
    type: 'SET_SELECTION',
    range: { start: pos(block, offset), end: pos(block, offset) },
    caret: pos(block, offset),
});

async function plain(page: Page): Promise<string> {
    return page.evaluate(async () => {
        const dispatch = (cmd: unknown): Promise<any> => (window as any).__dispatch(cmd);
        await dispatch({ type: 'SELECT_ALL' });
        const clip = await dispatch({ type: 'GET_SELECTION_AS_CLIPBOARD', include_docx: false });
        return clip.plain as string;
    });
}

test('review mode: Insert row below from the table menu is tracked, listed, and Accept all clears it', async ({
    page,
}) => {
    await boot(page);
    await run(page, [
        { type: 'SELECT_ALL' },
        { type: 'INSERT_TEXT', at: undefined, text: 'before' },
        { type: 'INSERT_TABLE', at: { steps: [{ kind: 'BLOCK', idx: 1 }] }, rows: 2, cols: 2 },
        caretIn(0, 0),
        { type: 'INSERT_TEXT', at: undefined, text: 'r0c0' },
        caretIn(0, 0),
    ]);
    const tableRows = page.locator('table[role="table"] tr[role="row"]');
    await expect(tableRows).toHaveCount(2);

    /* Review mode on through the toolbar toggle. */
    const track = page.getByRole('button', { name: 'Track changes' });
    await track.click();
    await expect(track).toHaveAttribute('aria-pressed', 'true');

    const sidebar = page.getByRole('complementary', { name: 'Track changes' });
    const rows = sidebar.locator('.nge-tc__row');
    await sidebar.getByRole('button', { name: 'Refresh' }).click();
    await expect(rows).toHaveCount(0);

    /* The caret is in a cell: the context menu offers the row commands. */
    await page.evaluate(() => {
        document.body.dispatchEvent(
            new MouseEvent('contextmenu', { clientX: 200, clientY: 200, bubbles: true, cancelable: true }),
        );
    });
    await page.getByRole('menuitem', { name: 'Insert row below' }).click();
    await expect(tableRows).toHaveCount(3);

    await sidebar.getByRole('button', { name: 'Refresh' }).click();
    await expect(rows).toHaveCount(1);
    await expect(rows.first()).toContainText('Table row inserted');
    await expect(rows.first()).toContainText('row 2');
    await expect(rows.first()).toHaveAttribute('data-depth', '1');

    await page.getByRole('button', { name: 'Accept all revisions' }).click();
    await settle(page);
    await sidebar.getByRole('button', { name: 'Refresh' }).click();
    await expect(rows).toHaveCount(0);
    /* Accepted: the row stays. */
    await expect(tableRows).toHaveCount(3);
});

test('review mode: Reject all removes a row inserted from the table menu', async ({ page }) => {
    await boot(page);
    await run(page, [
        { type: 'SELECT_ALL' },
        { type: 'INSERT_TEXT', at: undefined, text: 'before' },
        { type: 'INSERT_TABLE', at: { steps: [{ kind: 'BLOCK', idx: 1 }] }, rows: 2, cols: 2 },
        caretIn(1, 1),
        { type: 'TOGGLE_TRACK_CHANGES', enabled: true },
    ]);
    const tableRows = page.locator('table[role="table"] tr[role="row"]');
    await page.evaluate(() => {
        document.body.dispatchEvent(
            new MouseEvent('contextmenu', { clientX: 200, clientY: 200, bubbles: true, cancelable: true }),
        );
    });
    await page.getByRole('menuitem', { name: 'Insert row above' }).click();
    await expect(tableRows).toHaveCount(3);
    await page.getByRole('button', { name: 'Reject all revisions' }).click();
    await expect(tableRows).toHaveCount(2);
});

test('typing before a tracked insertion keeps the revision on the inserted text', async ({ page }) => {
    await boot(page);
    await run(page, [
        { type: 'SELECT_ALL' },
        { type: 'INSERT_TEXT', at: undefined, text: 'alpha omega' },
        caret(0, 6),
        { type: 'TOGGLE_TRACK_CHANGES', enabled: true },
        { type: 'INSERT_TEXT', at: undefined, text: 'NEW ' },
        { type: 'TOGGLE_TRACK_CHANGES', enabled: false },
        caret(0, 0),
    ]);
    const sidebar = page.getByRole('complementary', { name: 'Track changes' });
    const rows = sidebar.locator('.nge-tc__row');
    await sidebar.getByRole('button', { name: 'Refresh' }).click();
    await expect(rows).toHaveCount(1);
    await expect(rows.first()).toContainText('block 0 · 6–10');

    /* Type before it through the real hidden input, in one burst. */
    await burst(page, ['x', 'y', 'é']);
    await settle(page);
    expect(await plain(page)).toBe('xyéalpha NEW omega');

    await sidebar.getByRole('button', { name: 'Refresh' }).click();
    await expect(rows).toHaveCount(1);
    /* 'x' + 'y' + 'é' = 4 bytes: the revision moved with its text. */
    await expect(rows.first()).toContainText('block 0 · 10–14');
    const covered = await page.evaluate(async () => {
        const client = (window as any).__engineClient;
        const revs = await client.revisionsSnapshot();
        const dispatch = (cmd: unknown): Promise<any> => (window as any).__dispatch(cmd);
        await dispatch({ type: 'SELECT_ALL' });
        const clip = await dispatch({ type: 'GET_SELECTION_AS_CLIPBOARD', include_docx: false });
        const bytes = new TextEncoder().encode(clip.plain as string);
        const r = revs[0];
        return new TextDecoder().decode(bytes.slice(r.start, r.end));
    });
    expect(covered).toBe('NEW ');

    /* Reject removes exactly the inserted bytes. */
    await page.getByRole('button', { name: 'Reject all revisions' }).click();
    await expect.poll(() => plain(page)).toBe('xyéalpha omega');
});
