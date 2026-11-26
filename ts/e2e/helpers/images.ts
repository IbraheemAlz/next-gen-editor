import { expect, type Page } from '@playwright/test';
import { settle } from './editor';

/* Shared helpers for the picture specs (issues #506 / #554):
 * `image-insert-keys.spec.ts` and `image-selection-flows.spec.ts`.
 *
 * Evidence is engine truth (`GET_IMAGE_RECTS`, the selection events) plus the
 * worker-side pixel read-back `probePagePixels` — headless Chrome never
 * composites the interactive canvas (CLAUDE.md), so a screenshot would be
 * blank. Pictures enter through the toolbar's REAL file input, clicks go to a
 * page canvas through the real `pointer.ts`. */

/** The decoded-bitmap colours the in-page PNGs are filled with. */
export const RED = '#d02020';
export const GREEN = '#20b040';

export interface Img {
    at: number;
    rect: { x: number; y: number; w: number; h: number };
    media_key: string;
    story_rid?: string;
}

export const dispatch = (page: Page, cmd: unknown): Promise<any> =>
    page.evaluate((c) => (window as any).__dispatch(c), cmd);

export async function images(page: Page): Promise<Img[]> {
    const evt = await dispatch(page, { type: 'GET_IMAGE_RECTS' });
    if (evt.type !== 'IMAGE_RECTS') throw new Error(`GET_IMAGE_RECTS -> ${evt.type}`);
    return evt.images as Img[];
}

/** Feed a PNG filled with `color` through the toolbar's real file input: a
 *  `File` built from a canvas `toBlob`, put on the input with a
 *  `DataTransfer`, then the `change` event `InsertImageButton` listens for. */
export async function insertPng(page: Page, color: string): Promise<void> {
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
export async function waitForImages(page: Page, n: number): Promise<Img[]> {
    await expect.poll(async () => (await images(page)).length, { timeout: 15_000 }).toBe(n);
    return images(page);
}

/** RGBA at page-0-local device px (the worker reads its surface back). */
export async function pixels(
    page: Page,
    points: Array<{ x: number; y: number }>,
): Promise<Array<[number, number, number, number] | null>> {
    return page.evaluate((p) => (window as any).__engineClient.probePagePixels(0, p), points);
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
export async function expectPainted(page: Page, r: Img['rect'], hex: string): Promise<void> {
    await expect
        .poll(async () => (await pixels(page, inside(r))).every((p) => near(p, hex)), {
            message: `the picture's pixels are ${hex}`,
            timeout: 15_000,
        })
        .toBe(true);
}

/** Client coordinates of a page-0 device-px point (the real mouse's space). */
export async function clientPoint(
    page: Page,
    x: number,
    y: number,
): Promise<{ x: number; y: number }> {
    const box = await page.locator('.editor-page[data-page-index="0"] canvas').first().boundingBox();
    if (!box) throw new Error('page 0 canvas has no box');
    const dpr = await page.evaluate(() => window.devicePixelRatio || 1);
    return { x: box.x + x / dpr, y: box.y + y / dpr };
}

/** A real click on the centre of a picture's rect. */
export async function clickRect(page: Page, r: Img['rect']): Promise<void> {
    const p = await clientPoint(page, r.x + r.w / 2, r.y + r.h / 2);
    await page.mouse.click(p.x, p.y);
}

/** The caret overlay's box in page-0 CSS px (`null` when it is not drawn). */
export async function caretBox(page: Page): Promise<{ x: number; y: number; h: number } | null> {
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
