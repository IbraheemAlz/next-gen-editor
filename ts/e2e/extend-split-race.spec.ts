import { test, expect } from '@playwright/test';
import { boot, burst, documentText, settle } from './helpers/editor';

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
 *
 * Issue #310 -- the race tests drive the SHELL (`pointer.ts`,
 * `HiddenInput`) through the synchronous `burst` helper, not `__dispatch`
 * directly, so a deferral re-introduced in either listener fails them.
 * Proven against such deferrals -- see CLAUDE.md, e2e notes.
 */

/* Page-local device px on the "Hello world …" seed line (A4 margin ≈ 96
   device px at dpr 1, 24 pt seed text) — same band click-type-race uses. */
const LINE_Y = 110;
const ANCHOR_X = 150;
const EXTEND_X = 300;

test('insert fired immediately after a shift-click replaces the EXTENDED selection', async ({
    page,
}) => {
    await boot(page);
    const before = await documentText(page);

    /* Anchor placed and settled; then the shift-click and the keystroke
       land in ONE synchronous burst. */
    await burst(page, [{ pointerdown: { x: ANCHOR_X, y: LINE_Y } }, { pointerup: true }]);
    await settle(page);
    await burst(page, [
        { pointerdown: { x: EXTEND_X, y: LINE_Y, shift: true } },
        { pointerup: true },
        'EXT',
    ]);
    const after = await documentText(page);

    const idx = after.indexOf('EXT');
    expect(idx, `insert position in ${JSON.stringify(after)}`).toBeGreaterThan(0);
    /* The pre-extension (collapsed) selection would only ADD 3 bytes; the
       extended one replaces the glyphs between the two x positions. */
    expect(after.length).toBeLessThan(before.length + 3);
    /* Everything outside the replaced range is untouched. */
    expect(before.startsWith(after.slice(0, idx))).toBe(true);
    expect(before.endsWith(after.slice(idx + 3))).toBe(true);
});

test('real-input smoke: a Playwright shift-click then typing replaces the extended range', async ({
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

    /* Reference: the intended semantics straight at the engine, every step
       awaited (shell-independent, so a shell deferral cannot taint it):
       caret at the click, split, then 'Z' at the new paragraph's start. */
    const reference = await page.evaluate(
        async ({ y }) => {
            const dispatch = (window as any).__dispatch;
            await dispatch({ type: 'PLACE_CARET_AT_POINT', page: 0, at: { x: 200, y } });
            await dispatch({ type: 'SPLIT_PARAGRAPH', at: undefined });
            await dispatch({ type: 'INSERT_TEXT', at: undefined, text: 'Z' });
            await dispatch({ type: 'SELECT_ALL' });
            const clip = await dispatch({ type: 'GET_SELECTION_AS_CLIPBOARD' });
            return clip.type === 'CLIPBOARD_PAYLOAD' ? (clip.plain as string) : `<${clip.type}>`;
        },
        { y: LINE_Y },
    );

    /* Racy form on a fresh boot: click placement posted, reply NOT yet
       delivered -- and in the same task, Enter reaches HiddenInput while
       its caret mirror is stale (parked at offset 0). A trailing 'Z'
       keystroke queues right behind the Enter: if the shell defers the
       split even by one macrotask, the 'Z' overtakes it and lands on the
       wrong side of the break (the readback below arrives too late to
       notice a bare delay on its own). */
    await boot(page);
    await page.evaluate(async () => {
        const dispatch = (window as any).__dispatch;
        const home = { path: { steps: [{ kind: 'BLOCK', idx: 0 }] }, offset: 0 };
        await dispatch({ type: 'SET_SELECTION', range: { start: home, end: home }, caret: home });
    });
    await textarea.focus();
    await burst(page, [{ pointerdown: { x: 200, y: LINE_Y } }, { pointerup: true }, 'ENTER', 'Z']);
    const after = await documentText(page);

    /* One paragraph break more than before, not at the stale offset 0, and
       at EXACTLY the clicked position the settled reference split at (a
       stale mirror elsewhere in the line, e.g. its end, would still be a
       single plain break). */
    const breaks = (s: string): number => (s.match(/\n/g) ?? []).length;
    expect(breaks(after), JSON.stringify(after)).toBe(breaks(before) + 1);
    expect(after.startsWith('\n'), JSON.stringify(after)).toBe(false);
    expect(after.indexOf('\n')).toBeGreaterThan(0);
    expect(after.replace('\n', '').replace('Z', '')).toBe(before);
    expect(after, 'split lands where the settled click put the caret').toBe(reference);
    expect(after.replace('\n', '')).toContain('Z');
});
