import { test, expect, type Page } from '@playwright/test';

/* Issue #276 — typing after a formatted run continues its formatting (the
 * character before the caret; at a paragraph start the one after it), and
 * the toolbar's `attrs_at_caret` reports exactly what the next keystroke
 * produces — before and after typing. Pending (sticky) formatting still
 * overrides the inherited style. */

async function boot(page: Page): Promise<void> {
    await page.goto('/');
    await page.waitForFunction(() => (window as any).__paintIdle === true, undefined, {
        timeout: 30_000,
    });
}

test('typing after a bold run is bold; pending bold-off types plain', async ({ page }) => {
    await boot(page);
    const out = await page.evaluate(async () => {
        const dispatch = (window as any).__dispatch;
        const pos = (offset: number) => ({
            path: { steps: [{ kind: 'BLOCK', idx: 0 }] },
            offset,
        });
        const select = (a: number, b: number) =>
            dispatch({ type: 'SET_SELECTION', range: { start: pos(a), end: pos(b) }, caret: pos(b) });
        const bold = (on: boolean) =>
            dispatch({ type: 'APPLY_FORMATTING', range: undefined, attrs: { bold: on } });
        /* Bold the first three characters, park the caret right after. */
        await select(0, 3);
        await bold(true);
        const before = await select(3, 3);
        await dispatch({ type: 'INSERT_TEXT', at: undefined, text: 'Z' });
        /* The typed char itself, as a one-character range. */
        const typed = await select(3, 4);
        /* Pending bold-off at the end of the (now four-char) bold run. */
        await select(4, 4);
        await bold(false);
        await dispatch({ type: 'INSERT_TEXT', at: undefined, text: 'Q' });
        const plain = await select(4, 5);
        const boldStill = await select(3, 4);
        return {
            before: before.attrs_at_caret.bold as boolean,
            typed: typed.attrs_at_caret.bold as boolean,
            plain: plain.attrs_at_caret.bold as boolean,
            boldStill: boldStill.attrs_at_caret.bold as boolean,
        };
    });
    expect(out.before, 'toolbar previews the inherited bold').toBe(true);
    expect(out.typed, 'typed text continues the bold run').toBe(true);
    expect(out.plain, 'pending bold-off wins').toBe(false);
    expect(out.boldStill).toBe(true);
});

/* Issue #286 — formatting toggles are engine-side (`TOGGLE_FORMATTING`):
 * the shell posts them synchronously with no state read, so a toggle fired
 * before the previous keystroke's SELECTION_CHANGED lands still flips
 * against the engine's live style. Zero waits between keystrokes. */

const pos = (offset: number, idx = 0) => ({
    path: { steps: [{ kind: 'BLOCK', idx }] },
    offset,
});

/** Park a collapsed caret at `offset` in body paragraph 0. */
async function caretAt(page: Page, offset: number): Promise<void> {
    await page.evaluate(async (p) => {
        await (window as any).__dispatch({
            type: 'SET_SELECTION',
            range: { start: p, end: p },
            caret: p,
        });
    }, pos(offset));
}

/** Caret at the start of body paragraph 0, typing focus on the hidden
 *  textarea (the only text-input source). */
async function caretAtStart(page: Page): Promise<void> {
    await caretAt(page, 0);
    await page.locator('textarea[data-nge-hidden-input]').focus();
}

/** Bold state of each [a, b) range in the paragraph at `p0` (`bold` from
 *  the range's first char, `mixed` when the range disagrees), read after
 *  every queued keystroke has run (the worker queue is FIFO). */
async function boldOf(page: Page, ranges: Array<[number, number]>) {
    return page.evaluate(
        async ({ ranges, p0 }) => {
            const dispatch = (window as any).__dispatch;
            const mk = (o: number) => ({ ...p0, offset: o });
            const out: Array<{ bold: boolean; mixed: boolean }> = [];
            let story = false;
            for (const [a, b] of ranges) {
                const evt = await dispatch({
                    type: 'SET_SELECTION',
                    range: { start: mk(a), end: mk(b) },
                    caret: mk(b),
                });
                story = evt.editing_story !== undefined && evt.editing_story !== null;
                out.push({ bold: evt.attrs_at_caret.bold, mixed: evt.attrs_mixed.bold });
            }
            return { runs: out, story };
        },
        { ranges, p0: pos(0) },
    );
}

async function documentText(page: Page): Promise<string> {
    return page.evaluate(async () => {
        const dispatch = (window as any).__dispatch;
        await dispatch({ type: 'SELECT_ALL' });
        const clip = await dispatch({ type: 'GET_SELECTION_AS_CLIPBOARD' });
        return clip.type === 'CLIPBOARD_PAYLOAD' ? (clip.plain as string) : `<${clip.type}>`;
    });
}

/** Fire `steps` as ONE synchronous burst on the main thread: `'B'` is a
 *  Ctrl/Cmd+B keydown on the hidden textarea, `'BTN'` a click on the
 *  toolbar Bold button, anything else an `insertText` beforeinput. No
 *  engine reply can land between two steps (the main thread never
 *  yields), so the shell's mirrored toolbar state is guaranteed stale for
 *  every toggle after the first — the deterministic form of "typing
 *  speed". Playwright's `keyboard.type` round-trips through CDP per key,
 *  which is slow enough for the reply to win and hide the #286 race. */
async function burst(page: Page, steps: string[]): Promise<void> {
    await page.evaluate((steps) => {
        const ta = document.querySelector<HTMLTextAreaElement>('textarea[data-nge-hidden-input]');
        const btn = document.querySelector<HTMLButtonElement>('.nge-tfmt__btn--bold');
        if (!ta || !btn) throw new Error('editor input / bold button missing');
        const mac = /Mac|iPhone|iPad/.test(navigator.platform);
        for (const s of steps) {
            if (s === 'B') {
                ta.dispatchEvent(
                    new KeyboardEvent('keydown', {
                        key: 'b',
                        code: 'KeyB',
                        ctrlKey: !mac,
                        metaKey: mac,
                        bubbles: true,
                        cancelable: true,
                    }),
                );
            } else if (s === 'BTN') {
                btn.click();
            } else {
                ta.dispatchEvent(
                    new InputEvent('beforeinput', {
                        inputType: 'insertText',
                        data: s,
                        bubbles: true,
                        cancelable: true,
                    }),
                );
            }
        }
    }, steps);
}

async function expectBoldThenPlain(page: Page): Promise<void> {
    const out = await boldOf(page, [
        [0, 4],
        [4, 8],
    ]);
    expect(out.runs[0], '"bold" is one bold run').toEqual({ bold: true, mixed: false });
    expect(out.runs[1], '" end" is plain').toEqual({ bold: false, mixed: false });
    const text = await documentText(page);
    expect(text.startsWith('bold end'), JSON.stringify(text.slice(0, 40))).toBe(true);
}

test('Ctrl+B, type, Ctrl+B, type with zero waits gives two runs (#286)', async ({ page }) => {
    await boot(page);
    await caretAtStart(page);
    await burst(page, ['B', 'b', 'o', 'l', 'd', 'B', ' ', 'e', 'n', 'd']);
    await expectBoldThenPlain(page);
});

test('real Ctrl+B keystrokes interleaved with typing give two runs (#286)', async ({ page }) => {
    await boot(page);
    await caretAtStart(page);
    /* The same sequence through the real keyboard pipeline (no waits). */
    await page.keyboard.press('ControlOrMeta+KeyB');
    await page.keyboard.type('bold');
    await page.keyboard.press('ControlOrMeta+KeyB');
    await page.keyboard.type(' end');
    await expectBoldThenPlain(page);
});

test('toolbar Bold button, type, button, type with zero waits gives two runs (#286)', async ({
    page,
}) => {
    await boot(page);
    await caretAtStart(page);
    const btn = page.locator('.nge-tfmt__btn--bold');
    await expect(btn).toBeEnabled();
    await burst(page, ['BTN', 'b', 'o', 'l', 'd', 'BTN', ' ', 'e', 'n', 'd']);
    const out = await boldOf(page, [
        [0, 4],
        [4, 8],
    ]);
    expect(out.runs[0]).toEqual({ bold: true, mixed: false });
    expect(out.runs[1]).toEqual({ bold: false, mixed: false });
    const text = await documentText(page);
    expect(text.startsWith('bold end'), JSON.stringify(text.slice(0, 40))).toBe(true);
    /* The pressed state still mirrors SELECTION_CHANGED. */
    await caretAt(page, 2);
    await expect(btn).toHaveAttribute('aria-pressed', 'true');
    await caretAt(page, 6);
    await expect(btn).toHaveAttribute('aria-pressed', 'false');
});

/* Issue #296 — typing inside a header honours pending (sticky) formatting
 * exactly like the body: toolbar preview and typed text agree. */
test('Ctrl+B then typing in a header produces a bold header run (#296)', async ({ page }) => {
    await boot(page);
    await caretAtStart(page);
    const entered = await page.evaluate(async () =>
        (window as any).__dispatch({ type: 'ENTER_HEADER_FOOTER', page: 0, area: 'Header' }),
    );
    expect(entered.type).toBe('SELECTION_CHANGED');
    await page.locator('textarea[data-nge-hidden-input]').focus();
    await page.keyboard.press('ControlOrMeta+KeyB');
    await page.keyboard.type('Hi');
    await page.keyboard.press('ControlOrMeta+KeyB');
    await page.keyboard.type(' there');
    /* Story selections are story-rooted: block 0 = the header's first
       paragraph (an empty header part, so the typed text starts at 0). */
    const out = await boldOf(page, [
        [0, 2],
        [3, 8],
    ]);
    expect(out.story, 'still editing the header').toBe(true);
    expect(out.runs[0], '"Hi" is bold in the header').toEqual({ bold: true, mixed: false });
    expect(out.runs[1], '" there" is plain').toEqual({ bold: false, mixed: false });
});
