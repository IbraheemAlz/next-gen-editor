import { test, expect, type Page } from '@playwright/test';
import { boot, settle } from './helpers/editor';

/* Issues #552 / #507 — through the REAL UI (keyboard on the hidden
 * textarea, the toolbar's "Insert 3×3 table" button):
 *
 *  - the editor boots an LTR document, so the empty paragraph Enter opens
 *    after the Latin seed text is LTR: the a11y mirror says `dir="ltr"` and
 *    the caret overlay sits at the LEFT margin, where the seed line starts
 *    (it used to sit at the right margin: the boot base was RTL and an
 *    empty paragraph fell back to it);
 *  - the inserted table's empty cells are LTR, and ArrowRight / ArrowLeft
 *    walk them in reading order — along a row, then to the next row, then
 *    out of the table — instead of jumping to the PREVIOUS cell (#507).
 *
 * Every action is a real key press or click; `__dispatch` is never used
 * to act. Assertions read the caret overlay (`.caret`, plain DOM) and the
 * a11y mirror, both valid under headless Chrome (the canvas is never
 * screenshotted, see CLAUDE.md). */

const INPUT = 'textarea[data-nge-hidden-input]';

interface CaretAt {
    x: number;
    y: number;
}

/** The caret overlay's top-left in the viewport's SCROLL content (CSS px),
 *  so a caret-reveal scroll between two reads cannot fake a move. */
async function caretAt(page: Page): Promise<CaretAt> {
    await settle(page);
    return page.evaluate(() => {
        const c = document.querySelector<HTMLElement>('.caret');
        const v = document.querySelector<HTMLElement>('.editor-viewport');
        if (!c || !v) throw new Error('no caret overlay / viewport');
        const r = c.getBoundingClientRect();
        const vr = v.getBoundingClientRect();
        return { x: r.left - vr.left + v.scrollLeft, y: r.top - vr.top + v.scrollTop };
    });
}

async function press(page: Page, key: string): Promise<CaretAt> {
    await page.keyboard.press(key);
    return caretAt(page);
}

test('an empty paragraph and an inserted table are LTR; arrows walk the cells in reading order', async ({
    page,
}) => {
    test.setTimeout(90_000);
    await boot(page);
    const input = page.locator(INPUT);
    await input.focus();

    /* The seed paragraph's start: the left margin. */
    const start = await press(page, 'Control+Home');

    /* Enter at the end of the seed text opens an empty paragraph. */
    await press(page, 'Control+End');
    const empty = await press(page, 'Enter');
    const mirror = page.locator('.a11y-mirror');
    const paragraphs = mirror.locator(':scope > p');
    await expect(paragraphs).toHaveCount(2);
    await expect(paragraphs.nth(1)).toHaveAttribute('dir', 'ltr');
    expect(empty.y, 'the caret moved to the next line').toBeGreaterThan(start.y);
    expect(
        Math.abs(empty.x - start.x),
        `empty paragraph caret x ${empty.x} vs left margin ${start.x}`,
    ).toBeLessThan(2);

    /* The toolbar's table button: a 3 x 3 table after the caret's
       paragraph, the caret parked in cell (0, 0). */
    await page.getByRole('button', { name: 'Insert 3×3 table' }).click();
    await expect(mirror.locator('td[role="gridcell"]')).toHaveCount(9);
    const cellDirs = await mirror
        .locator('td[role="gridcell"] p')
        .evaluateAll((els) => els.map((el) => el.getAttribute('dir')));
    expect(cellDirs).toEqual(Array(9).fill('ltr'));
    await input.focus();

    /* Right from cell (0, 0) through (2, 2), then out of the table. */
    const walk: CaretAt[] = [await caretAt(page)];
    for (let i = 0; i < 9; i++) walk.push(await press(page, 'ArrowRight'));
    const cells = walk.slice(0, 9);
    const after = walk[9]!;
    for (let r = 0; r < 3; r++) {
        const row = cells.slice(r * 3, r * 3 + 3);
        expect(
            row.every((c) => Math.abs(c.y - row[0]!.y) < 1),
            `row ${r} stays on one line: ${JSON.stringify(row)}`,
        ).toBe(true);
        expect(
            row[0]!.x < row[1]!.x && row[1]!.x < row[2]!.x,
            `row ${r} walks left to right: ${JSON.stringify(row)}`,
        ).toBe(true);
        if (r > 0) {
            expect(row[0]!.y, `row ${r} is below row ${r - 1}`).toBeGreaterThan(cells[(r - 1) * 3]!.y);
            expect(
                Math.abs(row[0]!.x - cells[0]!.x),
                `row ${r} starts in the first column`,
            ).toBeLessThan(1);
        }
    }
    /* An LTR cell's caret sits at the cell's left edge: the first column's
       caret is near the left margin, far from the page centre. */
    expect(cells[0]!.x - start.x).toBeLessThan(cells[1]!.x - cells[0]!.x);
    expect(after.y, 'out of the table, below it').toBeGreaterThan(cells[8]!.y);
    expect(Math.abs(after.x - start.x), 'the paragraph after the table, left margin').toBeLessThan(
        2,
    );

    /* Left walks back through the same cells in reverse, then out above. */
    for (let i = 8; i >= 0; i--) {
        const got = await press(page, 'ArrowLeft');
        expect(
            Math.abs(got.x - cells[i]!.x) < 1 && Math.abs(got.y - cells[i]!.y) < 1,
            `Left #${9 - i}: ${JSON.stringify(got)} vs cell ${i} ${JSON.stringify(cells[i])}`,
        ).toBe(true);
    }
    const back = await press(page, 'ArrowLeft');
    expect(
        Math.abs(back.x - empty.x) < 1 && Math.abs(back.y - empty.y) < 1,
        `Left from cell (0, 0) returns to the empty paragraph: ${JSON.stringify(back)}`,
    ).toBe(true);
});
