import { test, expect, type Page } from '@playwright/test';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { boot, burst, settle } from './helpers/editor';

/* Issue #462 — navigation past the lazily laid-out band, through the REAL
 * worker + WASM engine and the shell's key handler.
 *
 * Lazy pagination lays out only the viewport band plus a runway; before
 * #462 a burst of ArrowDown presses parked the caret on the band's last
 * line (60 presses stopped at line 48 of 90) until the shell's scroll asked
 * for more layout. The engine now expands the band synchronously inside the
 * move, so the reply already carries the final caret. The race is driven
 * with `burst` (ONE synchronous evaluate: no reply, no scroll, no
 * `EXPAND_LAYOUT` can interleave — the deterministic form of a fast key
 * repeat), plus one slow real-keyboard smoke.
 *
 * Assertions ride engine replies (caret paths and rects, page tops) —
 * valid under headless Chrome, which never composites the interactive
 * canvas (see CLAUDE.md). */

const PERF_50P = fileURLToPath(new URL('../../tests/perf/50p.docx', import.meta.url));

const top = (idx: number, offset: number) => ({
    path: { steps: [{ kind: 'BLOCK', idx }] },
    offset,
});

async function openPlainLines(page: Page, count: number): Promise<void> {
    const lines = Array.from({ length: count }, (_, i) => `Line ${i + 1} of the long document`);
    const opened = await page.evaluate(async (text) => {
        const bytes = new TextEncoder().encode(text);
        return (
            await (window as any).__dispatch({
                type: 'OPEN_DOCUMENT',
                bytes,
                format: 'plain_text',
                name: 'long.txt',
            })
        ).type;
    }, lines.join('\n'));
    expect(opened).toBe('DOCUMENT_LOADED');
    await page.evaluate(async (pos) => {
        await (window as any).__dispatch({
            type: 'SET_SELECTION',
            range: { start: pos, end: pos },
            caret: pos,
        });
    }, top(0, 0));
}

/** The engine caret's top-level block and caret rect, WITHOUT moving it: a
 *  Shift+Right extends from the collapsed caret, so the ordered range's
 *  start IS the caret (undone by collapsing back onto it). */
async function caretNow(page: Page): Promise<{ block: number; y: number }> {
    return page.evaluate(async () => {
        const d = (window as any).__dispatch;
        const ext = await d({ type: 'MOVE_CARET', direction: 'Right', extend: true });
        const pos = ext.range.start;
        const back = await d({
            type: 'SET_SELECTION',
            range: { start: pos, end: pos },
            caret: pos,
        });
        return { block: pos.path.steps[0].idx as number, y: back.caret.y as number };
    });
}

/** Page tops / heights of the current band (a no-op paint request: an
 *  empty viewport raises no lazy target). */
async function pageGeometry(
    page: Page,
): Promise<{ tops: number[]; heights: number[]; full: boolean }> {
    return page.evaluate(async () => {
        const p = await (window as any).__dispatch({
            type: 'REQUEST_PAINT',
            viewport: { x: 0, y: 0, w: 0, h: 0 },
            dirty: undefined,
        });
        return { tops: p.page_tops, heights: p.page_heights, full: p.is_full_layout };
    });
}

function pageOf(y: number, tops: number[], heights: number[]): number {
    return tops.findIndex((t, i) => y >= t && y < t + (heights[i] ?? 0));
}

test('60 × ArrowDown in one synchronous burst crosses the lazy band edge (#462)', async ({
    page,
}) => {
    test.setTimeout(60_000);
    await boot(page);
    await openPlainLines(page, 90);
    await settle(page);
    /* The cold band must not already hold line 61, or the burst proves nothing. */
    const cold = await pageGeometry(page);
    expect(cold.full).toBe(false);

    await burst(
        page,
        Array.from({ length: 60 }, () => ({ key: 'ArrowDown' })),
    );
    await settle(page);
    const caret = await caretNow(page);
    /* From line 1, 60 presses land on line 61 (block 60). */
    expect(caret.block).toBe(60);
    const geom = await pageGeometry(page);
    expect(pageOf(caret.y, geom.tops, geom.heights)).toBeGreaterThanOrEqual(0);
});

test('Shift+PageDown in a burst extends past the band; PageUp comes back (#462)', async ({
    page,
}) => {
    test.setTimeout(60_000);
    await boot(page);
    await openPlainLines(page, 400);
    await settle(page);
    await burst(
        page,
        Array.from({ length: 20 }, () => ({ key: 'PageDown', shift: true })),
    );
    await settle(page);
    const sel = await page.evaluate(async () => {
        const clip = await (window as any).__dispatch({ type: 'GET_SELECTION_AS_CLIPBOARD' });
        return clip.plain as string;
    });
    /* One viewport per press (~7 lines of this pitch in the e2e window):
       the selection grew from line 1 well past the ~48-line cold band. */
    const selectedLines = sel.split(/\r?\n/).length;
    expect(selectedLines).toBeGreaterThan(90);
    await burst(page, [{ key: 'ArrowLeft' }, ...Array.from({ length: 40 }, () => ({ key: 'PageUp' }))]);
    await settle(page);
    expect((await caretNow(page)).block).toBe(0);
});

test('Ctrl+End on the 50-page fixture lands on the last page in one round-trip (#462)', async ({
    page,
}) => {
    test.setTimeout(90_000);
    await boot(page);
    const bytes = Array.from(readFileSync(PERF_50P));
    const opened = await page.evaluate(async (arr) => {
        const evt = await (window as any).__dispatch({
            type: 'OPEN_DOCUMENT',
            bytes: new Uint8Array(arr),
            format: 'docx',
            name: '50p.docx',
        });
        return evt.type;
    }, bytes);
    expect(opened).toBe('DOCUMENT_LOADED');
    await settle(page);
    expect((await pageGeometry(page)).full).toBe(false);

    await burst(page, [{ key: 'End', ctrl: true }]);
    await settle(page);
    const caret = await caretNow(page);
    const geom = await pageGeometry(page);
    expect(geom.full).toBe(true);
    expect(geom.tops.length).toBeGreaterThan(40);
    expect(pageOf(caret.y, geom.tops, geom.heights)).toBe(geom.tops.length - 1);

    /* And Ctrl+Home goes back to the first page. */
    await burst(page, [{ key: 'Home', ctrl: true }]);
    await settle(page);
    const home = await caretNow(page);
    expect(home.block).toBe(0);
    expect(pageOf(home.y, geom.tops, geom.heights)).toBe(0);
});

test('slow real-keyboard smoke: ArrowDown / PageDown through the hidden input (#462)', async ({
    page,
}) => {
    test.setTimeout(60_000);
    await boot(page);
    await openPlainLines(page, 200);
    await settle(page);
    await page.locator('textarea[data-nge-hidden-input]').focus();
    for (let i = 0; i < 5; i++) {
        await page.keyboard.press('ArrowDown');
        await settle(page);
    }
    expect((await caretNow(page)).block).toBe(5);
    await page.keyboard.press('PageDown');
    await settle(page);
    expect((await caretNow(page)).block).toBeGreaterThan(10);
    await page.keyboard.press('Control+End');
    await settle(page);
    expect((await caretNow(page)).block).toBe(199);
});
