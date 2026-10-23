import { test, expect, type Page } from '@playwright/test';
import { bandBox, box, pageCard } from './helpers/bands';
import { boot, scrollViewportTo, settle, viewportScroll } from './helpers/editor';

/* Issue #557 (cursor matrix #555 row 38, clean-room memo §11) — the caret
 * is drawn from the caret run's FONT, not the line box: its top is the
 * line's baseline minus that font's ascent, its height ascent + descent
 * (an empty paragraph: the paragraph mark's font). It is drawn in the text
 * colour at the caret, every caret move restarts its blink, and the view
 * scrolls to it only when it MOVED — never on a story exit alone.
 *
 * Before the fix the overlay spanned the whole line box: 56.25 CSS px on
 * the seed line (whose Arabic run sets the line in Amiri's tall metrics)
 * versus 36.8 px in an empty paragraph and after typing `ab` there — and
 * even a Latin-only line put Liberation Sans' line gap into the caret.
 *
 * Every action under test is a real mouse click or key press (or a toolbar
 * button); `__dispatch` only opens fixture documents and finds where a
 * paragraph sits (setup). The `.caret` overlay is a DOM element, valid
 * under headless Chrome. */

interface CaretBox {
    top: number;
    left: number;
    height: number;
}

async function caret(page: Page): Promise<CaretBox> {
    await expect(page.locator('.caret')).toHaveCount(1);
    return page.evaluate(() => {
        const r = document.querySelector<HTMLElement>('.caret')!.getBoundingClientRect();
        return { top: r.top, left: r.left, height: r.height };
    });
}

async function open(page: Page, text: string, format: 'plain_text' | 'html'): Promise<void> {
    const type = await page.evaluate(
        async ({ t, f }) => {
            const evt = await (window as any).__dispatch({
                type: 'OPEN_DOCUMENT',
                bytes: new TextEncoder().encode(t),
                format: f,
                name: f === 'html' ? 'c.html' : 'c.txt',
            });
            return evt.type as string;
        },
        { t: text, f: format },
    );
    expect(type).toBe('DOCUMENT_LOADED');
    await settle(page);
}

const top = (idx: number, offset: number) => ({
    path: { steps: [{ kind: 'BLOCK', idx }] },
    offset,
});

/** SETUP: where a caret at the start of top-level paragraph `idx` is drawn
 *  (so the test can click there for real). Leaves the caret there. */
async function caretOf(page: Page, idx: number): Promise<CaretBox> {
    await page.evaluate(
        (p) =>
            (window as any).__dispatch({
                type: 'SET_SELECTION',
                range: { start: p, end: p },
                caret: p,
            }),
        top(idx, 0),
    );
    await settle(page);
    return caret(page);
}

test('an empty paragraph and the text typed into it show one caret height, shorter than the line', async ({
    page,
}) => {
    await boot(page);
    await open(page, 'alpha beta\n\ngamma delta', 'plain_text');
    const emptyAt = await caretOf(page, 1);
    await caretOf(page, 0);

    /* Click into the empty paragraph, read the caret, type, read again. */
    await page.mouse.click(emptyAt.left + 2, emptyAt.top + emptyAt.height / 2);
    await settle(page);
    const empty = await caret(page);
    expect(Math.abs(empty.top - emptyAt.top)).toBeLessThan(1);
    await page.keyboard.type('ab');
    await settle(page);
    const typed = await caret(page);
    expect(typed.left).toBeGreaterThan(empty.left + 5);
    expect(Math.abs(typed.height - empty.height), `${empty.height} vs ${typed.height}`).toBeLessThan(
        0.5,
    );
    expect(Math.abs(typed.top - empty.top)).toBeLessThan(0.5);

    /* The line pitch (one Liberation Sans line, gap included) is the step
       to the next paragraph's caret; the caret leaves the gap out. */
    await page.keyboard.press('ArrowDown');
    await settle(page);
    const next = await caret(page);
    const pitch = next.top - typed.top;
    expect(pitch).toBeGreaterThan(20);
    expect(typed.height, `caret ${typed.height} vs line pitch ${pitch}`).toBeLessThan(pitch - 0.5);
});

test('the caret in a Latin run of a mixed Latin / Arabic line is as tall as in an empty paragraph (56 vs 37)', async ({
    page,
}) => {
    await boot(page);
    /* The seed paragraph: `Hello world` + Arabic, whose Amiri run makes the
       line box much taller than a Latin line. Locate its start (setup),
       then click just inside `Hello` for real. */
    const seed = await caretOf(page, 0);
    await page.mouse.click(seed.left + 4, seed.top + seed.height / 2);
    await settle(page);
    const inLatin = await caret(page);

    await page.keyboard.press('Control+End');
    await page.keyboard.press('Enter');
    await settle(page);
    const empty = await caret(page);
    await page.keyboard.type('ab');
    await settle(page);
    const typed = await caret(page);

    expect(
        Math.abs(inLatin.height - empty.height),
        `Latin run in a mixed line ${inLatin.height} vs empty paragraph ${empty.height}`,
    ).toBeLessThan(0.5);
    expect(Math.abs(typed.height - empty.height)).toBeLessThan(0.5);
});

test('every arrow press restarts the caret blink', async ({ page }) => {
    await boot(page);
    await open(page, 'alpha beta gamma', 'plain_text');
    const at = await caretOf(page, 0);
    await page.mouse.click(at.left + 2, at.top + at.height / 2);
    await settle(page);
    /* Let the blink run well into its cycle. */
    await page.waitForTimeout(700);
    const before = await page.evaluate(() => {
        const a = document.querySelector('.caret')!.getAnimations()[0]!;
        return {
            startTime: Number(a.startTime),
            now: Number(document.timeline.currentTime),
        };
    });
    expect(before.now - before.startTime).toBeGreaterThan(600);

    const x0 = (await caret(page)).left;
    await page.keyboard.press('ArrowRight');
    await expect.poll(async () => (await caret(page)).left).toBeGreaterThan(x0 + 1);
    /* The blink restarted at (or after) the key press, not 700 ms earlier. */
    const startTime = await page.evaluate(() =>
        Number(document.querySelector('.caret')!.getAnimations()[0]!.startTime),
    );
    expect(startTime).toBeGreaterThanOrEqual(before.now - 50);
});

test('the caret takes the text colour at the caret', async ({ page }) => {
    await boot(page);
    await open(
        page,
        '<p><span style="color:#c00000">red words here</span></p><p>plain words</p>',
        'html',
    );
    const red = await caretOf(page, 0);
    await caretOf(page, 1);
    await page.mouse.click(red.left + 20, red.top + red.height / 2);
    await settle(page);
    await expect
        .poll(() =>
            page.evaluate(
                () => getComputedStyle(document.querySelector('.caret')!).backgroundColor,
            ),
        )
        .toBe('rgb(192, 0, 0)');
});

/** Scroll so page `idx`'s bottom edge sits inside the viewport, then
 *  double-click its footer zone (below the body, inside the margin). */
async function dblclickFooterOfPage(page: Page, idx: number): Promise<number> {
    const card = await pageCard(page, idx);
    const v = (await box(page, '.editor-viewport'))!;
    const scroll = await viewportScroll(page);
    const target = Math.min(
        scroll.scrollHeight - scroll.clientHeight,
        scroll.scrollTop + card.bottom - v.bottom + 60,
    );
    await scrollViewportTo(page, target);
    const scrolled = (await viewportScroll(page)).scrollTop;
    const at = await pageCard(page, idx);
    await page.mouse.dblclick((at.left + at.right) / 2, at.bottom - 30);
    return scrolled;
}

test('Escape out of a footer entered at a scrolled position keeps the view (no jump to the page top)', async ({
    page,
}) => {
    await boot(page);
    const lines = Array.from({ length: 60 }, (_, i) => `Line ${i + 1} of the long document`);
    await open(page, lines.join('\n'), 'plain_text');
    /* The body caret sits at the document start, far above the view. */
    await caretOf(page, 0);

    const scrolled = await dblclickFooterOfPage(page, 0);
    expect(scrolled).toBeGreaterThan(400);
    await bandBox(page, 0, true);
    await page.keyboard.type('F');
    await settle(page);
    const inFooter = (await viewportScroll(page)).scrollTop;
    expect(Math.abs(inFooter - scrolled)).toBeLessThan(40);

    await page.keyboard.press('Escape');
    await expect(page.locator('.story-band')).toHaveCount(0);
    await settle(page);
    /* Two frames and the reveal's timer fallback: any pending reveal ran. */
    await page.evaluate(
        () =>
            new Promise<void>((resolve) =>
                requestAnimationFrame(() => requestAnimationFrame(() => setTimeout(resolve, 250))),
            ),
    );
    const after = (await viewportScroll(page)).scrollTop;
    expect(Math.abs(after - inFooter), `scrollTop ${inFooter} -> ${after}`).toBeLessThan(40);
});

test('a key that leaves the caret where it was does not scroll back to it', async ({ page }) => {
    await boot(page);
    const lines = Array.from({ length: 60 }, (_, i) => `Line ${i + 1} of the long document`);
    await open(page, lines.join('\n'), 'plain_text');
    await caretOf(page, 0);
    /* Ctrl+End moves the caret (and the view) to the document end. */
    await page.keyboard.press('Control+End');
    await settle(page);
    await expect.poll(async () => (await viewportScroll(page)).scrollTop).toBeGreaterThan(400);
    /* The user scrolls back to the top, then presses End: the caret is
       already at the end of its line, so nothing moves and the view stays. */
    await scrollViewportTo(page, 0);
    await page.keyboard.press('End');
    await settle(page);
    await page.evaluate(
        () =>
            new Promise<void>((resolve) =>
                requestAnimationFrame(() => requestAnimationFrame(() => setTimeout(resolve, 250))),
            ),
    );
    expect((await viewportScroll(page)).scrollTop).toBeLessThan(5);
    /* A key that DOES move the caret still reveals it. */
    await page.keyboard.press('ArrowLeft');
    await settle(page);
    await expect.poll(async () => (await viewportScroll(page)).scrollTop).toBeGreaterThan(400);
});
