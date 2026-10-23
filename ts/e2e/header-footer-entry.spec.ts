import { test, expect, type Page } from '@playwright/test';
import { bandBox, box, dblclickFooterAtBottom, pageCard, type Box } from './helpers/bands';
import { boot, scrollViewportTo, settle, viewportScroll } from './helpers/editor';

/* Issues #505 / #510 — entering a header / footer through the REAL UI.
 *
 * #505: the toolbar's Header / Footer buttons drew the story band but left
 * the caret overlay on the body's first line (an empty part paints no band,
 * so the engine had no story line and fell back to the body margin), and
 * the Footer button never scrolled the footer into view.
 *
 * #510: with the page scrolled to its bottom, a double-click in the footer
 * band entered nothing. The band gate swallowed the first press, the
 * hidden textarea's pointerup refocus (`focus()` without `preventScroll`)
 * scrolled the view back to the caret at the top of the page, and the
 * second press + `dblclick` landed on the body under the pointer — a word
 * selection and a view at the top, which read as "redirected to the
 * header".
 *
 * Every action under test is a real mouse / keyboard / button input;
 * `__dispatch` only opens a fixture document (setup). Rectangles come
 * from the DOM overlays (`.caret`, `.story-band`), which are valid under
 * headless Chrome (only the canvas pixels are not composited there). */

async function caretBox(page: Page): Promise<Box> {
    const c = await box(page, '.caret');
    expect(c, 'a caret overlay is shown').not.toBeNull();
    return c!;
}

/** The caret relative to page `idx`'s card. Entering / leaving a story
 *  swaps the toolbar's Header & Footer cluster, which can change the
 *  toolbar's height and move every page on screen, so positions compared
 *  across a mode change must be page-local. */
async function caretInPage(page: Page, idx: number): Promise<Box> {
    const c = await caretBox(page);
    const p = (await box(page, `.editor-page[data-page-index="${idx}"]`))!;
    return {
        top: c.top - p.top,
        bottom: c.bottom - p.top,
        left: c.left - p.left,
        right: c.right - p.left,
    };
}

/** The `.story-band` outline is drawn 2 px inside the band region on
 *  every side (`StoryModeOverlay`); a caret on a line that fills the band
 *  touches the region's edge, so compare against the region. */
const BAND_INSET = 2.5;

function expectInside(inner: Box, outer: Box, what: string): void {
    expect(inner.top, `${what}: top`).toBeGreaterThanOrEqual(outer.top - BAND_INSET);
    expect(inner.bottom, `${what}: bottom`).toBeLessThanOrEqual(outer.bottom + BAND_INSET);
    expect(inner.left, `${what}: left`).toBeGreaterThanOrEqual(outer.left - BAND_INSET);
    expect(inner.right, `${what}: right`).toBeLessThanOrEqual(outer.right + BAND_INSET);
}

async function expectCaretInViewport(page: Page): Promise<void> {
    const v = (await box(page, '.editor-viewport'))!;
    const c = await caretBox(page);
    expect(c.top, 'caret below the viewport top').toBeGreaterThanOrEqual(v.top);
    expect(c.bottom, 'caret above the viewport bottom').toBeLessThanOrEqual(v.bottom);
}

/** Replace the document with plain text (setup, not under test). */
async function openText(page: Page, text: string): Promise<void> {
    const type = await page.evaluate(async (t) => {
        const evt = await (window as any).__dispatch({
            type: 'OPEN_DOCUMENT',
            bytes: new TextEncoder().encode(t),
            format: 'plain_text',
            name: 'hf.txt',
        });
        return evt.type as string;
    }, text);
    expect(type).toBe('DOCUMENT_LOADED');
    await settle(page);
}

test.beforeEach(async ({ page }) => {
    await boot(page);
    await openText(page, 'alpha beta gamma delta epsilon zeta eta theta');
});

test('the Footer button puts the caret inside the footer band and scrolls it into view (#505)', async ({
    page,
}) => {
    const before = await caretInPage(page, 0);
    expect((await viewportScroll(page)).scrollTop).toBe(0);
    /* The footer sits below the fold of the 720 px viewport. */
    const card = await pageCard(page, 0);
    const viewport = (await box(page, '.editor-viewport'))!;
    expect(card.bottom).toBeGreaterThan(viewport.bottom);

    await page.getByRole('button', { name: 'Edit footer' }).click();
    const band = await bandBox(page, 0, true);
    const caret = await caretBox(page);
    expectInside(caret, band, 'entry caret vs footer band');
    await expectCaretInViewport(page);

    /* Typing lands in the footer and the caret stays in its band (the
       band bottom-anchors at the footer distance, so its top follows the
       line pitch of what was typed). */
    await page.keyboard.type('FTR');
    await settle(page);
    await expect(page.locator('.a11y-mirror [role="contentinfo"]')).toContainText('FTR');
    await expect(page.locator('.a11y-mirror > p').first()).not.toContainText('FTR');
    expectInside(await caretBox(page), await bandBox(page, 0, true), 'caret after typing');
    await expectCaretInViewport(page);

    /* Esc exits and restores the body caret from before the entry. */
    await page.keyboard.press('Escape');
    await expect(page.locator('.story-band')).toHaveCount(0);
    await scrollViewportTo(page, 0);
    const restored = await caretInPage(page, 0);
    expect(Math.abs(restored.top - before.top)).toBeLessThan(1);
    expect(Math.abs(restored.left - before.left)).toBeLessThan(1);
});

test('the Header button puts the caret inside the header band (#505)', async ({ page }) => {
    await page.getByRole('button', { name: 'Edit header' }).click();
    const band = await bandBox(page, 0, false);
    const caret = await caretInPage(page, 0);
    expectInside(await caretBox(page), band, 'entry caret vs header band');
    await expectCaretInViewport(page);

    /* The header band top-anchors at the header distance: typing keeps
       the caret on the same line top. */
    await page.keyboard.type('HDR');
    await settle(page);
    await expect(page.locator('.a11y-mirror [role="banner"]')).toContainText('HDR');
    const typed = await caretInPage(page, 0);
    expect(Math.abs(typed.top - caret.top)).toBeLessThan(1);
    expectInside(await caretBox(page), await bandBox(page, 0, false), 'caret after typing');
});

test('a double-click in the footer band of a page scrolled to its bottom enters the footer (#510)', async ({
    page,
}) => {
    /* No footer part yet: the double-click creates it, like the button. */
    const { scrollTop } = await dblclickFooterAtBottom(page);
    const band = await bandBox(page, 0, true);
    expectInside(await caretBox(page), band, 'entry caret vs footer band');
    /* The view stayed at the bottom (no jump to the caret at the top);
       the entry's caret reveal may nudge it by its margin at most. */
    expect(Math.abs((await viewportScroll(page)).scrollTop - scrollTop)).toBeLessThan(40);
    await page.keyboard.type('FTR');
    await settle(page);
    await expect(page.locator('.a11y-mirror [role="contentinfo"]')).toContainText('FTR');
    await expect(page.locator('.a11y-mirror > p').first()).toHaveText(/^alpha beta/);

    /* With the footer part present: exit, then double-click it again. */
    await page.keyboard.press('Escape');
    await expect(page.locator('.story-band')).toHaveCount(0);
    const again = await dblclickFooterAtBottom(page);
    const band2 = await bandBox(page, 0, true);
    expectInside(await caretBox(page), band2, 'second entry caret vs footer band');
    expect(Math.abs((await viewportScroll(page)).scrollTop - again.scrollTop)).toBeLessThan(40);
    /* The caret sits at the START of the part (Word): typing prefixes it. */
    await page.keyboard.type('X');
    await settle(page);
    await expect(page.locator('.a11y-mirror [role="contentinfo"]')).toContainText('XFTR');
});

test('a double-click in the header band of a later page enters THAT page (#510)', async ({
    page,
}) => {
    const lines = Array.from({ length: 60 }, (_, i) => `Line ${i + 1} of the long document`);
    await openText(page, lines.join('\n'));
    await expect(page.locator('.editor-page[data-page-index="1"]')).toHaveCount(1);
    /* Bring page 1's top margin into the middle of the viewport. */
    const first = await pageCard(page, 1);
    const v = (await box(page, '.editor-viewport'))!;
    const scroll = await viewportScroll(page);
    await scrollViewportTo(page, scroll.scrollTop + first.top - v.top - 200);
    const card = await pageCard(page, 1);
    await page.mouse.dblclick((card.left + card.right) / 2, card.top + 30);
    const band = await bandBox(page, 1, false);
    await expect(page.locator('.editor-page[data-page-index="0"] .story-band')).toHaveCount(0);
    expectInside(await caretBox(page), band, 'entry caret vs page-1 header band');

    /* A body double-click on page 1 exits and puts the caret THERE, with
       the view kept where it is. */
    const scrolled = (await viewportScroll(page)).scrollTop;
    const card1 = await pageCard(page, 1);
    const localX = 96 + 120;
    const localY = 96 + 60;
    await page.mouse.dblclick(card1.left + localX, card1.top + localY);
    await expect(page.locator('.story-band')).toHaveCount(0);
    await settle(page);
    expect(Math.abs((await viewportScroll(page)).scrollTop - scrolled)).toBeLessThan(40);
    const caret = await caretInPage(page, 1);
    expect(caret.top).toBeLessThanOrEqual(localY);
    expect(caret.bottom).toBeGreaterThanOrEqual(localY);
    expect(Math.abs(caret.left - localX)).toBeLessThan(20);
});

test('leaving a band by double-clicking the body does not scroll to the stashed caret', async ({
    page,
}) => {
    const lines = Array.from({ length: 60 }, (_, i) => `Line ${i + 1} of the long document`);
    await openText(page, lines.join('\n'));
    /* The body caret sits at the document start (page 0); the Footer
       button enters page 0's footer; then the user scrolls to page 1 and
       double-clicks its body. */
    await page.getByRole('button', { name: 'Edit footer' }).click();
    await bandBox(page, 0, true);
    await expect(page.locator('.editor-page[data-page-index="1"]')).toHaveCount(1);
    const p1 = await pageCard(page, 1);
    const v = (await box(page, '.editor-viewport'))!;
    const scroll = await viewportScroll(page);
    await scrollViewportTo(page, scroll.scrollTop + p1.top - v.top + 100);
    const scrolled = (await viewportScroll(page)).scrollTop;
    const card1 = await pageCard(page, 1);
    const localX = 96 + 120;
    const localY = 96 + 60;
    await page.mouse.dblclick(card1.left + localX, card1.top + localY);
    await expect(page.locator('.story-band')).toHaveCount(0);
    await settle(page);
    expect(Math.abs((await viewportScroll(page)).scrollTop - scrolled)).toBeLessThan(40);
    const caret = await caretInPage(page, 1);
    expect(caret.top).toBeLessThanOrEqual(localY);
    expect(caret.bottom).toBeGreaterThanOrEqual(localY);
});

test('a double-click in the body exits the story and places the caret at the click', async ({
    page,
}) => {
    await page.getByRole('button', { name: 'Edit footer' }).click();
    await bandBox(page, 0, true);
    await scrollViewportTo(page, 0);
    /* A single click on the dimmed body is ignored while a story is open. */
    const card = await pageCard(page, 0);
    const localX = 96 + 160;
    const localY = 96 + 14;
    await page.mouse.click(card.left + localX, card.top + localY);
    await settle(page);
    await expect(page.locator('.story-band')).toHaveCount(1);

    await page.mouse.dblclick(card.left + localX, card.top + localY);
    await expect(page.locator('.story-band')).toHaveCount(0);
    await settle(page);
    const caret = await caretInPage(page, 0);
    expect(caret.top).toBeLessThanOrEqual(localY);
    expect(caret.bottom).toBeGreaterThanOrEqual(localY);
    /* At the click, not back at the pre-entry caret (the line start). */
    expect(Math.abs(caret.left - localX)).toBeLessThan(20);
    await page.keyboard.type('Q');
    await settle(page);
    await expect(page.locator('.a11y-mirror > p').first()).toContainText('Q');
    await expect(page.locator('.a11y-mirror > p').first()).not.toHaveText(/^Qalpha/);
});

test('a single click in a band while editing the body is a body click on the nearest line', async ({
    page,
}) => {
    const card = await pageCard(page, 0);
    const localX = 96 + 200;
    /* Inside the top margin, above the first body line. */
    await page.mouse.click(card.left + localX, card.top + 40);
    await settle(page);
    await expect(page.locator('.story-band')).toHaveCount(0);
    const caret = await caretInPage(page, 0);
    /* The first body line's top is the 96 px margin; the caret hangs off
       its baseline (issue #557), so its top sits the face's line gap
       below the line top. */
    expect(caret.top).toBeGreaterThanOrEqual(96 - 0.5);
    expect(caret.top).toBeLessThan(96 + 4);
    expect(Math.abs(caret.left - localX)).toBeLessThan(20);
});
