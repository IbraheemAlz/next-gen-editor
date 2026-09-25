import { test, expect, type Page } from '@playwright/test';

/* Issue #64 regression guards — the rest of the #53 stale-position family.
 *
 * 1. Shift-click / drag extension used to be HIT_TEST_IN_PAGE → (await) →
 *    EXTEND_SELECTION, so a keystroke fired inside that round-trip executed
 *    against the PRE-extension selection. `pointer.ts` now posts the
 *    single-hop EXTEND_SELECTION_TO_POINT synchronously; the worker queue
 *    orders it ahead of the keystroke.
 * 2. Enter (SPLIT_PARAGRAPH) and IME start (BEGIN_COMPOSITION) carried the
 *    UI caret MIRROR, which only updates when SELECTION_CHANGED lands. Both
 *    now send `at: undefined` and the engine uses its live selection.
 */

async function boot(page: Page): Promise<void> {
    await page.goto('/');
    await page.waitForFunction(() => (window as any).__paintIdle === true, undefined, {
        timeout: 30_000,
    });
}

/** The whole document as plain text (paragraphs joined by the engine). */
async function documentText(page: Page): Promise<string> {
    return page.evaluate(async () => {
        const dispatch = (window as any).__dispatch;
        await dispatch({ type: 'SELECT_ALL' });
        const clip = await dispatch({ type: 'GET_SELECTION_AS_CLIPBOARD' });
        return clip.type === 'CLIPBOARD_PAYLOAD' ? (clip.plain as string) : `<${clip.type}>`;
    });
}

/* Page-local device px on the "Hello world …" seed line (A4 margin ≈ 96
   device px at dpr 1, 24 pt seed text) — same band click-type-race uses. */
const LINE_Y = 110;
const ANCHOR_X = 150;
const EXTEND_X = 300;

test('insert fired immediately after a point-extension replaces the EXTENDED selection', async ({
    page,
}) => {
    await boot(page);
    const before = await documentText(page);

    const after = await page.evaluate(
        async ({ y, ax, ex }) => {
            const dispatch = (window as any).__dispatch;
            await dispatch({ type: 'PLACE_CARET_AT_POINT', page: 0, at: { x: ax, y } });
            /* Fire-and-forget extension — deliberately NOT awaited. */
            void dispatch({ type: 'EXTEND_SELECTION_TO_POINT', page: 0, at: { x: ex, y } });
            await dispatch({ type: 'INSERT_TEXT', at: undefined, text: 'EXT' });
            await dispatch({ type: 'SELECT_ALL' });
            const clip = await dispatch({ type: 'GET_SELECTION_AS_CLIPBOARD' });
            return clip.type === 'CLIPBOARD_PAYLOAD' ? (clip.plain as string) : `<${clip.type}>`;
        },
        { y: LINE_Y, ax: ANCHOR_X, ex: EXTEND_X },
    );

    const idx = after.indexOf('EXT');
    expect(idx, `insert position in ${JSON.stringify(after)}`).toBeGreaterThan(0);
    /* The pre-extension (collapsed) selection would only ADD 3 bytes; the
       extended one replaces the glyphs between the two x positions. */
    expect(after.length).toBeLessThan(before.length + 3);
    /* Everything outside the replaced range is untouched. */
    expect(before.startsWith(after.slice(0, idx))).toBe(true);
    expect(before.endsWith(after.slice(idx + 3))).toBe(true);
});

test('a real shift-click followed immediately by typing replaces the extended range', async ({
    page,
}) => {
    await boot(page);
    const before = await documentText(page);
    const canvas = page.locator('.editor-page[data-page-index="0"] canvas').first();
    await expect(canvas).toBeVisible();
    /* The viewport may open scrolled into the page; bring its TOP (where
       the seed line sits) on screen before taking client coordinates. */
    await canvas.evaluate((el) => el.scrollIntoView({ block: 'start' }));
    const box = await canvas.boundingBox();
    if (!box) throw new Error('page canvas has no box');
    const dpr = await page.evaluate(() => window.devicePixelRatio || 1);
    const toClient = (x: number): [number, number] => [box.x + x / dpr, box.y + LINE_Y / dpr];

    await page.mouse.click(...toClient(ANCHOR_X));
    await page.keyboard.down('Shift');
    await page.mouse.click(...toClient(EXTEND_X));
    await page.keyboard.up('Shift');
    /* No wait between the shift-click and the keystroke. */
    await page.keyboard.type('Q');

    await expect
        .poll(() => documentText(page), { timeout: 10_000 })
        .toContain('Q');
    const after = await documentText(page);
    const idx = after.indexOf('Q');
    expect(idx).toBeGreaterThan(0);
    expect(after.length, JSON.stringify(after)).toBeLessThan(before.length + 1);
    expect(before.startsWith(after.slice(0, idx))).toBe(true);
    expect(before.endsWith(after.slice(idx + 1))).toBe(true);
});

test('Enter during a pending UI-mirror update splits at the engine caret', async ({ page }) => {
    await boot(page);
    const textarea = page.locator('textarea[data-nge-hidden-input]');
    const before = await documentText(page);

    /* Park the engine caret (and, once the reply lands, the UI mirror) at
       the start of the seed line so a stale mirror is unambiguous. */
    await page.evaluate(async () => {
        const dispatch = (window as any).__dispatch;
        const home = { path: { steps: [{ kind: 'BLOCK', idx: 0 }] }, offset: 0 };
        await dispatch({ type: 'SET_SELECTION', range: { start: home, end: home }, caret: home });
    });
    await textarea.focus();

    const after = await page.evaluate(async (y) => {
        const dispatch = (window as any).__dispatch;
        const input = document.querySelector<HTMLTextAreaElement>(
            'textarea[data-nge-hidden-input]',
        );
        if (!input) throw new Error('hidden input missing');
        /* Click placement posted, reply NOT yet delivered — and in the
           same task, Enter reaches HiddenInput while its caret mirror
           still says offset 0. */
        void dispatch({ type: 'PLACE_CARET_AT_POINT', page: 0, at: { x: 200, y } });
        input.dispatchEvent(
            new InputEvent('beforeinput', {
                inputType: 'insertLineBreak',
                bubbles: true,
                cancelable: true,
            }),
        );
        /* A round-trip queued behind the split. */
        await dispatch({ type: 'SELECT_ALL' });
        const clip = await dispatch({ type: 'GET_SELECTION_AS_CLIPBOARD' });
        return clip.type === 'CLIPBOARD_PAYLOAD' ? (clip.plain as string) : `<${clip.type}>`;
    }, LINE_Y);

    /* One paragraph break more than before, and NOT at the stale offset 0:
       the first line keeps a non-empty head of the seed text. */
    const breaks = (s: string): number => (s.match(/\n/g) ?? []).length;
    expect(breaks(after), JSON.stringify(after)).toBe(breaks(before) + 1);
    expect(after.startsWith('\n'), JSON.stringify(after)).toBe(false);
    const nl = after.indexOf('\n');
    expect(nl).toBeGreaterThan(0);
    expect(after.replace('\n', '')).toBe(before);
});
