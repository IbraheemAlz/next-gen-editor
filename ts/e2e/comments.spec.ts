import { test, expect, type Page, type Locator } from '@playwright/test';

/* Issue #266 — comments end to end through the REAL worker + WASM engine
 * and the `@nge/ui` comments rail:
 *
 *   - a comment inserted on a selection from the toolbar, a reply and a
 *     resolve from the rail (#27);
 *   - typing before the anchor keeps the comment on its text (#252): the
 *     rail's listed position moves, and "go to" still selects the same
 *     words;
 *   - a comment inside a table cell (#284 / #254): listed in document
 *     order through its full path, navigated into its cell, kept through
 *     an edit inside the cell;
 *   - every one of them survives a save / reload round trip (#282 — the
 *     comments land on paragraphs the writer replays from their source
 *     bytes, since each scenario saves + reloads its seed first).
 *
 * Assertions ride engine truth (the comments snapshot, the clipboard text
 * of the selection a "go to" produced) plus the rail's DOM — valid under
 * headless Chrome, which never composites the interactive canvas (see
 * CLAUDE.md). */

declare global {
    interface Window {
        __dispatch: (cmd: unknown) => Promise<any>;
        __engineClient: { commentsSnapshot: () => Promise<Snapshot[]> };
        __paintIdle?: boolean;
    }
}

type Step = { kind: 'BLOCK'; idx: number } | { kind: 'CELL'; row: number; col: number };
interface Snapshot {
    id: number;
    author: string;
    text: string;
    resolved: boolean;
    parent_id?: number;
    start_offset: number;
    end_offset: number;
    start_path: { steps: Step[] };
    end_path: { steps: Step[] };
}

async function boot(page: Page): Promise<void> {
    await page.goto('/');
    await page.waitForFunction(() => (window as any).__paintIdle === true, undefined, {
        timeout: 20_000,
    });
}

/** Run `cmds` (bridge commands) in order; returns the last reply. */
async function dispatch(page: Page, ...cmds: unknown[]): Promise<any> {
    return page.evaluate(async (cmds) => {
        let last: any;
        for (const cmd of cmds) last = await (window as any).__dispatch(cmd);
        return last;
    }, cmds);
}

/** Save, then load the saved bytes back (the document is "imported":
 *  every paragraph is written from its source bytes until touched). */
async function saveAndReload(page: Page): Promise<void> {
    const loaded = await page.evaluate(async () => {
        const d = (window as any).__dispatch;
        const saved = await d({ type: 'SAVE_DOCX' });
        return (await d({ type: 'LOAD_DOCX', bytes: saved.bytes })).type;
    });
    expect(loaded).toBe('DOCUMENT_LOADED');
}

async function snapshot(page: Page): Promise<Snapshot[]> {
    return page.evaluate(() => window.__engineClient.commentsSnapshot());
}

/** The plain text of the current selection. */
async function selectedText(page: Page): Promise<string> {
    const clip = await dispatch(page, { type: 'GET_SELECTION_AS_CLIPBOARD', include_docx: false });
    return clip.plain as string;
}

const top = (idx: number, offset: number) => ({
    path: { steps: [{ kind: 'BLOCK', idx }] },
    offset,
});
const inCell = (table: number, row: number, col: number, offset: number) => ({
    path: {
        steps: [
            { kind: 'BLOCK', idx: table },
            { kind: 'CELL', row, col },
            { kind: 'BLOCK', idx: 0 },
        ],
    },
    offset,
});
const caretAt = (pos: unknown) => ({
    type: 'SET_SELECTION',
    range: { start: pos, end: pos },
    caret: pos,
});

function card(page: Page, id: number): Locator {
    return page.locator(`.nge-cm__card[data-comment-id="${id}"]`);
}

/** Click a card's "Go to comment" and return the selected text. */
async function goTo(page: Page, id: number): Promise<string> {
    await card(page, id).getByRole('button', { name: 'Go to comment' }).click();
    await expect(card(page, id)).toHaveClass(/nge-cm__card--active/);
    return selectedText(page);
}

test('insert on a selection, reply, resolve; typing before the anchor; save / reload', async ({
    page,
}) => {
    test.setTimeout(60_000);
    await boot(page);
    await dispatch(
        page,
        { type: 'SELECT_ALL' },
        { type: 'INSERT_TEXT', at: undefined, text: 'alpha beta gamma' },
    );
    await saveAndReload(page);

    /* Insert on the selection "beta" from the toolbar. */
    await dispatch(page, {
        type: 'SET_SELECTION',
        range: { start: top(0, 6), end: top(0, 10) },
        caret: top(0, 10),
    });
    await page.getByRole('button', { name: 'New comment' }).click();
    await page.getByPlaceholder('Comment text…').fill('Check this word');
    await page.getByRole('button', { name: 'Comment', exact: true }).click();

    await expect.poll(async () => (await snapshot(page)).length).toBe(1);
    const [root] = await snapshot(page);
    expect(root!.text).toBe('Check this word');
    expect(root!.start_path.steps).toEqual([{ kind: 'BLOCK', idx: 0 }]);
    expect([root!.start_offset, root!.end_offset]).toEqual([6, 10]);
    const rootId = root!.id;
    await expect(card(page, rootId)).toContainText('Check this word');

    /* Reply from the rail. */
    await card(page, rootId).getByRole('button', { name: 'Reply to comment' }).click();
    await card(page, rootId).getByPlaceholder('Reply…').fill('Agreed');
    await card(page, rootId).getByRole('button', { name: 'Reply', exact: true }).click();
    await expect(card(page, rootId).locator('.nge-cm__reply')).toContainText('Agreed');

    /* Resolve from the rail. */
    await card(page, rootId).getByRole('button', { name: 'Resolve comment' }).click();
    await expect(card(page, rootId)).toHaveClass(/nge-cm__card--resolved/);
    expect((await snapshot(page)).find((c) => c.id === rootId)?.resolved).toBe(true);

    /* Type before the anchor: the comment stays on "beta". */
    await dispatch(page, caretAt(top(0, 0)), {
        type: 'INSERT_TEXT',
        at: undefined,
        text: 'Note: ',
    });
    await expect
        .poll(async () => {
            const r = (await snapshot(page)).find((c) => c.id === rootId)!;
            return [r.start_offset, r.end_offset];
        })
        .toEqual([12, 16]);
    await expect(card(page, rootId)).toHaveAttribute('data-anchor', 'block 0:12');
    expect(await goTo(page, rootId)).toBe('beta');
    /* The selection highlight is painted over the commented text. */
    await expect(page.locator('.selection-rect').first()).toBeVisible();

    /* Save / reload: text, thread, resolved state and anchor survive. */
    await saveAndReload(page);
    const back = await snapshot(page);
    const r2 = back.find((c) => c.parent_id === undefined || c.parent_id === null)!;
    const reply = back.find((c) => c.parent_id === r2.id);
    expect(r2.text).toBe('Check this word');
    expect(r2.resolved).toBe(true);
    expect(reply?.text).toBe('Agreed');
    expect([r2.start_offset, r2.end_offset]).toEqual([12, 16]);
    await expect(card(page, r2.id)).toContainText('Agreed');
    expect(await goTo(page, r2.id)).toBe('beta');
});

test('a comment inside a table cell is listed in document order, navigated and kept', async ({
    page,
}) => {
    test.setTimeout(60_000);
    await boot(page);
    await dispatch(
        page,
        { type: 'SELECT_ALL' },
        { type: 'INSERT_TEXT', at: undefined, text: 'before' },
        { type: 'INSERT_TABLE', at: { steps: [{ kind: 'BLOCK', idx: 1 }] }, rows: 2, cols: 2 },
        caretAt(inCell(1, 1, 0, 0)),
        { type: 'INSERT_TEXT', at: undefined, text: 'cell words here' },
        caretAt(top(2, 0)),
        { type: 'INSERT_TEXT', at: undefined, text: 'after table' },
    );
    await saveAndReload(page);

    /* The body comment AFTER the table is created first (lower id); the
       cell comment still lists first — document order, by full path. */
    await dispatch(page, {
        type: 'INSERT_COMMENT',
        range: { start: top(2, 0), end: top(2, 5) },
        text: 'body note',
        author: 'Reviewer',
    });
    await dispatch(page, {
        type: 'INSERT_COMMENT',
        range: { start: inCell(1, 1, 0, 5), end: inCell(1, 1, 0, 10) },
        text: 'cell note',
        author: 'Reviewer',
    });
    await expect.poll(async () => (await snapshot(page)).length).toBe(2);
    const rows = await snapshot(page);
    const body = rows.find((c) => c.text === 'body note')!;
    const cell = rows.find((c) => c.text === 'cell note')!;
    expect(body.id).toBeLessThan(cell.id);
    expect(cell.start_path.steps).toEqual([
        { kind: 'BLOCK', idx: 1 },
        { kind: 'CELL', row: 1, col: 0 },
        { kind: 'BLOCK', idx: 0 },
    ]);
    await expect(page.locator('.nge-cm__card')).toHaveCount(2);
    await expect(page.locator('.nge-cm__card').first()).toHaveAttribute(
        'data-comment-id',
        String(cell.id),
    );
    await expect(card(page, cell.id)).toHaveAttribute(
        'data-anchor',
        'block 1 › cell 1,0 › block 0:5',
    );
    expect(await goTo(page, cell.id)).toBe('words');
    expect(await goTo(page, body.id)).toBe('after');

    /* An edit inside the cell, before the anchor, keeps it on its text. */
    await dispatch(page, caretAt(inCell(1, 1, 0, 0)), {
        type: 'INSERT_TEXT',
        at: undefined,
        text: 'XX',
    });
    await expect
        .poll(async () => (await snapshot(page)).find((c) => c.id === cell.id)?.start_offset)
        .toBe(7);
    expect(await goTo(page, cell.id)).toBe('words');

    /* Delete the body comment from the rail, then save / reload: the cell
       comment comes back in its cell, the deleted one does not. */
    await card(page, body.id).getByRole('button', { name: 'Delete comment' }).click();
    await expect(page.locator('.nge-cm__card')).toHaveCount(1);
    await saveAndReload(page);
    const back = await snapshot(page);
    expect(back.map((c) => c.text)).toEqual(['cell note']);
    expect(back[0]!.start_path.steps).toEqual(cell.start_path.steps);
    expect(await goTo(page, back[0]!.id)).toBe('words');
});
