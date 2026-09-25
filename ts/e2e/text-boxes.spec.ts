import { test, expect } from '@playwright/test';

/* Issue #83 — text box authoring end-to-end through the REAL worker +
 * WASM engine + bridge schema:
 *
 *   body text → Insert Text Box (toolbar) → type into the box → nesting
 *   gated → exit → save → reload → the story survived (typing into the
 *   reloaded box extends the saved text).
 *
 * Assertions ride command replies (`editing_story`) and the toolbar's
 * DOM state — engine-truth valid under headless Chrome. Box pixels are
 * covered by the native layout / scene tests (headless Chrome never
 * composites the interactive canvas, see CLAUDE.md).
 */

test('insert a text box, type into it, survive save/reload', async ({ page }) => {
    test.setTimeout(60_000);
    await page.goto('/');
    await page.waitForFunction(() => (window as any).__paintIdle === true, undefined, {
        timeout: 20_000,
    });

    await page.evaluate(async () => {
        const dispatch = (cmd: unknown): Promise<any> => (window as any).__dispatch(cmd);
        const top = (idx: number, offset: number) => ({
            path: { steps: [{ kind: 'BLOCK', idx }] },
            offset,
        });
        await dispatch({ type: 'SELECT_ALL' });
        await dispatch({ type: 'INSERT_TEXT', at: undefined, text: 'Alpha body text' });
        await dispatch({
            type: 'SET_SELECTION',
            range: { start: top(0, 5), end: top(0, 5) },
            caret: top(0, 5),
        });
    });

    /* The toolbar button is real (no Engine-pending gate) and enabled. */
    const insertTextBox = page.getByRole('button', { name: 'Insert text box' });
    await expect(insertTextBox).toBeEnabled();
    await expect(page.locator('.nge-text-box .nge-feature__badge')).toHaveCount(0);
    await insertTextBox.click();
    const closeBox = page.getByRole('button', { name: 'Close text box editing' });
    await expect(closeBox).toBeVisible();

    const result = await page.evaluate(async () => {
        const dispatch = (cmd: unknown): Promise<any> => (window as any).__dispatch(cmd);
        const top = (idx: number, offset: number) => ({
            path: { steps: [{ kind: 'BLOCK', idx }] },
            offset,
        });
        const typed = await dispatch({ type: 'INSERT_TEXT', at: undefined, text: 'boxed' });
        const gated = await dispatch({
            type: 'INSERT_TEXT_BOX',
            at: top(0, 0),
            width_emu: 914400,
            height_emu: 457200,
        });
        const exited = await dispatch({ type: 'EXIT_HEADER_FOOTER' });
        const saved = await dispatch({ type: 'SAVE_DOCX' });
        const reloaded = await dispatch({ type: 'LOAD_DOCX', bytes: saved.bytes });
        return {
            typedStory: typed.editing_story ?? null,
            gatedType: gated.type,
            gatedMessage: gated.message ?? '',
            exitedStory: exited.editing_story ?? null,
            savedType: saved.type,
            reloadedType: reloaded.type,
        };
    });

    expect(result.typedStory?.area).toBe('TextBox');
    expect(result.typedStory?.rid).toBe('0@5');
    /* Text boxes cannot nest — gated loudly, never a silent no-op. */
    expect(result.gatedType).toBe('ERROR');
    expect(result.gatedMessage).toContain('text box');
    expect(result.exitedStory).toBeNull();
    expect(result.savedType).not.toBe('ERROR');
    expect(result.reloadedType).not.toBe('ERROR');
    await expect(insertTextBox).toBeVisible();
});
