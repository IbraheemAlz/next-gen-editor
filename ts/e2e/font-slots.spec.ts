import { test, expect, type Page } from '@playwright/test';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { boot, burst, settle } from './helpers/editor';
import { readZipEntry } from './zip-append';

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

/** `attrs_at_caret` (the slot the text reads) + `slot_formats` of a range
 *  of body paragraph `block`. */
async function rangeFormat(page: Page, block: number, start: number, end: number): Promise<any> {
    return page.evaluate(
        async ({ block, start, end }) => {
            const at = (offset: number) => ({
                path: { steps: [{ kind: 'BLOCK', idx: block }] },
                offset,
            });
            const evt = await (window as any).__dispatch({
                type: 'SET_SELECTION',
                range: { start: at(start), end: at(end) },
                caret: at(end),
            });
            return {
                size: evt.attrs_at_caret.font_size as number,
                bold: evt.attrs_at_caret.bold as boolean,
                slot: evt.caret_font_slot as string,
                formats: evt.slot_formats,
            };
        },
        { block, start, end },
    );
}

test('#420: an Arabic-only size + bold from the Font dialog — typed Latin and Arabic differ, and a save + reopen keeps it', async ({
    page,
}) => {
    test.setTimeout(45_000);
    await boot(page);
    await caretAt(page, 0, 0);

    /* Open the dialog from the toolbar; it seeds both sections from the
     * caret run's resolved values. */
    await page.locator('button.nge-font__more').click();
    const dialog = page.locator('.nge-font-dialog');
    await expect(dialog).toBeVisible();
    const field = (slot: string, name: string) =>
        dialog.locator(`[data-nge-font-slot="${slot}"][data-nge-field="${name}"]`);
    const latinSeed = Number(await field('Latin', 'size').inputValue());
    expect(latinSeed).toBeGreaterThan(0);
    expect(Number(await field('ComplexScript', 'size').inputValue())).toBe(latinSeed);
    /* Every control names the command it ends in (issue #342 parity). */
    for (const name of ['family', 'size', 'bold', 'italic']) {
        for (const slot of ['Latin', 'ComplexScript']) {
            await expect(field(slot, name)).toHaveAttribute('data-nge-command', 'APPLY_FORMATTING');
        }
    }

    const ARABIC_PT = latinSeed === 24 ? 28 : 24;
    await field('ComplexScript', 'size').fill(String(ARABIC_PT));
    await field('ComplexScript', 'bold').check();
    await page.locator('.nge-dialog__footer button', { hasText: 'Apply' }).click();
    await expect(dialog).toHaveCount(0);

    /* The collapsed caret armed the formatting; type Latin, then Arabic,
     * through the hidden textarea. */
    const LATIN = 'abc ';
    const ARABIC = 'نص';
    await burst(page, [LATIN, ARABIC]);
    await settle(page);
    const latinEnd = 3;
    const arabicStart = Buffer.byteLength(LATIN, 'utf8');
    const arabicEnd = arabicStart + Buffer.byteLength(ARABIC, 'utf8');

    const check = async (when: string) => {
        const latin = await rangeFormat(page, 0, 0, latinEnd);
        const arabic = await rangeFormat(page, 0, arabicStart, arabicEnd);
        expect(latin.slot, when).toBe('Latin');
        expect(arabic.slot, when).toBe('ComplexScript');
        expect(arabic.size, `${when}: the Arabic is at the complex-script size`).toBe(ARABIC_PT);
        expect(arabic.bold, `${when}: bCs`).toBe(true);
        expect(latin.size, `${when}: the Latin keeps its size`).toBe(latinSeed);
        expect(latin.bold, `${when}: no <w:b>`).toBe(false);
        expect(latin.size).not.toBe(arabic.size);
        /* Both slots, wherever the caret sits. */
        expect(arabic.formats.complex_script.font_size).toBe(ARABIC_PT);
        expect(arabic.formats.latin.font_size).toBe(latinSeed);
    };
    await check('typed');

    /* Round trip: save, check the saved run properties, reopen. */
    const saved: number[] = await page.evaluate(async () => {
        const evt = await (window as any).__dispatch({ type: 'SAVE_DOCUMENT', format: 'docx' });
        if (evt.type !== 'DOCUMENT_SAVED') throw new Error(JSON.stringify(evt));
        return Array.from(evt.bytes as Uint8Array);
    });
    const xml = Buffer.from(readZipEntry(new Uint8Array(saved), 'word/document.xml')!).toString(
        'utf8',
    );
    const halfPoints = ARABIC_PT * 2;
    expect(xml).toContain(`<w:szCs w:val="${halfPoints}"/>`);
    expect(xml).toContain('<w:bCs/>');
    expect(xml, 'the Latin slot was never written').not.toContain(`<w:sz w:val="${halfPoints}"/>`);
    expect(xml).not.toMatch(/<w:b\/>/);

    await page.evaluate(async (arr: number[]) => {
        const loaded = await (window as any).__dispatch({
            type: 'OPEN_DOCUMENT',
            bytes: new Uint8Array(arr),
            format: 'docx',
            name: 'font-slots.docx',
        });
        if (loaded.type === 'ERROR') throw new Error(loaded.message);
    }, saved);
    await check('reopened');
});
