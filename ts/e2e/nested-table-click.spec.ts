import { test, expect } from '@playwright/test';
import { appendStoredEntry } from './zip-append';
import { boot } from './helpers/editor';

/* Issue #377 — a click inside a table NESTED in a table cell places the
 * caret in the nested cell's paragraph, and typing lands there, end to
 * end through the REAL shell pointer path (a Playwright mouse click on
 * the page canvas → `pointer.ts` → `PLACE_CARET_AT_POINT`) + worker +
 * WASM engine. Before #377 the hit map stopped at the outer table's own
 * cells, so the click resolved to the nearest OUTER-cell line.
 *
 * The click target is read off the engine itself (the caret rect of a
 * position in the inner paragraph — caret rects of nested positions are
 * part of the same fix), so the spec does not depend on font metrics.
 * Assertions ride command replies and clipboard text — engine truth,
 * valid under headless Chrome (the canvas is never screenshotted, see
 * CLAUDE.md). */

declare global {
    interface Window {
        __dispatch: (cmd: unknown) => Promise<any>;
    }
}

/** One table level: cell (0,0) holds `heading` (+ the next level), cell
 *  (0,1) a sibling paragraph. */
function level(heading: string, inner = ''): string {
    const p = (t: string): string => `<w:p><w:r><w:t xml:space="preserve">${t}</w:t></w:r></w:p>`;
    return (
        '<w:tbl><w:tblGrid><w:gridCol w:w="3600"/><w:gridCol w:w="1800"/></w:tblGrid><w:tr>' +
        `<w:tc>${p(heading)}${inner}</w:tc>` +
        `<w:tc>${p(`${heading} sibling`)}</w:tc>` +
        '</w:tr></w:tbl>'
    );
}

/** A minimal STORED `.docx` around `body`. */
function docx(body: string): number[] {
    const xml = '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n';
    const entries: [string, string][] = [
        [
            '[Content_Types].xml',
            `${xml}<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">` +
                '<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>' +
                '<Default Extension="xml" ContentType="application/xml"/>' +
                '<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>' +
                '</Types>',
        ],
        [
            '_rels/.rels',
            `${xml}<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">` +
                '<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>' +
                '</Relationships>',
        ],
        [
            'word/document.xml',
            `${xml}<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">` +
                `<w:body>${body}<w:sectPr/></w:body></w:document>`,
        ],
    ];
    const eocd = new Uint8Array(22);
    new DataView(eocd.buffer).setUint32(0, 0x06054b50, true);
    let zip = eocd;
    for (const [name, text] of entries) {
        zip = appendStoredEntry(zip, name, new TextEncoder().encode(text));
    }
    return Array.from(zip);
}

const BODY =
    '<w:p><w:r><w:t>intro</w:t></w:r></w:p>' +
    level('outer cell', level('middle cell', level('inner cell text'))) +
    '<w:p><w:r><w:t>outro</w:t></w:r></w:p>';

type Step = { kind: 'BLOCK'; idx: number } | { kind: 'CELL'; row: number; col: number };

/** `[Block(1), Cell(0,0), Block(1), Cell(0,0), Block(1), Cell(0,0), Block(0)]`. */
const INNER: Step[] = [
    { kind: 'BLOCK', idx: 1 },
    { kind: 'CELL', row: 0, col: 0 },
    { kind: 'BLOCK', idx: 1 },
    { kind: 'CELL', row: 0, col: 0 },
    { kind: 'BLOCK', idx: 1 },
    { kind: 'CELL', row: 0, col: 0 },
    { kind: 'BLOCK', idx: 0 },
];

/** Plain text of the paragraph at `steps` (selected whole, then copied). */
async function paragraphText(
    page: import('@playwright/test').Page,
    steps: Step[],
    len: number,
): Promise<string> {
    return page.evaluate(
        async ({ steps, len }) => {
            const at = (offset: number) => ({ path: { steps }, offset });
            await window.__dispatch({
                type: 'SET_SELECTION',
                range: { start: at(0), end: at(len) },
                caret: at(len),
            });
            const clip = await window.__dispatch({ type: 'GET_SELECTION_AS_CLIPBOARD' });
            return clip.type === 'CLIPBOARD_PAYLOAD' ? (clip.plain as string) : `<${clip.type}>`;
        },
        { steps, len },
    );
}

test.use({ viewport: { width: 1280, height: 1400 } });

test('a click inside a 3-deep nested table cell places the caret there and typing lands there', async ({
    page,
}) => {
    test.setTimeout(60_000);
    await boot(page);

    /* Load, then read the device-px caret rect of a position inside the
       INNER paragraph ("inner |cell text") and park the caret back in
       the intro so only the click can move it. */
    const target = await page.evaluate(
        async ({ bytes, steps }) => {
            const loaded = await window.__dispatch({
                type: 'LOAD_DOCX',
                bytes: new Uint8Array(bytes),
            });
            if (loaded.type === 'ERROR') throw new Error(loaded.message);
            const at = { path: { steps }, offset: 'inner '.length };
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
            const caret = sel.caret as { x: number; y: number; w: number; h: number };
            /* The engine's own hit test of that point (document device
               px — page 0 starts at the origin). */
            const hit = await window.__dispatch({
                type: 'HIT_TEST',
                at: { x: caret.x, y: caret.y + caret.h / 2 },
            });
            return { caret, hit: hit.pos };
        },
        { bytes: docx(BODY), steps: INNER },
    );
    expect(target.caret.h).toBeGreaterThan(0);
    expect(target.hit.path.steps).toEqual(INNER);
    expect(target.hit.offset).toBe('inner '.length);

    /* A real mouse click on that point of the page-0 canvas. */
    const canvas = page.locator('.editor-page[data-page-index="0"] canvas').first();
    await expect(canvas).toBeVisible();
    await canvas.scrollIntoViewIfNeeded();
    const box = await canvas.boundingBox();
    if (!box) throw new Error('page canvas has no box');
    const dpr = await page.evaluate(() => window.devicePixelRatio || 1);
    await page.mouse.click(
        box.x + target.caret.x / dpr,
        box.y + (target.caret.y + target.caret.h / 2) / dpr,
    );
    await page.keyboard.type('Z');

    const inner = 'inner cell text';
    await expect
        .poll(() => paragraphText(page, INNER, inner.length + 1))
        .toBe('inner Zcell text');
    /* The outer and middle headings are untouched. */
    expect(
        await paragraphText(
            page,
            [{ kind: 'BLOCK', idx: 1 }, { kind: 'CELL', row: 0, col: 0 }, { kind: 'BLOCK', idx: 0 }],
            'outer cell'.length,
        ),
    ).toBe('outer cell');
    expect(
        await paragraphText(
            page,
            [
                { kind: 'BLOCK', idx: 1 },
                { kind: 'CELL', row: 0, col: 0 },
                { kind: 'BLOCK', idx: 1 },
                { kind: 'CELL', row: 0, col: 0 },
                { kind: 'BLOCK', idx: 0 },
            ],
            'middle cell'.length,
        ),
    ).toBe('middle cell');
});
