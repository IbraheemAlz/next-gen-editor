import { test, expect, type Page } from '@playwright/test';
import { boot, burst, documentText, settle } from './helpers/editor';

/* Issue #598 — epic #555 row 22 (clean-room cursor memo §5 / §6), through
 * the REAL shell: Playwright mouse events on the page canvas -> `pointer.ts`
 * (the press's `mousedown` click count -> `SELECT_UNITS_IN_PAGE`, the
 * `dblclick` / `click` fallbacks) -> worker -> WASM engine; keys through the
 * hidden textarea.
 *
 *  - a double-click selects the word WITH its trailing spaces; punctuation
 *    is a word of its own; a double-click on a space selects the word it
 *    trails; an Arabic word with a tatweel and harakat is one word (the ONE
 *    word model, `engine::words`, shared with Ctrl+arrows);
 *  - typing over a double-clicked word keeps its trailing space;
 *  - a drag after a double press grows by whole words, either way, the
 *    double-clicked word staying selected (anchor at its END when the drag
 *    goes back); Shift+click / Shift+arrows then extend from the anchor;
 *  - a drag after a triple press grows by whole paragraphs;
 *  - a key typed right after the double PRESS replaces the word (the press
 *    selects synchronously — a `burst`, so no engine reply interleaves).
 *
 * `__dispatch` only opens the document and reads caret rects for the click
 * targets (setup); the evidence is the engine's own clipboard readback of
 * the selection and the document text (headless Chrome never composites
 * the interactive canvas, CLAUDE.md). */

test.use({ viewport: { width: 1280, height: 900 } });

interface Rect {
    x: number;
    y: number;
    w: number;
    h: number;
}

const DOC = 'alpha beta, gamma delta epsilon\nsecond paragraph here\nthird one';
const utf8 = (s: string): number => new TextEncoder().encode(s).length;

const dispatch = (page: Page, cmd: unknown): Promise<any> =>
    page.evaluate((c) => (window as any).__dispatch(c), cmd);

/** Replace the document with plain text (one paragraph per line). */
async function openText(page: Page, text: string): Promise<void> {
    const evt = await dispatch(page, {
        type: 'OPEN_DOCUMENT',
        bytes: new TextEncoder().encode(text),
        format: 'plain_text',
        name: 'words.txt',
    });
    expect(evt.type).toBe('DOCUMENT_LOADED');
    await settle(page);
}

/** Setup: the caret rect at `(block, offset)` (page-0 local device px). */
async function caretRect(page: Page, block: number, offset: number): Promise<Rect> {
    const p = { path: { steps: [{ kind: 'BLOCK', idx: block }] }, offset };
    const evt = await dispatch(page, { type: 'SET_SELECTION', range: { start: p, end: p }, caret: p });
    expect(evt.type).toBe('SELECTION_CHANGED');
    return evt.caret as Rect;
}

/** The middle of the character cell `[a, b)` of paragraph `block`, in
 *  page-0 local DEVICE px (works for an RTL cell too: the midpoint of its
 *  two caret stops). */
async function cellDevice(page: Page, block: number, a: number, b: number): Promise<{ x: number; y: number }> {
    const ra = await caretRect(page, block, a);
    const rb = await caretRect(page, block, b);
    return { x: (ra.x + rb.x) / 2, y: ra.y + ra.h / 2 };
}

/** A page-0 local device-px point as viewport (mouse) coordinates. */
async function toClient(page: Page, p: { x: number; y: number }): Promise<{ x: number; y: number }> {
    const canvas = page.locator('.editor-page[data-page-index="0"] canvas').first();
    await expect(canvas).toBeVisible();
    const box = await canvas.boundingBox();
    if (!box) throw new Error('page 0 canvas has no box');
    const dpr = await page.evaluate(() => window.devicePixelRatio || 1);
    return { x: box.x + p.x / dpr, y: box.y + p.y / dpr };
}

/** The middle of cell `[a, b)` of paragraph `block`, as mouse coordinates. */
async function cell(page: Page, block: number, a: number, b: number): Promise<{ x: number; y: number }> {
    return toClient(page, await cellDevice(page, block, a, b));
}

/** The selected text, read through the worker queue (FIFO behind every
 *  command already posted); the selection itself is left untouched. */
async function selectedText(page: Page): Promise<string> {
    return page.evaluate(async () => {
        const clip = await (window as any).__dispatch({ type: 'GET_SELECTION_AS_CLIPBOARD' });
        return clip.type === 'CLIPBOARD_PAYLOAD' ? (clip.plain as string) : `<${clip.type}>`;
    });
}

async function paragraphs(page: Page): Promise<string[]> {
    return (await documentText(page)).split('\n');
}

test('a double-click selects the word with its trailing spaces; punctuation is its own word; typing keeps the space', async ({
    page,
}) => {
    test.setTimeout(60_000);
    await boot(page);
    await openText(page, DOC);
    const P0 = 'alpha beta, gamma delta epsilon';
    const at = (s: string, i = 1) => P0.indexOf(s) + i;
    const targets: [string, { x: number; y: number }][] = [
        ['alpha ', await cell(page, 0, at('alpha'), at('alpha') + 1)],
        /* `beta` is followed by the comma: no space to take. */
        ['beta', await cell(page, 0, at('beta'), at('beta') + 1)],
        /* The comma is a word of its own, with the space after it. */
        [', ', await cell(page, 0, at(',', 0), at(',', 1))],
        /* A double-click on a space selects the word it trails. */
        ['gamma ', await cell(page, 0, at(' delta', 0), at(' delta', 1))],
        /* The last word of the paragraph has no trailing space. */
        ['epsilon', await cell(page, 0, at('epsilon', 3), at('epsilon', 4))],
    ];
    for (const [expected, p] of targets) {
        await page.mouse.dblclick(p.x, p.y);
        await expect.poll(() => selectedText(page), { message: expected }).toBe(expected);
    }

    /* Typing over a double-clicked word replaces the word, not its space. */
    const gamma = await cell(page, 0, at('gamma'), at('gamma') + 1);
    await page.mouse.dblclick(gamma.x, gamma.y);
    await expect.poll(() => selectedText(page)).toBe('gamma ');
    await page.keyboard.type('G');
    await expect.poll(async () => (await paragraphs(page))[0]).toBe('alpha beta, G delta epsilon');
});

test('an Arabic word with a tatweel and harakat is one word', async ({ page }) => {
    test.setTimeout(60_000);
    await boot(page);
    const tatweelWord = 'كتـــاب';
    const markedWord = 'جَدِيدٌ';
    const text = `${tatweelWord} ${markedWord}، نص`;
    await openText(page, text);
    const tatweel = utf8('كتـ'); // the second tatweel's byte offset
    const marked = utf8(`${tatweelWord} جَ`); // the cell `دِ`
    const comma = utf8(`${tatweelWord} ${markedWord}`);
    const targets: [string, { x: number; y: number }][] = [
        /* On a tatweel: the whole word, with its space. */
        [`${tatweelWord} `, await cell(page, 0, tatweel, tatweel + utf8('ـ'))],
        /* On a letter carrying a haraka: the whole marked word. */
        [markedWord, await cell(page, 0, marked, marked + utf8('دِ'))],
        /* The Arabic comma is punctuation, its own word. */
        ['، ', await cell(page, 0, comma, comma + utf8('،'))],
    ];
    for (const [expected, p] of targets) {
        await page.mouse.dblclick(p.x, p.y);
        await expect.poll(() => selectedText(page), { message: expected }).toBe(expected);
    }
});

test('a drag after a double press grows by whole words; Shift extends from the anchor', async ({ page }) => {
    test.setTimeout(60_000);
    await boot(page);
    await openText(page, DOC);
    const P0 = 'alpha beta, gamma delta epsilon';
    const at = (s: string, i = 1) => P0.indexOf(s) + i;
    const gamma = await cell(page, 0, at('gamma'), at('gamma') + 1);
    const delta = await cell(page, 0, at('delta'), at('delta') + 1);
    const beta = await cell(page, 0, at('beta'), at('beta') + 1);
    const epsRect = await caretRect(page, 0, at('epsilon', 3));
    const eps = await toClient(page, { x: epsRect.x, y: epsRect.y + epsRect.h / 2 });

    /* Press, release, press again — and drag without releasing. */
    await page.mouse.move(gamma.x, gamma.y);
    await page.mouse.down();
    await page.mouse.up();
    await page.mouse.down({ clickCount: 2 });
    await expect.poll(() => selectedText(page)).toBe('gamma ');
    await page.mouse.move(delta.x, delta.y, { steps: 4 });
    await expect.poll(() => selectedText(page)).toBe('gamma delta ');
    /* Back past the pressed word: it stays selected, whole words before it. */
    await page.mouse.move(beta.x, beta.y, { steps: 6 });
    await expect.poll(() => selectedText(page)).toBe('beta, gamma ');
    /* The release (and its `dblclick`) keeps the dragged selection. */
    await page.mouse.up({ clickCount: 2 });
    await settle(page);
    expect(await selectedText(page)).toBe('beta, gamma ');
    /* The drag went back, so the caret is the START: Shift+Right moves it,
       the anchor stays at the end of `gamma `. */
    await page.keyboard.press('Shift+ArrowRight');
    await expect.poll(() => selectedText(page)).toBe('eta, gamma ');

    /* A double-click, then Shift+click: the selection extends from the
       word's start (the anchor) to the click. */
    await page.mouse.dblclick(gamma.x, gamma.y);
    await expect.poll(() => selectedText(page)).toBe('gamma ');
    await page.keyboard.down('Shift');
    await page.mouse.click(eps.x, eps.y);
    await page.keyboard.up('Shift');
    await expect.poll(() => selectedText(page)).toBe('gamma delta eps');
});

test('a drag after a triple press grows by whole paragraphs', async ({ page }) => {
    test.setTimeout(60_000);
    await boot(page);
    await openText(page, DOC);
    const p1 = await cell(page, 1, 3, 4);
    const p2 = await cell(page, 2, 2, 3);
    const p0 = await cell(page, 0, 2, 3);
    await page.mouse.move(p1.x, p1.y);
    await page.mouse.down();
    await page.mouse.up();
    await page.mouse.down({ clickCount: 2 });
    await page.mouse.up({ clickCount: 2 });
    await page.mouse.down({ clickCount: 3 });
    await expect.poll(() => selectedText(page)).toBe('second paragraph here');
    await page.mouse.move(p2.x, p2.y, { steps: 4 });
    await expect.poll(() => selectedText(page)).toBe('second paragraph here\nthird one');
    await page.mouse.move(p0.x, p0.y, { steps: 6 });
    await expect
        .poll(() => selectedText(page))
        .toBe('alpha beta, gamma delta epsilon\nsecond paragraph here');
    await page.mouse.up({ clickCount: 3 });
    await settle(page);
    expect(await selectedText(page)).toBe('alpha beta, gamma delta epsilon\nsecond paragraph here');
});

test('a key typed right after the double press replaces the word (burst)', async ({ page }) => {
    test.setTimeout(60_000);
    await boot(page);
    await openText(page, DOC);
    const P0 = 'alpha beta, gamma delta epsilon';
    const g = P0.indexOf('gamma');
    const p = await cellDevice(page, 0, g + 2, g + 3);
    /* Park the caret elsewhere so only the gesture can move it. */
    await caretRect(page, 2, 0);
    /* Two presses and a keystroke in ONE task: no engine reply can land in
       between, so the key must be queued behind the press's selection. */
    await burst(page, [
        { pointerdown: { x: p.x, y: p.y, clicks: 1 } },
        { pointerup: true },
        { pointerdown: { x: p.x, y: p.y, clicks: 2 } },
        { pointerup: true },
        'G',
    ]);
    expect((await paragraphs(page))[0]).toBe('alpha beta, G delta epsilon');
});
