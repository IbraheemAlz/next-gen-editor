import { test, expect, type Page } from '@playwright/test';
import { boot, settle } from './helpers/editor';

/* Issue #509 - formatting inside and across table cells, end to end through
 * the REAL UI: the toolbar's Table button inserts the table, the real
 * keyboard types into cells (Tab between them), real mouse clicks /
 * shift+clicks on the page canvas (`pointer.ts` -> engine hit test) make the
 * selection, and the toolbar's B / I / size / Bullet buttons format it.
 *
 * Evidence is the accessibility mirror (`[role=document]`: one <td> per
 * cell, one <span> per run carrying the run's bold / italic / underline as
 * inline style) - engine truth as the screen reader sees it - plus, for the
 * list marker (which the mirror does not carry), the worker-side opaque-ink
 * probe (`probePageInk`): headless Chrome never composites the full app's
 * canvas, so the pixels are read where they land, on the OffscreenCanvas.
 * Before #509 a selection spanning cells formatted the FIRST paragraph only
 * and a bullet on a cell paragraph set the list binding without ever
 * stamping a marker, so it painted nothing. */

declare global {
    interface Window {
        __dispatch: (cmd: unknown) => Promise<any>;
        __engineClient: { probePageInk(): Promise<Record<number, number>> };
    }
}

test.use({ viewport: { width: 1280, height: 1000 } });

type Step = { kind: 'BLOCK'; idx: number } | { kind: 'CELL'; row: number; col: number };

/** `[Block(table), Cell(row, col), Block(0)]`. */
function cellSteps(table: number, row: number, col: number): Step[] {
    return [
        { kind: 'BLOCK', idx: table },
        { kind: 'CELL', row, col },
        { kind: 'BLOCK', idx: 0 },
    ];
}

interface Rect {
    x: number;
    y: number;
    w: number;
    h: number;
}

/** The engine's caret rect (document device px) for `offset` in the cell
 *  paragraph - read by parking the caret there; the caller re-clicks. */
async function caretRect(page: Page, steps: Step[], offset: number): Promise<Rect> {
    return page.evaluate(
        async ({ steps, offset }) => {
            const at = { path: { steps }, offset };
            const sel = await window.__dispatch({
                type: 'SET_SELECTION',
                range: { start: at, end: at },
                caret: at,
            });
            if (sel.type !== 'SELECTION_CHANGED') throw new Error(sel.type);
            return sel.caret as { x: number; y: number; w: number; h: number };
        },
        { steps, offset },
    );
}

/** A real mouse click on the page-0 canvas at the vertical middle of `r`'s
 *  caret (document device px -> client CSS px). */
async function click(page: Page, r: Rect, opts: { shift?: boolean } = {}): Promise<void> {
    const canvas = page.locator('.editor-page[data-page-index="0"] canvas').first();
    await expect(canvas).toBeVisible();
    await canvas.scrollIntoViewIfNeeded();
    const box = await canvas.boundingBox();
    if (!box) throw new Error('page canvas has no box');
    const dpr = await page.evaluate(() => window.devicePixelRatio || 1);
    const x = box.x + r.x / dpr;
    const y = box.y + (r.y + r.h / 2) / dpr;
    if (opts.shift) await page.keyboard.down('Shift');
    await page.mouse.click(x, y);
    if (opts.shift) await page.keyboard.up('Shift');
    await settle(page);
}

/** The runs of the mirror's cell `(row, col)` as `{ text, style }`. */
async function cellRuns(
    page: Page,
    row: number,
    col: number,
): Promise<Array<{ text: string; bold: boolean; italic: boolean }>> {
    return page.evaluate(
        ({ row, col }) => {
            const td = document.querySelector(
                `[role="document"] td[data-row="${row}"][data-col="${col}"]`,
            );
            if (!td) return [];
            return [...td.querySelectorAll<HTMLElement>('p > span')].map((s) => ({
                text: s.textContent ?? '',
                bold: s.style.fontWeight === 'bold',
                italic: s.style.fontStyle === 'italic',
            }));
        },
        { row, col },
    );
}

/** Every run of the cell is bold / italic (and there is text to be). */
async function allRuns(
    page: Page,
    row: number,
    col: number,
    flag: 'bold' | 'italic',
): Promise<boolean> {
    const runs = (await cellRuns(page, row, col)).filter((r) => r.text !== '');
    return runs.length > 0 && runs.every((r) => r[flag]);
}

async function anyRun(
    page: Page,
    row: number,
    col: number,
    flag: 'bold' | 'italic',
): Promise<boolean> {
    return (await cellRuns(page, row, col)).some((r) => r[flag]);
}

const TEXT: Record<string, string> = {
    '0,0': 'alpha',
    '0,1': 'beta',
    '1,0': 'gamma',
    '1,1': 'delta',
};

/** Boot, insert a 3x3 table from the toolbar and type into its first two
 *  rows' first two cells with the real keyboard (Tab between cells). */
async function tableWithText(page: Page): Promise<number> {
    await boot(page);
    await page.getByRole('button', { name: 'Insert 3×3 table' }).click();
    await settle(page);
    /* The table landed after the seeded paragraph: block 1. Type with the
       real keyboard, Tab between cells. */
    await page.keyboard.type(TEXT['0,0']!);
    await page.keyboard.press('Tab');
    await page.keyboard.type(TEXT['0,1']!);
    await page.keyboard.press('Tab'); // (0,2)
    await page.keyboard.press('Tab'); // (1,0)
    await page.keyboard.type(TEXT['1,0']!);
    await page.keyboard.press('Tab');
    await page.keyboard.type(TEXT['1,1']!);
    await settle(page);
    await expect.poll(() => cellRuns(page, 1, 1).then((r) => r.map((x) => x.text).join(''))).toBe(
        'delta',
    );
    return 1;
}

test('the real UI: bold, italic across a cell selection formats every selected cell', async ({
    page,
}) => {
    test.setTimeout(90_000);
    const table = await tableWithText(page);

    /* Shift+click from the start of "alpha" to the end of "beta": two cells
       of one table -> a TABLE_CELLS selection. */
    const from = await caretRect(page, cellSteps(table, 0, 0), 0);
    const to = await caretRect(page, cellSteps(table, 0, 1), TEXT['0,1']!.length);
    await click(page, from);
    await click(page, to, { shift: true });

    await page.locator('.nge-tfmt__btn--bold').click();
    await settle(page);
    await expect.poll(() => allRuns(page, 0, 0, 'bold')).toBe(true);
    await expect.poll(() => allRuns(page, 0, 1, 'bold')).toBe(true);
    /* The cells outside the selection stay plain - and the toolbar did not
       format "the first paragraph only" (the (0,1) assertion above). */
    expect(await anyRun(page, 1, 0, 'bold')).toBe(false);
    expect(await anyRun(page, 1, 1, 'bold')).toBe(false);

    /* A second click: everything selected is already bold -> clears both. */
    await page.locator('.nge-tfmt__btn--bold').click();
    await settle(page);
    await expect.poll(() => anyRun(page, 0, 0, 'bold')).toBe(false);
    await expect.poll(() => anyRun(page, 0, 1, 'bold')).toBe(false);

    /* A 2 x 2 rectangle (alpha..delta): italic reaches all four cells. */
    const corner = await caretRect(page, cellSteps(table, 1, 1), TEXT['1,1']!.length);
    await click(page, from);
    await click(page, corner, { shift: true });
    await page.getByRole('button', { name: 'Italic' }).click();
    await settle(page);
    for (const [r, c] of [
        [0, 0],
        [0, 1],
        [1, 0],
        [1, 1],
    ] as const) {
        await expect.poll(() => allRuns(page, r, c, 'italic'), { message: `cell (${r},${c})` }).toBe(
            true,
        );
    }
});

test('the real UI: a bullet on a cell paragraph paints a marker', async ({ page }) => {
    test.setTimeout(90_000);
    const table = await tableWithText(page);

    const ink = async (): Promise<number> =>
        (await page.evaluate(() => window.__engineClient.probePageInk()))[0] ?? 0;

    /* Park the caret in "gamma" with a real click, then press Bullet. */
    const gamma = await caretRect(page, cellSteps(table, 1, 0), 2);
    await click(page, gamma);
    await settle(page);
    const before = await ink();
    expect(before).toBeGreaterThan(0);

    await page.getByRole('button', { name: 'Bulleted list' }).click();
    await settle(page);
    /* The marker is extra ink (a bullet glyph left of the text). */
    await expect.poll(async () => (await ink()) - before).toBeGreaterThan(15);

    /* Remove list takes it away again. */
    await page.getByRole('button', { name: 'Remove list' }).click();
    await settle(page);
    await expect.poll(async () => Math.abs((await ink()) - before)).toBeLessThan(15);
});
