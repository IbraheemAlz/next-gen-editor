import { test, expect, type Page } from '@playwright/test';
import { boot, documentText, settle } from './helpers/editor';

/* Issue #561 — a selection whose ends sit in different containers (body
 * text and a table cell) is deleted and copied across them, through the
 * REAL shell: mouse clicks on the page canvas (`pointer.ts`), Tab /
 * Shift+arrows / Shift+End / Delete / Ctrl+C / typing on the hidden
 * textarea (`HiddenInput`).
 *
 * Before the fix `DocumentTree::delete_range` clamped a cross-container
 * range to its START paragraph (only `intro`'s tail went, the table was
 * untouched) and the clipboard's plain text was that tail alone. The rules
 * (memo §6 / §7, UX_BEHAVIOR_SPEC §IV.10 / §V.1): no paragraph merges
 * across a cell boundary, a row the range covers whole is removed, a cell
 * it reaches keeps its place and loses the covered text; the plain text
 * joins a row's cells with tabs and rows with newlines.
 *
 * Setup only goes through `__dispatch` (the document, the cell texts, and a
 * SET_SELECTION that reads a click target's caret rect off the engine). */

declare global {
    interface Window {
        __dispatch: (cmd: unknown) => Promise<any>;
    }
}

type Step = { kind: 'BLOCK'; idx: number } | { kind: 'CELL'; row: number; col: number };
interface Pos {
    path: { steps: Step[] };
    offset: number;
}

const B = (idx: number): Step => ({ kind: 'BLOCK', idx });
const C = (row: number, col: number): Step => ({ kind: 'CELL', row, col });
const INTRO = [B(0)];
const cell = (row: number, col: number): Step[] => [B(1), C(row, col), B(0)];

test.use({ viewport: { width: 1280, height: 1400 } });

/** `intro`, a 2x2 table `a1 b1 / c1 d1`, `outro`; every SELECTION_CHANGED
 *  logged for the assertions. */
async function seed(page: Page): Promise<void> {
    await page.evaluate(async () => {
        const d = (c: unknown): Promise<any> => window.__dispatch(c);
        await d({ type: 'SELECT_ALL' });
        await d({ type: 'INSERT_TEXT', at: undefined, text: 'intro' });
        await d({ type: 'SPLIT_PARAGRAPH', at: undefined });
        await d({ type: 'INSERT_TEXT', at: undefined, text: 'outro' });
        const t = await d({
            type: 'INSERT_TABLE',
            at: { steps: [{ kind: 'BLOCK', idx: 1 }] },
            rows: 2,
            cols: 2,
        });
        if (t.type === 'ERROR') throw new Error(`INSERT_TABLE: ${t.message}`);
        const texts = [
            ['a1', 'b1'],
            ['c1', 'd1'],
        ];
        for (const [r, row] of texts.entries()) {
            for (const [c, text] of row.entries()) {
                const at = {
                    path: {
                        steps: [
                            { kind: 'BLOCK', idx: 1 },
                            { kind: 'CELL', row: r, col: c },
                            { kind: 'BLOCK', idx: 0 },
                        ],
                    },
                    offset: 0,
                };
                await d({ type: 'SET_SELECTION', range: { start: at, end: at }, caret: at });
                await d({ type: 'INSERT_TEXT', at: undefined, text });
            }
        }
        const log: any[] = [];
        (window as any).__selLog = log;
        (window as any).__engineClient.subscribe((e: any) => {
            if (e.type === 'SELECTION_CHANGED') log.push(e);
        });
    });
    await settle(page);
}

/** The engine's last reported selection (document order). */
async function lastSelection(page: Page): Promise<{ start: Pos; end: Pos }> {
    await settle(page);
    return page.evaluate(() => {
        const log = (window as any).__selLog as any[];
        const e = log[log.length - 1];
        return { start: e.range.start, end: e.range.end };
    });
}

/** A real mouse click on the position `offset` bytes into the paragraph at
 *  `steps`: the point is the engine's own caret rect for it (read with a
 *  SET_SELECTION, then the caret is parked at the document start so only
 *  the click can move it there). */
async function clickInto(page: Page, steps: Step[], offset: number): Promise<void> {
    const caret = await page.evaluate(
        async ({ steps, offset }) => {
            const at = { path: { steps }, offset };
            const sel = await window.__dispatch({
                type: 'SET_SELECTION',
                range: { start: at, end: at },
                caret: at,
            });
            if (sel.type !== 'SELECTION_CHANGED') throw new Error(sel.type);
            const home = { path: { steps: [{ kind: 'BLOCK', idx: 0 }] }, offset: 0 };
            await window.__dispatch({
                type: 'SET_SELECTION',
                range: { start: home, end: home },
                caret: home,
            });
            return sel.caret as { x: number; y: number; w: number; h: number };
        },
        { steps, offset },
    );
    expect(caret.h).toBeGreaterThan(0);
    const canvas = page.locator('.editor-page[data-page-index="0"] canvas').first();
    await canvas.scrollIntoViewIfNeeded();
    const box = await canvas.boundingBox();
    if (!box) throw new Error('page canvas has no box');
    const dpr = await page.evaluate(() => window.devicePixelRatio || 1);
    await page.mouse.click(box.x + caret.x / dpr, box.y + (caret.y + caret.h / 2) / dpr);
    const sel = await lastSelection(page);
    expect(sel.end).toEqual({ path: { steps }, offset });
    expect(sel.start).toEqual(sel.end);
}

/** From `in|tro`, Shift+Down twice (into row 1's first cell) and Shift+End:
 *  a selection from body text to the end of `c1`. */
async function selectFromIntroIntoRow1(page: Page): Promise<void> {
    await clickInto(page, INTRO, 2);
    await page.keyboard.press('Shift+ArrowDown');
    await page.keyboard.press('Shift+ArrowDown');
    await page.keyboard.press('Shift+End');
    const sel = await lastSelection(page);
    expect(sel.start).toEqual({ path: { steps: INTRO }, offset: 2 });
    expect(sel.end).toEqual({ path: { steps: cell(1, 0) }, offset: 2 });
}

test.beforeEach(async ({ page }) => {
    await boot(page);
    await seed(page);
    expect(await documentText(page)).toBe('intro\na1\tb1\nc1\td1\noutro');
});

test('typing over a Tab-selected cell replaces it; Delete over body text into the table deletes across the edge', async ({
    page,
}) => {
    /* Tab into (0,1) selects its content; a typed character replaces it. */
    await clickInto(page, cell(0, 0), 1);
    await page.keyboard.press('Tab');
    const tabbed = await lastSelection(page);
    expect(tabbed.start).toEqual({ path: { steps: cell(0, 1) }, offset: 0 });
    expect(tabbed.end).toEqual({ path: { steps: cell(0, 1) }, offset: 2 });
    await page.keyboard.type('Z');
    await settle(page);
    expect(await documentText(page)).toBe('intro\na1\tZ\nc1\td1\noutro');

    /* `in|tro` .. `c1|`, then Delete: `intro` keeps its head and is NOT
       merged into the cell; row 0 (covered whole) goes; `c1` (covered) is
       emptied, its cell stays; `d1` and `outro` are untouched. Before the
       fix only `tro` went. */
    await selectFromIntroIntoRow1(page);
    await page.keyboard.press('Delete');
    const caret = await lastSelection(page);
    expect(caret.end).toEqual({ path: { steps: INTRO }, offset: 2 });
    expect(caret.start).toEqual(caret.end);
    expect(await documentText(page)).toBe('in\n\td1\noutro');

    /* One undo step brings all of it back. */
    await page.keyboard.press('Control+z');
    await settle(page);
    expect(await documentText(page)).toBe('intro\na1\tZ\nc1\td1\noutro');
});

test('Ctrl+C copies a Tab-selected cell, and a body selection into the table cell by cell', async ({
    page,
    context,
}) => {
    await context.grantPermissions(['clipboard-read', 'clipboard-write']);
    const plain = (): Promise<string> =>
        page.evaluate(async () => {
            /* A read racing the shell's write throws "Clipboard data has
               changed": poll again. */
            try {
                for (const item of await navigator.clipboard.read()) {
                    if (item.types.includes('text/plain')) {
                        return (await item.getType('text/plain')).text();
                    }
                }
            } catch {
                /* fall through */
            }
            return '';
        });
    await page.evaluate(() => navigator.clipboard.writeText('before'));

    await clickInto(page, cell(0, 0), 1);
    await page.keyboard.press('Tab');
    await settle(page);
    await page.keyboard.press('Control+c');
    await expect.poll(plain, { timeout: 10_000 }).toBe('b1');

    /* Before the fix: `tro` — the start paragraph's tail alone. */
    await selectFromIntroIntoRow1(page);
    await page.keyboard.press('Control+c');
    await expect.poll(plain, { timeout: 10_000 }).toBe('tro\na1\tb1\nc1');
    /* Copy leaves the document alone. */
    expect(await documentText(page)).toBe('intro\na1\tb1\nc1\td1\noutro');
});
