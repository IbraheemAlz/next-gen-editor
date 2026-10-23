import { expect, type Page } from '@playwright/test';
import { scrollViewportTo, viewportScroll } from './editor';

/* Header / footer band helpers shared by the real-UI story specs
 * (`header-footer-entry.spec.ts`, `caret-extent.spec.ts`). Rectangles
 * come from the DOM overlays (`.caret`, `.story-band`, the page cards),
 * which are valid under headless Chrome (only the canvas pixels are not
 * composited there). */

export interface Box {
    top: number;
    bottom: number;
    left: number;
    right: number;
}

/** `selector`'s client rect, or `null` when nothing matches. */
export async function box(page: Page, selector: string): Promise<Box | null> {
    return page.evaluate((sel) => {
        const el = document.querySelector(sel);
        if (!el) return null;
        const r = el.getBoundingClientRect();
        return { top: r.top, bottom: r.bottom, left: r.left, right: r.right };
    }, selector);
}

/** Page `idx`'s card. */
export const pageCard = (page: Page, idx: number): Promise<Box> =>
    box(page, `.editor-page[data-page-index="${idx}"]`).then((b) => b!);

/** The anchor page's active band — waits for it to appear. */
export async function bandBox(page: Page, pageIdx: number, footer: boolean): Promise<Box> {
    const sel =
        `.editor-page[data-page-index="${pageIdx}"] .story-band` +
        (footer ? '.story-band--footer' : ':not(.story-band--footer)');
    await expect(page.locator(sel)).toHaveCount(1);
    return (await box(page, sel))!;
}

/** Scroll to the bottom of the (one-page) document and double-click the
 *  footer zone of page 0: below the body, inside the bottom margin. */
export async function dblclickFooterAtBottom(page: Page): Promise<{ scrollTop: number }> {
    const scroll = await viewportScroll(page);
    const target = scroll.scrollHeight - scroll.clientHeight;
    expect(target).toBeGreaterThan(200);
    await scrollViewportTo(page, target);
    const card = await pageCard(page, 0);
    const scrolled = (await viewportScroll(page)).scrollTop;
    await page.mouse.dblclick((card.left + card.right) / 2, card.bottom - 30);
    return { scrollTop: scrolled };
}
