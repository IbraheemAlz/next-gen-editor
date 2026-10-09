import { test, expect } from '@playwright/test';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

/* Issue #348 — a hostile package is refused with the TYPED error
 * (`Event::Error { kind: 'PackageTooLarge' }`) through the real shell +
 * worker + WASM engine — never a trap / crash-recovery cycle — and the
 * File menu's error banner says so. A package whose ZIP directory merely
 * LIES about a 4 GiB part opens normally: the wasm32 reader used to
 * `Vec::with_capacity(declared)` and trap on "capacity overflow".
 *
 * The packages are the committed `docx_reader` fuzz seeds, built by
 * `format_docx::test_fixtures` and pinned by `engine_fuzz::tests::
 * hostile_docx_seeds_are_committed_and_typed`.
 */

const seed = (name: string) =>
    Array.from(
        readFileSync(
            fileURLToPath(new URL(`../../fuzz/corpus/docx_reader/${name}`, import.meta.url)),
        ),
    );

declare global {
    interface Window {
        __dispatch: (cmd: unknown) => Promise<any>;
        __paintIdle?: boolean;
    }
}

test('OPEN_DOCUMENT refuses a 5000-deep package with a typed error, then opens a lying-size one', async ({
    page,
}) => {
    test.setTimeout(30_000);
    await page.goto('/');
    await page.waitForFunction(() => window.__paintIdle === true, undefined, {
        timeout: 20_000,
    });

    const deep = await page.evaluate(
        async (arr: number[]) =>
            window.__dispatch({
                type: 'OPEN_DOCUMENT',
                bytes: new Uint8Array(arr),
                format: 'docx',
                name: 'deep.docx',
            }),
        seed('hostile_sdt_5000_nested.docx'),
    );
    expect(deep.type).toBe('ERROR');
    expect(deep.kind).toBe('PackageTooLarge');
    expect(deep.message).toContain('XML nesting depth');
    await expect(page.locator('.nge-fm__error')).toContainText('too large or too deeply nested');

    const lying = await page.evaluate(
        async (arr: number[]) =>
            window.__dispatch({
                type: 'OPEN_DOCUMENT',
                bytes: new Uint8Array(arr),
                format: 'docx',
                name: 'lying.docx',
            }),
        seed('hostile_declared_4gib_part.docx'),
    );
    expect(lying.type).toBe('DOCUMENT_LOADED');

    const sel = await page.evaluate(() =>
        window.__dispatch({
            type: 'SET_SELECTION',
            range: {
                start: { path: { steps: [{ kind: 'BLOCK', idx: 0 }] }, offset: 0 },
                end: { path: { steps: [{ kind: 'BLOCK', idx: 0 }] }, offset: 5 },
            },
            caret: { path: { steps: [{ kind: 'BLOCK', idx: 0 }] }, offset: 5 },
        }),
    );
    expect(sel.type).toBe('SELECTION_CHANGED');
    /* No crash overlay: the worker never trapped (a trap would also have
       rejected the dispatches above). */
    await expect(page.locator('.nge-trap')).toHaveCount(0);
});
