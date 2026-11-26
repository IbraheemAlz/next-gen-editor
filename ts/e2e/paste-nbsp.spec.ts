import { test, expect, type Page } from '@playwright/test';
import { boot, documentText } from './helpers/editor';
import { readZipEntry } from './helpers/docx';

/* Issue #599 - `&nbsp;` in pasted / opened HTML decoded to an ASCII space and
 * the parser's whitespace collapse then merged a run of them into one, so
 * Word's `&nbsp;` indentation and a browser's `a&nbsp;&nbsp;&nbsp;b` lost
 * their gaps (and a blank line spelled `<p>&nbsp;</p>` was dropped). It now
 * decodes to U+00A0, which the collapse leaves alone, layout never breaks a
 * line at, and the `.docx` writer saves as the literal character.
 *
 * Every paste goes through the REAL clipboard path (the hidden textarea's
 * native `paste` event -> `HiddenInput.onPaste` -> `input/clipboard.ts` ->
 * `navigator.clipboard.read()` -> the worker); only the OS clipboard itself
 * is stubbed (headless Chrome has none). The open case drives the real File
 * menu's file input. */

const NBSP = ' ';

const STUB_CLIPBOARD = (): void => {
    const stub: { plain: string; html: string | undefined } = { plain: '', html: undefined };
    (window as any).__pasteStub = stub;
    const clip = navigator.clipboard;
    Object.defineProperty(clip, 'readText', {
        value: async () => stub.plain,
        configurable: true,
    });
    Object.defineProperty(clip, 'read', {
        value: async () => {
            const data: Record<string, Blob> = {};
            if (stub.html !== undefined) data['text/html'] = new Blob([stub.html], { type: 'text/html' });
            if (stub.plain !== '') data['text/plain'] = new Blob([stub.plain], { type: 'text/plain' });
            return [{ types: Object.keys(data), getType: async (t: string) => data[t] }];
        },
        configurable: true,
    });
};

/** Replace the document with `seed`, caret after it. */
async function seed(page: Page, text: string): Promise<void> {
    await page.evaluate(async (t) => {
        const dispatch = (window as any).__dispatch;
        await dispatch({ type: 'SELECT_ALL' });
        await dispatch({ type: 'INSERT_TEXT', at: undefined, text: t });
    }, text);
}

/** Fire the textarea's native `paste` event with `payload` on the clipboard. */
async function paste(page: Page, payload: { plain?: string; html?: string }): Promise<void> {
    await page.evaluate((p) => {
        const stub = (window as any).__pasteStub;
        stub.plain = p.plain ?? '';
        stub.html = p.html;
    }, payload);
    await page.locator('textarea[data-nge-hidden-input]').focus();
    await page.evaluate(() => {
        document
            .querySelector('textarea[data-nge-hidden-input]')!
            .dispatchEvent(new ClipboardEvent('paste', { bubbles: true, cancelable: true }));
    });
    /* `paste()` is fire-and-forget in the handler: wait for the worker. */
    await page.evaluate(async () => {
        await (window as any).__dispatch({ type: 'GET_SELECTION_AS_CLIPBOARD' });
    });
}

/** Save through the engine and return the saved `word/document.xml`. */
async function saveDocumentXml(page: Page): Promise<string> {
    const saved = await page.evaluate(async () => {
        const evt = await (window as any).__dispatch({ type: 'SAVE_DOCX' });
        return evt.type === 'DOCUMENT_SAVED'
            ? { ok: true, bytes: Array.from(evt.bytes as Uint8Array) }
            : { ok: false, evt };
    });
    expect(saved.ok, `SAVE_DOCX must succeed: ${JSON.stringify(saved.evt)}`).toBe(true);
    return readZipEntry(new Uint8Array(saved.bytes as number[]), 'word/document.xml');
}

test.beforeEach(async ({ page }) => {
    await page.addInitScript(STUB_CLIPBOARD);
    await boot(page);
});

test('a rich paste keeps every &nbsp; of a run as its own U+00A0', async ({ page }) => {
    await seed(page, 'seed ');
    /* The plain flavour carries ordinary spaces, as a browser's does; the
       rich flavour is what must win. */
    await paste(page, { plain: 'a   b', html: '<p>a&nbsp;&nbsp;&nbsp;b</p>' });
    expect(await documentText(page)).toBe(`seed a${NBSP}${NBSP}${NBSP}b`);

    /* The save writes the literal character inside a preserving `<w:t>`. */
    const xml = await saveDocumentXml(page);
    expect(xml).toContain(`seed a${NBSP}${NBSP}${NBSP}b`);
    expect(xml).not.toContain('seed a b');
});

test("Word's clipboard HTML keeps its indentation run and its blank line", async ({ page }) => {
    await seed(page, '');
    await paste(page, {
        plain: 'indented\n\ntail',
        html:
            '<p class=MsoNormal>&nbsp;&nbsp;&nbsp;&nbsp;indented</p>' +
            '<p class=MsoNormal><o:p>&nbsp;</o:p></p>' +
            '<p class=MsoNormal>tail</p>',
    });
    expect(await documentText(page)).toBe(
        `${NBSP}${NBSP}${NBSP}${NBSP}indented\n${NBSP}\ntail`,
    );
});

test('the numeric spellings and ordinary spaces beside them paste the same', async ({ page }) => {
    await seed(page, '');
    await paste(page, {
        plain: 'a b',
        html: '<p>a&#160;&#xA0; \n   &nbsp;b</p>',
    });
    /* Source formatting whitespace collapses to ONE ASCII space; each
       no-break space is kept apart from it. */
    expect(await documentText(page)).toBe(`a${NBSP}${NBSP} ${NBSP}b`);
});

test('an opened .html file keeps its &nbsp; runs', async ({ page }) => {
    test.setTimeout(60_000);
    await page.locator('.nge-fm input[type="file"]').setInputFiles({
        name: 'spaced.html',
        mimeType: 'text/html',
        buffer: Buffer.from(
            '<!DOCTYPE html><html><head><title>t&nbsp;t</title></head><body>' +
                '<p>a&nbsp;&nbsp;&nbsp;b</p><p>&nbsp;</p><p>tail</p></body></html>',
            'utf8',
        ),
    });
    await expect(page.locator('.a11y-mirror')).toContainText('tail', { timeout: 15_000 });
    expect(await documentText(page)).toBe(`a${NBSP}${NBSP}${NBSP}b\n${NBSP}\ntail`);
});
