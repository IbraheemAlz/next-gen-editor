import { test, expect, type Page } from '@playwright/test';
import { boot, burst, documentText, settle } from './helpers/editor';

/* Issues #506 (items 1 and 3) and #554 — a picture inserted through the
 * toolbar's file input, the way a user does it:
 *
 *   1. it PAINTS its pixels (the worker decoded its bytes and registered the
 *      bitmap; before, `INSERT_IMAGE` never triggered the decode and the
 *      renderer drew the flat grey placeholder for the rest of the session);
 *   2. a click selects it (handles, no caret — object selection REPLACES the
 *      caret); Delete / Backspace remove it in one undoable edit, and the
 *      undo brings it back painted;
 *   3. Left / Right collapse the selection to a caret at the picture's own
 *      line (visual left / right edge); Up / Down walk from that line, so Up
 *      lands in the table's LAST row, never its first;
 *   4. typing with the picture selected inserts before it.
 *
 * Evidence is engine truth (`GET_IMAGE_RECTS`, the selection events) plus the
 * worker-side pixel read-back `probePagePixels` — headless Chrome never
 * composites the interactive canvas (CLAUDE.md), so a screenshot would be
 * blank. The click and key presses go through the REAL shell: `pointer.ts`
 * -> `selectImageByPoint` -> `ImageHandlesOverlay`, and the hidden
 * textarea's `onKeyDown`. */

test.use({ viewport: { width: 1280, height: 1400 } });

/** The decoded-bitmap colours the in-page PNGs are filled with. */
const RED = '#d02020';
const GREEN = '#20b040';

interface Img {
    at: number;
    rect: { x: number; y: number; w: number; h: number };
    media_key: string;
}

const dispatch = (page: Page, cmd: unknown): Promise<any> =>
    page.evaluate((c) => (window as any).__dispatch(c), cmd);

async function images(page: Page): Promise<Img[]> {
    const evt = await dispatch(page, { type: 'GET_IMAGE_RECTS' });
    if (evt.type !== 'IMAGE_RECTS') throw new Error(`GET_IMAGE_RECTS -> ${evt.type}`);
    return evt.images as Img[];
}

/** `above`, a 2 x 3 table, and an empty paragraph with the caret in it. */
async function seed(page: Page): Promise<void> {
    await page.evaluate(async () => {
        const d = (c: unknown): Promise<any> => (window as any).__dispatch(c);
        await d({ type: 'SELECT_ALL' });
        await d({ type: 'INSERT_TEXT', at: undefined, text: 'above' });
        const t = await d({
            type: 'INSERT_TABLE',
            at: { steps: [{ kind: 'BLOCK', idx: 1 }] },
            rows: 2,
            cols: 3,
        });
        if (t.type === 'ERROR') throw new Error(`INSERT_TABLE: ${t.message}`);
        await d({ type: 'MOVE_CARET', direction: 'DocEnd', extend: false });
        /* Every SELECTION_CHANGED, for the caret assertions. */
        const log: any[] = [];
        (window as any).__selLog = log;
        (window as any).__engineClient.subscribe((e: any) => {
            if (e.type === 'SELECTION_CHANGED') log.push(e);
        });
    });
}

/** Feed a PNG filled with `color` through the toolbar's real file input: a
 *  `File` built from a canvas `toBlob`, put on the input with a
 *  `DataTransfer`, then the `change` event `InsertImageButton` listens for. */
async function insertPng(page: Page, color: string): Promise<void> {
    await page.evaluate(async (fill) => {
        const canvas = document.createElement('canvas');
        canvas.width = 160;
        canvas.height = 100;
        const g = canvas.getContext('2d')!;
        g.fillStyle = fill;
        g.fillRect(0, 0, 160, 100);
        const blob: Blob = await new Promise((resolve, reject) =>
            canvas.toBlob((b) => (b ? resolve(b) : reject(new Error('toBlob'))), 'image/png'),
        );
        const input = document.querySelector<HTMLInputElement>(
            'input[type=file][accept*="image/png"]',
        );
        if (!input) throw new Error('Insert image file input missing');
        const dt = new DataTransfer();
        dt.items.add(new File([blob], 'picture.png', { type: 'image/png' }));
        input.files = dt.files;
        input.dispatchEvent(new Event('change', { bubbles: true }));
    }, color);
}

/** Wait until exactly `n` pictures are listed and return them. */
async function waitForImages(page: Page, n: number): Promise<Img[]> {
    await expect.poll(async () => (await images(page)).length, { timeout: 15_000 }).toBe(n);
    return images(page);
}

/** RGBA at page-0-local device px (the worker reads its surface back). */
async function pixels(
    page: Page,
    points: Array<{ x: number; y: number }>,
): Promise<Array<[number, number, number, number] | null>> {
    return page.evaluate(
        (p) => (window as any).__engineClient.probePagePixels(0, p),
        points,
    );
}

const near = (px: [number, number, number, number] | null, hex: string): boolean => {
    if (!px) return false;
    const want = [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16));
    return px[3] === 255 && want.every((c, i) => Math.abs((px[i] as number) - c) <= 12);
};

/** Sample points well inside the picture. */
const inside = (r: Img['rect']): Array<{ x: number; y: number }> => [
    { x: r.x + r.w * 0.2, y: r.y + r.h * 0.2 },
    { x: r.x + r.w * 0.5, y: r.y + r.h * 0.5 },
    { x: r.x + r.w * 0.8, y: r.y + r.h * 0.8 },
];

/** The picture shows `hex` everywhere we sample (and so is not the flat grey
 *  placeholder). */
async function expectPainted(page: Page, r: Img['rect'], hex: string): Promise<void> {
    await expect
        .poll(async () => (await pixels(page, inside(r))).every((p) => near(p, hex)), {
            message: `the picture's pixels are ${hex}`,
            timeout: 15_000,
        })
        .toBe(true);
}

/** Client coordinates of a page-0 device-px point (the real mouse's space). */
async function clientPoint(page: Page, x: number, y: number): Promise<{ x: number; y: number }> {
    const box = await page.locator('.editor-page[data-page-index="0"] canvas').boundingBox();
    if (!box) throw new Error('page 0 canvas has no box');
    const dpr = await page.evaluate(() => window.devicePixelRatio || 1);
    return { x: box.x + x / dpr, y: box.y + y / dpr };
}

/** Park the engine's caret at the very top of the document, far from the
 *  picture. Object selection is shell-side: the engine's caret stays where
 *  it was, and a key that fell through to it (the pre-#554 behaviour) acted
 *  THERE - Delete ate the first letter of `above`, Up / Left stayed in it.
 *  Right after an insert the caret sits just after the picture, which would
 *  make Backspace and the arrows pass by accident. */
async function parkCaretAtTop(page: Page): Promise<void> {
    await dispatch(page, { type: 'MOVE_CARET', direction: 'DocHome', extend: false });
}

/** A real click on the picture's centre (caret parked at the top first). */
async function clickPicture(page: Page, r: Img['rect']): Promise<void> {
    await parkCaretAtTop(page);
    const p = await clientPoint(page, r.x + r.w / 2, r.y + r.h / 2);
    await page.mouse.click(p.x, p.y);
    await expect(page.locator('.image-handles')).toHaveCount(1);
}

/** The caret overlay's box in page-0 CSS px (`null` when it is not drawn). */
async function caretBox(page: Page): Promise<{ x: number; y: number; h: number } | null> {
    await settle(page);
    return page.evaluate(() => {
        const el = document.querySelector<HTMLElement>('.editor-page[data-page-index="0"] .caret');
        if (!el) return null;
        return {
            x: parseFloat(el.style.left),
            y: parseFloat(el.style.top),
            h: parseFloat(el.style.height),
        };
    });
}

/** The engine's last reported caret address. */
async function lastCaret(page: Page): Promise<{ block: number; row?: number }> {
    await settle(page);
    return page.evaluate(() => {
        const log = (window as any).__selLog as any[];
        const steps = log[log.length - 1].range.end.path.steps as any[];
        const cell = steps.find((s) => s.kind === 'CELL');
        return { block: steps[0].idx as number, row: cell?.row as number | undefined };
    });
}

test.describe('an inserted picture (#506, #554)', () => {
    test.beforeEach(async ({ page }) => {
        await boot(page);
        await seed(page);
    });

    test('paints its pixels, selects without a caret, deletes with Delete and undoes back painted', async ({
        page,
    }) => {
        await insertPng(page, RED);
        const [img] = await waitForImages(page, 1);
        /* Bytes reached the worker's image cache: the picture is its own
           colour, not the placeholder's flat grey (221, 221, 221). */
        await expectPainted(page, img!.rect, RED);
        const grey = await pixels(page, [{ x: img!.rect.x + 5, y: img!.rect.y + 5 }]);
        expect(grey[0]?.slice(0, 3)).not.toEqual([221, 221, 221]);

        await clickPicture(page, img!.rect);
        /* Object selection replaces the caret. */
        expect(await caretBox(page)).toBeNull();

        await page.keyboard.press('Delete');
        await expect(page.locator('.image-handles')).toHaveCount(0);
        expect(await waitForImages(page, 0)).toEqual([]);
        /* The key did not also eat a letter at the parked caret. */
        expect(await documentText(page)).toContain('above');
        /* The caret is back, at the picture's old anchor. */
        const back = await caretBox(page);
        expect(back).not.toBeNull();

        /* ONE undo brings the picture back, and painted (the bitmap was
           registered once and survives the delete). */
        await page.keyboard.press('Control+z');
        const [again] = await waitForImages(page, 1);
        await expectPainted(page, again!.rect, RED);
    });

    test('Backspace deletes the selected picture too', async ({ page }) => {
        await insertPng(page, RED);
        const [img] = await waitForImages(page, 1);
        await clickPicture(page, img!.rect);
        await page.keyboard.press('Backspace');
        await waitForImages(page, 0);
        expect(await documentText(page)).toContain('above');
    });

    test('a click then Delete in one synchronous burst resolves against the shell-side selection', async ({
        page,
    }) => {
        await insertPng(page, RED);
        const [img] = await waitForImages(page, 1);
        await parkCaretAtTop(page);
        await burst(page, [
            { pointerdown: { x: img!.rect.x + img!.rect.w / 2, y: img!.rect.y + img!.rect.h / 2 } },
            { pointerup: true },
            { key: 'Delete' },
        ]);
        await settle(page);
        await waitForImages(page, 0);
    });

    test('Left / Right collapse to a caret at the picture\'s edges, in its own line', async ({
        page,
    }) => {
        await insertPng(page, RED);
        const [img] = await waitForImages(page, 1);
        const r = img!.rect;
        for (const [key, edgeX] of [
            ['ArrowLeft', r.x],
            ['ArrowRight', r.x + r.w],
        ] as const) {
            await clickPicture(page, r);
            await page.keyboard.press(key);
            await expect(page.locator('.image-handles')).toHaveCount(0);
            const c = await caretBox(page);
            expect(c, `${key}: caret drawn`).not.toBeNull();
            /* In the picture's line - never up in the table above it. */
            expect(c!.y + c!.h, `${key}: caret reaches the picture's line`).toBeGreaterThan(r.y);
            expect(c!.y, `${key}: caret starts inside the picture's line`).toBeLessThan(r.y + r.h);
            /* At the visual edge in the direction of travel, whatever the
               paragraph's direction. */
            expect(Math.abs(c!.x - edgeX), `${key}: caret x ${c!.x} vs edge ${edgeX}`).toBeLessThan(4);
            expect((await lastCaret(page)).block, `${key}: caret in the picture's paragraph`).toBe(2);
        }
    });

    test('Up from the selected picture lands in the table\'s last row, Down stays in its line', async ({
        page,
    }) => {
        await insertPng(page, RED);
        const [img] = await waitForImages(page, 1);
        const r = img!.rect;

        await clickPicture(page, r);
        await page.keyboard.press('ArrowDown');
        /* Nothing below the picture's paragraph: the caret stays in it. */
        expect((await lastCaret(page)).block).toBe(2);
        const down = await caretBox(page);
        expect(down!.y + down!.h).toBeGreaterThan(r.y);

        await clickPicture(page, r);
        await page.keyboard.press('ArrowUp');
        const up = await lastCaret(page);
        expect(up.block, 'the table').toBe(1);
        expect(up.row, 'the table\'s LAST row (2 rows), not its first').toBe(1);
        const c = await caretBox(page);
        expect(c!.y + c!.h, 'above the picture').toBeLessThanOrEqual(r.y + 1);
    });

    test('typing with the picture selected inserts before it', async ({ page }) => {
        await insertPng(page, RED);
        const [img] = await waitForImages(page, 1);
        expect(img!.at).toBe(0);
        await clickPicture(page, img!.rect);
        await page.keyboard.type('x');
        await settle(page);
        const [after] = await waitForImages(page, 1);
        /* 'x' (1 byte) went in front of the picture's sentinel. */
        expect(after!.at).toBe(1);
    });

    test('after an undone insert, a DIFFERENT picture under the re-minted key paints its own pixels', async ({
        page,
    }) => {
        await insertPng(page, RED);
        const [first] = await waitForImages(page, 1);
        await expectPainted(page, first!.rect, RED);
        await page.keyboard.press('Control+z');
        await waitForImages(page, 0);
        await insertPng(page, GREEN);
        const [second] = await waitForImages(page, 1);
        /* The engine mints `nge_img_1` again; the cache still holds the red
           bitmap under that key. */
        expect(second!.media_key).toBe(first!.media_key);
        await expectPainted(page, second!.rect, GREEN);
    });
});
