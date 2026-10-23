import { test, expect, type Page } from '@playwright/test';
import { boot, settle } from './helpers/editor';
import { documentXml, readZipEntry, storedDocx } from './helpers/docx';

/* Issue #551 — Tab / Shift+Tab into a table cell that STARTS (or ends) with
 * a nested table selects the cell's content (memo §7: "Tab / Shift+Tab move
 * to the next or previous cell and select its content"), through the REAL
 * shell: a mouse click on the page canvas (`pointer.ts` ->
 * `PLACE_CARET_AT_POINT`), the Tab key on the hidden textarea
 * (`HiddenInput` -> `MOVE_CARET NextCell / PrevCell`), typing through
 * `beforeinput`.
 *
 * Before the fix the selection's anchor was `Block(0)` of the cell — the
 * nested TABLE, not a paragraph — so `clamp_pos` threw the caret to the
 * document's last paragraph: no cell selection, and the next keystroke
 * landed in `outro`. Now the span runs from the first paragraph in document
 * order inside the cell (inside the leading nested table) to the end of its
 * last one (inside a trailing nested table).
 *
 * Setup only goes through `__dispatch`: the fixture opens from a `.docx`
 * (the toolbar cannot author a nested table — Insert > Table in a cell
 * inserts a top-level table after the outer one, issue #563) and a
 * `SET_SELECTION` reads the click target's caret rect off the engine.
 * Typing over the span puts the character IN the cell; replacing the
 * WHOLE span (nested table included) is the cross-container delete of
 * issue #561, so the assertion is written to hold before and after it. */

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

const p = (t: string): string => `<w:p><w:r><w:t xml:space="preserve">${t}</w:t></w:r></w:p>`;
const tc = (inner: string): string => `<w:tc>${inner}</w:tc>`;
const tbl = (cols: number, rows: string[][]): string =>
    '<w:tbl><w:tblGrid>' +
    '<w:gridCol w:w="2400"/>'.repeat(cols) +
    '</w:tblGrid>' +
    rows.map((cells) => `<w:tr>${cells.map(tc).join('')}</w:tr>`).join('') +
    '</w:tbl>';

/* intro
 * | alpha | [inner1 | inner2] after |   <- (0,1) STARTS with a nested table
 * | gamma | delta [deep1 | deep2]   |   <- (1,1) ENDS with a nested table
 * outro */
const BODY =
    p('intro') +
    tbl(2, [
        [p('alpha'), tbl(2, [[p('inner1'), p('inner2')]]) + p('after')],
        [p('gamma'), p('delta') + tbl(2, [[p('deep1'), p('deep2')]])],
    ]) +
    p('outro') +
    '<w:sectPr/>';

const ALPHA = [B(1), C(0, 0), B(0)];
const GAMMA = [B(1), C(1, 0), B(0)];
const INNER1 = [B(1), C(0, 1), B(0), C(0, 0), B(0)];
const AFTER = [B(1), C(0, 1), B(1)];
const DELTA = [B(1), C(1, 1), B(0)];
const DEEP2 = [B(1), C(1, 1), B(1), C(0, 1), B(0)];

const ORIGINAL = ['intro', 'alpha', 'inner1', 'inner2', 'after', 'gamma', 'delta', 'deep1', 'deep2', 'outro'];

test.use({ viewport: { width: 1280, height: 1400 } });

/** Open the fixture and start logging every SELECTION_CHANGED (setup). */
async function openFixture(page: Page): Promise<void> {
    const bytes = Array.from(storedDocx(documentXml(BODY)));
    await page.evaluate(async (arr: number[]) => {
        const loaded = await window.__dispatch({
            type: 'OPEN_DOCUMENT',
            bytes: new Uint8Array(arr),
            format: 'docx',
            name: 'nested-cells.docx',
        });
        if (loaded.type === 'ERROR') throw new Error(loaded.message);
        const log: any[] = [];
        (window as any).__selLog = log;
        (window as any).__engineClient.subscribe((e: any) => {
            if (e.type === 'SELECTION_CHANGED') log.push(e);
        });
    }, bytes);
    await settle(page);
}

/** The engine's last reported selection, endpoints in document order as the
 *  event carries them. */
async function lastSelection(page: Page): Promise<{ start: Pos; end: Pos; kind: unknown }> {
    await settle(page);
    return page.evaluate(() => {
        const log = (window as any).__selLog as any[];
        const e = log[log.length - 1];
        return { start: e.range.start, end: e.range.end, kind: e.kind };
    });
}

/** Every paragraph's text, in document order, read from a real save. */
async function paragraphs(page: Page): Promise<string[]> {
    const saved = await page.evaluate(async () => {
        const evt = await window.__dispatch({ type: 'SAVE_DOCX' });
        return evt.type === 'DOCUMENT_SAVED'
            ? { ok: true, bytes: Array.from(evt.bytes as Uint8Array) }
            : { ok: false, evt };
    });
    expect(saved.ok, `SAVE_DOCX must succeed: ${JSON.stringify(saved.evt)}`).toBe(true);
    const xml = readZipEntry(new Uint8Array(saved.bytes as number[]), 'word/document.xml');
    const out: string[] = [];
    for (const m of xml.matchAll(/<w:p(?:\s[^>]*)?(?:\/>|>([\s\S]*?)<\/w:p>)/g)) {
        const text = [...(m[1] ?? '').matchAll(/<w:t(?:\s[^>]*)?>([^<]*)<\/w:t>/g)]
            .map((t) => t[1])
            .join('');
        if (text) out.push(text);
    }
    return out;
}

/** A real mouse click inside the paragraph at `steps`, `offset` bytes in.
 *  The point is the engine's own caret rect for that position (read with a
 *  SET_SELECTION, then the caret is parked in `intro` so only the click can
 *  move it there). */
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
    expect(sel.end.path.steps, 'the click put the caret in the cell').toEqual(steps);
    expect(sel.start).toEqual(sel.end);
}

/** The selection covers exactly `[from@0, to@toLen]`, the overlay shows it,
 *  and the document is unchanged. */
async function expectCellSelected(page: Page, from: Step[], to: Step[], toLen: number): Promise<void> {
    const sel = await lastSelection(page);
    expect(sel.start).toEqual({ path: { steps: from }, offset: 0 });
    expect(sel.end).toEqual({ path: { steps: to }, offset: toLen });
    await expect(page.locator('.editor-page[data-page-index="0"] .selection-rect').first()).toBeAttached();
    expect(await paragraphs(page)).toEqual(ORIGINAL);
}

test.beforeEach(async ({ page }) => {
    await boot(page);
    await openFixture(page);
    expect(await paragraphs(page)).toEqual(ORIGINAL);
});

test('Tab into a cell that starts with a nested table selects its content; typing lands in that cell', async ({
    page,
}) => {
    await clickInto(page, ALPHA, 2);
    await page.keyboard.press('Tab');
    await expectCellSelected(page, INNER1, AFTER, 'after'.length);

    await page.keyboard.type('Z');
    await settle(page);
    const after = await paragraphs(page);
    /* The typed character replaced the start of the selected content, inside
       cell (0,1): between `alpha` and `gamma`, and nothing outside the cell
       moved (before #551 it went to `outro`). */
    const z = after.findIndex((t) => t.startsWith('Z'));
    expect(z, JSON.stringify(after)).toBeGreaterThan(after.indexOf('alpha'));
    expect(z, JSON.stringify(after)).toBeLessThan(after.indexOf('gamma'));
    expect(after).not.toContain('inner1');
    const cell01 = new Set(['inner1', 'inner2', 'after']);
    expect(after.filter((t) => !cell01.has(t) && !t.startsWith('Z'))).toEqual(
        ORIGINAL.filter((t) => !cell01.has(t)),
    );
});

test('Shift+Tab back into the cell holding the nested table selects its content', async ({ page }) => {
    await clickInto(page, GAMMA, 2);
    await page.keyboard.press('Shift+Tab');
    await expectCellSelected(page, INNER1, AFTER, 'after'.length);
});

test('Tab into a cell that ends with a nested table selects through it; Shift+Tab steps back from that cell', async ({
    page,
}) => {
    await clickInto(page, GAMMA, 2);
    await page.keyboard.press('Tab');
    await expectCellSelected(page, DELTA, DEEP2, 'deep2'.length);

    /* The selection's end sits inside the nested table, but the step is
       taken from the selected CELL: back to (1,0), not to `deep1`. */
    await page.keyboard.press('Shift+Tab');
    await expectCellSelected(page, GAMMA, GAMMA, 'gamma'.length);
});
