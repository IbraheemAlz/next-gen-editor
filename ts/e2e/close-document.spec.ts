import { test, expect } from '@playwright/test';
import { boot, burst, documentText } from './helpers/editor';

/* Issue #338 — `closeDocument()` on the @nge/core facade used to dispatch
 * CLOSE_DOCUMENT into a `phase3_stub` (a promise that always resolved to
 * `Event::Error`). It now resets the engine to the seeded empty document:
 * typed text, undo history and the accessibility mirror are gone, and the
 * worker pins the next event-log snapshot as the new document's base
 * (`CommandMeta.new_document`, the #268 pinning set derived from
 * `bridge::meta`). Driven through `window.__engineClient` — the same handle
 * the manual QA step uses. */

type Log = { pinned: number | undefined; cmd: number[] };

async function readLog(page: import('@playwright/test').Page): Promise<Log> {
    return page.evaluate(
        () =>
            new Promise<Log>((resolve, reject) => {
                const open = indexedDB.open('engine-log');
                open.onerror = () => reject(open.error);
                open.onsuccess = () => {
                    const db = open.result;
                    const tx = db.transaction(['commands', 'meta'], 'readonly');
                    const cmd = tx.objectStore('commands').getAllKeys();
                    const pinned = tx.objectStore('meta').get('pinned');
                    tx.oncomplete = () => {
                        db.close();
                        resolve({
                            pinned: pinned.result?.seq as number | undefined,
                            cmd: cmd.result as number[],
                        });
                    };
                    tx.onerror = () => {
                        db.close();
                        reject(tx.error);
                    };
                };
            }),
    );
}

test('closeDocument() resets to the empty document, clears undo + the a11y mirror, re-pins the log base', async ({
    page,
}) => {
    test.setTimeout(60_000);
    await boot(page);
    /* The boot snapshot is the session's first pinned base. */
    await expect.poll(async () => (await readLog(page)).pinned, { timeout: 15_000 }).toBeDefined();
    const bootPin = (await readLog(page)).pinned!;

    /* The boot document carries the shell's seeded sample text; typing
       lands at the caret in front of it. */
    await burst(page, ['Closing ', 'time']);
    expect(await documentText(page)).toContain('Closing time');
    await expect(page.locator('.a11y-mirror')).toContainText('Closing time');

    const reply = await page.evaluate(async () => {
        const client = (window as any).__engineClient;
        const evt = await client.dispatch({ type: 'CLOSE_DOCUMENT' });
        return {
            type: evt.type as string,
            canUndo: evt.can_undo as boolean,
            canRedo: evt.can_redo as boolean,
            start: evt.range?.start?.offset as number,
        };
    });
    expect(reply).toEqual({ type: 'SELECTION_CHANGED', canUndo: false, canRedo: false, start: 0 });

    expect(await documentText(page)).toBe('');
    /* Undo has nothing to restore: the typed text stays gone. */
    await page.evaluate(() => (window as any).__dispatch({ type: 'UNDO' }));
    expect(await documentText(page)).toBe('');
    /* The accessibility mirror followed the mutation signal. */
    await expect(page.locator('.a11y-mirror')).not.toContainText('Closing');
    /* CLOSE_DOCUMENT is logged, and the snapshot after it is pinned. */
    await expect
        .poll(async () => (await readLog(page)).pinned ?? 0, { timeout: 15_000 })
        .toBeGreaterThan(bootPin);

    /* The engine keeps working on the fresh document. */
    await burst(page, ['fresh']);
    expect(await documentText(page)).toBe('fresh');
});
