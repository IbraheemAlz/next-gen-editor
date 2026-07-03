import { test, expect } from '@playwright/test';

/* Issue #53 regression guard — a keystroke fired immediately after a
 * click (no await between them) must insert at the CLICKED position,
 * not the previous caret.
 *
 * The old shell did HIT_TEST_IN_PAGE → (await) → SET_SELECTION, so an
 * INSERT_TEXT carrying the stale UI caret mirror could enter the worker
 * queue ahead of the SET_SELECTION. The fix is two-part: the single-hop
 * PLACE_CARET_AT_POINT posted synchronously at pointerdown, and
 * INSERT_TEXT{at: undefined} reading the engine's LIVE caret. This spec
 * drives exactly that wire sequence with zero waiting in between — the
 * worker's serialized queue plus the live-caret read must make the
 * insert land mid-line.
 */
test('insert fired immediately after caret placement lands at the clicked position', async ({
    page,
}) => {
    await page.goto('/');
    await page.waitForFunction(() => (window as any).__paintIdle === true, undefined, {
        timeout: 20_000,
    });

    const plain = await page.evaluate(async () => {
        const dispatch = (window as any).__dispatch;
        /* Fire-and-forget placement — deliberately NOT awaited. x=200
           lands several glyphs into the "Hello world …" seed line
           (A4 margin ≈ 96 device px at dpr 1, 24 pt seed text). */
        void dispatch({ type: 'PLACE_CARET_AT_POINT', page: 0, at: { x: 200, y: 110 } });
        /* Immediately queue the insert at the engine's live caret. */
        await dispatch({ type: 'INSERT_TEXT', at: undefined, text: 'RACE' });
        await dispatch({ type: 'SELECT_ALL' });
        const clip = await dispatch({ type: 'GET_SELECTION_AS_CLIPBOARD' });
        return clip.type === 'CLIPBOARD_PAYLOAD' ? (clip.plain as string) : `<${clip.type}>`;
    });

    expect(plain).toContain('RACE');
    const idx = plain.indexOf('RACE');
    /* The stale-caret failure mode inserts at the boot caret (offset 0)
       — the fix must land the text strictly inside the seed line. */
    expect(idx, `insert position in ${JSON.stringify(plain)}`).toBeGreaterThan(0);
});
