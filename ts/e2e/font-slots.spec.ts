import { test, expect, type Page } from '@playwright/test';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { boot } from './helpers/editor';

/* Issues #423 / #420 — complex-script font slots end to end through the
 * REAL shell + worker + WASM engine.
 *
 * `theme_word_default.docx` (`format_docx::test_fixtures::
 * theme_word_default_docx`, committed under the format-docx fixtures) is
 * Word's default template: docDefaults bind every font slot to the theme
 * (`minorHAnsi` → Calibri, `minorBidi` → the theme's `Arab` row → Arial),
 * so its body runs name no font at all. Before #423 the toolbar picker
 * could only show the layout default id there.
 *
 * Assertions read engine truth (the `SET_SELECTION` reply) and the DOM
 * of the toolbar picker — never a canvas screenshot (see CLAUDE.md:
 * headless Chrome does not composite the interactive canvas). */

const THEME_FIXTURE = fileURLToPath(
    new URL('../../crates/format-docx/tests/fixtures/theme_word_default.docx', import.meta.url),
);

/** `THEME_FIXTURE_TEXTS[2]` — the RTL Arabic paragraph. */
const ARABIC = 'نص عربي بخط السمة بخط العناوين.';

/** UTF-8 byte offset of `needle` in `text`, plus `extra` bytes. */
function byteOffset(text: string, needle: string, extra = 0): number {
    const at = text.indexOf(needle);
    if (at < 0) throw new Error(`needle ${needle} not in ${text}`);
    return Buffer.byteLength(text.slice(0, at), 'utf8') + extra;
}

async function openDocx(page: Page, path: string, name: string): Promise<void> {
    const bytes = Array.from(readFileSync(path));
    await page.evaluate(
        async ({ arr, name }) => {
            const loaded = await (window as any).__dispatch({
                type: 'OPEN_DOCUMENT',
                bytes: new Uint8Array(arr),
                format: 'docx',
                name,
            });
            if (loaded.type === 'ERROR') throw new Error(loaded.message);
        },
        { arr: bytes, name },
    );
}

/** Collapse the engine caret at `(block, offset)`; the reply. */
async function caretAt(page: Page, block: number, offset: number): Promise<any> {
    return page.evaluate(
        async ({ block, offset }) => {
            const at = { path: { steps: [{ kind: 'BLOCK', idx: block }] }, offset };
            return (window as any).__dispatch({
                type: 'SET_SELECTION',
                range: { start: at, end: at },
                caret: at,
            });
        },
        { block, offset },
    );
}

/** The text of the toolbar family picker's selected option. */
async function pickerLabel(page: Page): Promise<string | undefined> {
    return page
        .locator('select.nge-font__family')
        .evaluate((el: HTMLSelectElement) => el.selectedOptions[0]?.textContent?.trim());
}

test('#423: the picker shows the theme-resolved family, marked (theme); Arabic text shows the cs family', async ({
    page,
}) => {
    test.setTimeout(45_000);
    await boot(page);
    await openDocx(page, THEME_FIXTURE, 'theme_word_default.docx');

    /* Body paragraph: Calibri through docDefaults' theme binding. */
    const body = await caretAt(page, 1, 3);
    expect(body.type).toBe('SELECTION_CHANGED');
    expect(body.resolved_font_latin).toBe('Calibri');
    expect(body.resolved_font_cs).toBe('Arial');
    expect(body.font_source).toEqual({ latin: 'Theme', complex_script: 'Theme' });
    expect(body.caret_font_slot).toBe('Latin');
    expect(body.slot_formats.latin.font_family).toBe('calibri');
    await expect.poll(() => pickerLabel(page)).toBe('Calibri');
    await expect(page.locator('.nge-font__source')).toHaveText('(theme)');

    /* Arabic paragraph: the complex-script slot is the active one. */
    const arabic = await caretAt(page, 2, byteOffset(ARABIC, 'عربي', 2));
    expect(arabic.caret_font_slot).toBe('ComplexScript');
    expect(arabic.resolved_font_cs).toBe('Arial');
    await expect.poll(() => pickerLabel(page)).toBe('Arial');
    await expect(page.locator('.nge-font__source')).toHaveText('(theme)');

    /* The run that rebinds only its cs slot (`w:cstheme="majorBidi"`). */
    const rebound = await caretAt(page, 2, byteOffset(ARABIC, 'العناوين', 4));
    expect(rebound.resolved_font_cs).toBe('Times New Roman');
    expect(rebound.resolved_font_latin).toBe('Calibri');
    await expect.poll(() => pickerLabel(page)).toBe('Times New Roman');

    /* The explicit-Amiri run: a registry font, named by the run itself —
     * no theme marker. */
    const bodyText = 'Body text in the minor font, explicit Amiri, then the theme again.';
    const explicit = await caretAt(page, 1, byteOffset(bodyText, 'explicit Amiri', 4));
    expect(explicit.font_source).toEqual({ latin: 'Explicit', complex_script: 'Explicit' });
    await expect.poll(() => pickerLabel(page)).toMatch(/^Amiri/);
    await expect(page.locator('.nge-font__source')).toHaveCount(0);
});
