import { test, expect, type Locator, type Page } from '@playwright/test';
import { boot, settle } from './helpers/editor';

/* Issue #489 — formatting a WHOLE paragraph reaches its paragraph mark.
 *
 * Word's pilcrow belongs to a selection that runs past a paragraph's last
 * character, and to Select All (Ctrl+A) and a triple-click paragraph
 * selection; character formatting then applies to the mark too. The mark's
 * face sizes the paragraph's LAST line (CLAUDE.md "Line pitch has ONE
 * model"), so before the fix "Ctrl+A -> 8 pt" shrank the text and left
 * every (last) line at the old pitch. A selection that stops at the last
 * character (Shift+Home on a one-line paragraph) does NOT reach the mark,
 * so that line keeps the mark's pitch.
 *
 * Everything under test goes through the REAL shell: keys on the hidden
 * textarea (Ctrl+A, End, Enter, Shift+Home, typing), the toolbar's font
 * size box, a real triple-click on the page canvas. The line pitch is read
 * off the selection overlay — each `.selection-rect` is one laid-out line
 * box, so its height is the line's pitch (the canvas itself is never
 * screenshotted: headless Chrome does not composite it, CLAUDE.md).
 * `__dispatch` only READS the size of the typed text back. */

test.use({ viewport: { width: 1280, height: 1000 } });

declare global {
    interface Window {
        __dispatch: (cmd: unknown) => Promise<any>;
    }
}

const SMALL = 8;

const input = (page: Page): Locator => page.locator('textarea[data-nge-hidden-input]');
const sizeBox = (page: Page): Locator => page.getByLabel('Font size (pt)');

/** Replace the seeded document with two typed paragraphs. */
async function typeTwoParagraphs(page: Page): Promise<void> {
    await input(page).focus();
    await page.keyboard.press('ControlOrMeta+a');
    await page.keyboard.type('first paragraph');
    await page.keyboard.press('Enter');
    await page.keyboard.type('second paragraph');
    await settle(page);
}

/** The heights (CSS px) of the selection overlay's line boxes, top to
 *  bottom, once `n` of them are shown. */
async function lineBoxes(page: Page, n: number): Promise<number[]> {
    const rects = page.locator('.editor-page[data-page-index="0"] .selection-rect');
    await expect(rects).toHaveCount(n);
    return rects.evaluateAll((els) =>
        els
            .map((el) => el.getBoundingClientRect())
            .sort((a, b) => a.top - b.top)
            .map((r) => r.height),
    );
}

/** Ctrl+A, then the line boxes of the `n` lines it selects. */
async function selectAllLines(page: Page, n: number): Promise<number[]> {
    await input(page).focus();
    await page.keyboard.press('ControlOrMeta+a');
    await settle(page);
    return lineBoxes(page, n);
}

/** Set the size through the toolbar box the way a user does: click it,
 *  replace its contents, Enter (which commits through the box's own change
 *  handler and hands the focus back to the document). The contents go in
 *  with `fill` — headless Chrome drops synthesized key text into this
 *  `<input type="number" list>` (beforeinput fires, input never does). */
async function setSizeFromToolbar(page: Page, pt: number): Promise<void> {
    const box = sizeBox(page);
    await box.click();
    await box.fill(String(pt));
    await box.press('Enter');
    await expect(input(page), 'Enter hands the focus back to the document').toBeFocused();
    await settle(page);
}

/** The size the engine reports for the text of body paragraph `idx` (read
 *  back through a selection over it, not under test). */
async function textSize(page: Page, idx: number, len: number): Promise<number> {
    return page.evaluate(
        async ({ idx, len }) => {
            const at = (offset: number) => ({ path: { steps: [{ kind: 'BLOCK', idx }] }, offset });
            const evt = await window.__dispatch({
                type: 'SET_SELECTION',
                range: { start: at(0), end: at(len) },
                caret: at(len),
            });
            if (evt.type !== 'SELECTION_CHANGED') throw new Error(evt.type);
            return evt.attrs_at_caret.font_size as number;
        },
        { idx, len },
    );
}

function expectNear(got: number, want: number, what: string): void {
    expect(Math.abs(got - want), `${what}: ${got} vs ${want}`).toBeLessThan(want * 0.04);
}

test('Ctrl+A -> 8 pt shrinks every line (the marks are selected); End, Enter and typing continue at 8 pt', async ({
    page,
}) => {
    await boot(page);
    await typeTwoParagraphs(page);
    const before = await selectAllLines(page, 2);
    const original = Number(await sizeBox(page).inputValue());
    expect(original, 'the toolbar shows the typed size').toBeGreaterThan(SMALL);

    await setSizeFromToolbar(page, SMALL);
    /* Both paragraphs are one line, so each line is its paragraph's LAST
       one: only a mark at 8 pt lets it shrink with the text. */
    const ratio = SMALL / original;
    await expect
        .poll(async () => (await lineBoxes(page, 2)).map((h, i) => h / before[i]!))
        .toEqual([expect.closeTo(ratio, 2), expect.closeTo(ratio, 2)]);

    /* End, Enter, type: the new paragraph (and its mark) are 8 pt too. */
    await page.keyboard.press('End');
    await page.keyboard.press('Enter');
    await page.keyboard.type('third');
    await settle(page);
    await expect(sizeBox(page), 'the toolbar reads the typed text at 8').toHaveValue(String(SMALL));
    expect(await textSize(page, 2, 'third'.length)).toBe(SMALL);
    const after = await selectAllLines(page, 3);
    for (const [i, h] of after.entries()) expectNear(h, before[0]! * ratio, `line ${i}`);
});

test('a selection that stops at the last character leaves the mark; a triple-click selects it', async ({
    page,
}) => {
    await boot(page);
    await typeTwoParagraphs(page);
    const before = await selectAllLines(page, 2);
    const original = Number(await sizeBox(page).inputValue());

    /* Ctrl+End, Shift+Home: the second paragraph's text, up to its last
       character — not its mark. The text is 8 pt, the line keeps the
       mark's pitch. */
    await input(page).focus();
    await page.keyboard.press('ControlOrMeta+End');
    await page.keyboard.press('Shift+Home');
    await settle(page);
    await setSizeFromToolbar(page, SMALL);
    expect(await textSize(page, 1, 'second paragraph'.length)).toBe(SMALL);
    const kept = await selectAllLines(page, 2);
    expectNear(kept[0]!, before[0]!, 'first line (untouched)');
    expectNear(kept[1]!, before[1]!, 'second line (its mark was not selected)');

    /* A real triple-click on the FIRST line selects that paragraph WITH its
       mark: 8 pt shrinks the line. */
    const first = await page
        .locator('.editor-page[data-page-index="0"] .selection-rect')
        .evaluateAll((els) => {
            const r = els.map((el) => el.getBoundingClientRect()).sort((a, b) => a.top - b.top)[0]!;
            return { x: r.left + 10, y: r.top + r.height / 2 };
        });
    await page.mouse.click(first.x, first.y, { clickCount: 3 });
    await settle(page);
    await expect(sizeBox(page), 'the triple-click selected the 24 pt paragraph').toHaveValue(
        String(original),
    );
    await setSizeFromToolbar(page, SMALL);
    expect(await textSize(page, 0, 'first paragraph'.length)).toBe(SMALL);
    const shrunk = await selectAllLines(page, 2);
    expectNear(shrunk[0]!, before[0]! * (SMALL / original), 'first line (its mark selected)');
    expectNear(shrunk[1]!, before[1]!, 'second line (still its unselected mark)');
});
