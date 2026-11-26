import { test, expect, type Page } from '@playwright/test';
import { boot, scrollViewportTo, settle } from './helpers/editor';

/* Epic #555 — the NAVIGATION / STORY rows of the cursor-behaviour matrix,
 * one `test()` per row, each driving the REAL UI: keys go to the hidden
 * textarea (`HiddenInput.onKeyDown` / `beforeinput`), clicks to a page canvas
 * (`pointer.ts`), list / header buttons through the toolbar, paste through
 * the textarea's native `paste` event. `__dispatch` only opens the fixture
 * document (setup) and reads state back.
 *
 * Expected behaviour is the clean-room methods memo
 * `plans/cleanroom/memos/onlyoffice-cursor-memo.md` (§2, §4, §5, §8, §10).
 *
 * State is read from the engine's own replies (every `SELECTION_CHANGED` the
 * client fans out: the logical range, the caret rect, the active story, the
 * list level, the attributes at the caret) and from the accessibility
 * mirror — both valid under headless Chrome, which never composites the
 * interactive canvas (CLAUDE.md). */

test.use({ viewport: { width: 1280, height: 900 } });

const INPUT = 'textarea[data-nge-hidden-input]';

interface Rect {
    x: number;
    y: number;
    w: number;
    h: number;
}

type Step = { kind: 'BLOCK'; idx: number } | { kind: 'CELL'; row: number; col: number };

interface Pos {
    path: { steps: Step[] };
    offset: number;
}

interface Sel {
    range: { start: Pos; end: Pos };
    caret: Rect;
    editing_story?: { area: string; page: number };
    list_ilvl?: number;
    attrs_at_caret: { bold: boolean };
}

/** Record every SELECTION_CHANGED the client fans out (setup). */
async function recordSelection(page: Page): Promise<void> {
    await page.evaluate(() => {
        const w = window as any;
        if (w.__navSelLog) return;
        w.__navSelLog = true;
        w.__engineClient.subscribe((e: any) => {
            if (e.type === 'SELECTION_CHANGED') w.__navSel = e;
        });
    });
}

/** The engine's selection after every command already posted. */
async function sel(page: Page): Promise<Sel> {
    await settle(page);
    return page.evaluate(() => (window as any).__navSel as Sel);
}

/** Caret offset + top-level block index of the collapsed caret. */
async function caretPos(page: Page): Promise<{ block: number; offset: number; path: Step[] }> {
    const s = await sel(page);
    const steps = s.range.end.path.steps;
    const first = steps[0];
    return {
        block: first && first.kind === 'BLOCK' ? first.idx : -1,
        offset: s.range.end.offset,
        path: steps,
    };
}

/** Replace the document with plain text, caret at its start (setup). */
async function openText(page: Page, text: string): Promise<void> {
    const type = await page.evaluate(async (t) => {
        const evt = await (window as any).__dispatch({
            type: 'OPEN_DOCUMENT',
            bytes: new TextEncoder().encode(t),
            format: 'plain_text',
            name: 'nav.txt',
        });
        return evt.type as string;
    }, text);
    expect(type).toBe('DOCUMENT_LOADED');
    await recordSelection(page);
    await settle(page);
    await page.locator(INPUT).focus();
}

/** Park the caret at `pos` (setup only — never the action under test). */
async function park(page: Page, steps: Step[], offset: number): Promise<void> {
    await page.evaluate(
        async ({ steps, offset }) => {
            const at = { path: { steps }, offset };
            const r = await (window as any).__dispatch({
                type: 'SET_SELECTION',
                range: { start: at, end: at },
                caret: at,
            });
            if (r.type !== 'SELECTION_CHANGED') throw new Error(r.type);
        },
        { steps, offset },
    );
    await page.locator(INPUT).focus();
}

const top = (idx: number): Step[] => [{ kind: 'BLOCK', idx }];

interface Box {
    top: number;
    bottom: number;
    left: number;
    right: number;
}

async function box(page: Page, selector: string): Promise<Box | null> {
    return page.evaluate((s) => {
        const el = document.querySelector(s);
        if (!el) return null;
        const r = el.getBoundingClientRect();
        return { top: r.top, bottom: r.bottom, left: r.left, right: r.right };
    }, selector);
}

const pageCard = async (page: Page, idx: number): Promise<Box> =>
    (await box(page, `.editor-page[data-page-index="${idx}"]`))!;

/** Bring page `idx`'s top edge to the viewport's top area, so clicks into
 *  its header band land on screen. */
async function scrollToPage(page: Page, idx: number): Promise<void> {
    const card = await pageCard(page, idx);
    const v = (await box(page, '.editor-viewport'))!;
    const s = await page.evaluate(() => document.querySelector('.editor-viewport')!.scrollTop);
    await scrollViewportTo(page, Math.max(0, s + card.top - v.top - 40));
}

/** Bring page `idx`'s bottom edge into the viewport. */
async function scrollToPageBottom(page: Page, idx: number): Promise<void> {
    const card = await pageCard(page, idx);
    const v = (await box(page, '.editor-viewport'))!;
    const s = await page.evaluate(() => document.querySelector('.editor-viewport')!.scrollTop);
    await scrollViewportTo(page, Math.max(0, s + card.bottom - v.bottom + 60));
}

/** The page whose card contains document device-px `y` and its local y. */
async function pageOfCaret(page: Page): Promise<{ page: number; localY: number; localX: number }> {
    await settle(page);
    return page.evaluate(() => {
        const c = document.querySelector('.caret')!.getBoundingClientRect();
        const cards = [...document.querySelectorAll<HTMLElement>('.editor-page')];
        for (const card of cards) {
            const r = card.getBoundingClientRect();
            if (c.top >= r.top - 1 && c.top <= r.bottom) {
                return {
                    page: Number(card.dataset.pageIndex),
                    localY: c.top - r.top,
                    localX: c.left - r.left,
                };
            }
        }
        return { page: -1, localY: -1, localX: -1 };
    });
}

/** `n` one-line paragraphs, long enough to run over several pages. */
const lines = (n: number, text = (i: number) => `Line ${i + 1} of the navigation document`) =>
    Array.from({ length: n }, (_, i) => text(i)).join('\n');

test.beforeEach(async ({ page }) => {
    test.setTimeout(90_000);
    await boot(page);
    await recordSelection(page);
});

/* ---------------------------------------------------------------- row 8 */

test('row 08: while editing a header, a body click is ignored, a band click switches part, a body double-click exits at the click, Esc exits', async ({
    page,
}) => {
    await openText(page, lines(60));
    await expect(page.locator('.editor-page[data-page-index="1"]')).toHaveCount(1);

    await page.getByRole('button', { name: 'Edit header' }).click();
    await expect(page.locator('.editor-page[data-page-index="0"] .story-band')).toHaveCount(1);
    await page.keyboard.type('HDR');
    expect((await sel(page)).editing_story).toMatchObject({ area: 'Header', page: 0 });

    /* A single click on the dimmed body is ignored: still in the header,
       and typing continues there. */
    await scrollViewportTo(page, 0);
    let card = await pageCard(page, 0);
    await page.mouse.click(card.left + 96 + 150, card.top + 96 + 60);
    await settle(page);
    expect((await sel(page)).editing_story).toMatchObject({ area: 'Header', page: 0 });
    await page.keyboard.type('x');
    await expect(page.locator('.a11y-mirror [role="banner"]').first()).toContainText('HDRx');
    await expect(page.locator('.a11y-mirror > p').first()).toHaveText(/^Line 1 of/);

    /* A single click in the footer band of the same page switches to the
       footer. */
    await scrollToPageBottom(page, 0);
    card = await pageCard(page, 0);
    await page.mouse.click(card.left + 300, card.bottom - 30);
    await expect(page.locator('.editor-page[data-page-index="0"] .story-band--footer')).toHaveCount(
        1,
    );
    expect((await sel(page)).editing_story).toMatchObject({ area: 'Footer', page: 0 });

    /* A single click in the header band of the NEXT page switches to that
       page's header instance. */
    await scrollToPage(page, 1);
    card = await pageCard(page, 1);
    await page.mouse.click(card.left + 300, card.top + 30);
    await expect(page.locator('.editor-page[data-page-index="1"] .story-band')).toHaveCount(1);
    expect((await sel(page)).editing_story).toMatchObject({ area: 'Header', page: 1 });

    /* A double-click in the body exits and puts the body caret at the
       click. */
    card = await pageCard(page, 1);
    const localX = 96 + 140;
    const localY = 96 + 30;
    await page.mouse.dblclick(card.left + localX, card.top + localY);
    await expect(page.locator('.story-band')).toHaveCount(0);
    const after = await sel(page);
    expect(after.editing_story).toBeUndefined();
    const at = await pageOfCaret(page);
    expect(at.page).toBe(1);
    expect(Math.abs(at.localX - localX)).toBeLessThan(20);

    /* Esc exits a story too. */
    await page.getByRole('button', { name: 'Edit header' }).click();
    await expect(page.locator('.story-band')).toHaveCount(1);
    await page.keyboard.press('Escape');
    await expect(page.locator('.story-band')).toHaveCount(0);
    expect((await sel(page)).editing_story).toBeUndefined();
});

/* --------------------------------------------------------------- row 10 */

test('row 10: Up / Down never cross between a header or footer and the body', async ({ page }) => {
    await openText(page, lines(3));

    /* Body: Up on the first line and Down on the last stay in the body. */
    await park(page, top(0), 4);
    for (let i = 0; i < 3; i++) await page.keyboard.press('ArrowUp');
    let s = await sel(page);
    expect(s.editing_story).toBeUndefined();
    expect((await caretPos(page)).block).toBe(0);
    await park(page, top(2), 4);
    for (let i = 0; i < 3; i++) await page.keyboard.press('ArrowDown');
    s = await sel(page);
    expect(s.editing_story).toBeUndefined();
    expect((await caretPos(page)).block).toBe(2);
    await expect(page.locator('.story-band')).toHaveCount(0);

    /* Header: two lines; Up from the first and Down from the last stay in
       the part (typing proves where the caret is). */
    await page.getByRole('button', { name: 'Edit header' }).click();
    await expect(page.locator('.story-band')).toHaveCount(1);
    await page.keyboard.type('H1');
    await page.keyboard.press('Enter');
    await page.keyboard.type('H2');
    for (let i = 0; i < 3; i++) await page.keyboard.press('ArrowDown');
    expect((await sel(page)).editing_story).toMatchObject({ area: 'Header' });
    await page.keyboard.type('d');
    await page.keyboard.press('ArrowUp');
    for (let i = 0; i < 3; i++) await page.keyboard.press('ArrowUp');
    expect((await sel(page)).editing_story).toMatchObject({ area: 'Header' });
    await page.keyboard.type('u');
    const banner = page.locator('.a11y-mirror [role="banner"]').first();
    await expect(banner).toContainText('H2d');
    await expect(banner.locator('p').first()).toContainText('u');
    await expect(page.locator('.a11y-mirror > p').first()).toHaveText(/^Line 1 of/);
    await page.keyboard.press('Escape');

    /* Footer: Up from its first line stays in it. */
    await page.getByRole('button', { name: 'Edit footer' }).click();
    await expect(page.locator('.story-band--footer')).toHaveCount(1);
    await page.keyboard.type('F1');
    for (let i = 0; i < 3; i++) await page.keyboard.press('ArrowUp');
    for (let i = 0; i < 3; i++) await page.keyboard.press('ArrowDown');
    expect((await sel(page)).editing_story).toMatchObject({ area: 'Footer' });
    await page.keyboard.type('f');
    await expect(page.locator('.a11y-mirror [role="contentinfo"]').first()).toContainText(/F1f|fF1/);
    await expect(page.locator('.a11y-mirror > p').last()).toHaveText(/^Line 3 of/);
});

/* --------------------------------------------------------------- row 13 */

test('row 13: with a selection, Left collapses to its logical start and Right to its logical end (flipped in an RTL paragraph)', async ({
    page,
}) => {
    await openText(page, 'Hello brave world\nمرحبا بالعالم الجميل');

    /* LTR, forward selection 6..11 ("brave"): Left -> 6, no further move. */
    await park(page, top(0), 6);
    for (let i = 0; i < 5; i++) await page.keyboard.press('Shift+ArrowRight');
    let s = await sel(page);
    expect([s.range.start.offset, s.range.end.offset]).toEqual([6, 11]);
    await page.keyboard.press('ArrowLeft');
    expect(await caretPos(page)).toMatchObject({ block: 0, offset: 6 });

    /* LTR, backward selection (focus at the start): Right -> the end. */
    await park(page, top(0), 11);
    for (let i = 0; i < 5; i++) await page.keyboard.press('Shift+ArrowLeft');
    s = await sel(page);
    expect([s.range.start.offset, s.range.end.offset]).toEqual([6, 11]);
    await page.keyboard.press('ArrowRight');
    expect(await caretPos(page)).toMatchObject({ block: 0, offset: 11 });

    /* RTL paragraph: the key mapping flips, so Right (logical backward)
       collapses to the logical START and Left (logical forward) to the
       logical END — each to the visual edge the arrow points at. */
    const word = new TextEncoder().encode('مرحبا ').length;
    const wordEnd = word + new TextEncoder().encode('بالعالم').length;
    await park(page, top(1), word);
    for (let i = 0; i < 7; i++) await page.keyboard.press('Shift+ArrowLeft');
    s = await sel(page);
    expect([s.range.start.offset, s.range.end.offset]).toEqual([word, wordEnd]);
    await page.keyboard.press('ArrowRight');
    expect(await caretPos(page)).toMatchObject({ block: 1, offset: word });
    await park(page, top(1), wordEnd);
    for (let i = 0; i < 7; i++) await page.keyboard.press('Shift+ArrowRight');
    s = await sel(page);
    expect([s.range.start.offset, s.range.end.offset]).toEqual([word, wordEnd]);
    await page.keyboard.press('ArrowLeft');
    expect(await caretPos(page)).toMatchObject({ block: 1, offset: wordEnd });
});

/* --------------------------------------------------------------- row 17 */

test('row 17: Up / Down inside a header story go line by line and never leave it', async ({
    page,
}) => {
    await openText(page, lines(3));
    await page.getByRole('button', { name: 'Edit header' }).click();
    await expect(page.locator('.story-band')).toHaveCount(1);
    await page.keyboard.type('alpha');
    await page.keyboard.press('Enter');
    await page.keyboard.type('beta');
    await page.keyboard.press('Enter');
    await page.keyboard.type('gamma');
    expect((await caretPos(page)).block).toBe(2);
    await page.keyboard.press('ArrowUp');
    expect((await caretPos(page)).block).toBe(1);
    await page.keyboard.press('ArrowUp');
    expect((await caretPos(page)).block).toBe(0);
    await page.keyboard.press('ArrowUp');
    expect(await caretPos(page)).toMatchObject({ block: 0 });
    expect((await sel(page)).editing_story).toMatchObject({ area: 'Header' });
    await page.keyboard.press('ArrowDown');
    expect((await caretPos(page)).block).toBe(1);
    await page.keyboard.press('ArrowDown');
    expect((await caretPos(page)).block).toBe(2);
    await page.keyboard.press('ArrowDown');
    expect((await caretPos(page)).block).toBe(2);
    expect((await sel(page)).editing_story).toMatchObject({ area: 'Header' });
    /* The caret walked the band's own lines: its overlay stays in the
       header band the whole time. */
    const band = (await box(page, '.story-band'))!;
    const caret = (await box(page, '.caret'))!;
    expect(caret.top).toBeGreaterThanOrEqual(band.top - 3);
    expect(caret.bottom).toBeLessThanOrEqual(band.bottom + 3);
});

/* --------------------------------------------------------------- row 19 */

test('row 19: Home / End go to the logical line-segment edges (RTL Home = right edge) and set the column; Ctrl+Home / End stay in the active story', async ({
    page,
}) => {
    const long = Array.from({ length: 40 }, (_, i) => `word${i}`).join(' ');
    const arabic = Array.from({ length: 30 }, () => 'كلمة').join(' ');
    await openText(page, `${long}\n${arabic}\nlast line`);

    /* A wrapped LTR paragraph: park mid second line. */
    await park(page, top(0), 0);
    const line1 = (await sel(page)).caret;
    await page.keyboard.press('ArrowDown');
    await page.keyboard.press('ArrowRight');
    await page.keyboard.press('ArrowRight');
    const mid = await sel(page);
    expect(mid.caret.y).toBeGreaterThan(line1.y);
    await page.keyboard.press('Home');
    const home = await sel(page);
    expect(home.caret.y, 'Home stays on the same line').toBeCloseTo(mid.caret.y, 0);
    expect(Math.abs(home.caret.x - line1.x)).toBeLessThan(2);
    await page.keyboard.press('End');
    const end = await sel(page);
    expect(end.caret.y, 'End is drawn at the end of the SAME line').toBeCloseTo(mid.caret.y, 0);
    expect(end.caret.x).toBeGreaterThan(mid.caret.x);
    /* End set the column: Down keeps the line-end x. */
    await page.keyboard.press('ArrowDown');
    const down = await sel(page);
    expect(down.caret.y).toBeGreaterThan(end.caret.y);
    expect(Math.abs(down.caret.x - end.caret.x)).toBeLessThan(40);

    /* RTL paragraph: Home = logical start = the visual RIGHT edge. */
    await park(page, top(1), 10);
    await page.keyboard.press('Home');
    const rHome = await sel(page);
    expect(rHome.range.end.offset).toBe(0);
    await page.keyboard.press('End');
    const rEnd = await sel(page);
    expect(rEnd.caret.y).toBeCloseTo(rHome.caret.y, 0);
    expect(rHome.caret.x, 'RTL Home is right of RTL End').toBeGreaterThan(rEnd.caret.x);

    /* Ctrl+End / Ctrl+Home in the body: the document edges. */
    await page.keyboard.press('Control+End');
    expect(await caretPos(page)).toMatchObject({ block: 2, offset: 'last line'.length });
    await page.keyboard.press('Control+Home');
    expect(await caretPos(page)).toMatchObject({ block: 0, offset: 0 });

    /* In a header: the part's own edges, never the body's. */
    await page.getByRole('button', { name: 'Edit header' }).click();
    await expect(page.locator('.story-band')).toHaveCount(1);
    await page.keyboard.type('top');
    await page.keyboard.press('Enter');
    await page.keyboard.type('bottom');
    await page.keyboard.press('Control+Home');
    expect(await caretPos(page)).toMatchObject({ block: 0, offset: 0 });
    expect((await sel(page)).editing_story).toMatchObject({ area: 'Header' });
    await page.keyboard.press('Control+End');
    expect(await caretPos(page)).toMatchObject({ block: 1, offset: 'bottom'.length });
    expect((await sel(page)).editing_story).toMatchObject({ area: 'Header' });
    await page.keyboard.type('!');
    await expect(page.locator('.a11y-mirror [role="banner"]').first()).toContainText('bottom!');
});

/* --------------------------------------------------------------- row 16 */

/** `[Block(1), Cell(row, col), Block(0)]` — a cell of the table at block 1. */
const cell = (row: number, col: number): Step[] => [
    { kind: 'BLOCK', idx: 1 },
    { kind: 'CELL', row, col },
    { kind: 'BLOCK', idx: 0 },
];

/** Where the caret is, as a short label: `p<block>` for a body paragraph,
 *  `c<row><col>` for a cell of the table at block 1. */
async function where(page: Page): Promise<string> {
    const { path } = await caretPos(page);
    const c = path.find((s) => s.kind === 'CELL');
    if (c && c.kind === 'CELL') return `c${c.row}${c.col}`;
    const b = path[0];
    return b && b.kind === 'BLOCK' ? `p${b.idx}` : '?';
}

test('row 16: Up / Down in the body probe at the sticky x — across a page boundary, into and out of table cells, through a short line', async ({
    page,
}) => {
    /* (a) A page boundary: every press moves exactly one line, the page
       changes once, the column holds. */
    await openText(page, lines(60));
    await park(page, top(0), 9);
    const x0 = (await sel(page)).caret.x;
    let crossed = -1;
    let prevPage = 0;
    for (let i = 1; i < 45; i++) {
        await page.keyboard.press('ArrowDown');
        const s = await sel(page);
        expect((await caretPos(page)).block, `Down #${i}`).toBe(i);
        expect(Math.abs(s.caret.x - x0), `Down #${i} keeps the column`).toBeLessThan(10);
        const p = (await pageOfCaret(page)).page;
        if (p !== prevPage) {
            expect(p).toBe(prevPage + 1);
            crossed = i;
            prevPage = p;
            break;
        }
    }
    expect(crossed, 'the walk reached page 1').toBeGreaterThan(0);
    await page.keyboard.press('ArrowUp');
    expect((await caretPos(page)).block).toBe(crossed - 1);
    expect((await pageOfCaret(page)).page).toBe(0);
    await page.keyboard.press('ArrowDown');
    expect((await caretPos(page)).block).toBe(crossed);

    /* (b) A 2 x 2 table whose second column's first row is three lines
       tall: Down from the first column goes to the cell BELOW, not to the
       taller neighbour's second line; the second column walks its lines. */
    await openText(page, 'above\nbelow the table, far to the right');
    await page.evaluate(async () => {
        const d = (c: unknown): Promise<any> => (window as any).__dispatch(c);
        const t = await d({
            type: 'INSERT_TABLE',
            at: { steps: [{ kind: 'BLOCK', idx: 1 }] },
            rows: 2,
            cols: 2,
        });
        if (t.type === 'ERROR') throw new Error(`INSERT_TABLE: ${t.message}`);
        const put = async (row: number, col: number, text: string) => {
            const at = {
                path: {
                    steps: [
                        { kind: 'BLOCK', idx: 1 },
                        { kind: 'CELL', row, col },
                        { kind: 'BLOCK', idx: 0 },
                    ],
                },
                offset: 0,
            };
            await d({ type: 'SET_SELECTION', range: { start: at, end: at }, caret: at });
            await d({ type: 'INSERT_TEXT', at: undefined, text });
        };
        await put(0, 0, 'a');
        await put(0, 1, 'b1 b2 b3');
        await put(1, 0, 'c');
        await put(1, 1, 'd');
    });
    await expect(page.locator('.a11y-mirror td[role="gridcell"]')).toHaveCount(4);
    /* Column 0: "above" offset 1 is over the first column. */
    await park(page, top(0), 1);
    const walk0: string[] = [];
    for (let i = 0; i < 3; i++) {
        await page.keyboard.press('ArrowDown');
        walk0.push(await where(page));
    }
    expect(walk0).toEqual(['c00', 'c10', 'p2']);
    const up0: string[] = [];
    for (let i = 0; i < 3; i++) {
        await page.keyboard.press('ArrowUp');
        up0.push(await where(page));
    }
    expect(up0).toEqual(['c10', 'c00', 'p0']);

    /* Column 1, from inside the tall cell: line by line, then the row
       below, then out of the table. */
    await park(page, cell(0, 1), 0);
    const col1 = (await sel(page)).caret.x;
    const walk1: Array<{ at: string; offset: number }> = [];
    for (let i = 0; i < 4; i++) {
        await page.keyboard.press('ArrowDown');
        walk1.push({ at: await where(page), offset: (await caretPos(page)).offset });
    }
    expect(walk1.map((w) => w.at)).toEqual(['c01', 'c01', 'c11', 'p2']);
    expect(walk1[0]!.offset).toBeLessThan(walk1[1]!.offset);

    /* Column 1, from below: a caret on "below ..." right of the column
       boundary walks up into the second column, through all three lines
       of the tall cell, then out above the table. */
    await park(page, top(2), 0);
    const belowY = (await sel(page)).caret.y;
    let off = 1;
    for (; off < 'below the table, far to the right'.length; off++) {
        await park(page, top(2), off);
        if ((await sel(page)).caret.x > col1 + 10) break;
    }
    expect((await sel(page)).caret.y, 'precondition: still the first line').toBe(belowY);
    const up1: Array<{ at: string; offset: number }> = [];
    for (let i = 0; i < 5; i++) {
        await page.keyboard.press('ArrowUp');
        up1.push({ at: await where(page), offset: (await caretPos(page)).offset });
    }
    expect(up1.map((w) => w.at)).toEqual(['c11', 'c01', 'c01', 'c01', 'p0']);
    expect(up1[1]!.offset).toBeGreaterThan(up1[2]!.offset);
    expect(up1[2]!.offset).toBeGreaterThan(up1[3]!.offset);

    /* (c) A short line between two long ones: the caret lands at the short
       line's end and the column survives for the next press. */
    await openText(page, 'a longer first line\nshort\na longer third line');
    await park(page, top(0), 16);
    const start = (await sel(page)).caret.x;
    await page.keyboard.press('ArrowDown');
    expect(await caretPos(page)).toMatchObject({ block: 1, offset: 'short'.length });
    await page.keyboard.press('ArrowDown');
    const third = await sel(page);
    expect((await caretPos(page)).block).toBe(2);
    expect(Math.abs(third.caret.x - start), 'the sticky x came back').toBeLessThan(6);
    await page.keyboard.press('ArrowUp');
    await page.keyboard.press('ArrowUp');
    expect(await caretPos(page)).toMatchObject({ block: 0, offset: 16 });
});

/* --------------------------------------------------------------- row 21 */

test('row 21: PageDown / PageUp move by the viewport height and keep the sticky x', async ({
    page,
}) => {
    await openText(page, lines(150));
    await park(page, top(0), 9);
    const s0 = await sel(page);
    const dpr = await page.evaluate(() => window.devicePixelRatio || 1);
    const viewH =
        (await page.evaluate(() => document.querySelector('.editor-viewport')!.clientHeight)) * dpr;
    await page.keyboard.press('PageDown');
    const s1 = await sel(page);
    const moved = s1.caret.y - s0.caret.y;
    expect(moved, 'about one viewport height').toBeGreaterThan(viewH - 3 * s0.caret.h);
    expect(moved).toBeLessThan(viewH + 3 * s0.caret.h);
    expect(Math.abs(s1.caret.x - s0.caret.x), 'the column holds').toBeLessThan(10);
    await page.keyboard.press('PageDown');
    const s2 = await sel(page);
    expect(s2.caret.y - s1.caret.y).toBeGreaterThan(viewH - 3 * s0.caret.h);
    await page.keyboard.press('PageUp');
    await page.keyboard.press('PageUp');
    const back = await caretPos(page);
    expect(back.block).toBe(0);
    expect(Math.abs(back.offset - 9)).toBeLessThanOrEqual(1);
});

/* --------------------------------------------------------------- row 33 */

/** The body paragraphs' text, from the accessibility mirror. */
async function paragraphs(page: Page): Promise<string[]> {
    await settle(page);
    return page.locator('.a11y-mirror > p').allTextContents();
}

test('row 33: Enter puts the caret at the start of the second half; Enter in an empty list item removes the list instead of splitting', async ({
    page,
}) => {
    await openText(page, 'HelloWorld');
    await park(page, top(0), 5);
    await page.keyboard.press('Enter');
    expect(await caretPos(page)).toMatchObject({ block: 1, offset: 0 });
    await page.keyboard.type('X');
    await expect.poll(() => paragraphs(page)).toEqual(['Hello', 'XWorld']);

    /* At a paragraph's start the caret stays with the original text. */
    await park(page, top(0), 0);
    await page.keyboard.press('Enter');
    expect(await caretPos(page)).toMatchObject({ block: 1, offset: 0 });
    await page.keyboard.type('Y');
    await expect.poll(() => paragraphs(page)).toEqual(['', 'YHello', 'XWorld']);

    for (const kind of ['Bulleted list', 'Numbered list']) {
        await openText(page, 'item');
        await park(page, top(0), 4);
        await page.getByRole('button', { name: kind }).click();
        await page.locator(INPUT).focus();
        expect((await sel(page)).list_ilvl, `${kind}: the item is a list item`).toBe(0);
        await page.keyboard.press('Enter');
        let s = await sel(page);
        expect(await caretPos(page)).toMatchObject({ block: 1, offset: 0 });
        expect(s.list_ilvl, `${kind}: Enter continues the list`).toBe(0);
        /* Enter in the EMPTY item: the list ends there, no new paragraph. */
        await page.keyboard.press('Enter');
        s = await sel(page);
        expect(await caretPos(page)).toMatchObject({ block: 1, offset: 0 });
        expect(s.list_ilvl, `${kind}: the empty item left the list`).toBeUndefined();
        await expect.poll(() => paragraphs(page)).toHaveLength(2);
    }
});

/* --------------------------------------------------------------- row 35 */

/** Make the page's clipboard read `plain` (the OS clipboard is stubbed —
 *  headless Chrome has none); the paste itself is the textarea's native
 *  `paste` event -> `HiddenInput.onPaste` -> `input/clipboard.ts`. */
async function pasteText(page: Page, plain: string): Promise<void> {
    await page.evaluate((text) => {
        const clip = navigator.clipboard;
        Object.defineProperty(clip, 'readText', { value: async () => text, configurable: true });
        Object.defineProperty(clip, 'read', {
            value: async () => [
                {
                    types: ['text/plain'],
                    getType: async () => new Blob([text], { type: 'text/plain' }),
                },
            ],
            configurable: true,
        });
        document
            .querySelector('textarea[data-nge-hidden-input]')!
            .dispatchEvent(new ClipboardEvent('paste', { bubbles: true, cancelable: true }));
    }, plain);
}

test('row 35: paste leaves the caret after the pasted content; Undo / Redo restore the caret of their snapshot', async ({
    page,
}) => {
    await openText(page, 'Hello world');
    await park(page, top(0), 5);
    await pasteText(page, 'PASTED');
    await expect.poll(() => paragraphs(page)).toEqual(['HelloPASTED world']);
    expect(await caretPos(page)).toMatchObject({ block: 0, offset: 11 });

    /* A multi-paragraph paste: the caret ends after the last pasted line. */
    await pasteText(page, 'one\ntwo');
    await expect.poll(() => paragraphs(page)).toEqual(['HelloPASTEDone', 'two world']);
    expect(await caretPos(page)).toMatchObject({ block: 1, offset: 3 });

    /* Undo puts the caret back where the paste started; Redo after it. */
    await page.keyboard.press('Control+End');
    await page.keyboard.press('Control+z');
    await expect.poll(() => paragraphs(page)).toEqual(['HelloPASTED world']);
    expect(await caretPos(page)).toMatchObject({ block: 0, offset: 11 });
    await page.keyboard.press('Control+Home');
    await page.keyboard.press('Control+y');
    await expect.poll(() => paragraphs(page)).toEqual(['HelloPASTEDone', 'two world']);
    expect(await caretPos(page)).toMatchObject({ block: 1, offset: 3 });

    /* Typing, then a caret move, then Undo: the caret returns to where the
       undone typing was (not wherever it was moved to). */
    await openText(page, 'Hello world');
    await park(page, top(0), 0);
    await page.keyboard.type('abc');
    await page.keyboard.press('End');
    await page.keyboard.press('Control+z');
    const [text] = await paragraphs(page);
    const typed = text!.length - 'Hello world'.length;
    expect(typed).toBeGreaterThanOrEqual(0);
    expect(typed).toBeLessThan(3);
    expect(await caretPos(page)).toMatchObject({ block: 0, offset: typed });
    await page.keyboard.press('End');
    await page.keyboard.press('Control+y');
    await expect.poll(() => paragraphs(page)).toEqual(['abcHello world']);
    expect(await caretPos(page)).toMatchObject({ block: 0, offset: 3 });
});

/* --------------------------------------------------------------- row 36 */

/** The runs of body paragraph `idx` from the mirror: text + bold flag. */
async function runs(page: Page, idx: number): Promise<Array<{ text: string; bold: boolean }>> {
    await settle(page);
    return page.evaluate(
        (i) =>
            [
                ...document.querySelectorAll<HTMLElement>(
                    `.a11y-mirror > p:nth-of-type(${i + 1}) > span`,
                ),
            ]
                .map((s) => ({ text: s.textContent ?? '', bold: s.style.fontWeight === 'bold' }))
                .filter((r) => r.text !== ''),
        idx,
    );
}

test('row 36: formatting at a collapsed caret is pending for the next typed text and is discarded by a caret move', async ({
    page,
}) => {
    await openText(page, 'Hello world');
    await park(page, top(0), 5);
    await page.keyboard.press('Control+b');
    expect((await sel(page)).attrs_at_caret.bold, 'the caret shows the pending bold').toBe(true);
    await page.keyboard.type('X');
    await expect
        .poll(() => runs(page, 0))
        .toEqual([
            { text: 'Hello', bold: false },
            { text: 'X', bold: true },
            { text: ' world', bold: false },
        ]);

    /* A pending format dropped by an arrow press. */
    await park(page, top(0), 0);
    await page.keyboard.press('Control+b');
    expect((await sel(page)).attrs_at_caret.bold).toBe(true);
    await page.keyboard.press('ArrowRight');
    expect((await sel(page)).attrs_at_caret.bold, 'the move discarded it').toBe(false);
    await page.keyboard.type('Y');
    await expect
        .poll(() => runs(page, 0))
        .toEqual([
            { text: 'HYello', bold: false },
            { text: 'X', bold: true },
            { text: ' world', bold: false },
        ]);

    /* ... and by a click. */
    await page.keyboard.press('Control+b');
    expect((await sel(page)).attrs_at_caret.bold).toBe(true);
    const s = await sel(page);
    const canvas = (await page.locator('.editor-page[data-page-index="0"] canvas').boundingBox())!;
    await page.mouse.click(canvas.x + s.caret.x + 60, canvas.y + s.caret.y + s.caret.h / 2);
    expect((await sel(page)).attrs_at_caret.bold, 'the click discarded it').toBe(false);
    await page.keyboard.type('Z');
    const after = await runs(page, 0);
    expect(after.filter((r) => r.bold).map((r) => r.text)).toEqual(['X']);
});
