import { test, expect, type Page } from '@playwright/test';
import { boot, documentText, settle } from './helpers/editor';
import { RED, clickRect, insertPng, waitForImages } from './helpers/images';

/* Issue #580 — the plain-text flavour of a copied picture is the model's
 * object sentinel `U+FFFC`. Pasting it unformatted (Ctrl+Shift+V, which
 * reads `text/plain` only) used to insert a bare `U+FFFC` with no picture
 * behind it: an orphan every reader of "U+FFFC <=> an inline object" then
 * tripped on. The one text-input table (`engine::text_input`) now drops it
 * on every text door, so the paste inserts nothing.
 *
 * Real UI: the picture enters through the toolbar's file input, a mouse
 * click selects it, Ctrl+C copies it, Ctrl+Shift+V leaves the object
 * selection with the caret before the picture (memo §9, `image-keys.ts`)
 * and pastes unformatted (`HiddenInput` -> `paste(client, true)`).
 * `__dispatch` only seeds the document and reads results. */

test.use({ viewport: { width: 1280, height: 1400 } });

/** `above` and an empty paragraph with the caret in it. */
async function seed(page: Page): Promise<void> {
    await page.evaluate(async () => {
        const d = (c: unknown): Promise<any> => (window as any).__dispatch(c);
        await d({ type: 'SELECT_ALL' });
        await d({ type: 'INSERT_TEXT', at: undefined, text: 'above' });
        await d({ type: 'SPLIT_PARAGRAPH', at: undefined });
    });
}

/** The system clipboard's `text/plain` ('' while a write is in flight). */
const clipboardPlain = (page: Page): Promise<string> =>
    page.evaluate(async () => {
        try {
            for (const item of await navigator.clipboard.read()) {
                if (item.types.includes('text/plain')) {
                    return (await item.getType('text/plain')).text();
                }
            }
        } catch {
            /* "Clipboard data has changed": poll again */
        }
        return '';
    });

const sentinels = (text: string): number => [...text].filter((c) => c === '\u{FFFC}').length;

test('Ctrl+Shift+V of a copied picture inserts no orphan object sentinel', async ({ page, context }) => {
    await context.grantPermissions(['clipboard-read', 'clipboard-write']);
    await boot(page);
    await seed(page);
    await insertPng(page, RED);
    const [img] = await waitForImages(page, 1);
    await settle(page);
    const before = await documentText(page);
    expect(before).toBe('above\n\u{FFFC}');

    /* Copy the picture: its text/plain flavour is the bare sentinel. */
    await page.evaluate(() => navigator.clipboard.writeText('before'));
    await clickRect(page, img!.rect);
    await expect(page.locator('.image-handles')).toHaveCount(1);
    await page.keyboard.press('Control+c');
    await expect.poll(() => clipboardPlain(page), { timeout: 10_000 }).toBe('\u{FFFC}');

    /* The paste reads the clipboard asynchronously: record the PASTE_PLAIN
       the shell dispatches (and its reply) so the read-back waits for it. */
    await page.evaluate(() => {
        const w = window as any;
        const client = w.__engineClient;
        const original = client.dispatch.bind(client);
        w.__pastes = [];
        client.dispatch = async (cmd: any) => {
            const reply = await original(cmd);
            if (cmd?.type === 'PASTE_PLAIN') w.__pastes.push({ text: cmd.text, reply: reply.type });
            return reply;
        };
    });
    await page.keyboard.press('Control+Shift+v');
    await page.waitForFunction(() => (window as any).__pastes.length > 0);
    const pastes = await page.evaluate(() => (window as any).__pastes);
    expect(pastes, 'the unformatted paste carried the bare sentinel').toEqual([
        { text: '\u{FFFC}', reply: 'SELECTION_CHANGED' },
    ]);
    await settle(page);

    /* Nothing went in: the text and the one picture are as they were —
       before the fix a second, object-less U+FFFC sat in front of it. */
    const after = await documentText(page);
    expect(after).toBe(before);
    const pictures = await waitForImages(page, 1);
    expect(sentinels(after)).toBe(pictures.length);
});
