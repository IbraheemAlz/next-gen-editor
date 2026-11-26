import { test, expect, type Page } from '@playwright/test';
import { boot, settle } from './helpers/editor';
import { buildDoc, docSprm } from './helpers/doc';

/* Issue #567 — a legacy Word 97-2003 binary document (`.doc`) opens through
 * the REAL File menu: its hidden file input accepts `.doc`, the @nge/core
 * facade sends it as `docx` (the name never decides the format), and the
 * engine routes it by its compound-file signature to `format-doc`. Before
 * #567 the same file was refused (`InvalidDocument`). Assertions read the
 * accessibility mirror (paragraph text and `dir`, the bold run's inline
 * style) — engine truth, valid under headless Chrome — the open-warnings
 * banner (a list marker is not imported: Honest UX says so), and the
 * File → Save download name (`.doc` -> `.docx`). The `.doc` is built in
 * memory (`helpers/doc.ts`); no binary fixture is committed.
 *
 * Issue #576 — a `.doc` table opens as a TABLE: the mirror holds a
 * `<table role="table">` (its rows, its cells, the merged cell's
 * `aria-colspan`) where it used to hold one paragraph per cell, nothing
 * about the table is reported as not imported, its column widths place
 * the cells (the caret walks them with the arrow keys of the hidden
 * textarea), and typing lands in a cell. */

const DOC = buildDoc([
    {
        runs: [
            { text: 'Legacy ' },
            { text: 'bold', bold: true },
            { text: ' document' },
        ],
    },
    { runs: [{ text: 'مرحبا بالعالم' }], rtl: true, center: true },
    { runs: [{ text: 'cell text' }], cell: true },
    { runs: [{ text: 'second cell' }], cell: true },
    { runs: [], rowEnd: [-108, 2772, 5652] },
    { runs: [{ text: 'listed' }], list: true },
    { runs: [{ text: 'Last paragraph.' }] },
]);

async function openDoc(page: Page, name: string, bytes: Uint8Array): Promise<void> {
    await page.locator('.nge-fm__trigger').click();
    const menu = page.locator('.nge-fm__menu').first();
    await expect(menu).toContainText('.doc ');
    await expect(menu).not.toContainText('Engine pending');
    await page.locator('.nge-fm input[type="file"]').setInputFiles({
        name,
        mimeType: 'application/msword',
        buffer: Buffer.from(bytes),
    });
}

test('a .doc opens through File -> Open with its text, bold run, RTL paragraph and table', async ({
    page,
}) => {
    test.setTimeout(60_000);
    await boot(page);
    await openDoc(page, 'legacy.doc', DOC);

    const mirror = page.locator('.a11y-mirror');
    const paragraphs = mirror.locator(':scope > p');
    await expect(paragraphs).toHaveCount(4, { timeout: 15_000 });
    await expect(paragraphs.nth(0)).toHaveText('Legacy bold document');
    await expect(paragraphs.nth(0)).toHaveAttribute('dir', 'ltr');
    /* One span per style run; only the bold run carries the weight. */
    const spans = paragraphs.nth(0).locator('span');
    await expect(spans).toHaveText(['Legacy ', 'bold', ' document']);
    await expect(spans.nth(1)).toHaveCSS('font-weight', '700');
    await expect(spans.nth(0)).not.toHaveCSS('font-weight', '700');
    await expect(paragraphs.nth(1)).toHaveText('مرحبا بالعالم');
    await expect(paragraphs.nth(1)).toHaveAttribute('dir', 'rtl');
    /* #576: the cells are a table, not paragraphs. */
    const table = mirror.locator(':scope > table[role="table"]');
    await expect(table).toHaveCount(1);
    await expect(table.locator('td[role="gridcell"]')).toHaveText(['cell text', 'second cell']);
    await expect(paragraphs.nth(2)).toHaveText('listed');
    await expect(paragraphs.nth(3)).toHaveText('Last paragraph.');
    /* No refusal banner: the file opened. */
    await expect(page.locator('.nge-fm__error')).toHaveCount(0);

    /* Honest UX: the list marker that was not imported is reported; the
     * table was imported, so it is not. */
    const warnings = page.locator('.nge-open-warnings');
    await expect(warnings).toBeVisible();
    await expect(warnings).toHaveAttribute('data-kinds', 'LegacyContentNotImported');
    await expect(warnings).toContainText('Word 97-2003 (.doc)');
    const items = warnings.locator('.nge-open-warnings__item');
    await expect(items).toHaveCount(1);
    await expect(items).toContainText('list numbering');
    await expect(items).not.toContainText('tables');

    /* A save writes .docx, named after the opened file. */
    const download = page.waitForEvent('download');
    await page.locator('.nge-fm__trigger').click();
    await page.locator('.nge-fm__item[data-nge-command="SAVE_DOCUMENT"]').first().click();
    expect((await download).suggestedFilename()).toBe('legacy.docx');
});

/* Issue #575 — the style sheet and the font table come in. Normal is
 * right-to-left (sprmPFBiDi in its UpxPapx) in 12 pt Arial, Heading 1 is
 * bold 16 pt and "Quote Char" an italic character style. A paragraph that
 * says nothing itself — Latin text included — is RTL through Normal; one
 * with a direct left-to-right sprm stays LTR; the run in "Quote Char" is
 * italic. Before #575 the style sheet was never read: the Latin
 * paragraphs resolved LTR from their text, the run stayed upright, and the
 * open reported "styles and fonts" as not imported. */
const STYLED_DOC = buildDoc(
    [
        { runs: [{ text: 'Plain body text' }] },
        { runs: [{ text: 'Chapter One' }], istd: 1 },
        {
            runs: [{ text: 'Before ' }, { text: 'quoted', charStyle: 15 }, { text: ' after' }],
        },
        { runs: [{ text: 'Left to right' }], rtl: false },
    ],
    {
        styles: [
            {
                sti: 0,
                kind: 'paragraph',
                name: 'Normal',
                papx: [...docSprm.rtlParagraph(true), ...docSprm.spaceAfter(160)],
                chpx: [...docSprm.font(1), ...docSprm.sizeHalfPoints(24)],
            },
            {
                sti: 1,
                kind: 'paragraph',
                name: 'heading 1',
                base: 0,
                next: 0,
                chpx: [...docSprm.bold(true), ...docSprm.sizeHalfPoints(32)],
            },
            ...Array<null>(8).fill(null),
            { sti: 65, kind: 'character', name: 'Default Paragraph Font' },
            ...Array<null>(4).fill(null),
            {
                sti: 0xffe,
                kind: 'character',
                name: 'Quote Char',
                base: 10,
                chpx: docSprm.italic(true),
            },
        ],
        fonts: ['Times New Roman', 'Arial'],
    },
);

test('a styled .doc opens with the direction and formatting its styles give', async ({ page }) => {
    test.setTimeout(60_000);
    await boot(page);

    await page.locator('.nge-fm__trigger').click();
    await page.locator('.nge-fm input[type="file"]').setInputFiles({
        name: 'styled.doc',
        mimeType: 'application/msword',
        buffer: Buffer.from(STYLED_DOC),
    });

    const paragraphs = page.locator('.a11y-mirror > p');
    await expect(paragraphs).toHaveCount(4, { timeout: 15_000 });
    await expect(paragraphs.nth(0)).toHaveText('Plain body text');
    /* Engine truth: each paragraph's resolved base direction. */
    await expect(paragraphs.nth(0)).toHaveAttribute('dir', 'rtl');
    await expect(paragraphs.nth(1)).toHaveText('Chapter One');
    await expect(paragraphs.nth(1)).toHaveAttribute('dir', 'rtl');
    await expect(paragraphs.nth(2)).toHaveAttribute('dir', 'rtl');
    await expect(paragraphs.nth(3)).toHaveAttribute('dir', 'ltr');
    const spans = paragraphs.nth(2).locator('span');
    await expect(spans).toHaveText(['Before ', 'quoted', ' after']);
    await expect(spans.nth(1)).toHaveCSS('font-style', 'italic');
    await expect(spans.nth(0)).not.toHaveCSS('font-style', 'italic');
    /* Nothing was left out: no open warnings, no refusal. */
    await expect(page.locator('.nge-fm__error')).toHaveCount(0);
    await expect(page.locator('.nge-open-warnings')).toHaveCount(0);
});

const INPUT = 'textarea[data-nge-hidden-input]';

/** The caret overlay's top-left in the viewport's scroll content (CSS px). */
async function caretAt(page: Page): Promise<{ x: number; y: number }> {
    await settle(page);
    return page.evaluate(() => {
        const c = document.querySelector<HTMLElement>('.caret');
        const v = document.querySelector<HTMLElement>('.editor-viewport');
        if (!c || !v) throw new Error('no caret overlay / viewport');
        const r = c.getBoundingClientRect();
        const vr = v.getBoundingClientRect();
        return { x: r.left - vr.left + v.scrollLeft, y: r.top - vr.top + v.scrollTop };
    });
}

async function press(page: Page, key: string): Promise<{ x: number; y: number }> {
    await page.keyboard.press(key);
    return caretAt(page);
}

test('a .doc table opens with its grid, a merged cell and its column widths, and takes typing', async ({
    page,
}) => {
    test.setTimeout(90_000);
    await boot(page);
    /* Row 1: three columns 1 in, 2 in, 1 in wide (Word's default table:
     * the first edge one cell margin left of the text margin). Row 2: one
     * cell over the first two columns (how Word 2000+ stores a horizontal
     * merge), then the third column. */
    await openDoc(
        page,
        'table.doc',
        buildDoc([
            { runs: [{ text: 'Before the table.' }] },
            { runs: [{ text: 'A1' }], cell: true },
            { runs: [{ text: 'B1' }], cell: true },
            { runs: [{ text: 'C1' }], cell: true },
            { runs: [], rowEnd: [-108, 1332, 4212, 5652] },
            { runs: [{ text: 'wide' }], cell: true },
            { runs: [{ text: 'C2' }], cell: true },
            { runs: [], rowEnd: [-108, 4212, 5652] },
            { runs: [{ text: 'After the table.' }] },
        ]),
    );

    const mirror = page.locator('.a11y-mirror');
    const table = mirror.locator(':scope > table[role="table"]');
    await expect(table).toHaveCount(1, { timeout: 15_000 });
    await expect(mirror.locator(':scope > p')).toHaveText(['Before the table.', 'After the table.']);
    await expect(table.locator('tr[role="row"]')).toHaveCount(2);
    const cells = table.locator('td[role="gridcell"]');
    await expect(cells).toHaveText(['A1', 'B1', 'C1', 'wide', 'C2']);
    await expect(cells.nth(3)).toHaveAttribute('aria-colspan', '2');
    expect(await cells.nth(0).getAttribute('aria-colspan')).toBeNull();
    /* A clean open: nothing about the table is reported. */
    await expect(page.locator('.nge-open-warnings')).toHaveCount(0);

    /* Real keys on the hidden textarea: down from the first paragraph
     * into cell A1, then right across the row's cells. */
    const input = page.locator(INPUT);
    await input.focus();
    const before = await press(page, 'Control+Home');
    const a1 = await press(page, 'ArrowDown');
    expect(a1.y, 'into the first row').toBeGreaterThan(before.y);
    await press(page, 'End');
    const b1 = await press(page, 'ArrowRight');
    await press(page, 'End');
    const c1 = await press(page, 'ArrowRight');
    /* The file's cell edges place the text: A1 starts at the text margin
     * (the table's leading edge sits one cell margin left of it), and the
     * columns are 1 in and 2 in wide, so C1 starts three times as far from
     * A1 as B1 does — whatever the zoom. */
    expect(Math.abs(a1.x - before.x), `A1 ${a1.x} vs margin ${before.x}`).toBeLessThan(2);
    expect(Math.abs(b1.y - a1.y) < 1 && Math.abs(c1.y - a1.y) < 1, 'one row').toBe(true);
    const ratio = (c1.x - a1.x) / (b1.x - a1.x);
    expect(ratio, `column ratio ${ratio} (A1 ${a1.x}, B1 ${b1.x}, C1 ${c1.x})`).toBeGreaterThan(2.9);
    expect(ratio).toBeLessThan(3.1);

    /* Typing at the caret lands in cell C1. */
    await page.keyboard.type('!');
    await expect(cells.nth(2)).toHaveText('!C1');
    await expect(cells).toHaveText(['A1', 'B1', '!C1', 'wide', 'C2']);
});
