import { test, expect, type Page, type Locator } from '@playwright/test';
import { boot, settle } from './helpers/editor';

/* Issue #387 — through the REAL worker + WASM engine and the shell:
 *
 *   - keyboard navigation keeps the caret visible: 60 × ArrowDown from the
 *     top of a long document scrolls `.editor-viewport` until the caret
 *     overlay is inside it, while a pointer click on the page never
 *     scrolls (the engine stamps `SELECTION_CHANGED.reveal_caret` from the
 *     command's `bridge::meta` classification);
 *   - commented text carries a persistent `.nge-comment-highlight` overlay
 *     (per-line rects through the engine geometry, like selection rects):
 *     present right after a comment is inserted from the toolbar, exactly
 *     over the commented words, following them when text is typed before
 *     the anchor; hover shows the author, a click selects the comment's
 *     card in the rail, deleting the comment clears it.
 *
 * Assertions ride DOM overlay geometry and engine replies — valid under
 * headless Chrome, which never composites the interactive canvas (see
 * CLAUDE.md). */

async function dispatch(page: Page, ...cmds: unknown[]): Promise<any> {
    return page.evaluate(async (cmds) => {
        let last: any;
        for (const cmd of cmds) last = await (window as any).__dispatch(cmd);
        return last;
    }, cmds);
}

const top = (idx: number, offset: number) => ({
    path: { steps: [{ kind: 'BLOCK', idx }] },
    offset,
});
const select = (a: [number, number], b: [number, number]) => ({
    type: 'SET_SELECTION',
    range: { start: top(...a), end: top(...b) },
    caret: top(...b),
});

async function scrollTop(page: Page): Promise<number> {
    return page.evaluate(() => document.querySelector<HTMLElement>('.editor-viewport')!.scrollTop);
}

/** Whether the caret overlay is fully inside the viewport's visible box. */
async function caretVisible(page: Page): Promise<boolean> {
    return page.evaluate(() => {
        const v = document.querySelector<HTMLElement>('.editor-viewport')!.getBoundingClientRect();
        const c = document.querySelector<HTMLElement>('.caret');
        if (!c) return false;
        const r = c.getBoundingClientRect();
        return r.height > 0 && r.top >= v.top && r.bottom <= v.bottom;
    });
}

test('arrow down 60 lines scrolls the caret into view; a pointer click does not scroll', async ({
    page,
}) => {
    test.setTimeout(90_000);
    await boot(page);
    const lines = Array.from({ length: 90 }, (_, i) => `Line ${i + 1} of the long document`);
    const opened = await page.evaluate(async (text) => {
        const bytes = new TextEncoder().encode(text);
        return (await (window as any).__dispatch({
            type: 'OPEN_DOCUMENT',
            bytes,
            format: 'plain_text',
            name: 'long.txt',
        })).type;
    }, lines.join('\n'));
    expect(opened).toBe('DOCUMENT_LOADED');
    await dispatch(page, select([0, 0], [0, 0]));
    await settle(page);
    await expect.poll(() => scrollTop(page)).toBe(0);
    expect(await caretVisible(page)).toBe(true);

    /* Real key presses through the hidden textarea, at typing speed (each
       answered before the next: a burst outruns the lazy layout band —
       `MOVE_CARET Down` stops at the band's last line until the shell's
       scroll-driven `EXPAND_LAYOUT` catches up). */
    await page.locator('textarea[data-nge-hidden-input]').focus();
    for (let i = 0; i < 60; i++) {
        await page.keyboard.press('ArrowDown');
        await settle(page);
    }

    await expect.poll(() => scrollTop(page), { timeout: 10_000 }).toBeGreaterThan(300);
    await expect.poll(() => caretVisible(page), { timeout: 10_000 }).toBe(true);
    /* The engine caret really is 60 lines down. */
    const caretPara = await page.evaluate(async () => {
        const evt = await (window as any).__dispatch({ type: 'MOVE_CARET', direction: 'Left', extend: false });
        return evt.type === 'SELECTION_CHANGED' ? evt.range.end.path.steps[0].idx : -1;
    });
    expect(caretPara).toBeGreaterThanOrEqual(59);

    /* A pointer click inside the visible page area places the caret but
       leaves the scroll where it is. */
    const before = await scrollTop(page);
    const box = (await page.locator('.editor-viewport').boundingBox())!;
    await page.mouse.click(box.x + box.width / 2, box.y + box.height * 0.8);
    await settle(page);
    await page.waitForTimeout(250);
    expect(await scrollTop(page)).toBe(before);
});

function highlight(page: Page): Locator {
    return page.locator('.nge-comment-highlight');
}

async function boxOf(l: Locator): Promise<{ x: number; y: number; width: number; height: number }> {
    const b = await l.boundingBox();
    expect(b).not.toBeNull();
    return b!;
}

/** The selection overlay's box for `[a, b)` (the highlight's reference). */
async function selectionBox(page: Page, a: [number, number], b: [number, number]) {
    await dispatch(page, select(a, b));
    await expect(page.locator('.selection-rect')).toHaveCount(1);
    return boxOf(page.locator('.selection-rect').first());
}

test('a comment highlight appears on insert, follows typing before the anchor, shows its author', async ({
    page,
}) => {
    test.setTimeout(60_000);
    await boot(page);
    await dispatch(
        page,
        { type: 'SELECT_ALL' },
        { type: 'INSERT_TEXT', at: undefined, text: 'alpha beta gamma' },
    );
    await expect(highlight(page)).toHaveCount(0);

    /* Insert on the selection "beta" from the toolbar. */
    await dispatch(page, select([0, 6], [0, 10]));
    await page.getByRole('button', { name: 'New comment' }).click();
    await page.getByPlaceholder('Comment text…').fill('Check this word');
    await page.getByRole('button', { name: 'Comment', exact: true }).click();

    await expect(highlight(page)).toHaveCount(1);
    const id = Number(await highlight(page).getAttribute('data-comment-id'));
    const author = (await highlight(page).getAttribute('data-author')) ?? '';
    const ref = await selectionBox(page, [0, 6], [0, 10]);
    let hl = await boxOf(highlight(page));
    expect(Math.abs(hl.x - ref.x)).toBeLessThan(1);
    expect(Math.abs(hl.width - ref.width)).toBeLessThan(1);
    expect(Math.abs(hl.y - ref.y)).toBeLessThan(1);

    /* Typing before the anchor moves the highlight with the word. */
    await dispatch(page, select([0, 0], [0, 0]), {
        type: 'INSERT_TEXT',
        at: undefined,
        text: 'Note: ',
    });
    await expect.poll(async () => (await boxOf(highlight(page))).x).toBeGreaterThan(hl.x + 10);
    const moved = await selectionBox(page, [0, 12], [0, 16]);
    hl = await boxOf(highlight(page));
    expect(Math.abs(hl.x - moved.x)).toBeLessThan(1);
    expect(Math.abs(hl.width - moved.width)).toBeLessThan(1);

    /* Hover shows the author. */
    await dispatch(page, select([0, 0], [0, 0]));
    await page.mouse.move(hl.x + hl.width / 2, hl.y + hl.height / 2);
    const tip = page.locator('.nge-comment-highlight__tip');
    await expect(tip).toBeVisible();
    await expect(tip).toHaveText(author || 'Anonymous');
    await expect(highlight(page)).toHaveClass(/nge-comment-highlight--hover/);

    /* A click on the highlight places the caret (it is click-through) and
       selects the comment's card in the rail. */
    await page.mouse.click(hl.x + hl.width / 2, hl.y + hl.height / 2);
    const card = page.locator(`.nge-cm__card[data-comment-id="${id}"]`);
    await expect(card).toHaveClass(/nge-cm__card--active/);
    await expect(highlight(page)).toHaveClass(/nge-comment-highlight--active/);
    const caret = await page.evaluate(async () => {
        const evt = await (window as any).__dispatch({ type: 'GET_SELECTION_AS_CLIPBOARD' });
        return evt.plain as string;
    });
    expect(caret).toBe('');

    /* Deleting the comment clears its highlight. */
    await card.getByRole('button', { name: 'Delete comment' }).click();
    await expect(highlight(page)).toHaveCount(0);
});
