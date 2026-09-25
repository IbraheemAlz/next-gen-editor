import { test, expect } from '@playwright/test';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

/* Issue #196 — a text box NESTED in a text box's story is clickable and
 * editable end to end through the REAL shell pointer path + worker +
 * WASM engine:
 *
 *   load `text_boxes_nested.docx` → a real mouse click over the nested
 *   box enters ITS story (rid = the nested a11y region id `1@0/1@0`) →
 *   keyboard typing lands in the nested story → a click on the outer
 *   story switches to it → save → reload → the nested edit survived.
 *
 * Fixture geometry (see tools/roundtrip `nested_text_boxes_document_xml`,
 * pinned natively by `nested_text_box_fixture_routes_presses_by_geometry`):
 * outer box 3" × 2" at (1", 3") on the page; nested box 1.5" × 0.6" at
 * (2.1", 3.55"). Assertions ride command replies (`editing_story`) and
 * clipboard text — engine truth, valid under headless Chrome (the canvas
 * is never screenshotted, see CLAUDE.md). */

const FIXTURE = fileURLToPath(
    new URL('../../crates/format-docx/tests/fixtures/text_boxes_nested.docx', import.meta.url),
);

/* Tall enough that every click target (≤ 5" down page 0) is on screen. */
test.use({ viewport: { width: 1280, height: 1400 } });

type Top = { path: { steps: { kind: 'BLOCK'; idx: number }[] }; offset: number };

test('click into a nested text box, type, survive save/reload', async ({ page }) => {
    test.setTimeout(60_000);
    await page.goto('/');
    await page.waitForFunction(() => (window as any).__paintIdle === true, undefined, {
        timeout: 20_000,
    });

    const bytes = Array.from(readFileSync(FIXTURE));
    /* Load the fixture, then read the device scale off the body caret:
       a caret at the start of the first (LTR, unindented) paragraph sits
       on the 1" left margin. */
    const scale = await page.evaluate(async (arr: number[]) => {
        const dispatch = (cmd: unknown): Promise<any> => (window as any).__dispatch(cmd);
        const top = (idx: number, offset: number): Top => ({
            path: { steps: [{ kind: 'BLOCK', idx }] },
            offset,
        });
        const loaded = await dispatch({ type: 'LOAD_DOCX', bytes: new Uint8Array(arr) });
        if (loaded.type === 'ERROR') throw new Error(loaded.message);
        const sel = await dispatch({
            type: 'SET_SELECTION',
            range: { start: top(0, 0), end: top(0, 0) },
            caret: top(0, 0),
        });
        return (sel.caret.x as number) / 72;
    }, bytes);
    expect(scale).toBeGreaterThan(0.5);

    const canvas = page.locator('.editor-page[data-page-index="0"] canvas').first();
    await expect(canvas).toBeVisible();
    const dpr = await page.evaluate(() => window.devicePixelRatio || 1);
    /* Page inches → a real mouse click on the page-0 canvas. */
    const clickAt = async (xIn: number, yIn: number): Promise<void> => {
        await canvas.scrollIntoViewIfNeeded();
        const box = await canvas.boundingBox();
        if (!box) throw new Error('page canvas has no box');
        await page.mouse.click(
            box.x + (xIn * 72 * scale) / dpr,
            box.y + (yIn * 72 * scale) / dpr,
        );
    };
    /* The active story, read back off a SELECT_ALL reply (scoped to the
       story being edited; only ever used where the selection is about
       to be replaced anyway). */
    const activeRid = (): Promise<string | null> =>
        page.evaluate(async () => {
            const dispatch = (cmd: unknown): Promise<any> => (window as any).__dispatch(cmd);
            const evt = await dispatch({ type: 'SELECT_ALL' });
            return (evt.editing_story?.rid as string | undefined) ?? null;
        });
    const storyText = (): Promise<string> =>
        page.evaluate(async () => {
            const dispatch = (cmd: unknown): Promise<any> => (window as any).__dispatch(cmd);
            await dispatch({ type: 'SELECT_ALL' });
            const clip = await dispatch({ type: 'GET_SELECTION_AS_CLIPBOARD' });
            return clip.type === 'CLIPBOARD_PAYLOAD' ? (clip.plain as string) : `<${clip.type}>`;
        });

    /* 1 — a real click over the nested box enters the NESTED story. */
    await clickAt(2.85, 3.8);
    await expect.poll(activeRid).toBe('1@0/1@0');

    /* 2 — keyboard typing lands in the nested story (at its start). */
    await page.evaluate(async () => {
        const dispatch = (cmd: unknown): Promise<any> => (window as any).__dispatch(cmd);
        const home = { path: { steps: [{ kind: 'BLOCK', idx: 0 }] }, offset: 0 };
        await dispatch({ type: 'SET_SELECTION', range: { start: home, end: home }, caret: home });
    });
    await page.keyboard.type('typed ');
    await expect.poll(storyText).toContain('typed Inner story text.');
    expect(await activeRid()).toBe('1@0/1@0');

    /* 3 — a click on the outer story (below the nested box) switches. */
    await clickAt(1.5, 4.6);
    await expect.poll(activeRid).toBe('1@0');
    const outer = await storyText();
    expect(outer).toContain('Outer story text.');
    expect(outer).not.toContain('typed');

    /* 4 — save → reload → the nested edit survived. */
    const reloaded = await page.evaluate(async () => {
        const dispatch = (cmd: unknown): Promise<any> => (window as any).__dispatch(cmd);
        await dispatch({ type: 'EXIT_HEADER_FOOTER' });
        const saved = await dispatch({ type: 'SAVE_DOCX' });
        if (saved.type === 'ERROR') return `save: ${saved.message}`;
        const back = await dispatch({ type: 'LOAD_DOCX', bytes: saved.bytes });
        return back.type;
    });
    expect(reloaded).not.toBe('ERROR');
    expect(reloaded).not.toContain('save:');
    await clickAt(2.85, 3.8);
    await expect.poll(activeRid).toBe('1@0/1@0');
    await expect.poll(storyText).toContain('typed Inner story text.');
});
