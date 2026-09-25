import { test, expect } from '@playwright/test';

/* Issue #80 — footnote / endnote authoring end-to-end through the REAL
 * worker + WASM engine + bridge schema:
 *
 *   body text → Insert Footnote (toolbar) → type into the note → exit →
 *   INSERT_ENDNOTE → type → exit → save → reload → the note stories
 *   survived (the next footnote mints id 2, not 1).
 *
 * Assertions ride command replies (`editing_story`) and the toolbar's
 * DOM state — engine-truth valid under headless Chrome. Band pixels are
 * covered by the native paginator / scene tests (headless Chrome never
 * composites the interactive canvas, see CLAUDE.md).
 */

test('insert footnote + endnote, type into them, survive save/reload', async ({ page }) => {
    test.setTimeout(60_000);
    await page.goto('/');
    await page.waitForFunction(() => (window as any).__paintIdle === true, undefined, {
        timeout: 20_000,
    });

    /* Deterministic base: replace the seed document, caret after "Alpha". */
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
    const insertFootnote = page.getByRole('button', { name: 'Insert footnote' });
    await expect(insertFootnote).toBeEnabled();
    await expect(page.locator('.nge-notes .nge-feature__badge')).toHaveCount(0);
    await insertFootnote.click();
    /* Note mode: the button cluster flips to "Close Footnote". */
    const closeNote = page.getByRole('button', { name: 'Close footnote editing' });
    await expect(closeNote).toBeVisible();

    const result = await page.evaluate(async () => {
        const dispatch = (cmd: unknown): Promise<any> => (window as any).__dispatch(cmd);
        const top = (idx: number, offset: number) => ({
            path: { steps: [{ kind: 'BLOCK', idx }] },
            offset,
        });
        const typed = await dispatch({ type: 'INSERT_TEXT', at: undefined, text: 'note one' });
        const gated = await dispatch({ type: 'INSERT_ENDNOTE', at: top(0, 0) });
        const exited = await dispatch({ type: 'EXIT_HEADER_FOOTER' });
        const endnote = await dispatch({ type: 'INSERT_ENDNOTE', at: top(0, 0) });
        await dispatch({ type: 'INSERT_TEXT', at: undefined, text: 'end one' });
        await dispatch({ type: 'EXIT_HEADER_FOOTER' });

        const saved = await dispatch({ type: 'SAVE_DOCX' });
        const reloaded = await dispatch({ type: 'LOAD_DOCX', bytes: saved.bytes });
        const again = await dispatch({ type: 'INSERT_FOOTNOTE', at: top(0, 0) });
        await dispatch({ type: 'EXIT_HEADER_FOOTER' });
        return {
            typedStory: typed.editing_story ?? null,
            gatedType: gated.type,
            gatedMessage: gated.message ?? '',
            exitedStory: exited.editing_story ?? null,
            endnoteStory: endnote.editing_story ?? null,
            savedType: saved.type,
            reloadedType: reloaded.type,
            againStory: again.editing_story ?? null,
        };
    });

    expect(result.typedStory?.area).toBe('Footnote');
    expect(result.typedStory?.rid).toBe('1');
    /* Notes cannot nest — gated loudly, never a silent no-op. */
    expect(result.gatedType).toBe('ERROR');
    expect(result.gatedMessage).toContain('footnote or endnote');
    expect(result.exitedStory).toBeNull();
    expect(result.endnoteStory?.area).toBe('Endnote');
    expect(result.savedType).not.toBe('ERROR');
    expect(result.reloadedType).not.toBe('ERROR');
    /* Footnote 1 survived the round trip, so the next one mints id 2. */
    expect(result.againStory?.area).toBe('Footnote');
    expect(result.againStory?.rid).toBe('2');
});
