import { test, expect, type Page } from '@playwright/test';
import { boot, settle } from './helpers/editor';

/* Issue #312 - a zoom keeps what the user is looking at where it is: the
 * caret when it is in view, otherwise the centre of the viewport. Before,
 * every page card resized and the scroll offset stayed put, so the caret
 * (or whatever was in the middle of the window) flew away on most zoom
 * changes.
 *
 * The zoom goes through the REAL zoom control (the footer widget's select:
 * `ZoomControls` -> `cmd.setZoom`, the one place a user zoom is requested);
 * a raw `__dispatch({ type: 'SET_ZOOM' })` is a test hook, not a user
 * gesture, and is deliberately not anchored. Ctrl+wheel is not bound to the
 * editor zoom in this shell (the browser zooms the page), so the select is
 * the gesture under test. Evidence is DOM geometry: the caret overlay's box
 * and the page cards' boxes relative to `.editor-viewport` (headless Chrome
 * never composites the interactive canvas, but the DOM boxes are exactly
 * what the user sees move). */

test.use({ viewport: { width: 1280, height: 900 } });

const top = (idx: number, offset: number) => ({
    path: { steps: [{ kind: 'BLOCK', idx }] },
    offset,
});

async function openLongDoc(page: Page): Promise<void> {
    const lines = Array.from({ length: 120 }, (_, i) => `Line ${i + 1} of the long document`);
    const type = await page.evaluate(async (text) => {
        const evt = await (window as any).__dispatch({
            type: 'OPEN_DOCUMENT',
            bytes: new TextEncoder().encode(text),
            format: 'plain_text',
            name: 'long.txt',
        });
        return evt.type as string;
    }, lines.join('\n'));
    expect(type).toBe('DOCUMENT_LOADED');
    /* A multi-page document: the third page card is mounted. */
    await expect(page.locator('.editor-page[data-page-index="2"]')).toBeAttached();
}

const caretAt = (idx: number) => ({
    type: 'SET_SELECTION',
    range: { start: top(idx, 0), end: top(idx, 0) },
    caret: top(idx, 0),
});

/** Two frames and a timer: the zoom's repaint, the anchor's frame pass and
 *  the scroll-driven viewport / layout traffic it triggers have all run. */
async function afterPaint(page: Page): Promise<void> {
    await page.evaluate(
        () =>
            new Promise<void>((resolve) =>
                requestAnimationFrame(() => requestAnimationFrame(() => setTimeout(resolve, 300))),
            ),
    );
}

interface View {
    zoom: number;
    scrollTop: number;
    clientHeight: number;
    /** The caret overlay's top below the viewport's top edge, or `null`. */
    caretY: number | null;
    /** The page card under the viewport's vertical centre, and the centre's
     *  distance below that card's top at 100 % zoom. */
    centre: { page: number; offset100: number } | null;
}

async function view(page: Page): Promise<View> {
    return page.evaluate(() => {
        const v = document.querySelector<HTMLElement>('.editor-viewport')!;
        const vr = v.getBoundingClientRect();
        const zoom = parseFloat(
            document.querySelector<HTMLSelectElement>('.nge-zoom__select')!.value,
        );
        const caret = document.querySelector<HTMLElement>('.editor-page .caret');
        const mid = vr.top + v.clientTop + v.clientHeight / 2;
        let centre: { page: number; offset100: number } | null = null;
        for (const card of document.querySelectorAll<HTMLElement>('.editor-page[data-page-index]')) {
            const r = card.getBoundingClientRect();
            if (r.top <= mid) {
                centre = {
                    page: Number(card.dataset.pageIndex),
                    offset100: (mid - r.top) / zoom,
                };
            }
        }
        return {
            zoom,
            scrollTop: v.scrollTop,
            clientHeight: v.clientHeight,
            caretY: caret ? caret.getBoundingClientRect().top - vr.top : null,
            centre,
        };
    });
}

/** Pick `zoom` in the footer's zoom control and wait until the new zoom has
 *  painted and settled (the page card has its new size and the anchor's
 *  frame pass has run). */
async function pickZoom(page: Page, zoom: number): Promise<void> {
    await page.locator('.nge-zoom__select').nth(1).selectOption(String(zoom));
    await expect
        .poll(
            () =>
                page.evaluate(() => {
                    const card = document.querySelector<HTMLElement>(
                        '.editor-page[data-page-index="0"]',
                    )!;
                    return card.getBoundingClientRect().height;
                }),
            { message: `page card at ${zoom * 100} %` },
        )
        .toBeGreaterThan(1123 * zoom - 3);
    await expect
        .poll(() =>
            page.evaluate(
                (z) =>
                    Math.abs(
                        document
                            .querySelector<HTMLElement>('.editor-page[data-page-index="0"]')!
                            .getBoundingClientRect().height -
                            1123 * z,
                    ) < 3,
                zoom,
            ),
        )
        .toBe(true);
    await afterPaint(page);
}

/** Scroll `.editor-viewport` so the caret overlay sits `frac` of the way
 *  down it, and wait for the view to be still. */
async function scrollCaretTo(page: Page, frac: number): Promise<void> {
    for (let attempt = 0; attempt < 6; attempt++) {
        const delta = await page.evaluate((f) => {
            const v = document.querySelector<HTMLElement>('.editor-viewport')!;
            const caret = document.querySelector<HTMLElement>('.editor-page .caret');
            if (!caret) return Number.NaN;
            const vr = v.getBoundingClientRect();
            const want = v.clientTop + v.clientHeight * f;
            const have = caret.getBoundingClientRect().top - vr.top;
            v.scrollTop += have - want;
            return have - want;
        }, frac);
        await afterPaint(page);
        if (Math.abs(delta) < 1) return;
    }
}

test('the caret stays where it is through 100 -> 150 -> 75 -> 100 %', async ({ page }) => {
    test.setTimeout(90_000);
    await boot(page);
    await openLongDoc(page);

    /* A caret deep in the document (page 2), then the view moved so the
       caret is mid-viewport. SET_SELECTION is setup; the zoom is the user's. */
    await page.evaluate((c) => (window as any).__dispatch(c), caretAt(70));
    await settle(page);
    await afterPaint(page);
    await scrollCaretTo(page, 0.5);

    /* Every selection event after this point must NOT ask for a reveal:
       a zoom moves no caret. */
    await page.evaluate(() => {
        (window as any).__reveals = 0;
        (window as any).__engineClient.subscribe((e: any) => {
            if (e.type === 'SELECTION_CHANGED' && e.reveal_caret === true) {
                (window as any).__reveals += 1;
            }
        });
    });

    let prev = await view(page);
    expect(prev.zoom).toBe(1);
    expect(prev.caretY).not.toBeNull();
    const startY = prev.caretY!;
    expect(startY).toBeGreaterThan(prev.clientHeight * 0.3);
    expect(startY).toBeLessThan(prev.clientHeight * 0.7);
    expect(prev.scrollTop).toBeGreaterThan(1000);

    for (const zoom of [1.5, 0.75, 1]) {
        await pickZoom(page, zoom);
        const now = await view(page);
        expect(now.zoom).toBe(zoom);
        expect(now.caretY, `caret overlay at ${zoom * 100} %`).not.toBeNull();
        /* The caret's viewport y moved by less than 2 px. */
        expect(Math.abs(now.caretY! - prev.caretY!), `caret y ${prev.caretY} -> ${now.caretY} at ${zoom * 100} %`).toBeLessThan(2);
        /* ...and not because nothing scrolled: the document did resize. */
        expect(Math.abs(now.scrollTop - prev.scrollTop)).toBeGreaterThan(100);
        prev = now;
    }
    /* The whole round trip stays put too. */
    expect(Math.abs(prev.caretY! - startY)).toBeLessThan(3);
    /* No caret reveal fired for any of them. */
    expect(await page.evaluate(() => (window as any).__reveals)).toBe(0);
});

test('with the caret out of view the viewport centre stays put', async ({ page }) => {
    test.setTimeout(90_000);
    await boot(page);
    await openLongDoc(page);

    /* The caret at the very top; the user scrolled down to page 2. */
    await page.evaluate((c) => (window as any).__dispatch(c), caretAt(2));
    await settle(page);
    await afterPaint(page);
    await page.evaluate(() => {
        const v = document.querySelector<HTMLElement>('.editor-viewport')!;
        const card = document.querySelector<HTMLElement>('.editor-page[data-page-index="1"]')!;
        const vr = v.getBoundingClientRect();
        v.scrollTop += card.getBoundingClientRect().top - vr.top + 300;
    });
    await afterPaint(page);

    let prev = await view(page);
    expect(prev.scrollTop).toBeGreaterThan(800);
    /* The caret overlay is not inside the viewport. */
    expect(
        prev.caretY === null || prev.caretY < 0 || prev.caretY > prev.clientHeight,
        `caret y ${prev.caretY}`,
    ).toBe(true);
    expect(prev.centre).not.toBeNull();

    for (const zoom of [1.5, 0.75]) {
        await pickZoom(page, zoom);
        const now = await view(page);
        expect(now.zoom).toBe(zoom);
        /* The same point of the same page is under the viewport's centre
           (within 2 px at the new zoom). */
        expect(now.centre?.page).toBe(prev.centre?.page);
        expect(
            Math.abs(now.centre!.offset100 - prev.centre!.offset100) * zoom,
            `centre of page ${prev.centre?.page}: ${prev.centre?.offset100} -> ${now.centre?.offset100} at ${zoom * 100} %`,
        ).toBeLessThan(2);
        prev = now;
    }
});

test('the zoom button and Fit width anchor too (the same facade call)', async ({ page }) => {
    test.setTimeout(90_000);
    await boot(page);
    await openLongDoc(page);
    await page.evaluate((c) => (window as any).__dispatch(c), caretAt(70));
    await settle(page);
    await afterPaint(page);
    await scrollCaretTo(page, 0.4);

    const before = await view(page);
    await page.locator('.nge-zoom').nth(1).getByRole('button', { name: 'Zoom in' }).click();
    await expect.poll(async () => (await view(page)).zoom).toBeGreaterThan(1.05);
    await afterPaint(page);
    const stepped = await view(page);
    expect(Math.abs(stepped.caretY! - before.caretY!)).toBeLessThan(2);

    /* Fit width: a different source of the same facade call. */
    await page.locator('.nge-zoom').nth(1).getByRole('button', { name: 'Fit page width' }).click();
    await expect.poll(async () => (await view(page)).zoom).not.toBe(stepped.zoom);
    await afterPaint(page);
    const fit = await view(page);
    expect(Math.abs(fit.caretY! - stepped.caretY!)).toBeLessThan(2);
});
