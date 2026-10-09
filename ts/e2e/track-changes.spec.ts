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

const BACKSPACE = { type: 'DELETE_AT_CARET', forward: false, by_word: false };

test('cross-paragraph tracked delete: marked, then accept merges and reject restores', async ({
    page,
}) => {
    await boot(page);
    const original = 'alpha beta\nmiddle\ngamma delta';
    await seed(page, ['alpha beta', 'middle', 'gamma delta']);
    expect(await plain(page)).toBe(original);
    const depth = (await run(page, [caret(0, 0)])).undo_depth as number;
    const marked = await run(page, [
        {
            type: 'SET_SELECTION',
            range: { start: pos(0, 6), end: pos(2, 6) },
            caret: pos(2, 6),
        },
        BACKSPACE,
    ]);
    /* Not a silent no-op: one undo step, the text stays (struck). */
    expect(marked.type).toBe('SELECTION_CHANGED');
    expect(marked.undo_depth).toBe(depth + 1);
    expect(await plain(page)).toBe(original);
    await run(page, [{ type: 'ACCEPT_ALL_REVISIONS' }]);
    expect(await plain(page)).toBe('alpha delta');
    await run(page, [{ type: 'UNDO' }]);
    expect(await plain(page)).toBe(original);
    await run(page, [{ type: 'REJECT_ALL_REVISIONS' }]);
    expect(await plain(page)).toBe(original);
    /* Nothing pending after the reject. */
    const settled = (await run(page, [caret(0, 0)])).undo_depth as number;
    const again = await run(page, [{ type: 'ACCEPT_ALL_REVISIONS' }]);
    expect(again.undo_depth).toBe(settled);
    expect(await plain(page)).toBe(original);
});

test('tracked Backspace at a paragraph start marks the break; Backspace over your own Enter removes it', async ({
    page,
}) => {
    await boot(page);
    await seed(page, ['one', 'two']);
    const marked = await run(page, [caret(1, 0), BACKSPACE]);
    expect(marked.type).toBe('SELECTION_CHANGED');
    expect(await plain(page)).toBe('one\ntwo');
    await run(page, [{ type: 'ACCEPT_ALL_REVISIONS' }]);
    expect(await plain(page)).toBe('onetwo');
    /* A tracked Enter of your own, then Backspace: no change is left. */
    await run(page, [caret(0, 3), { type: 'SPLIT_PARAGRAPH', at: undefined }]);
    expect(await plain(page)).toBe('one\ntwo');
    /* `plain()` selected everything — put the caret back at the start
     * of the new paragraph first. */
    await run(page, [caret(1, 0), BACKSPACE]);
    expect(await plain(page)).toBe('onetwo');
    const depth = (await run(page, [caret(0, 0)])).undo_depth as number;
    const nothing = await run(page, [{ type: 'REJECT_ALL_REVISIONS' }]);
    expect(nothing.undo_depth).toBe(depth);
});

/* Issue #366 — pasted text and IME commits are tracked insertions too:
 * reject all removes them (merging the paragraphs a multi-line paste
 * split), accept all keeps them. */

for (const [how, cmd] of [
    ['plain', { type: 'PASTE_PLAIN', text: 'one\ntwo' }],
    ['HTML', { type: 'PASTE_HTML', html: '<p>one</p><p>two</p>' }],
] as const) {
    test(`a tracked ${how} paste of two paragraphs rejects to the original`, async ({ page }) => {
        await boot(page);
        await seed(page, ['alpha beta']);
        const depth = (await run(page, [caret(0, 5)])).undo_depth as number;
        const pasted = await run(page, [cmd]);
        expect(pasted.type).toBe('SELECTION_CHANGED');
        expect(pasted.undo_depth).toBe(depth + 1);
        expect(await plain(page)).toBe('alphaone\ntwo beta');
        await run(page, [{ type: 'REJECT_ALL_REVISIONS' }]);
        expect(await plain(page)).toBe('alpha beta');
        await run(page, [{ type: 'UNDO' }]);
        expect(await plain(page)).toBe('alphaone\ntwo beta');
        await run(page, [{ type: 'ACCEPT_ALL_REVISIONS' }]);
        expect(await plain(page)).toBe('alphaone\ntwo beta');
        /* Nothing left pending. */
        const settled = (await run(page, [caret(0, 0)])).undo_depth as number;
        const again = await run(page, [{ type: 'REJECT_ALL_REVISIONS' }]);
        expect(again.undo_depth).toBe(settled);
    });
}

test('a tracked IME commit through the hidden input is an insertion reject removes', async ({
    page,
}) => {
    await boot(page);
    await seed(page, ['alpha beta']);
    await run(page, [caret(0, 6)]);
    /* The real HiddenInput composition handlers (BEGIN / UPDATE /
     * END_COMPOSITION), fired as one synchronous burst. */
    await page.evaluate(() => {
        const ta = document.querySelector<HTMLTextAreaElement>('textarea[data-nge-hidden-input]');
        if (!ta) throw new Error('editor hidden input missing');
        const fire = (type: string, data: string): void => {
            ta.dispatchEvent(new CompositionEvent(type, { data, bubbles: true }));
        };
        fire('compositionstart', '');
        fire('compositionupdate', 'に');
        fire('compositionupdate', '日本');
        fire('compositionend', '日本');
    });
    expect(await plain(page)).toBe('alpha 日本beta');
    /* One change, by the reviewer: the sidebar's single reject removes it. */
    const rows = await page.evaluate(async () => {
        const client = (window as any).__engineClient;
        return (await client.revisionsSnapshot()) as Array<{ kind: string; start: number; end: number; revision_id: number }>;
    });
    expect(rows.map((r) => [r.kind, r.start, r.end])).toEqual([['insert', 6, 12]]);
    await run(page, [
        {
            type: 'REJECT_REVISION',
            block: 0,
            start: 6,
            end: 12,
            revision_id: rows[0]!.revision_id,
        },
    ]);
    expect(await plain(page)).toBe('alpha beta');
});
