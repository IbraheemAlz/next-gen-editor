import { test, expect, type Page } from '@playwright/test';
import { boot, burst, scrollViewportTo, settle, viewportScroll } from './helpers/editor';

/* Issue #486 - the caret reveal (#387) is the CONSEQUENCE of a keyboard /
 * programmatic caret move; it must never undo a scroll the user made after
 * that move. It used to land one animation frame (+ a 120 ms timer) after
 * the selection event and yank the view back to the caret: a wheel flick
 * right after a key press, or a scroll right after a programmatic move
 * (#473's root cause, worked around on the spec side only), snapped back.
 *
 * Why the moves below are NOT pointer clicks: a click is `PlaceCaretAtPoint`,
 * which the engine classifies `reveals_caret: false` (the user clicked where
 * they are looking) - it never queues a reveal, so "click then wheel" passes
 * against any shell. The reveal-bearing moves are keyboard navigation, typing
 * and `SET_SELECTION`.
 *
 * Why `burst` for the first case: a wheel in the same synchronous task as
 * the key press reaches the shell BEFORE the engine's reply, which is the
 * window a real flick falls into when the reply is slow (a big document).
 * The second case scrolls in the microtask right after the reply is
 * received - between the selection event and the next frame, where #473's
 * flake lived. Both fail against the pre-#486 shell. */

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
}

const top = (idx: number, offset: number) => ({
    path: { steps: [{ kind: 'BLOCK', idx }] },
    offset,
});

/** A caret at the start of paragraph `idx` (a revealing programmatic move). */
const caretAt = (idx: number) => ({
    type: 'SET_SELECTION',
    range: { start: top(idx, 0), end: top(idx, 0) },
    caret: top(idx, 0),
});

/** Two frames plus a timer queued after the reveal's 120 ms fallback: by
 *  then any pending reveal pass has run. */
async function afterReveal(page: Page): Promise<void> {
    await page.evaluate(
        () =>
            new Promise<void>((resolve) =>
                requestAnimationFrame(() => requestAnimationFrame(() => setTimeout(resolve, 250))),
            ),
    );
}

async function scrollTop(page: Page): Promise<number> {
    return (await viewportScroll(page)).scrollTop;
}

/** Whether the caret overlay is fully inside the viewport's visible box. */
async function caretVisible(page: Page): Promise<boolean> {
    return page.evaluate(() => {
        const v = document.querySelector<HTMLElement>('.editor-viewport')!.getBoundingClientRect();
        const c = document.querySelector<HTMLElement>('.caret');
        if (!c) return false;
        const r = c.getBoundingClientRect();
        return r.height > 0 && r.top >= v.top && r.bottom <= v.bottom;
    });
}

async function bootLongDoc(page: Page): Promise<void> {
    await boot(page);
    await openLongDoc(page);
    await page.evaluate((c) => (window as any).__dispatch(c), caretAt(0));
    await settle(page);
    await afterReveal(page);
    expect(await scrollTop(page)).toBe(0);
}

test('a wheel flick right after a key press (before the engine replies) is not undone by the reveal', async ({
    page,
}) => {
    test.setTimeout(60_000);
    await bootLongDoc(page);

    /* ArrowDown moves the caret one line (a reveal-bearing reply is on its
       way) and the wheel scrolls 700 px down in the SAME task - the caret is
       then far above the view, which is exactly where the old reveal pulled
       it back to. */
    await burst(page, [{ key: 'ArrowDown' }, { wheel: { dy: 700 } }]);
    await settle(page);
    await afterReveal(page);

    expect(await scrollTop(page)).toBeGreaterThanOrEqual(600);
    expect(await caretVisible(page)).toBe(false);
});

test('a scroll made right after a selection reply (before the next frame) stands', async ({
    page,
}) => {
    test.setTimeout(90_000);
    await bootLongDoc(page);

    /* Far away first (re-applied until it stays: the lazy layout clamps a
       one-shot write, #473). */
    const far = await scrollViewportTo(page, 900);

    /* A programmatic move to paragraph 1 reveals it (the view goes back up);
       the scroll the page makes in the continuation of the awaited reply -
       after the selection event, before the frame - is the later word. */
    const final = await page.evaluate(async (cmd) => {
        const v = document.querySelector<HTMLElement>('.editor-viewport')!;
        await (window as any).__dispatch(cmd);
        v.scrollTop = 900;
        await new Promise<void>((resolve) =>
            requestAnimationFrame(() => requestAnimationFrame(() => setTimeout(resolve, 250))),
        );
        return v.scrollTop;
    }, caretAt(1));

    expect(far.scrollTop).toBeGreaterThanOrEqual(899);
    expect(final).toBeGreaterThanOrEqual(850);
});

test('a wheel flick BEFORE the key press does not suppress the reveal; the keyboard still scrolls', async ({
    page,
}) => {
    test.setTimeout(60_000);
    await bootLongDoc(page);

    /* The user scrolled away, then pressed a key: the key is the later word
       and brings the caret back into view. */
    await burst(page, [{ wheel: { dy: 700 } }, { key: 'ArrowDown' }]);
    await settle(page);
    await afterReveal(page);

    await expect.poll(() => caretVisible(page), { timeout: 10_000 }).toBe(true);
    expect(await scrollTop(page)).toBeLessThan(200);
});
