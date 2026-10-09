import { test, expect } from '@playwright/test';
import { boot, documentText } from './helpers/editor';

/* Issue #407 — a NaN / infinite number in ANY numeric command field is
 * refused by the engine's command-boundary finite() guard, through the
 * REAL shell + worker + WASM engine.
 *
 * A JS `NaN` / `Infinity` survives `postMessage` (structured clone) and
 * `serde-wasm-bindgen` hands it to an `f32` / `f64` field unchanged; before
 * #407 only the zoom pair (#186) rejected it, and `f32::clamp` passes NaN
 * straight through, so e.g. `APPLY_FORMATTING { font_size: NaN }` reached
 * the model. Now every such command answers `ERROR { kind:
 * "InvalidArgument" }` naming the field, nothing changes, no user toast is
 * shown (a host bug, not a user action) and the Dev HUD's last-error row
 * records it.
 */

test('NaN / infinite numeric arguments are refused as InvalidArgument and change nothing', async ({
    page,
}) => {
    test.setTimeout(45_000);
    await boot(page);
    const before = await documentText(page);

    const replies = await page.evaluate(async () => {
        const dispatch = (window as any).__dispatch;
        return [
            await dispatch({ type: 'APPLY_FORMATTING', attrs: { font_size: Number.NaN } }),
            await dispatch({ type: 'SET_ZOOM', scale: Number.POSITIVE_INFINITY }),
            await dispatch({
                type: 'REQUEST_PAINT',
                viewport: { x: 0, y: 0, w: 100, h: Number.NaN },
            }),
            await dispatch({ type: 'EXPAND_LAYOUT', target_y: Number.NEGATIVE_INFINITY }),
        ];
    });
    expect(replies.map((r: any) => [r.type, r.kind])).toEqual([
        ['ERROR', 'InvalidArgument'],
        ['ERROR', 'InvalidArgument'],
        ['ERROR', 'InvalidArgument'],
        ['ERROR', 'InvalidArgument'],
    ]);
    expect(replies[0].message).toMatch(/^ApplyFormatting: attrs\.font_size is NaN /);
    expect(replies[1].message).toMatch(/^SetZoom: scale is inf /);
    expect(replies[2].message).toMatch(/^RequestPaint: viewport\.h is NaN /);
    expect(replies[3].message).toMatch(/^ExpandLayout: target_y is -inf /);

    /* The Dev HUD records the last one; no user-facing toast. */
    await expect(page.locator('.nge-toast__message')).toHaveCount(0);
    await page.locator('textarea[data-nge-hidden-input]').focus();
    await page.keyboard.press('Control+Shift+D');
    const row = page.locator('.nge-hud__lasterror');
    await expect(row).toBeVisible();
    await expect(row).toContainText('ExpandLayout · InvalidArgument · #4');

    /* Nothing changed, and the same commands with finite numbers work. */
    expect(await documentText(page)).toBe(before);
    const zoom = await page.evaluate(() =>
        (window as any).__dispatch({ type: 'SET_ZOOM', scale: 1.5 }),
    );
    expect(zoom.type).toBe('SELECTION_CHANGED');
    expect(zoom.zoom).toBe(1.5);
});
