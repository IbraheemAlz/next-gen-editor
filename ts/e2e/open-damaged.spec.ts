import { test, expect } from '@playwright/test';
import { boot, documentText } from './helpers/editor';
import { documentXml, storedDocx } from './helpers/docx';

/* Issue #491 (item 2) - a package whose main part is beyond a faithful
 * repair (#483: here a SECOND root element behind the real one) is refused
 * with the typed `Event::Error { kind: 'DocumentUnreadable' }` through the
 * real shell + worker + WASM engine, not an untyped error the user never
 * sees. The File menu's open banner (the surface `PackageTooLarge` uses)
 * says the document is damaged and names the part and what is wrong - never
 * the document's own text; the Dev HUD's "Last error" row records the kind;
 * the previous document stays open (issue #341). */

const BODY = (text: string) => `<w:p><w:r><w:t>${text}</w:t></w:r></w:p>`;

/** `word/document.xml` with a second `<w:document>` root appended (its XML
 *  declaration dropped, so the root count is the only defect). */
const TWO_ROOT_DOCX = (() => {
    const first = documentXml(BODY('zzfirstsecret'));
    const second = documentXml(BODY('zzsecondsecret')).replace(/^<\?xml[^>]*\?>/, '');
    return storedDocx(first + second);
})();

test('a damaged .docx is refused with a visible typed message; the document stays', async ({
    page,
}) => {
    test.setTimeout(60_000);
    await boot(page);
    await page.evaluate(async () => {
        const dispatch = (cmd: unknown): Promise<any> => (window as any).__dispatch(cmd);
        await dispatch({ type: 'SELECT_ALL' });
        await dispatch({ type: 'INSERT_TEXT', at: undefined, text: 'previous document' });
    });
    const before = await documentText(page);
    expect(before).toContain('previous document');

    /* The real File -> Open flow. */
    await page.locator('.nge-fm input[type="file"]').setInputFiles({
        name: 'damaged.docx',
        mimeType: 'application/vnd.openxmlformats-officedocument.wordprocessingml.document',
        buffer: Buffer.from(TWO_ROOT_DOCX),
    });

    const banner = page.locator('.nge-fm__error');
    await expect(banner).toContainText('This document is damaged and cannot be opened');
    await expect(banner).toContainText('word/document.xml');
    await expect(banner).toContainText('second root element');
    /* The part name and the defect, never the document's own text. */
    await expect(banner).not.toContainText('secret');
    /* The banner is the open surface; the toast is not used for this kind. */
    await expect(page.locator('.nge-toast')).toHaveText('');

    /* The Dev HUD's last-error row records the typed kind. */
    await page.locator('textarea[data-nge-hidden-input]').focus();
    await page.keyboard.press('Control+Shift+D');
    const row = page.locator('.nge-hud__lasterror');
    await expect(row).toBeVisible();
    await expect(row).toContainText('DocumentUnreadable');

    /* Still the previous document, and the worker never trapped. */
    expect(await documentText(page)).toBe(before);
    await expect(page.locator('.nge-trap')).toHaveCount(0);

    /* The next, intact open works (the refusal left the session usable). */
    const intact = await page.evaluate(
        async (arr: number[]) =>
            (window as any).__dispatch({
                type: 'OPEN_DOCUMENT',
                bytes: new Uint8Array(arr),
                format: 'docx',
                name: 'intact.docx',
            }),
        Array.from(storedDocx(documentXml(BODY('after the refusal')))),
    );
    expect(intact.type).toBe('DOCUMENT_LOADED');
    expect(await documentText(page)).toContain('after the refusal');
});
