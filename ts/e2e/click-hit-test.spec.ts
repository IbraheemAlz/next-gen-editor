import { test, expect, type Page } from '@playwright/test';
import { boot, settle } from './helpers/editor';
import { documentXml, storedDocx } from './helpers/docx';

/* Issues #568 / #572 (epic #555 row 3, clean-room cursor memo §2) — where a
 * CLICK puts the caret, through the REAL shell: Playwright mouse events on
 * the page canvas (`pointer.ts` -> `PLACE_CARET_AT_POINT` /
 * `SELECT_WORD_AT`) and typing through the hidden textarea. `__dispatch`
 * only opens the fixture and measures caret rectangles before the action
 * under test (setup); the evidence is engine truth (every
 * `SELECTION_CHANGED` the client receives, the document text read back
 * through the worker queue) — headless Chrome never composites the
 * interactive canvas (CLAUDE.md), so nothing here reads a screenshot.
 *
 *  - #568: a click in the EMPTY lower part of a short table cell, beside a
 *    taller neighbour cell of the same row, landed on the neighbour's line
 *    at that height. The cell under the point is resolved first (its border
 *    box: the merged extent for a vertical merge, mirrored for a
 *    `<w:bidiVisual>` table), then its nearest line: below its last line =
 *    its last line.
 *  - #572: a click in paragraph spacing went to the nearest line box. The
 *    space-after belongs to the paragraph above, the space-before to the
 *    paragraph below (memo §2: "the block whose next sibling starts below
 *    y"). */

test.use({ viewport: { width: 1280, height: 900 } });

type Step = { kind: 'BLOCK'; idx: number } | { kind: 'CELL'; row: number; col: number };
interface Path {
    steps: Step[];
}
interface Pos {
    path: Path;
    offset: number;
}
interface Rect {
    x: number;
    y: number;
    w: number;
    h: number;
}

const top = (block: number): Path => ({ steps: [{ kind: 'BLOCK', idx: block }] });
/** Paragraph `block` of cell `(row, col)` of the table at body block `table`. */
const cellIn = (table: number, row: number, col: number, block = 0): Path => ({
    steps: [
        { kind: 'BLOCK', idx: table },
        { kind: 'CELL', row, col },
        { kind: 'BLOCK', idx: block },
    ],
});
/** ... of the table at body block 1. */
const cell = (row: number, col: number, block = 0): Path => cellIn(1, row, col, block);

const dispatch = (page: Page, cmd: unknown): Promise<any> =>
    page.evaluate((c) => (window as any).__dispatch(c), cmd);

/* ------------------------------------------------------------------ */
/* `.docx` fixtures built in the test (no binary in the tree). */

/** US Letter, 1 in margins: the page is 612 pt wide. */
const PAGE_W_PT = 612;
const SECT =
    '<w:sectPr><w:pgSz w:w="12240" w:h="15840"/>' +
    '<w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440" w:header="720" w:footer="720" w:gutter="0"/>' +
    '</w:sectPr>';

const run = (text: string): string => `<w:r><w:t xml:space="preserve">${text}</w:t></w:r>`;
const p = (text: string, ppr = ''): string => `<w:p>${ppr ? `<w:pPr>${ppr}</w:pPr>` : ''}${run(text)}</w:p>`;
const spacing = (beforePt: number, afterPt: number): string =>
    `<w:spacing w:before="${beforePt * 20}" w:after="${afterPt * 20}"/>`;
const tc = (body: string, tcpr = ''): string =>
    `<w:tc><w:tcPr><w:tcW w:w="2400" w:type="dxa"/>${tcpr}</w:tcPr>${body}</w:tc>`;
const tr = (cells: string): string => `<w:tr>${cells}</w:tr>`;
/** A fixed 2-column table (2 x 120 pt), optionally `<w:bidiVisual>`. */
const table = (rows: string, bidi = false): string =>
    '<w:tbl><w:tblPr><w:tblW w:w="4800" w:type="dxa"/><w:tblLayout w:type="fixed"/>' +
    `${bidi ? '<w:bidiVisual/>' : ''}</w:tblPr>` +
    '<w:tblGrid><w:gridCol w:w="2400"/><w:gridCol w:w="2400"/></w:tblGrid>' +
    `${rows}</w:tbl>`;

async function openDocx(page: Page, body: string): Promise<void> {
    const bytes = storedDocx(documentXml(body + SECT));
    const type = await page.evaluate(async (arr: number[]) => {
        const evt = await (window as any).__dispatch({
            type: 'OPEN_DOCUMENT',
            bytes: new Uint8Array(arr),
            format: 'docx',
            name: 'click-hit-test.docx',
        });
        return evt.type as string;
    }, Array.from(bytes));
    expect(type).toBe('DOCUMENT_LOADED');
    await settle(page);
}

/* ------------------------------------------------------------------ */
/* Measuring (setup) and reading the engine's answer. */

/** Record every `SELECTION_CHANGED` the client sees (replies to the shell's
 *  own commands included). */
async function watchSelection(page: Page): Promise<void> {
    await page.evaluate(() => {
        const log: any[] = [];
        (window as any).__selLog = log;
        (window as any).__engineClient.subscribe((e: any) => {
            if (e.type === 'SELECTION_CHANGED') log.push(e);
        });
    });
}

/** The engine's latest selection range, after every posted command. */
async function selection(page: Page): Promise<{ start: Pos; end: Pos }> {
    await settle(page);
    return page.evaluate(() => {
        const log = (window as any).__selLog as any[];
        const e = log[log.length - 1];
        return { start: e.range.start, end: e.range.end };
    });
}

/** The collapsed caret (asserts the selection IS collapsed). */
async function caret(page: Page): Promise<Pos> {
    const s = await selection(page);
    expect(s.start, 'a collapsed caret').toEqual(s.end);
    return s.start;
}

/** Setup: the caret rectangle (page-0 local device px) of `(path, offset)`. */
async function caretRect(page: Page, path: Path, offset: number): Promise<Rect> {
    const at = { path, offset };
    const evt = await dispatch(page, { type: 'SET_SELECTION', range: { start: at, end: at }, caret: at });
    expect(evt.type).toBe('SELECTION_CHANGED');
    return evt.caret as Rect;
}

const mid = (r: Rect): number => r.y + r.h / 2;

async function canvasBox(page: Page): Promise<{ x: number; y: number; width: number; dpr: number }> {
    const box = await page.locator('.editor-page[data-page-index="0"] canvas').first().boundingBox();
    if (!box) throw new Error('page 0 canvas has no box');
    const dpr = await page.evaluate(() => window.devicePixelRatio || 1);
    return { x: box.x, y: box.y, width: box.width, dpr };
}

/** Device px per point at the current zoom (the page is `PAGE_W_PT` wide). */
async function pxPerPt(page: Page): Promise<number> {
    const b = await canvasBox(page);
    return (b.width * b.dpr) / PAGE_W_PT;
}

/** A real mouse click at a page-0 device-px point. */
async function clickAt(page: Page, x: number, y: number): Promise<void> {
    const b = await canvasBox(page);
    await page.mouse.click(b.x + x / b.dpr, b.y + y / b.dpr);
}

async function dblclickAt(page: Page, x: number, y: number): Promise<void> {
    const b = await canvasBox(page);
    await page.mouse.dblclick(b.x + x / b.dpr, b.y + y / b.dpr);
}

/** The text of the paragraph at `path`, read back through the worker (the
 *  engine clamps the range's end to the paragraph's length). */
async function paragraphText(page: Page, path: Path): Promise<string> {
    return page.evaluate(async (path) => {
        const d = (window as any).__dispatch;
        const start = { path, offset: 0 };
        const end = { path, offset: 1 << 20 };
        await d({ type: 'SET_SELECTION', range: { start, end }, caret: end });
        const clip = await d({ type: 'GET_SELECTION_AS_CLIPBOARD' });
        return clip.type === 'CLIPBOARD_PAYLOAD' ? (clip.plain as string) : `<${clip.type}>`;
    }, path);
}

test.beforeEach(async ({ page }) => {
    await boot(page);
    await watchSelection(page);
    await page.locator('.editor-page[data-page-index="0"] canvas').first().scrollIntoViewIfNeeded();
});

/* ------------------------------------------------------------------ */
/* #568 — the cell first. */

test('#568: a click in the empty part of a short cell stays in that cell, not its taller neighbour', async ({
    page,
}) => {
    await openDocx(
        page,
        p('intro') +
            table(
                tr(tc(p('a')) + tc(p('b1') + p('b2') + p('b3') + p('b4'))) +
                    tr(tc(p('c')) + tc(p('d'))),
            ) +
            p('outro'),
    );
    const a = await caretRect(page, cell(0, 0), 1);
    const b2 = await caretRect(page, cell(0, 1, 1), 0);
    const b4 = await caretRect(page, cell(0, 1, 3), 2);
    const c = await caretRect(page, cell(1, 0), 0);
    expect(b2.y, '`b2` sits below `a`').toBeGreaterThan(a.y + a.h);
    /* x: right of `a`'s ink, well inside cell (0,0) (120 pt wide). */
    const unit = await pxPerPt(page);
    const x = a.x + 40 * unit;
    expect(x, 'still left of cell (0,1)').toBeLessThan(b2.x - 10 * unit);

    for (const [what, y] of [
        ["b2's height", mid(b2)],
        ["b4's height", mid(b4)],
        /* The bottom of row 0, a hair above row 1's `c`. */
        ['the bottom of row 0', c.y - 4 * unit],
    ] as const) {
        await clickAt(page, x, y);
        expect(await caret(page), `click at ${what}`).toEqual({ path: cell(0, 0), offset: 1 });
    }
    /* What the user sees: typing there continues `a`. */
    await clickAt(page, x, mid(b4));
    await page.keyboard.type('Z');
    expect(await paragraphText(page, cell(0, 0))).toBe('aZ');
    expect(await paragraphText(page, cell(0, 1, 3))).toBe('b4');

    /* A double-click there selects `a` (the cell's word), not `b2`. */
    await dblclickAt(page, x, mid(b2));
    const s = await selection(page);
    expect([s.start, s.end]).toEqual([
        { path: cell(0, 0), offset: 0 },
        { path: cell(0, 0), offset: 2 },
    ]);

    /* The neighbour keeps its own clicks. */
    await clickAt(page, b2.x + 2 * unit, mid(b2));
    expect((await caret(page)).path).toEqual(cell(0, 1, 1));
});

test('#568: a vertical merge resolves to its merge start; an RTL table keeps the mirrored short cell', async ({
    page,
}) => {
    const restart = '<w:vMerge w:val="restart"/>';
    const cont = '<w:vMerge/>';
    await openDocx(
        page,
        p('intro') +
            table(
                tr(tc(p('m'), restart) + tc(p('r0'))) +
                    tr(tc(p(''), cont) + tc(p('r1'))) +
                    tr(tc(p(''), cont) + tc(p('r2'))),
            ) +
            p('between') +
            table(tr(tc(p('a')) + tc(p('b1') + p('b2') + p('b3') + p('b4'))), true) +
            p('outro'),
    );
    const unit = await pxPerPt(page);

    /* Rows 1 and 2 of the merged column: the restart cell (0,0). */
    const m = await caretRect(page, cell(0, 0), 1);
    for (const row of [1, 2]) {
        const r = await caretRect(page, cell(row, 1), 0);
        await clickAt(page, m.x + 40 * unit, mid(r));
        expect(await caret(page), `row ${row} of the merge`).toEqual({ path: cell(0, 0), offset: 1 });
    }

    /* The `<w:bidiVisual>` table (body block 3): its cell (0,0) is the
       RIGHT one. Below its `a`, at the height of `b3`. */
    const a = await caretRect(page, cellIn(3, 0, 0), 1);
    const b3 = await caretRect(page, cellIn(3, 0, 1, 2), 0);
    expect(a.x, 'cell (0,0) is right of cell (0,1)').toBeGreaterThan(b3.x);
    await clickAt(page, a.x + 30 * unit, mid(b3));
    expect(await caret(page)).toEqual({ path: cellIn(3, 0, 0), offset: 1 });
});

/* ------------------------------------------------------------------ */
/* #572 — paragraph spacing belongs to its paragraph. */

test('#572: a click in paragraph spacing lands in the paragraph that owns it', async ({ page }) => {
    const UPPER = 'upper';
    const LOWER = 'lower text';
    /* [space-after of upper, space-before of lower] in pt, and the click
       depths into the gap (pt below upper's line) with the paragraph each
       must land in. The issue's measured case is the first: all of the gap
       is upper's space-after, so even 90 % down it (nearer `lower`) stays
       in `upper`. */
    const cases: { after: number; before: number; clicks: [number, 0 | 1][] }[] = [
        { after: 24, before: 0, clicks: [[6, 0], [12, 0], [18, 0], [21.6, 0]] },
        { after: 24, before: 12, clicks: [[3, 0], [12, 0], [20, 0], [23, 0], [25, 1], [30, 1], [34, 1]] },
        { after: 0, before: 24, clicks: [[2, 1], [6, 1], [12, 1], [20, 1]] },
    ];
    for (const k of cases) {
        await openDocx(page, p(UPPER, spacing(0, k.after)) + p(LOWER, spacing(k.before, 0)) + p('third'));
        const unit = await pxPerPt(page);
        const up = await caretRect(page, top(0), UPPER.length);
        const low = await caretRect(page, top(1), 0);
        const gapTop = up.y + up.h; // the caret's bottom = the line box's (descent)
        const gap = low.y - gapTop;
        /* The lower caret's top also skips its line's (small) line gap. */
        expect(gap, `${k.after}/${k.before}: the gap`).toBeGreaterThan((k.after + k.before) * unit - 1);
        expect(gap, `${k.after}/${k.before}: the gap`).toBeLessThan((k.after + k.before) * unit + 6 * unit);
        /* Right of both lines' ink: each paragraph's logical end. */
        const x = Math.max(up.x, (await caretRect(page, top(1), LOWER.length)).x) + 60 * unit;
        for (const [depthPt, want] of k.clicks) {
            await clickAt(page, x, gapTop + depthPt * unit);
            expect(await caret(page), `${k.after}/${k.before}: ${depthPt} pt into the gap`).toEqual(
                want === 0 ? { path: top(0), offset: UPPER.length } : { path: top(1), offset: LOWER.length },
            );
        }
    }
    /* What the user sees (last case open: 0 / 24): typing after a click in
       lower's space-before continues `lower text`. */
    const up = await caretRect(page, top(0), UPPER.length);
    const unit = await pxPerPt(page);
    await clickAt(page, up.x + 100 * unit, up.y + up.h + 4 * unit);
    await page.keyboard.type('!');
    expect(await paragraphText(page, top(1))).toBe(`${LOWER}!`);
    expect(await paragraphText(page, top(0))).toBe(UPPER);
});
