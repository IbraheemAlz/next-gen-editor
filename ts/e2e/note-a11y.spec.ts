import { test, expect } from '@playwright/test';

/* Issue #203 — footnote / endnote stories in the screen-reader mirror, end
 * to end through the REAL worker + WASM engine + a11y delta stream:
 *
 *   body text → insert a footnote (toolbar) → a `role="doc-footnote"` region
 *   appears right after the referencing paragraph, `aria-current` while its
 *   story is edited, announced "Editing footnote 1"; the reference mark is a
 *   `role="doc-noteref"` link to the region's id → typing in the note
 *   patches ONLY that region (the body `<p>` keeps its DOM identity) →
 *   an endnote lands in a trailing `role="doc-endnotes"` section, also
 *   patched in place → leaving the story clears `aria-current`.
 *
 * DOM-only assertions: the mirror is plain DOM, valid under headless
 * Chrome (the canvas is never screenshotted, see CLAUDE.md). */

test('footnotes and endnotes are mirrored as DPUB note regions', async ({ page }) => {
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

    const mirror = page.locator('.a11y-mirror');
    const footnote = mirror.locator(':scope > [role="doc-footnote"]');
    await expect(footnote).toHaveCount(0);

    /* Insert through the real toolbar button (enters the note's story). */
    await page.getByRole('button', { name: 'Insert footnote' }).click();
    await expect(page.getByRole('button', { name: 'Close footnote editing' })).toBeVisible();

    await expect(footnote).toHaveCount(1);
    await expect(footnote).toHaveAttribute('id', 'nge-a11y-footnote-1');
    await expect(footnote).toHaveAttribute('aria-label', 'Footnote 1');
    await expect(footnote).toHaveAttribute('aria-current', 'true');
    await expect(page.locator('[role="status"][aria-live="polite"]')).toHaveText(
        'Editing footnote 1',
    );
    /* Placed right after the referencing paragraph. */
    const order = await mirror.evaluate((root) =>
        Array.from(root.children).map((c) => c.getAttribute('role') ?? c.tagName),
    );
    expect(order.slice(0, 2)).toEqual(['P', 'doc-footnote']);

    /* The reference mark: a noteref link to the region, out of tab order,
       reading the marker — no U+FFFC placeholder in the mirror. */
    const noteref = mirror.locator(':scope > p [role="doc-noteref"]');
    await expect(noteref).toHaveCount(1);
    await expect(noteref).toHaveText('1');
    await expect(noteref).toHaveAttribute('href', '#nge-a11y-footnote-1');
    await expect(noteref).toHaveAttribute('tabindex', '-1');
    await expect(noteref).toHaveAttribute('aria-label', 'Footnote 1');
    await expect(mirror.locator(':scope > p').first()).toHaveText('Alpha1 body text');

    /* Tag the body paragraph's DOM node: a note edit must leave it alone. */
    await mirror.evaluate((root) => {
        (root.children[0] as any).__nge203 = true;
    });
    await page.evaluate(async () => {
        const dispatch = (cmd: unknown): Promise<any> => (window as any).__dispatch(cmd);
        await dispatch({ type: 'INSERT_TEXT', at: undefined, text: 'note words' });
    });
    await expect(footnote.locator('p')).toHaveText('1 note words');
    await expect(footnote.locator('p')).toHaveAttribute('dir', 'ltr');
    expect(
        await mirror.evaluate((root) => (root.children[0] as any).__nge203 === true),
        'the body <p> was not rebuilt',
    ).toBe(true);

    /* Leave the note: the marker clears, the region stays. */
    await page.evaluate(async () => {
        const dispatch = (cmd: unknown): Promise<any> => (window as any).__dispatch(cmd);
        await dispatch({ type: 'EXIT_HEADER_FOOTER' });
    });
    await expect(footnote).not.toHaveAttribute('aria-current', 'true');

    /* An endnote: collected in the trailing doc-endnotes section. */
    await page.evaluate(async () => {
        const dispatch = (cmd: unknown): Promise<any> => (window as any).__dispatch(cmd);
        const top = (idx: number, offset: number) => ({
            path: { steps: [{ kind: 'BLOCK', idx }] },
            offset,
        });
        await dispatch({ type: 'INSERT_ENDNOTE', at: top(0, 0) });
        await dispatch({ type: 'INSERT_TEXT', at: undefined, text: 'مرحبا' });
    });
    const section = mirror.locator(':scope > section[role="doc-endnotes"]');
    await expect(section).toHaveCount(1);
    const endnote = section.locator('ol > li');
    await expect(endnote).toHaveCount(1);
    await expect(endnote).toHaveAttribute('id', 'nge-a11y-endnote-1');
    await expect(endnote).toHaveAttribute('aria-current', 'true');
    await expect(endnote.locator('p')).toHaveAttribute('dir', 'rtl');
    await expect(page.locator('[role="status"][aria-live="polite"]')).toHaveText(
        'Editing endnote 1',
    );
    expect(
        await mirror.evaluate((root) => root.lastElementChild?.getAttribute('role')),
        'the endnotes section is the last child of the mirror',
    ).toBe('doc-endnotes');
    await expect(
        mirror.locator(':scope > p [role="doc-noteref"][href="#nge-a11y-endnote-1"]'),
    ).toHaveText('1');

    /* An endnote edit patches its own <li> in place. */
    await endnote.evaluate((el) => {
        (el as any).__nge203 = true;
    });
    await mirror.evaluate((root) => {
        (root.children[0] as any).__nge203b = true;
    });
    await page.evaluate(async () => {
        const dispatch = (cmd: unknown): Promise<any> => (window as any).__dispatch(cmd);
        await dispatch({ type: 'INSERT_TEXT', at: undefined, text: ' عالم' });
    });
    await expect(endnote.locator('p')).toContainText('عالم');
    expect(await endnote.evaluate((el) => (el as any).__nge203 === true)).toBe(false);
    expect(
        await mirror.evaluate((root) => (root.children[0] as any).__nge203b === true),
        'the body <p> was not rebuilt by an endnote edit',
    ).toBe(true);
    await expect(footnote.locator('p')).toHaveText('1 note words');
});
