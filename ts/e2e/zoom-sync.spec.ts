import { test, expect, type Page } from '@playwright/test';

/* Issue #52 — the footer mounts TWO zoom widgets (the StatusBar's embedded
   `ZoomControls` and the standalone one). Both must mirror the ENGINE's
   zoom (`SELECTION_CHANGED.zoom`), never a per-widget local signal: a
   change through either — mouse, keyboard, the +/- buttons, or a raw
   `SET_ZOOM` from outside the UI — shows up in both. */

const zoomValues = (page: Page): Promise<string[]> =>
    page.$$eval('.nge-zoom__select', (els) =>
        els.map((el) => (el as HTMLSelectElement).value),
    );

async function expectBoth(page: Page, value: string): Promise<void> {
    await expect.poll(() => zoomValues(page)).toEqual([value, value]);
}

test('both zoom selects show the engine zoom, whichever control changed it', async ({
    page,
}) => {
    await page.goto('/');
    await page.waitForFunction(() => (window as any).__paintIdle === true, undefined, {
        timeout: 15_000,
    });
    const selects = page.locator('.nge-zoom__select');
    await expect(selects).toHaveCount(2);
    await expectBoth(page, '1');

    /* Mouse-style pick on the SECOND widget → the first follows. */
    await selects.nth(1).selectOption('1.25');
    await expectBoth(page, '1.25');

    /* Keyboard on the FIRST widget (focus + ArrowDown = next preset). */
    await selects.nth(0).focus();
    await page.keyboard.press('ArrowDown');
    await expectBoth(page, '1.5');

    /* The step buttons land off-preset; both display the same value.
       (The D2.5 EngineStats debug strip overlays the footer's right edge.) */
    await page.evaluate(() => document.getElementById('stats')?.remove());
    await page.locator('.nge-zoom').nth(1).getByRole('button', { name: 'Zoom in' }).click();
    await expectBoth(page, '1.6');
    await expect(selects.nth(0).locator('option:checked')).toHaveText('160%');

    /* A zoom the UI never issued (raw dispatch) still re-syncs both —
       the engine, not the widget, is the source of truth. */
    await page.evaluate(() => (window as any).__dispatch({ type: 'SET_ZOOM', scale: 0.75 }));
    await expectBoth(page, '0.75');

    /* Out-of-range requests show the engine's CLAMPED value. */
    await page.evaluate(() => (window as any).__dispatch({ type: 'SET_ZOOM', scale: 9 }));
    await expectBoth(page, '4');
});

/* Issue #231 — `SET_ZOOM` / `SET_DEVICE_SCALE` repaint (a fresh
   `page_tops` / `page_heights` / `document_height`) without mutating
   the document, so the worker's `document_mutation_seq` gate never fired
   a `PAINTED` broadcast for them: the overlays + page geometry stayed at
   their PRE-zoom values until the user typed a character. Both checks
   below run with ZERO edits in between the `SET_ZOOM` and the assertion —
   that's the bug: everything must already be correct. */
test('zoom rescales page geometry and the caret overlay before any edit', async ({ page }) => {
    await page.goto('/');
    await page.waitForFunction(() => (window as any).__paintIdle === true, undefined, {
        timeout: 15_000,
    });

    /* Caret overlay height at 100 % — a real DOM element positioned in
       absolute CSS px (`CaretOverlay.tsx`) — since #280 the page card
       grows with the zoom too (see the #280 DOM-size tests below). */
    const caret = page.locator('.editor-page[data-page-index="0"] .caret');
    const before = await caret.evaluate((el) => el.getBoundingClientRect().height);
    expect(before).toBeGreaterThan(0);

    /* Install a listener BEFORE dispatching so no broadcast is missed,
       then poll its most recent reading — robust against any number of
       intervening `PAINTED`s (e.g. a stray scroll-driven `EXPAND_LAYOUT`)
       since we only care that the LATEST one already reflects 150 %. */
    await page.evaluate(() => {
        (window as any).__lastPageHeight = undefined;
        (window as any).__engineClient.subscribe((evt: any) => {
            if (evt.type === 'PAINTED' && evt.page_heights.length > 0) {
                (window as any).__lastPageHeight = evt.page_heights[0];
            }
        });
    });
    const dpr = await page.evaluate(() => window.devicePixelRatio || 1);
    /* `PAGE_H_PT` / `SCREEN_DPI_SCALE` mirror `ts/src/state/engine-store.ts` —
       A4 height in layout pt × the print-to-screen DPI factor. */
    const PAGE_H_PT = 841.9;
    const SCREEN_DPI_SCALE = 4 / 3;
    const expectedPageHeightAt150 = PAGE_H_PT * dpr * SCREEN_DPI_SCALE * 1.5;

    await page.evaluate(() => (window as any).__dispatch({ type: 'SET_ZOOM', scale: 1.5 }));

    /* Page geometry: the broadcast PAINTED already carries the 150 %
       height — no edit dispatched anywhere above. */
    await expect
        .poll(() => page.evaluate(() => (window as any).__lastPageHeight))
        .toBeGreaterThan(expectedPageHeightAt150 * 0.99);

    /* Overlay rect: the caret grew proportionally with the new scale. */
    await expect
        .poll(() => caret.evaluate((el) => el.getBoundingClientRect().height))
        .toBeGreaterThan(before * 1.49);
});

/* ——— Issue #280 — zoom VISIBLY resizes the page ———
   The page card used to be a fixed 794 × 1123 CSS box; `SET_ZOOM` only
   densified the backing store inside it. Every card is now sized from the
   engine's reported page geometry, so the assertions below are on DOM
   sizes (headless Chrome never composites the transferred canvas — a
   screenshot would be blank; the DOM box is the truth the user sees). */

interface Box {
    x: number;
    y: number;
    w: number;
    h: number;
}

const boxOf = (page: Page, selector: string): Promise<Box | null> =>
    page.evaluate((sel) => {
        const el = document.querySelector(sel);
        if (!el) return null;
        const r = el.getBoundingClientRect();
        return { x: r.left, y: r.top, w: r.width, h: r.height };
    }, selector);

const PAGE0 = '.editor-page[data-page-index="0"]';

async function boot(page: Page): Promise<void> {
    await page.goto('/');
    await page.waitForFunction(() => (window as any).__paintIdle === true, undefined, {
        timeout: 15_000,
    });
}

async function setZoom(page: Page, z: number): Promise<void> {
    await page.evaluate((scale) => (window as any).__dispatch({ type: 'SET_ZOOM', scale }), z);
    await expectBoth(page, String(z));
}

/** Poll until page 0's card is `factor ×` the 100 % box (±1.5 CSS px —
 *  the card is the ceil'd device bitmap ÷ the device-px ratio). */
async function expectCardScaled(page: Page, base: Box, factor: number): Promise<Box> {
    await expect
        .poll(
            async () => {
                const b = await boxOf(page, PAGE0);
                return (
                    b !== null &&
                    Math.abs(b.w - base.w * factor) <= 1.5 &&
                    Math.abs(b.h - base.h * factor) <= 1.5
                );
            },
            { message: `page card at ${factor * 100} %` },
        )
        .toBe(true);
    return (await boxOf(page, PAGE0))!;
}

test('zoom resizes the page card, the canvas, the scroll range and the overlays (DOM size)', async ({
    page,
}) => {
    await boot(page);
    /* Select the seed text so the selection overlay has rects to measure;
       the caret sits at the selection's end. */
    await page.evaluate(() => (window as any).__dispatch({ type: 'SELECT_ALL' }));
    await expect(page.locator(`${PAGE0} .selection-rect`).first()).toBeAttached();

    const measure = async () => {
        const card = (await boxOf(page, PAGE0))!;
        const canvas = (await boxOf(page, `${PAGE0} canvas`))!;
        const caret = (await boxOf(page, `${PAGE0} .caret`))!;
        const sel = (await boxOf(page, `${PAGE0} .selection-rect`))!;
        const scrollH = await page.evaluate(
            () => document.querySelector('.editor-viewport')!.scrollHeight,
        );
        const strip = await boxOf(page, '.nge-ruler__strip');
        return {
            card,
            canvas,
            /* Overlays in PAGE-LOCAL CSS px — the space that must scale. */
            caret: { x: caret.x - card.x, y: caret.y - card.y, w: caret.w, h: caret.h },
            sel: { x: sel.x - card.x, y: sel.y - card.y, w: sel.w, h: sel.h },
            scrollH,
            strip,
        };
    };

    const base = await measure();
    /* 100 %: A4 at 96 DPI. */
    expect(Math.abs(base.card.w - 794)).toBeLessThanOrEqual(1);
    expect(Math.abs(base.card.h - 1123)).toBeLessThanOrEqual(1);

    for (const factor of [1.5, 0.75]) {
        await setZoom(page, factor);
        const card = await expectCardScaled(page, base.card, factor);
        /* The overlays follow the page geometry — poll: the SELECTION_CHANGED
           reply and the PAINTED broadcast land a task apart. */
        await expect
            .poll(
                async () => {
                    const m = await measure();
                    const near = (a: number, b: number, tol: number) => Math.abs(a - b) <= tol;
                    return {
                        canvasFillsCard:
                            near(m.canvas.w, card.w, 0.5) && near(m.canvas.h, card.h, 0.5),
                        caretX: near(m.caret.x, base.caret.x * factor, 2),
                        caretY: near(m.caret.y, base.caret.y * factor, 2),
                        caretH: near(m.caret.h, base.caret.h * factor, 1),
                        selX: near(m.sel.x, base.sel.x * factor, 2),
                        selY: near(m.sel.y, base.sel.y * factor, 2),
                        selW: near(m.sel.w, base.sel.w * factor, 2),
                        selH: near(m.sel.h, base.sel.h * factor, 1),
                        /* The ruler strip spans exactly the zoomed page,
                           pinned to its left edge. */
                        ruler:
                            m.strip === null ||
                            (near(m.strip.w, card.w, 1.5) && near(m.strip.x, card.x, 1.5)),
                    };
                },
                { message: `overlays at ${factor * 100} %` },
            )
            .toEqual({
                canvasFillsCard: true,
                caretX: true,
                caretY: true,
                caretH: true,
                selX: true,
                selY: true,
                selW: true,
                selH: true,
                ruler: true,
            });
        /* The scroll container grows / shrinks with the page. */
        const scrollH = (await measure()).scrollH;
        if (factor > 1) expect(scrollH).toBeGreaterThan(base.scrollH * 1.3);
        else expect(scrollH).toBeLessThanOrEqual(base.scrollH);
    }
});

test('every page card and the inter-page gap scale with zoom (multi-page)', async ({ page }) => {
    await boot(page);
    const lines = Array.from(
        { length: 60 },
        (_, i) => `Line ${i + 1}: the quick brown fox jumps over the lazy dog.`,
    ).join('\n');
    await page.evaluate((text) => (window as any).__dispatch({ type: 'PASTE_PLAIN', text }), lines);
    await expect
        .poll(() => page.locator('.editor-page').count(), { timeout: 10_000 })
        .toBeGreaterThanOrEqual(2);

    const pages = () =>
        page.$$eval('.editor-page', (els) =>
            els.slice(0, 2).map((el) => {
                const r = el.getBoundingClientRect();
                return { top: r.top, bottom: r.bottom, w: r.width, h: r.height };
            }),
        );
    const [p0, p1] = await pages();
    expect(Math.abs(p1!.top - p0!.bottom - 64)).toBeLessThanOrEqual(1);

    await setZoom(page, 1.5);
    await expect
        .poll(async () => {
            const [a, b] = await pages();
            return (
                Math.abs(b!.w - p1!.w * 1.5) <= 1.5 &&
                Math.abs(b!.h - p1!.h * 1.5) <= 1.5 &&
                Math.abs(b!.top - a!.bottom - 96) <= 1
            );
        })
        .toBe(true);

    /* Past the backing-store cap (4 device px per pt): 400 % on a DPR-1
       display paints at the capped density, but the CARD is still exactly
       4 × — the shell mirrors the engine's cap when converting geometry. */
    await setZoom(page, 4);
    await expect
        .poll(async () => {
            const [a] = await pages();
            return Math.abs(a!.w - p0!.w * 4) <= 2 && Math.abs(a!.h - p0!.h * 4) <= 2;
        })
        .toBe(true);
});

test('crash recovery at 150 % keeps the zoomed page size on fresh canvases', async ({ page }) => {
    test.setTimeout(60_000);
    await boot(page);
    const base = (await boxOf(page, PAGE0))!;
    await setZoom(page, 1.5);
    const zoomed = await expectCardScaled(page, base, 1.5);
    await page.evaluate(() => {
        (window as any).__preCanvas = document.querySelector('.editor-page canvas');
    });

    await page.evaluate(async () => {
        const client = (window as any).__engineClient;
        await client.armTrap(1);
        await (window as any).__dispatch({ type: 'PING' }).catch(() => undefined);
        for (let i = 0; i < 600 && (window as any).__recovered !== true; i++) {
            await new Promise((r) => setTimeout(r, 50));
        }
    });
    expect(await page.evaluate(() => (window as any).__recovered)).toBe(true);
    await expectBoth(page, '1.5');

    /* A fresh <canvas> (never re-transferred), filling a card that kept
       its 150 % size. */
    expect(
        await page.evaluate(
            () => document.querySelector('.editor-page canvas') !== (window as any).__preCanvas,
        ),
    ).toBe(true);
    await expect
        .poll(async () => {
            const card = (await boxOf(page, PAGE0))!;
            const canvas = (await boxOf(page, `${PAGE0} canvas`))!;
            return (
                Math.abs(card.w - zoomed.w) <= 0.5 &&
                Math.abs(card.h - zoomed.h) <= 0.5 &&
                Math.abs(canvas.w - card.w) <= 0.5 &&
                Math.abs(canvas.h - card.h) <= 0.5
            );
        })
        .toBe(true);
});

test('Fit width fills the viewport, follows resizes, and yields to any other zoom', async ({
    page,
}) => {
    await page.setViewportSize({ width: 1400, height: 900 });
    await boot(page);
    /* The D2.5 EngineStats debug strip overlays the footer's right edge. */
    await page.evaluate(() => document.getElementById('stats')?.remove());
    const fit = page.locator('.nge-zoom').first().getByRole('button', { name: 'Fit page width' });

    const expected = () =>
        page.evaluate(() => {
            const v = document.querySelector<HTMLElement>('.editor-viewport')!;
            const cs = getComputedStyle(v);
            const avail =
                v.clientWidth - parseFloat(cs.paddingLeft) - parseFloat(cs.paddingRight);
            return { avail, zoom: Math.floor((avail / ((595.3 * 96) / 72)) * 100) / 100 };
        });
    /* The page card fills the viewport's content width (within the 1 %
       zoom quantum) without overflowing it, and the controls show the
       computed zoom. */
    const cardFits = async (): Promise<boolean> => {
        const { avail, zoom } = await expected();
        const card = await boxOf(page, PAGE0);
        const shown = Number((await zoomValues(page))[0]);
        return (
            card !== null &&
            /* One 1 % quantum of slack: the widget measures the laid-out
               card (a whole device px wide) rather than the 793.73 px A4. */
            Math.abs(shown - Math.max(0.25, Math.min(4, zoom))) <= 0.0101 &&
            card.w <= avail + 0.5 &&
            card.w >= avail - 0.02 * card.w - 1
        );
    };

    await fit.click();
    await expect(fit).toHaveAttribute('aria-pressed', 'true');
    /* Both widgets share the mode. */
    await expect(
        page.locator('.nge-zoom').nth(1).getByRole('button', { name: 'Fit page width' }),
    ).toHaveAttribute('aria-pressed', 'true');
    await expect.poll(cardFits).toBe(true);

    /* The mode is sticky: a narrower window re-fits. */
    const before = Number((await zoomValues(page))[0]);
    await page.setViewportSize({ width: 1000, height: 900 });
    await expect.poll(async () => Number((await zoomValues(page))[0])).toBeLessThan(before);
    await expect.poll(cardFits).toBe(true);

    /* Any other zoom leaves the mode. */
    await page.locator('.nge-zoom__select').first().selectOption('1');
    await expect(fit).toHaveAttribute('aria-pressed', 'false');
    await page.setViewportSize({ width: 1300, height: 900 });
    await page.waitForTimeout(300);
    expect(await zoomValues(page)).toEqual(['1', '1']);
});

/* Issue #280 — on a HiDPI display the backing-store cap bites earlier
   (DPR 2 × 4/3 × 2 = 5.33 > 4 device px per pt at 200 %): the engine
   paints at the capped density, and the card + overlays must still be
   exactly 2 × — the shell's device-px ratio mirrors the cap. */
test.describe('HiDPI', () => {
    test.use({ deviceScaleFactor: 2 });

    test('the paint-scale cap never changes the visible zoom', async ({ page }) => {
        await boot(page);
        expect(await page.evaluate(() => window.devicePixelRatio)).toBe(2);
        const base = (await boxOf(page, PAGE0))!;
        const caret0 = (await boxOf(page, `${PAGE0} .caret`))!;
        expect(Math.abs(base.w - 794)).toBeLessThanOrEqual(1);
        for (const factor of [1.5, 2]) {
            await setZoom(page, factor);
            await expectCardScaled(page, base, factor);
            await expect
                .poll(async () => (await boxOf(page, `${PAGE0} .caret`))!.h)
                .toBeCloseTo(caret0.h * factor, 0);
        }
    });
});
