import { test, expect } from '@playwright/test';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { boot } from './helpers/editor';

/* Issue #329 — metric-compatible font substitution end to end through the
 * REAL shell + worker + WASM engine.
 *
 * The boot sequence loads the manifest's `substitutes` (Carlito, Caladea,
 * Liberation Serif / Mono, Gelasio, Selawik, Noto Naskh Arabic) next to
 * the defaults. `theme_word_default.docx` is Word's default template: its
 * runs name Calibri / Calibri Light (Latin) and Arial / Times New Roman
 * (Arabic) through the theme — none of which the editor ships. The open
 * reply reports what layout substitutes, and the Dev HUD lists it.
 *
 * Assertions read engine truth (the `OPEN_DOCUMENT` reply) and the HUD's
 * DOM — never a canvas screenshot (CLAUDE.md: headless Chrome does not
 * composite the interactive canvas). */

const THEME_FIXTURE = fileURLToPath(
    new URL('../../crates/format-docx/tests/fixtures/theme_word_default.docx', import.meta.url),
);

test('#329: an opened document reports its font substitutions and the Dev HUD lists them', async ({
    page,
}) => {
    test.setTimeout(45_000);
    await boot(page);

    /* The substitutes are resident once boot settles. */
    const resident = await page.evaluate(() => {
        const reg = (window as any).__fontRegistry;
        return ['carlito', 'caladea', 'liberation-serif', 'noto-naskh'].map((id) =>
            reg.isLoaded(id),
        );
    });
    expect(resident).toEqual([true, true, true, true]);

    const bytes = Array.from(readFileSync(THEME_FIXTURE));
    const loaded = await page.evaluate(
        async (arr) =>
            (window as any).__dispatch({
                type: 'OPEN_DOCUMENT',
                bytes: new Uint8Array(arr),
                format: 'docx',
                name: 'theme_word_default.docx',
            }),
        bytes,
    );
    expect(loaded.type).toBe('DOCUMENT_LOADED');
    expect(loaded.substituted).toEqual([
        {
            family: 'Calibri',
            slot: 'Latin',
            substitute: 'Carlito',
            substitute_id: 'carlito',
            metric_compatible: true,
        },
        {
            family: 'Calibri Light',
            slot: 'Latin',
            substitute: 'Carlito',
            substitute_id: 'carlito',
            metric_compatible: false,
        },
        {
            family: 'Arial',
            slot: 'ComplexScript',
            substitute: 'Noto Naskh Arabic',
            substitute_id: 'noto-naskh',
            metric_compatible: false,
        },
        {
            family: 'Times New Roman',
            slot: 'ComplexScript',
            substitute: 'Noto Naskh Arabic',
            substitute_id: 'noto-naskh',
            metric_compatible: false,
        },
    ]);

    await page.evaluate(() => window.dispatchEvent(new Event('nge-toggle-hud')));
    const rows = page.locator('.nge-hud .nge-hud__substitution');
    await expect(rows).toHaveText([
        'Calibri → Carlito',
        'Calibri Light → Carlito',
        'Arial (CS) → Noto Naskh Arabic',
        'Times New Roman (CS) → Noto Naskh Arabic',
    ]);
    /* Metric-compatible clones read normally; closest-style picks muted. */
    await expect(rows.nth(0)).not.toHaveClass(/nge-hud__substitution--approx/);
    await expect(rows.nth(2)).toHaveClass(/nge-hud__substitution--approx/);
});
