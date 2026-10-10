import { test, expect, type Page } from '@playwright/test';
import { boot, settle } from './helpers/editor';

/* Issue #465 — through the real worker + WASM engine and the shell:
 *   - a comment (ranged or on a point) draws a dotted connector path from
 *     its anchor to its card in the comments rail, and stops drawing it
 *     when the comment is deleted;
 *   - the rail card whose range holds the caret is active (ArrowRight
 *     entering the range activates it, leaving deactivates it);
 *   - clicking a card selects the commented text and reveals it.
 * DOM overlay geometry only (headless Chrome never composites the canvas,
 * see CLAUDE.md). */

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
const range = (a: number, b: number) => ({ start: top(0, a), end: top(0, b) });
const caretAt = (offset: number) => ({
    type: 'SET_SELECTION',
    range: range(offset, offset),
    caret: top(0, offset),
});

async function key(page: Page, k: string): Promise<void> {
    await page.locator('textarea[data-nge-hidden-input]').focus();
    await page.keyboard.press(k);
    await settle(page);
}

test('a point comment draws a connector to its card; deleting it removes the connector', async ({
    page,
}) => {
    test.setTimeout(60_000);
    await boot(page);
    await dispatch(
        page,
        { type: 'SELECT_ALL' },
        { type: 'INSERT_TEXT', at: undefined, text: 'alpha beta gamma' },
    );
    const ev = await dispatch(page, {
        type: 'INSERT_COMMENT',
        range: range(6, 6),
        text: 'point note',
        author: 'Tester',
    });
    expect(ev.type).not.toBe('ERROR');
    await expect(page.locator('.nge-cm__card')).toHaveCount(1);
    const id = await page.locator('.nge-cm__card').getAttribute('data-comment-id');
    const path = page.locator(`path.nge-comment-connector[data-comment-id="${id}"]`);
    await expect(path).toHaveCount(1);
    const d = (await path.getAttribute('d')) ?? '';
    expect(d).toMatch(/^M [\d.-]+ [\d.-]+ L [\d.-]+ [\d.-]+ L [\d.-]+ [\d.-]+$/);

    /* It ends at the rail edge, at the card's height. */
    const nums = d.match(/-?[\d.]+/g)!.map(Number);
    const railBox = (await page.locator('.nge-cm').boundingBox())!;
    const cardBox = (await page.locator('.nge-cm__card').boundingBox())!;
    expect(Math.abs(nums[4]! - railBox.x)).toBeLessThan(2);
    expect(nums[5]!).toBeGreaterThanOrEqual(cardBox.y);
    expect(nums[5]!).toBeLessThanOrEqual(cardBox.y + cardBox.height);

    await page.getByRole('button', { name: 'Delete comment' }).click();
    await expect(page.locator('path.nge-comment-connector')).toHaveCount(0);
});

test('the card is active while the caret is inside the comment range (ArrowRight in / out)', async ({
    page,
}) => {
    test.setTimeout(60_000);
    await boot(page);
    await dispatch(
        page,
        { type: 'SELECT_ALL' },
        { type: 'INSERT_TEXT', at: undefined, text: 'alpha beta gamma' },
    );
    await dispatch(page, {
        type: 'INSERT_COMMENT',
        range: range(6, 8),
        text: 'range note',
        author: 'Tester',
    });
    const card = page.locator('.nge-cm__card');
    await expect(card).toHaveCount(1);

    await dispatch(page, caretAt(5));
    await settle(page);
    await expect(card).not.toHaveClass(/nge-cm__card--active/);
    await key(page, 'ArrowRight'); // 6: first character of the range
    await expect(card).toHaveClass(/nge-cm__card--active/);
    await key(page, 'ArrowRight'); // 7
    await expect(card).toHaveClass(/nge-cm__card--active/);
    await key(page, 'ArrowRight'); // 8: just after the range
    await expect(card).not.toHaveClass(/nge-cm__card--active/);
    await key(page, 'ArrowLeft'); // back to 7
    await expect(card).toHaveClass(/nge-cm__card--active/);
});

test('clicking a card selects the commented text and reveals it', async ({ page }) => {
    test.setTimeout(90_000);
    await boot(page);
    const lines = Array.from({ length: 120 }, (_, i) => `Line ${i + 1} of the long document`);
    await page.evaluate(async (text) => {
        await (window as any).__dispatch({
            type: 'OPEN_DOCUMENT',
            bytes: new TextEncoder().encode(text),
            format: 'plain_text',
            name: 'long.txt',
        });
    }, lines.join('\n'));
    await dispatch(page, {
        type: 'INSERT_COMMENT',
        range: range(0, 4),
        text: 'top note',
        author: 'Tester',
    });
    const card = page.locator('.nge-cm__card');
    await expect(card).toHaveCount(1);

    /* Scroll the caret far away, then click the card text. */
    await dispatch(page, caretAt(0));
    await page.evaluate(() => {
        document.querySelector<HTMLElement>('.editor-viewport')!.scrollTop = 1200;
    });
    await expect
        .poll(() =>
            page.evaluate(() => document.querySelector<HTMLElement>('.editor-viewport')!.scrollTop),
        )
        .toBeGreaterThan(500);
    await card.locator('.nge-cm__text').click();
    await expect(card).toHaveClass(/nge-cm__card--active/);
    await expect
        .poll(() =>
            page.evaluate(() => document.querySelector<HTMLElement>('.editor-viewport')!.scrollTop),
        )
        .toBeLessThan(100);
});
