import { test, expect, type Page } from '@playwright/test';
import { boot, settle } from './helpers/editor';
import { documentXml, storedDocx } from './helpers/docx';

/* Epic #555 — the TABLE rows of the cursor-behaviour matrix (rows 18, 24,
 * 25, 26, 27, 28), pinned through the REAL UI. The expected behaviour is the
 * clean-room methods memo `plans/cleanroom/memos/onlyoffice-cursor-memo.md`
 * §4 (Shift past the document edge, Shift across a cell boundary), §6
 * (Select All follows the active story) and §7 (cell order, Tab, Delete /
 * Backspace at a table's edges). Row 26 (`<w:bidiVisual>`) has no model in
 * the memo: it pins OUR intent (`cell_exit_step`: cell exits and Tab follow
 * the TABLE's reading direction) — Word unverified. Row 27's "the innermost
 * table handles Tab" case is not pinned here: Tab into a nested cell
 * selects the outer cell (#566) and Tab in a nested table's last cell grows
 * the outer table (#562).
 *
 * Every action under test is a real key press on the hidden textarea, a
 * toolbar button or a pointer event on the page canvas (`pointer.ts`).
 * `__dispatch` is used only to OPEN fixture documents (setup) and to READ
 * (`GET_SELECTION_AS_CLIPBOARD` returns the selected text without moving
 * the selection). Selection state is the engine's own `SELECTION_CHANGED`
 * stream, observed through `__engineClient.subscribe`; the cell structure
 * is the accessibility mirror (`.a11y-mirror`, one `<td data-row data-col>`
 * per cell), both valid under headless Chrome. */

test.use({ viewport: { width: 1280, height: 1000 } });

const INPUT = 'textarea[data-nge-hidden-input]';

type Step = { kind: 'BLOCK'; idx: number } | { kind: 'CELL'; row: number; col: number };

interface Pos {
    path: { steps: Step[] };
    offset: number;
}

interface Sel {
    range: { start: Pos; end: Pos };
    caret: { x: number; y: number; w: number; h: number };
    selection_kind:
        | { kind: 'LINEAR' }
        | {
              kind: 'TABLE_CELLS';
              table_path: { steps: Step[] };
              from_row: number;
              from_col: number;
              to_row: number;
              to_col: number;
          };
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

/** The engine's selection after every command already posted. */
async function selection(page: Page): Promise<Sel> {
    await settle(page);
    const sel = await page.evaluate(() => (window as any).__lastSel);
    expect(sel, 'a SELECTION_CHANGED was observed').toBeTruthy();
    return sel as Sel;
}

/** The selected text, read without moving the selection. */
async function selectedText(page: Page): Promise<string> {
    await settle(page);
    return page.evaluate(async () => {
        const clip = await (window as any).__dispatch({ type: 'GET_SELECTION_AS_CLIPBOARD' });
        return clip.type === 'CLIPBOARD_PAYLOAD' ? (clip.plain as string) : `<${clip.type}>`;
    });
}

/** Compact `B1/C0,1/B0@2` rendering of a position, for readable failures. */
function at(p: Pos): string {
    const steps = p.path.steps
        .map((s) => (s.kind === 'BLOCK' ? `B${s.idx}` : `C${s.row},${s.col}`))
        .join('/');
    return `${steps}@${p.offset}`;
}

/** The collapsed caret's position (fails when the selection is a range). */
async function caretPos(page: Page): Promise<string> {
    const sel = await selection(page);
    expect(at(sel.range.start), 'the selection is collapsed').toBe(at(sel.range.end));
    return at(sel.range.end);
}

async function press(page: Page, key: string): Promise<void> {
    await page.keyboard.press(key);
    await settle(page);
}

/** The caret overlay's top-left in the viewport's scroll content (CSS px). */
async function caretXY(page: Page): Promise<{ x: number; y: number }> {
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

/** The mirror's top-level table cells as `"r,c": text` (nested tables
 *  excluded), plus the body paragraphs around it. */
async function mirror(page: Page): Promise<{ body: string[]; cells: Record<string, string> }> {
    await settle(page);
    return page.evaluate(() => {
        const root = document.querySelector('.a11y-mirror');
        if (!root) throw new Error('no a11y mirror');
        const body = [...root.querySelectorAll(':scope > p')].map((p) => p.textContent ?? '');
        const cells: Record<string, string> = {};
        for (const td of root.querySelectorAll<HTMLElement>(
            ':scope > table > tbody > tr > td',
        )) {
            cells[`${td.dataset.row},${td.dataset.col}`] = [
                ...td.querySelectorAll(':scope > p'),
            ]
                .map((p) => p.textContent ?? '')
                .join('\n');
        }
        return { body, cells };
    });
}

const LABEL = (r: number, c: number): string => `${'abc'[c]}${r + 1}`;

/** Boot, replace the seed text with `Intro`, insert the toolbar's 3 x 3
 *  table (it lands after `Intro`, with an empty paragraph after it) and type
 *  `a1 b1 c1 / a2 b2 c2 / a3 b3 c3` into its cells with Tab between them
 *  (skipping the cells in `empty`), then `Outro` into the paragraph after
 *  the table. Body: [Intro] [table] [Outro]. */
async function tableDoc(page: Page, empty: string[] = []): Promise<void> {
    await boot(page);
    await watchSelection(page);
    const input = page.locator(INPUT);
    await input.focus();
    await page.keyboard.press('Control+A');
    await page.keyboard.type('Intro');
    await page.getByRole('button', { name: 'Insert 3×3 table' }).click();
    await expect(page.locator('.a11y-mirror td[role="gridcell"]')).toHaveCount(9);
    await input.focus();
    for (let r = 0; r < 3; r++) {
        for (let c = 0; c < 3; c++) {
            if (!empty.includes(`${r},${c}`)) await page.keyboard.type(LABEL(r, c));
            if (r < 2 || c < 2) await page.keyboard.press('Tab');
        }
    }
    await page.keyboard.press('Control+End');
    await page.keyboard.type('Outro');
    const m = await mirror(page);
    expect(m.body).toEqual(['Intro', 'Outro']);
    for (let r = 0; r < 3; r++) {
        for (let c = 0; c < 3; c++) {
            expect(m.cells[`${r},${c}`]).toBe(empty.includes(`${r},${c}`) ? '' : LABEL(r, c));
        }
    }
}

const BODY = (idx: number, offset: number): string => `B${idx}@${offset}`;
const CELL = (r: number, c: number, offset: number): string => `B1/C${r},${c}/B0@${offset}`;

/** Ctrl+Home, End, Right: the caret at the start of cell (0, 0). */
async function toFirstCell(page: Page): Promise<void> {
    await press(page, 'Control+Home');
    await press(page, 'End');
    await press(page, 'ArrowRight');
    expect(await caretPos(page)).toBe(CELL(0, 0, 0));
}

test('row 18: Shift+Up/Down past the document edge extends to its start/end; Shift across a cell boundary selects cells', async ({
    page,
}) => {
    test.setTimeout(90_000);
    await tableDoc(page);

    /* Shift+Up on the first line: the focus goes to the document start. */
    await press(page, 'Control+Home');
    for (let i = 0; i < 3; i++) await press(page, 'ArrowRight');
    expect(await caretPos(page)).toBe(BODY(0, 3));
    await press(page, 'Shift+ArrowUp');
    let sel = await selection(page);
    expect([at(sel.range.start), at(sel.range.end)]).toEqual([BODY(0, 0), BODY(0, 3)]);
    expect(await selectedText(page)).toBe('Int');

    /* Shift+Down on the last line: the focus goes to the document end. */
    await press(page, 'Control+End');
    await press(page, 'ArrowLeft');
    await press(page, 'ArrowLeft');
    expect(await caretPos(page)).toBe(BODY(2, 3));
    await press(page, 'Shift+ArrowDown');
    sel = await selection(page);
    expect([at(sel.range.start), at(sel.range.end)]).toEqual([BODY(2, 3), BODY(2, 5)]);
    expect(await selectedText(page)).toBe('ro');

    /* Shift+Right out of a cell: a cell selection (TABLE_CELLS). */
    await toFirstCell(page);
    await press(page, 'End');
    await press(page, 'Shift+ArrowRight');
    sel = await selection(page);
    expect(sel.selection_kind).toEqual({
        kind: 'TABLE_CELLS',
        table_path: { steps: [{ kind: 'BLOCK', idx: 1 }] },
        from_row: 0,
        from_col: 0,
        to_row: 0,
        to_col: 1,
    });
    /* Shift+Down from there grows the rectangle by a row. */
    await press(page, 'Shift+ArrowDown');
    sel = await selection(page);
    expect(sel.selection_kind).toMatchObject({
        kind: 'TABLE_CELLS',
        from_row: 0,
        from_col: 0,
        to_row: 1,
        to_col: 1,
    });
    /* Shift+Up / Shift+Left back into the anchor cell: a text selection
       again (inside one cell). */
    await press(page, 'Shift+ArrowUp');
    await press(page, 'Shift+ArrowLeft');
    sel = await selection(page);
    expect(sel.selection_kind).toEqual({ kind: 'LINEAR' });
    expect(at(sel.range.start).startsWith('B1/C0,0/')).toBe(true);
    expect(at(sel.range.end).startsWith('B1/C0,0/')).toBe(true);
});

test('row 24: Select All follows the active story: the whole body from a cell, the part only in a header', async ({
    page,
}) => {
    test.setTimeout(90_000);
    await tableDoc(page);

    /* From inside cell (1, 1): the whole body, a linear selection. */
    await toFirstCell(page);
    for (let i = 0; i < 4; i++) await press(page, 'Tab');
    expect(await selectedText(page)).toBe('b2');
    await press(page, 'Control+A');
    const sel = await selection(page);
    expect(sel.selection_kind).toEqual({ kind: 'LINEAR' });
    expect([at(sel.range.start), at(sel.range.end)]).toEqual([BODY(0, 0), BODY(2, 5)]);
    /* (The plain-text flavour skips the table's cells, #561; the range
       above is the engine truth.) */
    const all = await selectedText(page);
    for (const word of ['Intro', 'Outro']) expect(all).toContain(word);

    /* In a header: the part only. */
    await page.getByRole('button', { name: 'Edit header' }).click();
    await expect(page.locator('.story-band')).toHaveCount(1);
    await page.locator(INPUT).focus();
    await page.keyboard.type('Header words');
    await expect(page.locator('.a11y-mirror [role="banner"]')).toContainText('Header words');
    await press(page, 'Control+A');
    expect(await selectedText(page)).toBe('Header words');
    const hsel = await selection(page);
    expect([at(hsel.range.start), at(hsel.range.end)]).toEqual([BODY(0, 0), BODY(0, 12)]);
    await press(page, 'Escape');
    await expect(page.locator('.story-band')).toHaveCount(0);
});

test('row 25: Left/Right walk the cells in logical order, skip merge continuations, and leave the table at its first/last cell', async ({
    page,
}) => {
    test.setTimeout(120_000);
    await tableDoc(page);

    /* Plain 3 x 3: Right from the end of `Intro` visits every caret stop
       of every cell in row-major order, then the paragraph after. */
    const forward: string[] = [BODY(0, 5)];
    for (let r = 0; r < 3; r++) {
        for (let c = 0; c < 3; c++) {
            for (let o = 0; o <= 2; o++) forward.push(CELL(r, c, o));
        }
    }
    forward.push(BODY(2, 0));
    await press(page, 'Control+Home');
    await press(page, 'End');
    const walked = [await caretPos(page)];
    for (let i = 1; i < forward.length; i++) {
        await press(page, 'ArrowRight');
        walked.push(await caretPos(page));
    }
    expect(walked).toEqual(forward);
    /* ... and Left walks the same stops back to `Intro`. */
    const back = [await caretPos(page)];
    for (let i = 1; i < forward.length; i++) {
        await press(page, 'ArrowLeft');
        back.push(await caretPos(page));
    }
    expect(back).toEqual([...forward].reverse());
});

test('row 25: Left/Right skip a vertical merge continuation and cross a horizontal merge as one cell', async ({
    page,
}) => {
    test.setTimeout(120_000);
    await boot(page);
    await watchSelection(page);
    const input = page.locator(INPUT);
    await input.focus();
    await page.keyboard.press('Control+A');
    await page.keyboard.type('Intro');
    await page.getByRole('button', { name: 'Insert 3×3 table' }).click();
    await expect(page.locator('.a11y-mirror td[role="gridcell"]')).toHaveCount(9);
    await input.focus();

    /* Merge (0, 1)-(1, 1) vertically and (2, 0)-(2, 1) horizontally, through
       a keyboard cell selection and the table context menu. */
    const merge = async (): Promise<void> => {
        const sel = await selection(page);
        expect(sel.selection_kind.kind).toBe('TABLE_CELLS');
        const canvas = page.locator('.editor-page[data-page-index="0"] canvas').first();
        const box = (await canvas.boundingBox())!;
        const dpr = await page.evaluate(() => window.devicePixelRatio || 1);
        await page.mouse.click(box.x + sel.caret.x / dpr, box.y + (sel.caret.y + 4) / dpr, {
            button: 'right',
        });
        await page.getByRole('menuitem', { name: 'Merge cells' }).click();
        await settle(page);
        await input.focus();
    };
    expect(await caretPos(page)).toBe(CELL(0, 0, 0));
    await press(page, 'Tab'); // (0, 1)
    await press(page, 'Shift+ArrowDown'); // (0, 1)-(1, 1)
    await merge();
    await expect(page.locator('.a11y-mirror td[aria-rowspan="2"]')).toHaveCount(1);
    await press(page, 'Control+End'); // the empty paragraph after the table
    await press(page, 'ArrowLeft'); // (2, 2)
    await press(page, 'ArrowLeft'); // (2, 1)
    await press(page, 'Shift+ArrowLeft'); // (2, 1)-(2, 0)
    await merge();
    await expect(page.locator('.a11y-mirror td[aria-colspan="2"]')).toHaveCount(1);

    /* Label the seven visible cells with Tab (which skips the continuation). */
    await toFirstCell(page);
    for (const [i, label] of ['A', 'B', 'C', 'D', 'E', 'F', 'G'].entries()) {
        if (i > 0) await press(page, 'Tab');
        await page.keyboard.type(label);
    }
    const m = await mirror(page);
    expect(m.cells).toEqual({
        '0,0': 'A',
        '0,1': 'B',
        '0,2': 'C',
        '1,0': 'D',
        '1,2': 'E',
        '2,0': 'F',
        '2,1': 'G',
    });

    /* Right from the end of `Intro`: every visible cell once, in logical
       order — (1, 1), the continuation, is never a stop. */
    const order: Array<[number, number]> = [
        [0, 0],
        [0, 1],
        [0, 2],
        [1, 0],
        [1, 2],
        [2, 0],
        [2, 1],
    ];
    const forward: string[] = [BODY(0, 5)];
    for (const [r, c] of order) forward.push(CELL(r, c, 0), CELL(r, c, 1));
    forward.push(BODY(2, 0));
    await press(page, 'Control+Home');
    await press(page, 'End');
    const walked = [await caretPos(page)];
    for (let i = 1; i < forward.length; i++) {
        await press(page, 'ArrowRight');
        walked.push(await caretPos(page));
    }
    expect(walked).toEqual(forward);
    const back = [await caretPos(page)];
    for (let i = 1; i < forward.length; i++) {
        await press(page, 'ArrowLeft');
        back.push(await caretPos(page));
    }
    expect(back).toEqual([...forward].reverse());
});

/* Row 26 — a `<w:bidiVisual>` table of right-to-left (Arabic, `<w:bidi/>`)
 * paragraphs, between two Latin paragraphs. Its first cell is the RIGHTMOST
 * one. */
const AR = ['اب', 'تث', 'جح', 'خد'];
function rtlTableDocx(): Uint8Array {
    const cell = (text: string): string =>
        '<w:tc><w:tcPr><w:tcW w:w="3000" w:type="dxa"/></w:tcPr>' +
        `<w:p><w:pPr><w:bidi/></w:pPr><w:r><w:rPr><w:rtl/></w:rPr><w:t>${text}</w:t></w:r></w:p>` +
        '</w:tc>';
    const row = (a: string, b: string): string => `<w:tr>${cell(a)}${cell(b)}</w:tr>`;
    const table =
        '<w:tbl><w:tblPr><w:bidiVisual/><w:tblW w:w="6000" w:type="dxa"/></w:tblPr>' +
        '<w:tblGrid><w:gridCol w:w="3000"/><w:gridCol w:w="3000"/></w:tblGrid>' +
        row(AR[0]!, AR[1]!) +
        row(AR[2]!, AR[3]!) +
        '</w:tbl>';
    const p = (t: string): string => `<w:p><w:r><w:t>${t}</w:t></w:r></w:p>`;
    return storedDocx(documentXml(p('intro') + table + p('outro')));
}

async function openDocx(page: Page, bytes: Uint8Array, name: string): Promise<void> {
    const type = await page.evaluate(
        async ({ arr, name }) => {
            const evt = await (window as any).__dispatch({
                type: 'OPEN_DOCUMENT',
                bytes: new Uint8Array(arr),
                format: 'docx',
                name,
            });
            return evt.type as string;
        },
        { arr: Array.from(bytes), name },
    );
    expect(type).toBe('DOCUMENT_LOADED');
    await settle(page);
}

test('row 26: in a bidiVisual table the cell exits and Tab follow the table direction (pinned, Word unverified)', async ({
    page,
}) => {
    test.setTimeout(120_000);
    await boot(page);
    await openDocx(page, rtlTableDocx(), 'rtl-table.docx');
    await watchSelection(page);
    await expect(page.locator('.a11y-mirror td[role="gridcell"]')).toHaveCount(4);
    await page.locator(INPUT).focus();

    /* Into the first cell from the end of `intro`. */
    await press(page, 'Control+Home');
    await press(page, 'End');
    await press(page, 'ArrowRight');
    expect(await caretPos(page)).toBe(CELL(0, 0, 0));

    /* Left — the table's reading direction — walks each RTL cell from its
       logical start (right edge) to its end (left edge), then on to the
       START of the next cell in reading order, which sits further LEFT;
       after the last cell it leaves to the paragraph after the table. */
    const forward: string[] = [];
    for (let r = 0; r < 2; r++) {
        for (let c = 0; c < 2; c++) forward.push(CELL(r, c, 0), CELL(r, c, 2), CELL(r, c, 4));
    }
    forward.push(BODY(2, 0));
    const walked = [await caretPos(page)];
    const xy: Record<string, { x: number; y: number }> = { [walked[0]!]: await caretXY(page) };
    for (let i = 1; i < forward.length; i++) {
        await press(page, 'ArrowLeft');
        const here = await caretPos(page);
        walked.push(here);
        xy[here] = await caretXY(page);
    }
    expect(walked).toEqual(forward);
    /* Visual order: each cell's logical start is right of its end, and the
       second cell of a row starts LEFT of the first cell's end. */
    for (let r = 0; r < 2; r++) {
        for (let c = 0; c < 2; c++) {
            expect(xy[CELL(r, c, 0)]!.x).toBeGreaterThan(xy[CELL(r, c, 4)]!.x);
        }
        expect(xy[CELL(r, 1, 0)]!.x).toBeLessThan(xy[CELL(r, 0, 4)]!.x);
        expect(Math.abs(xy[CELL(r, 1, 0)]!.y - xy[CELL(r, 0, 0)]!.y)).toBeLessThan(1);
    }
    expect(xy[CELL(1, 0, 0)]!.y).toBeGreaterThan(xy[CELL(0, 0, 0)]!.y);

    /* Right walks the same stops back, and out of the first cell to the
       END of `intro`. */
    await press(page, 'ArrowLeft'); // outro@0 -> the last cell's end
    expect(await caretPos(page)).toBe(CELL(1, 1, 4));
    const back = [await caretPos(page)];
    const reverse = [...forward].reverse().slice(1);
    for (let i = 1; i < reverse.length; i++) {
        await press(page, 'ArrowRight');
        back.push(await caretPos(page));
    }
    expect(back).toEqual(reverse);
    await press(page, 'ArrowRight');
    expect(await caretPos(page)).toBe(BODY(0, 5));

    /* Tab / Shift+Tab: the next / previous cell in reading order (to the
       LEFT / RIGHT on screen), its content selected. */
    await press(page, 'ArrowRight'); // back into (0, 0)
    expect(await caretPos(page)).toBe(CELL(0, 0, 0));
    const first = await caretXY(page);
    await press(page, 'Tab');
    expect(await selectedText(page)).toBe(AR[1]);
    let sel = await selection(page);
    expect([at(sel.range.start), at(sel.range.end)]).toEqual([CELL(0, 1, 0), CELL(0, 1, 4)]);
    expect((await caretXY(page)).x).toBeLessThan(first.x);
    await press(page, 'Tab');
    expect(await selectedText(page)).toBe(AR[2]);
    await press(page, 'Shift+Tab');
    await press(page, 'Shift+Tab');
    expect(await selectedText(page)).toBe(AR[0]);
    sel = await selection(page);
    expect([at(sel.range.start), at(sel.range.end)]).toEqual([CELL(0, 0, 0), CELL(0, 0, 4)]);
});

test('row 27: Tab / Shift+Tab move to the next / previous cell and select its content; Tab in the last cell appends a row', async ({
    page,
}) => {
    test.setTimeout(120_000);
    await tableDoc(page, ['1,1']);

    await toFirstCell(page);
    const range = async (): Promise<string[]> => {
        const sel = await selection(page);
        return [at(sel.range.start), at(sel.range.end)];
    };
    await press(page, 'Tab');
    expect(await selectedText(page)).toBe('b1');
    expect(await range()).toEqual([CELL(0, 1, 0), CELL(0, 1, 2)]);
    await press(page, 'Tab');
    expect(await selectedText(page)).toBe('c1');
    await press(page, 'Tab'); // wraps to the next row
    expect(await selectedText(page)).toBe('a2');
    expect(await range()).toEqual([CELL(1, 0, 0), CELL(1, 0, 2)]);
    /* An empty cell: the caret at its start. */
    await press(page, 'Tab');
    expect(await caretPos(page)).toBe(CELL(1, 1, 0));
    await press(page, 'Tab');
    expect(await selectedText(page)).toBe('c2');
    await press(page, 'Shift+Tab');
    expect(await caretPos(page)).toBe(CELL(1, 1, 0));
    await press(page, 'Shift+Tab');
    expect(await selectedText(page)).toBe('a2');
    await press(page, 'Shift+Tab'); // wraps to the previous row's last cell
    expect(await selectedText(page)).toBe('c1');
    expect(await range()).toEqual([CELL(0, 2, 0), CELL(0, 2, 2)]);

    /* Tab in the last cell appends a row and lands in its first cell. */
    await press(page, 'Control+End');
    await press(page, 'Home');
    await press(page, 'ArrowLeft');
    expect(await caretPos(page)).toBe(CELL(2, 2, 2));
    await press(page, 'Tab');
    await expect(page.locator('.a11y-mirror td[role="gridcell"]')).toHaveCount(12);
    expect(await caretPos(page)).toBe(CELL(3, 0, 0));
    await page.keyboard.type('new');
    const m = await mirror(page);
    expect(m.cells['3,0']).toBe('new');
    expect(m.cells['2,2']).toBe('c3');
    expect(m.body).toEqual(['Intro', 'Outro']);
});

test('row 28: Delete before a table joins the first cell; Backspace after a table and at cell edges deletes nothing', async ({
    page,
}) => {
    test.setTimeout(120_000);
    await tableDoc(page);
    const expectUntouched = async (): Promise<void> => {
        const m = await mirror(page);
        expect(m.body).toEqual(['Intro', 'Outro']);
        for (let r = 0; r < 3; r++) {
            for (let c = 0; c < 3; c++) expect(m.cells[`${r},${c}`]).toBe(LABEL(r, c));
        }
    };

    /* Backspace at the start of the paragraph after the table: nothing. */
    await press(page, 'Control+End');
    await press(page, 'Home');
    expect(await caretPos(page)).toBe(BODY(2, 0));
    await press(page, 'Backspace');
    await expectUntouched();
    expect(await caretPos(page)).toBe(BODY(2, 0));

    /* Backspace at the start of a cell, Delete at its end: nothing. */
    await toFirstCell(page);
    await press(page, 'Tab');
    await press(page, 'Tab');
    await press(page, 'Tab'); // a2 selected
    await press(page, 'Home');
    expect(await caretPos(page)).toBe(CELL(1, 0, 0));
    await press(page, 'Backspace');
    await expectUntouched();
    expect(await caretPos(page)).toBe(CELL(1, 0, 0));
    await press(page, 'End');
    await press(page, 'Delete');
    await expectUntouched();
    expect(await caretPos(page)).toBe(CELL(1, 0, 2));
    /* The first cell's start (after `Intro`) and the last cell's end
       (before `Outro`) are cell boundaries too. */
    await toFirstCell(page);
    await press(page, 'Backspace');
    await expectUntouched();
    expect(await caretPos(page)).toBe(CELL(0, 0, 0));
    await press(page, 'Control+End');
    await press(page, 'Home');
    await press(page, 'ArrowLeft');
    expect(await caretPos(page)).toBe(CELL(2, 2, 2));
    await press(page, 'Delete');
    await expectUntouched();
    expect(await caretPos(page)).toBe(CELL(2, 2, 2));

    /* Delete at the end of `Intro`: the paragraph joins the first cell's
       first paragraph, the caret at the join. */
    await press(page, 'Control+Home');
    await press(page, 'End');
    await press(page, 'Delete');
    const m = await mirror(page);
    expect(m.body).toEqual(['Outro']);
    expect(m.cells['0,0']).toBe('Introa1');
    expect(m.cells['0,1']).toBe('b1');
    expect(await caretPos(page)).toBe('B0/C0,0/B0@5');
    /* One undo step brings `Intro` back as its own paragraph. */
    await press(page, 'Control+Z');
    await expectUntouched();
});
