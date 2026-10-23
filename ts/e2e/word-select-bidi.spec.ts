import { test, expect, type Page } from '@playwright/test';
import { boot, documentText } from './helpers/editor';

/* Issue #553 — a double-click on the page selects the word at a LOGICAL
 * place, end to end through the REAL shell pointer path (Playwright mouse
 * events on the page canvas -> `pointer.ts` -> `PLACE_CARET_AT_POINT` /
 * `SELECT_WORD_AT` -> worker -> WASM engine).
 *
 * The seeded paragraph `Hello world مرحبا بالعالم` is an LTR paragraph
 * that ends in an RTL run. The empty space to the right of its ink (and
 * below its last line) is the paragraph's logical END, but the visually
 * rightmost caret stop is the logical START of the Arabic run — so a
 * double-click there selected `مرحبا` (the first Arabic word) instead of
 * `بالعالم` (the word before the end), and a click placed the caret in
 * the middle of the Arabic text.
 *
 * Click targets are fractions of the page canvas (CSS px), so the spec
 * does not depend on font metrics; assertions ride the engine's own
 * clipboard readback (`GET_SELECTION_AS_CLIPBOARD`), valid under headless
 * Chrome (the canvas is never screenshotted, see CLAUDE.md). */

declare global {
    interface Window {
        __dispatch: (cmd: unknown) => Promise<any>;
    }
}

const SEED = 'Hello world مرحبا بالعالم';

test.use({ viewport: { width: 1280, height: 900 } });

/** The selected text, read through the worker queue (FIFO behind every
 *  command already posted); the selection itself is left untouched. */
async function selectedText(page: Page): Promise<string> {
    return page.evaluate(async () => {
        const clip = await window.__dispatch({ type: 'GET_SELECTION_AS_CLIPBOARD' });
        return clip.type === 'CLIPBOARD_PAYLOAD' ? (clip.plain as string) : `<${clip.type}>`;
    });
}

interface Targets {
    /** Page-0 canvas origin and size, CSS px. */
    box: { x: number; y: number; width: number; height: number };
    /** The seeded line's bottom edge, CSS px from the canvas top. */
    lineBottom: number;
}

/** Where the seeded line is, read off the engine (the caret rect at the
 *  paragraph's end), and the canvas box to click into. */
async function targets(page: Page): Promise<Targets> {
    expect(await documentText(page)).toBe(SEED);
    const caret = await page.evaluate(async (len) => {
        const end = { path: { steps: [{ kind: 'BLOCK', idx: 0 }] }, offset: len };
        const sel = await window.__dispatch({
            type: 'SET_SELECTION',
            range: { start: end, end },
            caret: end,
        });
        if (sel.type !== 'SELECTION_CHANGED') throw new Error(sel.type);
        return sel.caret as { x: number; y: number; w: number; h: number };
    }, new TextEncoder().encode(SEED).length);
    const canvas = page.locator('.editor-page[data-page-index="0"] canvas').first();
    await expect(canvas).toBeVisible();
    await canvas.scrollIntoViewIfNeeded();
    const box = await canvas.boundingBox();
    if (!box) throw new Error('page canvas has no box');
    const dpr = await page.evaluate(() => window.devicePixelRatio || 1);
    return { box, lineBottom: (caret.y + caret.h) / dpr };
}

/** A page point in the empty space BELOW the seeded line (still inside
 *  the page body), at fraction `fx` of the page width. */
function below(t: Targets, fx: number): { x: number; y: number } {
    return { x: t.box.x + t.box.width * fx, y: t.box.y + t.lineBottom + 24 };
}

test('a double-click below the last line of a mixed paragraph selects its logical last word', async ({
    page,
}) => {
    test.setTimeout(60_000);
    await boot(page);
    const t = await targets(page);

    /* Right of the ink: the paragraph's logical end -> the last word. */
    const right = below(t, 0.8);
    await page.mouse.dblclick(right.x, right.y);
    await expect.poll(() => selectedText(page)).toBe('بالعالم');

    /* Left of the ink (the page margin): the logical start -> the first
       word. */
    const left = below(t, 0.05);
    await page.mouse.dblclick(left.x, left.y);
    await expect.poll(() => selectedText(page)).toBe('Hello');

    /* On the line's own band, right of the ink: the same last word. */
    const onLine = { x: right.x, y: right.y - 24 - 6 };
    await page.mouse.dblclick(onLine.x, onLine.y);
    await expect.poll(() => selectedText(page)).toBe('بالعالم');

    /* A triple-click there selects the whole paragraph. */
    await page.mouse.click(right.x, right.y, { clickCount: 3 });
    await expect.poll(() => selectedText(page)).toBe(SEED);
});

test('a click past the end of a mixed line puts the caret at the logical end; Ctrl+Left walks logical words', async ({
    page,
}) => {
    test.setTimeout(60_000);
    await boot(page);
    const t = await targets(page);

    /* Park the caret at the start so only the click can move it. */
    await page.evaluate(async () => {
        const home = { path: { steps: [{ kind: 'BLOCK', idx: 0 }] }, offset: 0 };
        await window.__dispatch({
            type: 'SET_SELECTION',
            range: { start: home, end: home },
            caret: home,
        });
    });
    const right = below(t, 0.8);
    await page.mouse.click(right.x, right.y);
    await page.keyboard.type('!');
    await expect.poll(() => documentText(page)).toBe(`${SEED}!`);

    /* Ctrl+Left from the logical end lands on the start of the last
       LOGICAL word (never along the visual order). */
    await page.mouse.click(right.x, right.y);
    await page.keyboard.press('Control+ArrowLeft');
    await page.keyboard.type('|');
    await expect.poll(() => documentText(page)).toBe('Hello world مرحبا |بالعالم!');
});
