import { test, expect } from '@playwright/test';
import { boot } from './helpers/editor';
import { GREEN, RED, expectPainted, pixels, waitForImages } from './helpers/images';
import { pictureDocx } from './helpers/picture-docx';

/* Issue #574 — the pictures of an OPENED document paint as their bitmaps
 * without any further input. The worker decodes a new document's pictures
 * after the open's reply, but used to skip the repaint for a new document
 * ("its page canvases are re-registered next and paint then"): they are
 * only when the page count changes, so a one-page document opened over the
 * one-page boot document kept the grey placeholder (221, 221, 221) until
 * the next edit. The `.docx` goes through the REAL File > Open input; the
 * evidence is the worker-side pixel read-back (`probePagePixels`) - headless
 * Chrome never composites the interactive canvas (CLAUDE.md). Nothing after
 * the open edits the document: `GET_IMAGE_RECTS` and the probe are queries.
 *
 * The document: a body picture (green, `pictureDocx`) and a header picture
 * (red) — both 1 in x 1/2 in at the 1 in left margin, the header's on the
 * 1/2 in header distance. */

test.use({ viewport: { width: 1280, height: 900 } });

const PLACEHOLDER = [221, 221, 221];

test('a document opened through File > Open paints its pictures before any edit', async ({ page }) => {
    test.setTimeout(60_000);
    await boot(page);
    /* The boot document is one page, and so is the opened one: no page
       canvas is re-registered by the open. */
    await expect(page.locator('.editor-page')).toHaveCount(1);

    await page.locator('.nge-fm__trigger').click();
    await page.locator('.nge-fm input[type="file"]').setInputFiles({
        name: 'pictures.docx',
        mimeType: 'application/vnd.openxmlformats-officedocument.wordprocessingml.document',
        buffer: Buffer.from(pictureDocx()),
    });
    await expect(page.locator('.a11y-mirror > p').first()).toHaveText('Body text above the picture', {
        timeout: 15_000,
    });
    await expect(page.locator('.editor-page')).toHaveCount(1);

    /* The body picture: its real colour, not the placeholder. */
    const [body] = await waitForImages(page, 1);
    await expectPainted(page, body!.rect, GREEN);

    /* The header picture (not listed while the body is edited, #573): the
       same size as the body one, at the same left margin, its top on the
       header distance (1/2 in = 36 pt; device px per pt from the body
       picture's 72 pt width). */
    const scale = body!.rect.w / 72;
    const header = { x: body!.rect.x, y: 36 * scale, w: body!.rect.w, h: body!.rect.h };
    await expectPainted(page, header, RED);

    /* And explicitly: no sampled pixel is the placeholder grey. */
    const centre = (r: { x: number; y: number; w: number; h: number }) => ({
        x: r.x + r.w / 2,
        y: r.y + r.h / 2,
    });
    for (const px of await pixels(page, [centre(body!.rect), centre(header)])) {
        expect(px?.slice(0, 3)).not.toEqual(PLACEHOLDER);
    }
});
