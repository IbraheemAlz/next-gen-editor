import { test, expect, type Page } from '@playwright/test';
import { boot, burst, documentText } from './helpers/editor';

/* Issue #276 — typing after a formatted run continues its formatting (the
 * character before the caret; at a paragraph start the one after it), and
 * the toolbar's `attrs_at_caret` reports exactly what the next keystroke
 * produces — before and after typing. Pending (sticky) formatting still
 * overrides the inherited style. */

test('typing after a bold run is bold; pending bold-off types plain', async ({ page }) => {
    await boot(page);
    const out = await page.evaluate(async () => {
        const dispatch = (window as any).__dispatch;
        const pos = (offset: number) => ({
            path: { steps: [{ kind: 'BLOCK', idx: 0 }] },
            offset,
        });
        const select = (a: number, b: number) =>
            dispatch({ type: 'SET_SELECTION', range: { start: pos(a), end: pos(b) }, caret: pos(b) });
        const bold = (on: boolean) =>
            dispatch({ type: 'APPLY_FORMATTING', range: undefined, attrs: { bold: on } });
        /* Bold the first three characters, park the caret right after. */
        await select(0, 3);
        await bold(true);
        const before = await select(3, 3);
        await dispatch({ type: 'INSERT_TEXT', at: undefined, text: 'Z' });
        /* The typed char itself, as a one-character range. */
        const typed = await select(3, 4);
        /* Pending bold-off at the end of the (now four-char) bold run. */
        await select(4, 4);
        await bold(false);
        await dispatch({ type: 'INSERT_TEXT', at: undefined, text: 'Q' });
        const plain = await select(4, 5);
        const boldStill = await select(3, 4);
        return {
            before: before.attrs_at_caret.bold as boolean,
            typed: typed.attrs_at_caret.bold as boolean,
            plain: plain.attrs_at_caret.bold as boolean,
            boldStill: boldStill.attrs_at_caret.bold as boolean,
        };
    });
    expect(out.before, 'toolbar previews the inherited bold').toBe(true);
    expect(out.typed, 'typed text continues the bold run').toBe(true);
    expect(out.plain, 'pending bold-off wins').toBe(false);
    expect(out.boldStill).toBe(true);
});

/* Issue #286 — formatting toggles are engine-side (`TOGGLE_FORMATTING`):
 * the shell posts them synchronously with no state read, so a toggle fired
 * before the previous keystroke's SELECTION_CHANGED lands still flips
 * against the engine's live style. Zero waits between keystrokes. */

const pos = (offset: number, idx = 0) => ({
    path: { steps: [{ kind: 'BLOCK', idx }] },
    offset,
});

/** Park a collapsed caret at `offset` in body paragraph 0. */
async function caretAt(page: Page, offset: number): Promise<void> {
    await page.evaluate(async (p) => {
        await (window as any).__dispatch({
            type: 'SET_SELECTION',
            range: { start: p, end: p },
            caret: p,
        });
    }, pos(offset));
}

/** Caret at the start of body paragraph 0, typing focus on the hidden
 *  textarea (the only text-input source). */
async function caretAtStart(page: Page): Promise<void> {
    await caretAt(page, 0);
    await page.locator('textarea[data-nge-hidden-input]').focus();
}

/** Bold state of each [a, b) range in the paragraph at `p0` (`bold` from
 *  the range's first char, `mixed` when the range disagrees), read after
 *  every queued keystroke has run (the worker queue is FIFO). */
async function boldOf(page: Page, ranges: Array<[number, number]>) {
    return page.evaluate(
        async ({ ranges, p0 }) => {
            const dispatch = (window as any).__dispatch;
            const mk = (o: number) => ({ ...p0, offset: o });
            const out: Array<{ bold: boolean; mixed: boolean }> = [];
            let story = false;
            for (const [a, b] of ranges) {
                const evt = await dispatch({
                    type: 'SET_SELECTION',
                    range: { start: mk(a), end: mk(b) },
                    caret: mk(b),
                });
                story = evt.editing_story !== undefined && evt.editing_story !== null;
                out.push({ bold: evt.attrs_at_caret.bold, mixed: evt.attrs_mixed.bold });
            }
            return { runs: out, story };
        },
        { ranges, p0: pos(0) },
    );
}

async function expectBoldThenPlain(page: Page): Promise<void> {
    const out = await boldOf(page, [
        [0, 4],
        [4, 8],
    ]);
    expect(out.runs[0], '"bold" is one bold run').toEqual({ bold: true, mixed: false });
    expect(out.runs[1], '" end" is plain').toEqual({ bold: false, mixed: false });
    const text = await documentText(page);
    expect(text.startsWith('bold end'), JSON.stringify(text.slice(0, 40))).toBe(true);
}

test('Ctrl+B, type, Ctrl+B, type with zero waits gives two runs (#286)', async ({ page }) => {
    await boot(page);
    await caretAtStart(page);
    await burst(page, ['B', 'b', 'o', 'l', 'd', 'B', ' ', 'e', 'n', 'd']);
    await expectBoldThenPlain(page);
});

test('real Ctrl+B keystrokes interleaved with typing give two runs (#286)', async ({ page }) => {
    await boot(page);
    await caretAtStart(page);
    /* The same sequence through the real keyboard pipeline (no waits). */
    await page.keyboard.press('ControlOrMeta+KeyB');
    await page.keyboard.type('bold');
    await page.keyboard.press('ControlOrMeta+KeyB');
    await page.keyboard.type(' end');
    await expectBoldThenPlain(page);
});

test('toolbar Bold button, type, button, type with zero waits gives two runs (#286)', async ({
    page,
}) => {
    await boot(page);
    await caretAtStart(page);
    const btn = page.locator('.nge-tfmt__btn--bold');
    await expect(btn).toBeEnabled();
    await burst(page, ['BTN', 'b', 'o', 'l', 'd', 'BTN', ' ', 'e', 'n', 'd']);
    const out = await boldOf(page, [
        [0, 4],
        [4, 8],
    ]);
    expect(out.runs[0]).toEqual({ bold: true, mixed: false });
    expect(out.runs[1]).toEqual({ bold: false, mixed: false });
    const text = await documentText(page);
    expect(text.startsWith('bold end'), JSON.stringify(text.slice(0, 40))).toBe(true);
    /* The pressed state still mirrors SELECTION_CHANGED. */
    await caretAt(page, 2);
    await expect(btn).toHaveAttribute('aria-pressed', 'true');
    await caretAt(page, 6);
    await expect(btn).toHaveAttribute('aria-pressed', 'false');
});

/* Issue #296 — typing inside a header honours pending (sticky) formatting
 * exactly like the body: toolbar preview and typed text agree. */
test('Ctrl+B then typing in a header produces a bold header run (#296)', async ({ page }) => {
    await boot(page);
    await caretAtStart(page);
    const entered = await page.evaluate(async () =>
        (window as any).__dispatch({ type: 'ENTER_HEADER_FOOTER', page: 0, area: 'Header' }),
    );
    expect(entered.type).toBe('SELECTION_CHANGED');
    await page.locator('textarea[data-nge-hidden-input]').focus();
    await page.keyboard.press('ControlOrMeta+KeyB');
    await page.keyboard.type('Hi');
    await page.keyboard.press('ControlOrMeta+KeyB');
    await page.keyboard.type(' there');
    /* Story selections are story-rooted: block 0 = the header's first
       paragraph (an empty header part, so the typed text starts at 0). */
    const out = await boldOf(page, [
        [0, 2],
        [3, 8],
    ]);
    expect(out.story, 'still editing the header').toBe(true);
    expect(out.runs[0], '"Hi" is bold in the header').toEqual({ bold: true, mixed: false });
    expect(out.runs[1], '" there" is plain').toEqual({ bold: false, mixed: false });
});

/* Issue #292 — Backspace at the start of the paragraph after a heading,
 * and Delete at the heading's end, join the paragraphs into ONE heading
 * (the merge used to clear the paragraph style). */
test('Backspace / Delete across the break after a heading keep the heading (#292)', async ({
    page,
}) => {
    await boot(page);
    const out = await page.evaluate(async () => {
        const dispatch = (window as any).__dispatch;
        const pos = (idx: number, offset: number) => ({
            path: { steps: [{ kind: 'BLOCK', idx }] },
            offset,
        });
        const caret = (idx: number, offset: number) =>
            dispatch({
                type: 'SET_SELECTION',
                range: { start: pos(idx, offset), end: pos(idx, offset) },
                caret: pos(idx, offset),
            });
        const styleOf = async (idx: number) =>
            (await caret(idx, 0)).paragraph_style_id as string | undefined;
        const del = (forward: boolean) =>
            dispatch({ type: 'DELETE_AT_CARET', forward, by_word: false });
        /* A fresh "Title" heading followed by an unstyled "Body". */
        await caret(0, 0);
        await dispatch({ type: 'SPLIT_PARAGRAPH', at: undefined });
        await caret(0, 0);
        await dispatch({ type: 'INSERT_TEXT', at: undefined, text: 'Title' });
        await dispatch({
            type: 'APPLY_STYLE',
            range: { start: pos(0, 0), end: pos(0, 0) },
            style_id: 'Heading1',
        });
        const split = async () => {
            await caret(0, 5);
            await dispatch({ type: 'SPLIT_PARAGRAPH', at: undefined });
            await caret(1, 0);
            await dispatch({ type: 'INSERT_TEXT', at: undefined, text: 'Body' });
            await dispatch({
                type: 'APPLY_STYLE',
                range: { start: pos(1, 0), end: pos(1, 0) },
                style_id: undefined,
            });
        };
        await split();
        const bodyBefore = await styleOf(1);
        await caret(1, 0);
        await del(false);
        const afterBackspace = await styleOf(0);
        /* Undo the merge, then the forward-delete variant. */
        await dispatch({ type: 'UNDO' });
        await caret(0, 5);
        await del(true);
        const afterDelete = await styleOf(0);
        await dispatch({
            type: 'SET_SELECTION',
            range: { start: pos(0, 0), end: pos(0, 9) },
            caret: pos(0, 9),
        });
        const clip = await dispatch({ type: 'GET_SELECTION_AS_CLIPBOARD' });
        return {
            bodyBefore,
            afterBackspace,
            afterDelete,
            text: clip.plain as string,
        };
    });
    expect(out.bodyBefore ?? null, 'the body starts unstyled').toBeNull();
    expect(out.afterBackspace, 'Backspace keeps the heading').toBe('Heading1');
    expect(out.afterDelete, 'Delete keeps the heading').toBe('Heading1');
    expect(out.text.startsWith('TitleBody'), JSON.stringify(out.text)).toBe(true);
});

/* Issue #293 — Enter at the end of a bold run gives a new, empty paragraph
 * whose MARK is bold (`<w:pPr><w:rPr><w:b/>`); typing there is bold, as in
 * Word, and the toolbar previews it before the first keystroke. */
test('Enter at the end of a bold run, then typing, is bold (#293)', async ({ page }) => {
    await boot(page);
    const out = await page.evaluate(async () => {
        const dispatch = (window as any).__dispatch;
        const pos = (idx: number, offset: number) => ({
            path: { steps: [{ kind: 'BLOCK', idx }] },
            offset,
        });
        const select = (idx: number, a: number, b: number) =>
            dispatch({
                type: 'SET_SELECTION',
                range: { start: pos(idx, a), end: pos(idx, b) },
                caret: pos(idx, b),
            });
        /* A fresh "Hello world" paragraph with "world" bold. */
        await select(0, 0, 0);
        await dispatch({ type: 'SPLIT_PARAGRAPH', at: undefined });
        await select(0, 0, 0);
        await dispatch({ type: 'INSERT_TEXT', at: undefined, text: 'Hello world' });
        await select(0, 6, 11);
        await dispatch({ type: 'APPLY_FORMATTING', range: undefined, attrs: { bold: true } });
        await select(0, 11, 11);
        await dispatch({ type: 'SPLIT_PARAGRAPH', at: undefined });
        const preview = await select(1, 0, 0);
        await dispatch({ type: 'INSERT_TEXT', at: undefined, text: 'Next' });
        const typed = await select(1, 0, 4);
        const first = await select(0, 0, 5);
        return {
            preview: preview.attrs_at_caret.bold as boolean,
            typed: typed.attrs_at_caret.bold as boolean,
            mixed: typed.attrs_mixed.bold as boolean,
            hello: first.attrs_at_caret.bold as boolean,
        };
    });
    expect(out.preview, 'the empty paragraph previews bold').toBe(true);
    expect(out.typed, 'typed text is bold').toBe(true);
    expect(out.mixed, 'all of it').toBe(false);
    expect(out.hello, '"Hello" stays plain').toBe(false);
});
