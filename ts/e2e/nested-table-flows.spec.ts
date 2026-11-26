import { test, expect, type Page } from '@playwright/test';
import { boot, settle } from './helpers/editor';
import { documentXml, readZipEntry, storedDocx } from './helpers/docx';

/* Issues #563, #562 and #566 — NESTED tables through the REAL UI.
 *
 * #563: Insert > Table with the caret in a table cell nests the new table in
 *   that cell (right after the caret's paragraph, a paragraph after it inside
 *   the cell). Before, the facade sent the caret's FIRST block step, so the
 *   table landed at top level after the outer table — a user could not
 *   author a nested table.
 * #562: Tab in the LAST cell of a nested table appends a row to the NESTED
 *   table and lands in its first new cell (the innermost table handles Tab,
 *   `onlyoffice-cursor-memo.md` §7). Before, the OUTER table grew and the
 *   caret was thrown out of the table.
 * #566: Tab from a nested cell selects the next NESTED cell, not the whole
 *   outer cell (fixed by #551; pinned here on the issue's own document).
 *
 * Every action under test is a real toolbar click, a real mouse click on the
 * page canvas (`pointer.ts`) or a real key on the hidden textarea.
 * `__dispatch` is used only for SETUP (opening the #566 fixture, reading a
 * click target's caret rect) and READS (`SAVE_DOCX` for the table structure,
 * `GET_SELECTION_AS_CLIPBOARD` for the selected text — neither moves the
 * selection). The structure is read from the saved `word/document.xml`: the
 * accessibility mirror does not show nested tables yet (#198). Selection
 * state is the engine's own `SELECTION_CHANGED` stream. */

declare global {
    interface Window {
        __dispatch: (cmd: unknown) => Promise<any>;
    }
}

test.use({ viewport: { width: 1280, height: 1400 } });

const INPUT = 'textarea[data-nge-hidden-input]';
const INSERT_TABLE = 'Insert 3×3 table';

type Step = { kind: 'BLOCK'; idx: number } | { kind: 'CELL'; row: number; col: number };

interface Pos {
    path: { steps: Step[] };
    offset: number;
}

/** Compact `B1/C0,1/B0@2` rendering of a position, for readable failures. */
function at(p: Pos): string {
    const steps = p.path.steps
        .map((s) => (s.kind === 'BLOCK' ? `B${s.idx}` : `C${s.row},${s.col}`))
        .join('/');
    return `${steps}@${p.offset}`;
}

/** Record every `SELECTION_CHANGED` the engine answers (read-only). */
async function watchSelection(page: Page): Promise<void> {
    await page.evaluate(() => {
        const w = window as any;
        w.__lastSel = undefined;
        w.__engineClient.subscribe((e: any) => {
            if (e.type === 'SELECTION_CHANGED') w.__lastSel = e;
        });
    });
}

/** The engine's selection after every command already posted, as
 *  `[start, end]` in compact form. */
async function selection(page: Page): Promise<[string, string]> {
    await settle(page);
    const sel = await page.evaluate(() => (window as any).__lastSel);
    expect(sel, 'a SELECTION_CHANGED was observed').toBeTruthy();
    return [at(sel.range.start as Pos), at(sel.range.end as Pos)];
}

/** The collapsed caret's position (fails when the selection is a range). */
async function caretPos(page: Page): Promise<string> {
    const [start, end] = await selection(page);
    expect(start, 'the selection is collapsed').toBe(end);
    return end;
}

/** The selected text, read without moving the selection. */
async function selectedText(page: Page): Promise<string> {
    await settle(page);
    return page.evaluate(async () => {
        const clip = await window.__dispatch({ type: 'GET_SELECTION_AS_CLIPBOARD' });
        return clip.type === 'CLIPBOARD_PAYLOAD' ? (clip.plain as string) : `<${clip.type}>`;
    });
}

async function press(page: Page, key: string): Promise<void> {
    await page.keyboard.press(key);
    await settle(page);
}

/* ---- the saved document's block structure ---------------------------- */

type Block = { p: string } | { tbl: Block[][][] };

/** `word/document.xml`'s body as a block tree: a paragraph is `{ p: text }`,
 *  a table `{ tbl: rows[cells[blocks]] }`, at any depth. */
function parseBody(xml: string): Block[] {
    const body = xml.slice(xml.indexOf('<w:body'), xml.lastIndexOf('</w:body>'));
    const root: Block[] = [];
    /* Containers being filled: the body, then each open cell. */
    const blocks: Block[][] = [root];
    const tables: Block[][][][] = [];
    let para: { p: string } | null = null;
    let inText = false;
    const tag = /<(\/?)w:(tbl|tr|tc|p|t)(?=[\s/>])[^>]*?(\/?)>|([^<]+)|<[^>]*>/g;
    for (const m of body.matchAll(tag)) {
        const [, close, name, selfClose, text] = m;
        if (text !== undefined) {
            if (inText && para) para.p += text;
            continue;
        }
        if (!name) continue;
        const top = blocks[blocks.length - 1]!;
        if (close) {
            if (name === 't') inText = false;
            else if (name === 'p') para = null;
            else if (name === 'tc') blocks.pop();
            else if (name === 'tbl') tables.pop();
        } else if (name === 'p') {
            para = { p: '' };
            top.push(para);
            if (selfClose) para = null;
        } else if (name === 't') {
            inText = !selfClose;
        } else if (name === 'tbl') {
            const rows: Block[][][] = [];
            top.push({ tbl: rows });
            tables.push(rows);
        } else if (name === 'tr') {
            tables[tables.length - 1]!.push([]);
        } else if (name === 'tc') {
            const rows = tables[tables.length - 1]!;
            const cell: Block[] = [];
            rows[rows.length - 1]!.push(cell);
            blocks.push(cell);
        }
    }
    return root;
}

/** The saved document's body blocks (a real `SAVE_DOCX`, read-only). */
async function savedBody(page: Page): Promise<Block[]> {
    const saved = await page.evaluate(async () => {
        const evt = await window.__dispatch({ type: 'SAVE_DOCX' });
        return evt.type === 'DOCUMENT_SAVED'
            ? { ok: true, bytes: Array.from(evt.bytes as Uint8Array) }
            : { ok: false, evt };
    });
    expect(saved.ok, `SAVE_DOCX must succeed: ${JSON.stringify(saved.evt)}`).toBe(true);
    return parseBody(readZipEntry(new Uint8Array(saved.bytes as number[]), 'word/document.xml'));
}

function table(b: Block | undefined): Block[][][] {
    if (!b || !('tbl' in b)) throw new Error(`not a table: ${JSON.stringify(b)}`);
    return b.tbl;
}

/** A block list in short form: `"text"` for a paragraph, `RxC` for a table. */
function shape(blocks: Block[]): string[] {
    return blocks.map((b) =>
        'p' in b ? JSON.stringify(b.p) : `${b.tbl.length}x${b.tbl[0]?.length ?? 0}`,
    );
}

/** Every cell's text (paragraphs joined by `|`, nested tables as `RxC`). */
function cellTexts(t: Block[][][]): string[][] {
    return t.map((row) => row.map((cell) => shape(cell).join('|')));
}

/** The accessibility mirror's top-level table rows (it lists only top-level
 *  tables, #198 — exactly what tells the OUTER table's row count). */
async function outerMirrorRows(page: Page): Promise<number> {
    await settle(page);
    return page.locator('.a11y-mirror > table > tbody > tr').count();
}

/** A real mouse click inside the paragraph at `steps`, `offset` bytes in.
 *  The point is the engine's own caret rect for that position (read with a
 *  SET_SELECTION, then the caret is parked at the document start so only
 *  the click can move it there). */
async function clickInto(page: Page, steps: Step[], offset: number): Promise<void> {
    const caret = await page.evaluate(
        async ({ steps, offset }) => {
            const pos = { path: { steps }, offset };
            const sel = await window.__dispatch({
                type: 'SET_SELECTION',
                range: { start: pos, end: pos },
                caret: pos,
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
    expect(await caretPos(page)).toBe('B0@0');
    const canvas = page.locator('.editor-page[data-page-index="0"] canvas').first();
    await canvas.scrollIntoViewIfNeeded();
    const box = await canvas.boundingBox();
    if (!box) throw new Error('page canvas has no box');
    const dpr = await page.evaluate(() => window.devicePixelRatio || 1);
    await page.mouse.click(box.x + caret.x / dpr, box.y + (caret.y + caret.h / 2) / dpr);
}

const B = (idx: number): Step => ({ kind: 'BLOCK', idx });
const C = (row: number, col: number): Step => ({ kind: 'CELL', row, col });
const LABEL = (r: number, c: number): string => `${'abc'[c]}${r + 1}`;
const NESTED = (r: number, c: number, offset = 0): string => `B1/C1,1/B1/C${r},${c}/B0@${offset}`;

test('Insert Table in a cell nests a table there; Tab past its last cell grows the NESTED table (#563, #562)', async ({
    page,
}) => {
    test.setTimeout(120_000);
    await boot(page);
    await watchSelection(page);
    const input = page.locator(INPUT);

    /* Intro, then the toolbar's 3 x 3 table filled a1..c3 with Tab, Outro. */
    await input.focus();
    await page.keyboard.press('Control+A');
    await page.keyboard.type('Intro');
    await page.getByRole('button', { name: INSERT_TABLE }).click();
    await expect(page.locator('.a11y-mirror td[role="gridcell"]')).toHaveCount(9);
    await input.focus();
    for (let r = 0; r < 3; r++) {
        for (let c = 0; c < 3; c++) {
            await page.keyboard.type(LABEL(r, c));
            if (r < 2 || c < 2) await page.keyboard.press('Tab');
        }
    }
    await page.keyboard.press('Control+End');
    await page.keyboard.type('Outro');
    let body = await savedBody(page);
    expect(shape(body)).toEqual(['"Intro"', '3x3', '"Outro"']);

    /* Click into cell (1,1) after `b2`, then Insert > Table. */
    await clickInto(page, [B(1), C(1, 1), B(0)], 2);
    expect(await caretPos(page), 'the click put the caret in cell (1,1)').toBe('B1/C1,1/B0@2');
    await page.getByRole('button', { name: INSERT_TABLE }).click();
    expect(await caretPos(page), 'the caret is in the NESTED table').toBe(NESTED(0, 0));

    body = await savedBody(page);
    expect(shape(body), 'no table was added at top level').toEqual(['"Intro"', '3x3', '"Outro"']);
    const outer = table(body[1]);
    expect(shape(outer[1]![1]!), 'cell (1,1): b2, the nested table, a paragraph').toEqual([
        '"b2"',
        '3x3',
        '""',
    ]);
    expect(cellTexts(outer)).toEqual([
        ['"a1"', '"b1"', '"c1"'],
        ['"a2"', '"b2"|3x3|""', '"c2"'],
        ['"a3"', '"b3"', '"c3"'],
    ]);
    expect(await outerMirrorRows(page)).toBe(3);

    /* Type into every nested cell, Tab between them, to its LAST cell. */
    await input.focus();
    for (let r = 0; r < 3; r++) {
        for (let c = 0; c < 3; c++) {
            expect(await caretPos(page)).toBe(NESTED(r, c));
            await page.keyboard.type(`n${r * 3 + c + 1}`);
            if (r < 2 || c < 2) await press(page, 'Tab');
        }
    }
    expect(await caretPos(page)).toBe(NESTED(2, 2, 2));

    /* Tab in the nested table's last cell: a row for the NESTED table, the
       caret in its first new cell; the outer table keeps 3 rows. */
    await press(page, 'Tab');
    expect(await caretPos(page)).toBe(NESTED(3, 0));
    expect(await outerMirrorRows(page), 'the outer table did not grow').toBe(3);
    await page.keyboard.type('x');

    body = await savedBody(page);
    expect(shape(body)).toEqual(['"Intro"', '3x3', '"Outro"']);
    const outerAfter = table(body[1]);
    expect(outerAfter).toHaveLength(3);
    const cell = outerAfter[1]![1]!;
    expect(shape(cell)).toEqual(['"b2"', '4x3', '""']);
    expect(cellTexts(table(cell[1]))).toEqual([
        ['"n1"', '"n2"', '"n3"'],
        ['"n4"', '"n5"', '"n6"'],
        ['"n7"', '"n8"', '"n9"'],
        ['"x"', '""', '""'],
    ]);
});

/* The #566 document: intro / a 1 x 2 table whose cell (0,0) holds `o1`, a
 * nested 1 x 2 table (`i1`, `i2`) and an empty paragraph, (0,1) `o2` /
 * outro. */
const p = (t: string): string => `<w:p><w:r><w:t xml:space="preserve">${t}</w:t></w:r></w:p>`;
const tbl = (rows: string[][]): string =>
    '<w:tbl><w:tblGrid>' +
    '<w:gridCol w:w="2400"/>'.repeat(rows[0]!.length) +
    '</w:tblGrid>' +
    rows.map((cells) => `<w:tr>${cells.map((c) => `<w:tc>${c}</w:tc>`).join('')}</w:tr>`).join('') +
    '</w:tbl>';
const ISSUE_566 =
    p('intro') +
    tbl([[p('o1') + tbl([[p('i1'), p('i2')]]) + '<w:p/>', p('o2')]]) +
    p('outro') +
    '<w:sectPr/>';

test('Tab between nested cells selects the NESTED cell; Tab in its last cell grows the nested table (#566, #562)', async ({
    page,
}) => {
    await boot(page);
    const bytes = Array.from(storedDocx(documentXml(ISSUE_566)));
    await page.evaluate(async (arr: number[]) => {
        const loaded = await window.__dispatch({
            type: 'OPEN_DOCUMENT',
            bytes: new Uint8Array(arr),
            format: 'docx',
            name: 'nested-566.docx',
        });
        if (loaded.type === 'ERROR') throw new Error(loaded.message);
    }, bytes);
    await settle(page);
    await watchSelection(page);
    expect(cellTexts(table((await savedBody(page))[1]))).toEqual([['"o1"|1x2|""', '"o2"']]);

    /* The issue's key path to `i1`. */
    await page.locator(INPUT).focus();
    for (const key of ['Control+Home', 'End', 'ArrowRight', 'End', 'ArrowRight']) {
        await press(page, key);
    }
    expect(await caretPos(page)).toBe('B1/C0,0/B1/C0,0/B0@0');

    await press(page, 'Tab');
    expect(await selection(page)).toEqual(['B1/C0,0/B1/C0,1/B0@0', 'B1/C0,0/B1/C0,1/B0@2']);
    expect(await selectedText(page)).toBe('i2');

    await press(page, 'Shift+Tab');
    expect(await selectedText(page)).toBe('i1');
    await press(page, 'Tab');
    expect(await selectedText(page)).toBe('i2');

    /* `i2` is the nested table's last cell: Tab appends a row to it. */
    expect(await outerMirrorRows(page)).toBe(1);
    await press(page, 'Tab');
    expect(await caretPos(page)).toBe('B1/C0,0/B1/C1,0/B0@0');
    expect(await outerMirrorRows(page), 'the outer table still has one row').toBe(1);
    await page.keyboard.type('z');

    const body = await savedBody(page);
    expect(shape(body)).toEqual(['"intro"', '1x2', '"outro"']);
    const outer = table(body[1]);
    expect(outer).toHaveLength(1);
    expect(shape(outer[0]![0]!)).toEqual(['"o1"', '2x2', '""']);
    expect(cellTexts(table(outer[0]![0]![1]))).toEqual([
        ['"i1"', '"i2"'],
        ['"z"', '""'],
    ]);
});
