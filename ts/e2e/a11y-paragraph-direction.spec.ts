import { test, expect } from '@playwright/test';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

/* Issue #195 — the screen-reader mirror sets `dir` on every `<p>` from
 * that paragraph's OWN resolved base direction (explicit bidi →
 * first-strong auto-direction → document base; the engine's layout
 * resolution), not the document base direction. The interactive editor
 * boots an RTL document, so before the fix a Latin paragraph was mirrored
 * `dir="rtl"` and read with the wrong UAX #9 base.
 *
 * Mixed-direction fixture through the REAL worker + WASM engine, DOM-only
 * assertions (the mirror is plain DOM, valid under headless Chrome). */

const top = (idx: number, offset: number) => ({
    path: { steps: [{ kind: 'BLOCK', idx }] },
    offset,
});
const para = (idx: number) => ({ start: top(idx, 0), end: top(idx, 0) });

const ARABIC = 'مرحبا بالعالم';

test('each mirrored paragraph carries its own resolved dir', async ({ page }) => {
    test.setTimeout(60_000);
    await page.goto('/');
    await page.waitForFunction(() => (window as any).__paintIdle === true, undefined, {
        timeout: 20_000,
    });

    await page.evaluate(
        async ({ texts, directions }) => {
            const w = window as any;
            const dispatch = (cmd: unknown): Promise<any> => w.__dispatch(cmd);
            await dispatch({ type: 'SELECT_ALL' });
            const bytes = (t: string) => new TextEncoder().encode(t).length;
            for (let i = 0; i < texts.length; i++) {
                if (i > 0) {
                    /* Split at the end of the previous paragraph; the
                       engine moves the caret into the new one. */
                    await dispatch({
                        type: 'SPLIT_PARAGRAPH',
                        at: {
                            path: { steps: [{ kind: 'BLOCK', idx: i - 1 }] },
                            offset: bytes(texts[i - 1]!),
                        },
                    });
                }
                await dispatch({ type: 'INSERT_TEXT', at: undefined, text: texts[i] });
            }
            for (const d of directions) {
                await dispatch({ type: 'SET_PARAGRAPH_DIRECTION', range: d.range, direction: d.dir });
            }
            await dispatch({ type: 'PING' });
        },
        {
            texts: ['Hello world', ARABIC, 'Explicit right to left', ARABIC, '12345'],
            directions: [
                { range: para(2), dir: 'Rtl' },
                { range: para(3), dir: 'Ltr' },
            ],
        },
    );

    const mirror = page.locator('.a11y-mirror');
    const paragraphs = mirror.locator(':scope > p');
    await expect(paragraphs).toHaveCount(5);
    await expect(paragraphs.nth(4)).toHaveText('12345');
    const dirs = await paragraphs.evaluateAll((els) => els.map((el) => el.getAttribute('dir')));
    expect(dirs).toEqual([
        'ltr', // Latin, auto-direction — the RTL document base must not win
        'rtl', // Arabic, auto-direction
        'rtl', // explicit RTL over Latin text
        'ltr', // explicit LTR over Arabic text
        'rtl', // neutral digits → the (RTL) document base
    ]);

    /* A direction flip re-mirrors just that paragraph with the new dir. */
    await mirror.evaluate((root) => {
        (root.children[1] as any).__nge195 = true;
    });
    await page.evaluate(async (range) => {
        const w = window as any;
        await w.__dispatch({ type: 'SET_PARAGRAPH_DIRECTION', range, direction: 'Rtl' });
        await w.__dispatch({ type: 'PING' });
    }, para(0));
    await expect(paragraphs.nth(0)).toHaveAttribute('dir', 'rtl');
    const kept = await mirror.evaluate((root) => (root.children[1] as any).__nge195 === true);
    expect(kept, 'untouched paragraphs keep their DOM nodes').toBe(true);
});

test('text-box region paragraphs resolve on their own; regions carry no dir', async ({ page }) => {
    test.setTimeout(60_000);
    await page.goto('/');
    await page.waitForFunction(() => (window as any).__paintIdle === true, undefined, {
        timeout: 20_000,
    });

    await page.evaluate(async (at) => {
        const w = window as any;
        const dispatch = (cmd: unknown): Promise<any> => w.__dispatch(cmd);
        await dispatch({ type: 'SELECT_ALL' });
        await dispatch({ type: 'INSERT_TEXT', at: undefined, text: 'مرحبا' });
        await dispatch({ type: 'SET_SELECTION', range: { start: at, end: at }, caret: at });
    }, top(0, 4));
    await page.getByRole('button', { name: 'Insert text box' }).click();
    await expect(page.getByRole('button', { name: 'Close text box editing' })).toBeVisible();
    await page.evaluate(async () => {
        const w = window as any;
        await w.__dispatch({ type: 'INSERT_TEXT', at: undefined, text: 'Latin inside' });
        await w.__dispatch({ type: 'PING' });
    });

    const mirror = page.locator('.a11y-mirror');
    const region = mirror.locator(':scope > [role="group"]');
    await expect(region.locator('p')).toHaveText('Latin inside');
    await expect(mirror.locator(':scope > p').first()).toHaveAttribute('dir', 'rtl');
    await expect(region.locator('p')).toHaveAttribute('dir', 'ltr');
    expect(await region.getAttribute('dir')).toBeNull();
});

/* Issue #202 — a paragraph whose direction comes from its STYLE
 * (`RtlBody` basedOn `RtlBase`, which sets `<w:bidi/>`) mirrors as RTL
 * even though its text starts with a Latin word; a direct
 * `<w:bidi w:val="false"/>` over the same style mirrors LTR. Fixture:
 * tools/roundtrip `build_style_bidi_docx`. */
const STYLE_BIDI_FIXTURE = fileURLToPath(
    new URL('../../crates/format-docx/tests/fixtures/pPr_bidi_style.docx', import.meta.url),
);

test('a style-inherited paragraph direction drives the mirrored dir', async ({ page }) => {
    test.setTimeout(60_000);
    await page.goto('/');
    await page.waitForFunction(() => (window as any).__paintIdle === true, undefined, {
        timeout: 20_000,
    });

    const bytes = Array.from(readFileSync(STYLE_BIDI_FIXTURE));
    await page.evaluate(async (arr: number[]) => {
        const dispatch = (cmd: unknown): Promise<any> => (window as any).__dispatch(cmd);
        const loaded = await dispatch({
            type: 'OPEN_DOCUMENT',
            bytes: new Uint8Array(arr),
            format: 'docx',
            name: undefined,
        });
        if (loaded.type === 'ERROR') throw new Error(loaded.message);
        await dispatch({ type: 'PING' });
    }, bytes);

    const paragraphs = page.locator('.a11y-mirror > p');
    await expect(paragraphs).toHaveCount(3);
    await expect(paragraphs.nth(2)).toHaveText('plain');
    const dirs = await paragraphs.evaluateAll((els) => els.map((el) => el.getAttribute('dir')));
    expect(dirs).toEqual([
        'rtl', // style bidi beats the Latin first-strong character
        'ltr', // direct w:val="false" beats the style
        'ltr', // unstyled Latin → first strong
    ]);
});
