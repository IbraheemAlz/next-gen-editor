import { test, expect, type Page } from '@playwright/test';
import { bandBox, pageCard } from './helpers/bands';
import { boot, settle } from './helpers/editor';

/* Issue #565 — Backspace / Delete inside a table cell of a HEADER.
 *
 * `story_delete_at_caret` addressed top-level blocks of the story, so for a
 * caret in a header table cell (whose path starts at the TABLE) the range
 * straddled two containers and nothing was removed: typing worked, deleting
 * did not. The story now plans a collapsed delete with the body's rule over
 * the story tree (clean-room cursor memo §7: a collapsed delete never
 * crosses a cell boundary).
 *
 * Every action is a real input: the header is entered by double-clicking
 * the page's top margin, the table comes from the toolbar's Insert-table
 * button, the text and the keys go through the hidden textarea. The header
 * text is read from the accessibility mirror (`[role="banner"]`, the part's
 * landmark), which is the DOM view of the engine's header part;
 * `__dispatch` only opens the fixture document (setup). */

async function openText(page: Page, text: string): Promise<void> {
    const type = await page.evaluate(async (t) => {
        const evt = await (window as any).__dispatch({
            type: 'OPEN_DOCUMENT',
            bytes: new TextEncoder().encode(t),
            format: 'plain_text',
            name: 'header-table.txt',
        });
        return evt.type as string;
    }, text);
    expect(type).toBe('DOCUMENT_LOADED');
    await settle(page);
}

/** Enter page 0's header by double-clicking its top margin, insert a 3x3
 *  table through the toolbar (the caret lands in its first cell). */
async function headerWithTable(page: Page): Promise<void> {
    const card = await pageCard(page, 0);
    await page.mouse.dblclick((card.left + card.right) / 2, card.top + 30);
    await bandBox(page, 0, false);
    await page.getByRole('button', { name: 'Insert 3×3 table' }).click();
    await settle(page);
    await expect(page.locator('.a11y-mirror [role="banner"] td')).toHaveCount(9);
}

const cell = (page: Page, n: number) => page.locator('.a11y-mirror [role="banner"] td').nth(n);

test.beforeEach(async ({ page }) => {
    await boot(page);
    await openText(page, 'body text of the document');
    await headerWithTable(page);
});

test('Backspace and Delete edit the text of a header table cell', async ({ page }) => {
    await page.keyboard.type('abc');
    await expect(cell(page, 0)).toHaveText('abc');

    /* Left, Backspace: the character before the caret goes (`b`). */
    await page.keyboard.press('ArrowLeft');
    await page.keyboard.press('Backspace');
    await expect(cell(page, 0)).toHaveText('ac');

    /* Delete: the character after the caret goes (`c`). */
    await page.keyboard.press('Delete');
    await expect(cell(page, 0)).toHaveText('a');

    /* Typing proves the caret is still in the cell, and the neighbours and
       the body were never touched. */
    await page.keyboard.type('Z');
    await expect(cell(page, 0)).toHaveText('aZ');
    for (let n = 1; n < 9; n++) await expect(cell(page, n)).toHaveText('');
    await expect(page.locator('.a11y-mirror > p').first()).toHaveText(
        'body text of the document',
    );
});

test('Ctrl+Backspace deletes a word of the header cell', async ({ page }) => {
    await page.keyboard.type('one two');
    await expect(cell(page, 0)).toHaveText('one two');
    await page.keyboard.press('Control+Backspace');
    await expect(cell(page, 0)).toHaveText('one');
});

test('Backspace at the start of a cell and Delete at its end delete nothing', async ({ page }) => {
    await page.keyboard.type('abc');
    await page.keyboard.press('Tab');
    await page.keyboard.type('xy');
    await expect(cell(page, 0)).toHaveText('abc');
    await expect(cell(page, 1)).toHaveText('xy');

    /* Home, Backspace in the second cell: it would join the first one if a
       delete crossed the boundary. */
    await page.keyboard.press('Home');
    await page.keyboard.press('Backspace');
    /* End, Delete: the end of the second cell is not the start of the third. */
    await page.keyboard.press('End');
    await page.keyboard.press('Delete');
    await settle(page);
    await expect(cell(page, 0)).toHaveText('abc');
    await expect(cell(page, 1)).toHaveText('xy');
    await expect(cell(page, 2)).toHaveText('');

    /* Still in the second cell: typing lands at its end. */
    await page.keyboard.type('!');
    await expect(cell(page, 1)).toHaveText('xy!');
    await expect(cell(page, 0)).toHaveText('abc');
});
