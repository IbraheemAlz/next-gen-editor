import { test, expect } from '@playwright/test';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

/* Issue #165 — text-box stories in the screen-reader mirror, end to end
 * through the REAL worker + WASM engine + a11y delta stream:
 *
 *   body text → insert a text box → its `role="group"` region appears
 *   right after the anchor paragraph, marked `aria-current` while its
 *   story is edited → typing in the box patches ONLY that region (the
 *   body `<p>` keeps its DOM identity — a delta, not a full rebuild) →
 *   leaving the story clears `aria-current`.
 *
 * Issue #215 — the text box's OWN anchor run (the `U+FFFC` sentinel in
 * its host paragraph) never reaches the mirror raw: it is an `<a>`
 * carrying no visible text, `href`-linking to the region's own DOM id
 * (`textBoxDomId`). Inline pictures get the same treatment as an
 * `<img role="img">` instead of an `<a>`. See the second test below for
 * pictures (including one nested two boxes deep) through a real fixture.
 *
 * DOM-only assertions: the mirror is plain DOM, valid under headless
 * Chrome (the canvas is never screenshotted, see CLAUDE.md). */

const TBP_FIXTURE = fileURLToPath(
    new URL('../../crates/format-docx/tests/fixtures/text_box_pictures.docx', import.meta.url),
);

test('text box story is mirrored as a live group region', async ({ page }) => {
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
    const region = mirror.locator(':scope > [role="group"]');
    await expect(region).toHaveCount(0);

    /* Insert through the real toolbar button (enters the box's story). */
    await page.getByRole('button', { name: 'Insert text box' }).click();
    await expect(page.getByRole('button', { name: 'Close text box editing' })).toBeVisible();

    await expect(region).toHaveCount(1);
    await expect(region).toHaveAttribute('aria-label', 'Text box');
    await expect(region).toHaveAttribute('data-story-id', '0@5');
    await expect(region).toHaveAttribute('aria-current', 'true');
    /* Placed right after its anchor paragraph, body-shaped inside. */
    const order = await mirror.evaluate((root) =>
        Array.from(root.children).map((c) => c.getAttribute('role') ?? c.tagName),
    );
    expect(order.slice(0, 2)).toEqual(['P', 'group']);
    await expect(region.locator('p')).toHaveCount(1);

    /* Issue #215 — the anchor paragraph's sentinel run is a bare `<a>`
     * (no `role="doc-noteref"`, that's notes only), never the raw
     * `U+FFFC`, linking to the region's own DOM id. */
    const anchorP = mirror.locator(':scope > p').first();
    await expect(anchorP).not.toContainText('\u{FFFC}');
    const objectRef = anchorP.locator('a');
    await expect(objectRef).toHaveCount(1);
    await expect(objectRef).toHaveText('');
    await expect(objectRef).toHaveAttribute('aria-label', 'Text box');
    const regionDomId = await region.evaluate((el) => el.id);
    expect(regionDomId).toMatch(/^nge-a11y-tb-/);
    await expect(objectRef).toHaveAttribute('href', `#${regionDomId}`);
    await expect(anchorP).toHaveText('Alpha body text');

    /* Tag the body paragraph's DOM node: a delta must leave it alone. */
    await mirror.evaluate((root) => {
        (root.children[0] as any).__nge165 = true;
    });

    await page.evaluate(async () => {
        const dispatch = (cmd: unknown): Promise<any> => (window as any).__dispatch(cmd);
        await dispatch({ type: 'INSERT_TEXT', at: undefined, text: 'boxed words' });
    });
    await expect(region.locator('p')).toHaveText('boxed words');
    await expect(region.locator('p')).toHaveAttribute('dir', /^(ltr|rtl)$/);
    await expect(region.locator('p > span').first()).toHaveText('boxed words');
    const kept = await mirror.evaluate((root) => (root.children[0] as any).__nge165 === true);
    expect(kept, 'the body <p> was not rebuilt').toBe(true);
    await expect(mirror.locator(':scope > p').first()).toContainText('Alpha');

    /* Leaving the story clears the active marker; the region stays. */
    await page.evaluate(async () => {
        const dispatch = (cmd: unknown): Promise<any> => (window as any).__dispatch(cmd);
        await dispatch({ type: 'EXIT_HEADER_FOOTER' });
    });
    await expect(region).not.toHaveAttribute('aria-current', 'true');
    await expect(region.locator('p')).toHaveText('boxed words');
});

/* Issue #215 — the real `text_box_pictures.docx` fixture (also driving
 * `text-box-pictures.spec.ts`) nests a picture in the outer box, a second
 * text box in the outer box (itself anchored by a reference run), and a
 * further picture in that nested box. Every one of those four `U+FFFC`
 * sentinels must reach the mirror as an object run, never raw. */
test('inline pictures and a nested text box replace every U+FFFC (real fixture)', async ({
    page,
}) => {
    test.setTimeout(60_000);
    await page.goto('/');
    await page.waitForFunction(() => (window as any).__paintIdle === true, undefined, {
        timeout: 20_000,
    });

    const bytes = Array.from(readFileSync(TBP_FIXTURE));
    await page.evaluate(async (arr: number[]) => {
        const d = (cmd: unknown): Promise<any> => (window as any).__dispatch(cmd);
        const loaded = await d({ type: 'LOAD_DOCX', bytes: new Uint8Array(arr) });
        if (loaded.type === 'ERROR') throw new Error(loaded.message);
    }, bytes);

    const mirror = page.locator('.a11y-mirror');
    await expect(mirror.locator(':scope > p')).toHaveCount(2);
    const hasSentinel = await mirror.evaluate((root) => (root.textContent ?? '').includes('￼'));
    expect(hasSentinel, 'no raw U+FFFC anywhere in the mirror').toBe(false);

    /* The host paragraph anchors the outer box: a bare `<a>` reference
     * (no visible text) linking to the region's own DOM id. */
    const hostP = mirror.locator(':scope > p').nth(1);
    await expect(hostP).toHaveText('Host paragraph.');
    const outerRef = hostP.locator('a');
    await expect(outerRef).toHaveCount(1);
    await expect(outerRef).toHaveText('');
    await expect(outerRef).toHaveAttribute('aria-label', 'Text box: Text Box 1');

    const outerRegion = mirror.locator(':scope > [role="group"]');
    await expect(outerRegion).toHaveCount(1);
    const outerId = await outerRegion.evaluate((el) => el.id);
    expect(outerId).toMatch(/^nge-a11y-tb-/);
    await expect(outerRef).toHaveAttribute('href', `#${outerId}`);

    /* Inside the outer box: its own picture is an `<img>`, and its
     * second paragraph anchors the NESTED box the same way. */
    const outerParas = outerRegion.locator(':scope > p');
    await expect(outerParas).toHaveCount(2);
    const outerImg = outerParas.nth(0).locator('img');
    await expect(outerImg).toHaveCount(1);
    await expect(outerImg).toHaveAttribute('role', 'img');
    await expect(outerImg).toHaveAttribute('alt', 'Picture 3');
    await expect(outerParas.nth(0)).toHaveText('Outer story text flows beside the picture.');

    await expect(outerParas.nth(1)).toHaveText('Nested host.');
    const innerRef = outerParas.nth(1).locator('a');
    await expect(innerRef).toHaveCount(1);
    await expect(innerRef).toHaveText('');
    await expect(innerRef).toHaveAttribute('aria-label', 'Text box: Text Box 2');

    const innerRegion = outerRegion.locator(':scope > [role="group"]');
    await expect(innerRegion).toHaveCount(1);
    const innerId = await innerRegion.evaluate((el) => el.id);
    expect(innerId).toMatch(/^nge-a11y-tb-/);
    expect(innerId).not.toBe(outerId);
    await expect(innerRef).toHaveAttribute('href', `#${innerId}`);

    /* The nested box's own picture. */
    const innerImg = innerRegion.locator('p img');
    await expect(innerImg).toHaveCount(1);
    await expect(innerImg).toHaveAttribute('alt', 'Picture 4');
    await expect(innerRegion.locator('p')).toHaveText('Inner text.');
});
