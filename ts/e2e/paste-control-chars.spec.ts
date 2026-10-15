import { test, expect, type Page } from '@playwright/test';
import { boot, documentText } from './helpers/editor';
import { readZipEntry } from './helpers/docx';

/* Issue #467 - a pasted control character used to reach `<w:t>` raw: the
 * saved `document.xml` was not well-formed, Word refused the file, and since
 * #468's save gate `SAVE_DOCX` itself FAILED. Text is now cleaned at the
 * input boundary (engine `text_input_guard`: C0 controls dropped, a vertical
 * tab a line break, a form feed a page break, U+FFFE / U+FFFF refused) and
 * the shell refuses a lone UTF-16 surrogate before the engine's string glue
 * can turn it into U+FFFD.
 *
 * Every case pastes through the REAL clipboard path: the hidden textarea's
 * native `paste` event -> `HiddenInput.onPaste` -> `input/clipboard.ts`
 * `paste()` -> `navigator.clipboard.read()` / `readText()` -> the worker.
 * Only the system clipboard itself is stubbed (the OS clipboard does not
 * round-trip control characters reliably, and headless Chrome has none). */

/** Make the page's clipboard read a payload the spec chooses. */
const STUB_CLIPBOARD = (): void => {
    const stub: { plain: string; html: string | undefined; noRead: boolean } = {
        plain: '',
        html: undefined,
        noRead: false,
    };
    (window as any).__pasteStub = stub;
    const clip = navigator.clipboard;
    Object.defineProperty(clip, 'readText', {
        value: async () => stub.plain,
        configurable: true,
    });
    Object.defineProperty(clip, 'read', {
        value: async () => {
            /* `read()` blocked: `paste()` falls back to `readText()`, the only
               tier whose string can hold a lone surrogate (a Blob is UTF-8). */
            if (stub.noRead) throw new DOMException('blocked', 'NotAllowedError');
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

/** A lone surrogate cannot cross CDP (the JSON turns it into U+FFFD), so a
 *  payload spells one `{U+D800}` and the PAGE builds the code unit. */
const UNIT_TOKEN = /\{U\+([0-9A-F]{4})\}/g;

/** Fire the textarea's native `paste` event with `payload` on the clipboard. */
async function paste(
    page: Page,
    payload: { plain?: string; html?: string; noRead?: boolean },
): Promise<void> {
    await page.evaluate(
        ([p, token]) => {
            const decode = (s: string): string =>
                s.replace(new RegExp(token, 'g'), (_, hex: string) =>
                    String.fromCharCode(parseInt(hex, 16)),
                );
            const stub = (window as any).__pasteStub;
            stub.plain = decode(p.plain ?? '');
            stub.html = p.html === undefined ? undefined : decode(p.html);
            stub.noRead = p.noRead ?? false;
        },
        [payload, UNIT_TOKEN.source] as const,
    );
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

/** Save through the engine; the reply must be `DOCUMENT_SAVED`. */
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

const RAW_FORBIDDEN = /[\u0000-\u0008\u000B\u000C\u000E-\u001F￾￿]/;

test.beforeEach(async ({ page }) => {
    await page.addInitScript(STUB_CLIPBOARD);
    await boot(page);
});

test('a pasted U+0001 is dropped, and the document still saves', async ({ page }) => {
    await seed(page, 'seed ');
    await paste(page, { plain: 'a\u0001b' });
    expect(await documentText(page)).toBe('seed ab');

    const xml = await saveDocumentXml(page);
    expect(xml).toContain('seed ab');
    expect(xml).not.toMatch(RAW_FORBIDDEN);
});

test('a vertical tab pastes as a line break, a form feed as a page break', async ({ page }) => {
    await seed(page, '');
    await paste(page, { plain: 'one\u000Btwo\u000Cthree' });
    const xml = await saveDocumentXml(page);
    expect(xml).toContain('<w:br/>');
    expect(xml).toContain('<w:br w:type="page"/>');
    expect(xml).not.toMatch(RAW_FORBIDDEN);
});

test('a rich paste repairs a control character spelled as an entity', async ({ page }) => {
    await seed(page, 'seed ');
    await paste(page, { plain: 'xy', html: '<p>x&#1;y&#xFFFE;z</p>' });
    const xml = await saveDocumentXml(page);
    expect(xml).toContain('xy�z');
    expect(xml).not.toMatch(RAW_FORBIDDEN);
});

test('U+FFFE is refused as InvalidArgument: toast, nothing inserted, saving still works', async ({
    page,
}) => {
    await seed(page, 'seed ');
    await paste(page, { plain: 'a￾b' });
    await expect(page.locator('.nge-toast__message')).toContainText('A value was not valid');
    expect(await documentText(page)).toBe('seed ');
    await saveDocumentXml(page);
});

test('a lone surrogate is refused by the shell before the engine can turn it into U+FFFD', async ({
    page,
}) => {
    await seed(page, 'seed ');
    await paste(page, { plain: 'a{U+D800}b', noRead: true });
    await expect(page.locator('.nge-toast__message')).toContainText('A value was not valid');
    expect(await documentText(page)).toBe('seed ');

    /* The typed reply, straight from the worker. */
    const reply = await page.evaluate(() =>
        (window as any).__dispatch({
            type: 'INSERT_TEXT',
            at: undefined,
            text: 'x' + String.fromCharCode(0xdc00),
        }),
    );
    expect(reply).toMatchObject({
        type: 'ERROR',
        kind: 'InvalidArgument',
        command: 'INSERT_TEXT',
    });
    expect(reply.message).toMatch(/^InsertText: text: .*lone UTF-16 surrogate/);
    expect(await documentText(page)).toBe('seed ');
    await saveDocumentXml(page);
});
