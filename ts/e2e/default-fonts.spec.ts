import { test, expect, type Page } from '@playwright/test';
import { boot } from './helpers/editor';

/* Issue #472 - the per-script DEFAULT face (text that names no font)
 * through the REAL shell + worker + WASM engine.
 *
 * The boot sequence loads Amiri (Arabic + Latin glyphs) and Liberation Sans
 * (Latin only). By face id alone `amiri` sorts first and covers Latin, so
 * the interactive editor used to shape Latin text with Amiri's glyphs and
 * tall line metrics. `fonts.json` `defaults_by_script` now declares Latin ->
 * Liberation Sans and Arabic -> Amiri; the shell sends it as
 * `SET_FONT_PRIORITY` after the fonts load.
 *
 * Assertions read engine truth - the `SET_SELECTION` reply's font read-back
 * (`resolved_font_*` display names, `font_source`) - never a canvas screenshot (headless
 * Chrome does not composite the interactive canvas, CLAUDE.md). */

/** The boot seed `setupEngine` paints. */
const SEED = 'Hello world مرحبا بالعالم';
/** UTF-8 byte offsets into the seed paragraph. */
const IN_LATIN = Buffer.byteLength(SEED.slice(0, 3), 'utf8');
const IN_ARABIC = Buffer.byteLength(SEED.slice(0, SEED.indexOf('م') + 2), 'utf8');

/** Collapse the engine caret at `(block 0, offset)`; the reply. */
async function caretAt(page: Page, offset: number): Promise<any> {
    return page.evaluate(async (offset) => {
        const at = { path: { steps: [{ kind: 'BLOCK', idx: 0 }] }, offset };
        return (window as any).__dispatch({
            type: 'SET_SELECTION',
            range: { start: at, end: at },
            caret: at,
        });
    }, offset);
}

test('#472: Latin text with no font shapes with Liberation Sans, Arabic with Amiri', async ({
    page,
}) => {
    test.setTimeout(45_000);
    await boot(page);

    const latin = await caretAt(page, IN_LATIN);
    expect(latin.type).toBe('SELECTION_CHANGED');
    expect(latin.caret_font_slot).toBe('Latin');
    expect(latin.resolved_font_latin).toBe('Liberation Sans');
    expect(latin.font_source.latin).toBe('Default');
    expect(latin.slot_formats.latin.font_family).toBe('liberation');

    const arabic = await caretAt(page, IN_ARABIC);
    expect(arabic.caret_font_slot).toBe('ComplexScript');
    expect(arabic.resolved_font_cs).toBe('Amiri');
    expect(arabic.font_source.complex_script).toBe('Default');
    /* The Latin slot of an Arabic caret is still the Latin default. */
    expect(arabic.resolved_font_latin).toBe('Liberation Sans');
});

test('#472: the declared defaults are what moves Latin - an empty table falls back to the first covering id', async ({
    page,
}) => {
    test.setTimeout(45_000);
    await boot(page);

    /* The engine reports the EFFECTIVE table (loaded, covering ids). */
    const applied = await page.evaluate(() => (window as any).__fontRegistry.applyScriptDefaults());
    expect(applied).toEqual([
        { script: 'Latin', ids: ['liberation'] },
        { script: 'Arabic', ids: ['amiri', 'noto-naskh'] },
        /* Issue #490 - script-neutral text and the list-marker glyph
         * fallback have an explicit default, not the seed face implicitly. */
        { script: 'Common', ids: ['liberation'] },
    ]);

    /* Declaring nothing is the pre-#472 behaviour: `amiri` < `liberation`
     * and Amiri covers Latin, so it answers Latin text. */
    const cleared = await page.evaluate(() =>
        (window as any).__dispatch({ type: 'SET_FONT_PRIORITY', priorities: [] }),
    );
    expect(cleared).toEqual({ type: 'FONT_PRIORITY_APPLIED', priorities: [] });
    expect((await caretAt(page, IN_LATIN)).resolved_font_latin).toBe('Amiri');

    /* ... and the shell's declaration restores it (what a recovery does). */
    await page.evaluate(() => (window as any).__fontRegistry.applyScriptDefaults());
    expect((await caretAt(page, IN_LATIN)).resolved_font_latin).toBe('Liberation Sans');
});

test('#472: crash recovery re-declares the defaults on the respawned engine', async ({ page }) => {
    test.setTimeout(60_000);
    await boot(page);

    await page.evaluate(() => (window as any).__engineClient.forceTrap());
    await page.waitForFunction(() => (window as any).__recovered === true, undefined, {
        timeout: 30_000,
    });
    await page.waitForFunction(() => (window as any).__paintIdle === true, undefined, {
        timeout: 30_000,
    });

    const latin = await caretAt(page, IN_LATIN);
    expect(latin.type).toBe('SELECTION_CHANGED');
    expect(latin.resolved_font_latin).toBe('Liberation Sans');
    expect((await caretAt(page, IN_ARABIC)).resolved_font_cs).toBe('Amiri');
});
