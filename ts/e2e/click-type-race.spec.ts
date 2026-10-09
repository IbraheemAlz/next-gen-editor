import { test, expect } from '@playwright/test';
import { boot, burst, documentText } from './helpers/editor';

/* Issue #53 regression guard -- a keystroke fired immediately after a
 * click (no yield between them) must insert at the CLICKED position,
 * not the previous caret.
 *
 * The old shell did HIT_TEST_IN_PAGE -> (await) -> SET_SELECTION, so an
 * INSERT_TEXT carrying the stale UI caret mirror could enter the worker
 * queue ahead of the SET_SELECTION. The fix is two-part: the single-hop
 * PLACE_CARET_AT_POINT posted synchronously at pointerdown, and
 * INSERT_TEXT{at: undefined} reading the engine's LIVE caret.
 *
 * Issue #310 -- this spec now drives the SHELL: a real `pointerdown` on
 * the page canvas (through `pointer.ts`) and a `beforeinput` on the hidden
 * textarea (through `HiddenInput`), in ONE synchronous burst. The previous
 * version called `__dispatch` directly, which bypassed both listeners, so
 * re-introducing an await/deferral in `pointer.ts` would not have failed
 * it. Proven against such a deferral -- see CLAUDE.md, e2e notes. */
test('insert fired immediately after a click lands at the clicked position', async ({ page }) => {
    await boot(page);

    /* x=200 lands several glyphs into the "Hello world ..." seed line
       (A4 margin ~ 96 device px at dpr 1, 24 pt seed text). */
    await burst(page, [{ pointerdown: { x: 200, y: 110 } }, { pointerup: true }, 'RACE']);
    const plain = await documentText(page);

    expect(plain).toContain('RACE');
    const idx = plain.indexOf('RACE');
    /* The stale-caret failure mode inserts at the boot caret (offset 0)
       -- the fix must land the text strictly inside the seed line. */
    expect(idx, `insert position in ${JSON.stringify(plain)}`).toBeGreaterThan(0);
});
