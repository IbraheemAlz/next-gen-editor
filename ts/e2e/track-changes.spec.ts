import { test, expect, type Page } from '@playwright/test';

/* Issues #301 / #298 — structural tracked edits. With track changes on,
 * Enter records the new paragraph mark as inserted (reject merges the
 * halves back) and a delete across paragraph marks records them deleted
 * (accept merges, reject restores) instead of silently doing nothing.
 * Assertions ride command replies (`SELECTION_CHANGED.undo_depth`, the
 * clipboard payload's plain text — paragraphs joined by `\n`): engine
 * truth, valid under headless Chrome.
 */

async function boot(page: Page): Promise<void> {
    await page.goto('/');
    await page.waitForFunction(() => (window as any).__paintIdle === true, undefined, {
        timeout: 30_000,
    });
}

/** Replace the document with `paras` (untracked), then turn tracking on. */
async function seed(page: Page, paras: string[]): Promise<void> {
    await page.evaluate(async (paras) => {
        const dispatch = (cmd: unknown): Promise<any> => (window as any).__dispatch(cmd);
        await dispatch({ type: 'SELECT_ALL' });
        for (const [i, text] of paras.entries()) {
            if (i > 0) await dispatch({ type: 'SPLIT_PARAGRAPH', at: undefined });
            await dispatch({ type: 'INSERT_TEXT', at: undefined, text });
        }
        await dispatch({ type: 'TOGGLE_TRACK_CHANGES', enabled: true });
    }, paras);
}

/** Every command in `cmds` in order; returns the last reply. */
async function run(page: Page, cmds: unknown[]): Promise<any> {
    return page.evaluate(async (cmds) => {
        let last: any;
        for (const cmd of cmds) last = await (window as any).__dispatch(cmd);
        return last;
    }, cmds);
}

async function plain(page: Page): Promise<string> {
    return page.evaluate(async () => {
        const dispatch = (cmd: unknown): Promise<any> => (window as any).__dispatch(cmd);
        await dispatch({ type: 'SELECT_ALL' });
        const clip = await dispatch({ type: 'GET_SELECTION_AS_CLIPBOARD', include_docx: false });
        return clip.plain as string;
    });
}

const pos = (block: number, offset: number) => ({
    path: { steps: [{ kind: 'BLOCK', idx: block }] },
    offset,
});
const caret = (block: number, offset: number) => ({
    type: 'SET_SELECTION',
    range: { start: pos(block, offset), end: pos(block, offset) },
    caret: pos(block, offset),
});

test('tracked Enter: reject all merges the paragraphs back, accept keeps the break', async ({
    page,
}) => {
    await boot(page);
    await seed(page, ['alpha beta']);
    const split = await run(page, [caret(0, 5), { type: 'SPLIT_PARAGRAPH', at: undefined }]);
    expect(split.type).toBe('SELECTION_CHANGED');
    expect(await plain(page)).toBe('alpha\n beta');
    const depth = (await run(page, [caret(0, 0)])).undo_depth as number;
    /* Reject all: one undo step, one paragraph again. */
    const rejected = await run(page, [{ type: 'REJECT_ALL_REVISIONS' }]);
    expect(rejected.undo_depth).toBe(depth + 1);
    expect(await plain(page)).toBe('alpha beta');
    /* Undo brings the tracked break back; accept all keeps it. */
    await run(page, [{ type: 'UNDO' }]);
    expect(await plain(page)).toBe('alpha\n beta');
    await run(page, [{ type: 'ACCEPT_ALL_REVISIONS' }]);
    expect(await plain(page)).toBe('alpha\n beta');
    /* Nothing left to resolve. */
    const after = await run(page, [{ type: 'REJECT_ALL_REVISIONS' }]);
    expect(after.undo_depth).toBe(depth + 1);
    expect(await plain(page)).toBe('alpha\n beta');
});

test('tracked Enter: the single mark row rejects through the sidebar address', async ({
    page,
}) => {
    await boot(page);
    await seed(page, ['one two']);
    await run(page, [caret(0, 3), { type: 'SPLIT_PARAGRAPH', at: undefined }]);
    expect(await plain(page)).toBe('one\n two');
    /* The mark row is the empty range at the paragraph end. */
    await run(page, [{ type: 'REJECT_REVISION', block: 0, start: 3, end: 3 }]);
    expect(await plain(page)).toBe('one two');
});
