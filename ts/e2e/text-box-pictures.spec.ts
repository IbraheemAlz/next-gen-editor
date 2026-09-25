import { test, expect, type Page } from '@playwright/test';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

/* Issue #206 — pictures inside text-box stories are selectable, draggable,
 * resizable and re-wrappable end to end through the REAL shell pointer
 * path (image hit-test → `ImageHandlesOverlay` → `@nge/ui`
 * `ImageWrapPicker`) + worker + WASM engine:
 *
 *   load `text_box_pictures.docx` → a real click on the picture in the
 *   box's story selects it (handles + wrap picker, Square checked) → drag
 *   its body → drag the SE resize handle → "Top and bottom" → the same on
 *   the picture inside the NESTED box → enter the outer box's story and
 *   re-wrap the nested picture from there → save → reload → every edit
 *   survived.
 *
 * Fixture geometry (tools/roundtrip `text_box_pictures_document_xml`,
 * pinned natively by `text_box_pictures_fixture_reports_page_geometry`):
 * outer box 3" × 2" at (1", 3"); its picture 0.75" × 0.5" at (1.1",
 * 3.05"); nested picture 0.5" × 0.3" at (2.4", 4.0"). Assertions ride
 * engine replies (`GET_IMAGE_RECTS`, `editing_story`) and the DOM overlay
 * — engine truth, valid under headless Chrome (the canvas is never
 * screenshotted, see CLAUDE.md). Click targets come from the engine's own
 * rects, so the spec is independent of zoom / DPR. */

const FIXTURE = fileURLToPath(
    new URL('../../crates/format-docx/tests/fixtures/text_box_pictures.docx', import.meta.url),
);

test.use({ viewport: { width: 1280, height: 1400 } });

type Hop = { path: unknown; at: number };
type Img = {
    rel_id: string;
    at: number;
    rect: { x: number; y: number; w: number; h: number };
    width_emu: number;
    height_emu: number;
    floating: boolean;
    frame_x: number;
    frame_y: number;
    wrap?: string;
    story?: Hop[];
    story_rid?: string;
};

const dispatch = (page: Page, cmd: unknown): Promise<any> =>
    page.evaluate((c) => (window as any).__dispatch(c), cmd);

/** The engine's picture rects, keyed by owning story (`1@0` = the outer
 *  box's story, `1@0/1@0` = the nested box's). */
async function pictures(page: Page): Promise<Record<string, Img>> {
    const evt = await dispatch(page, { type: 'GET_IMAGE_RECTS' });
    if (evt.type !== 'IMAGE_RECTS') throw new Error(`GET_IMAGE_RECTS → ${evt.type}`);
    const out: Record<string, Img> = {};
    for (const im of evt.images as Img[]) out[im.story_rid ?? ''] = im;
    return out;
}

test('select, drag, resize and re-wrap pictures inside text boxes; survive save/reload', async ({
    page,
}) => {
    test.setTimeout(90_000);
    await page.goto('/');
    await page.waitForFunction(() => (window as any).__paintIdle === true, undefined, {
        timeout: 20_000,
    });

    const bytes = Array.from(readFileSync(FIXTURE));
    const scale = await page.evaluate(async (arr: number[]) => {
        const d = (cmd: unknown): Promise<any> => (window as any).__dispatch(cmd);
        const top = { path: { steps: [{ kind: 'BLOCK', idx: 0 }] }, offset: 0 };
        const loaded = await d({ type: 'LOAD_DOCX', bytes: new Uint8Array(arr) });
        if (loaded.type === 'ERROR') throw new Error(loaded.message);
        const sel = await d({
            type: 'SET_SELECTION',
            range: { start: top, end: top },
            caret: top,
        });
        /* The body caret at the start of the first paragraph sits on the
           1" left margin. */
        return (sel.caret.x as number) / 72;
    }, bytes);
    expect(scale).toBeGreaterThan(0.5);
    const inch = 72 * scale; // device px per inch

    const start = await pictures(page);
    expect(Object.keys(start).sort()).toEqual(['1@0', '1@0/1@0']);
    expect(start['1@0']!.floating).toBe(true);
    expect(start['1@0']!.wrap).toBe('square');
    expect(start['1@0']!.story).toHaveLength(1);
    expect(start['1@0/1@0']!.story).toHaveLength(2);

    const canvas = page.locator('.editor-page[data-page-index="0"] canvas').first();
    await expect(canvas).toBeVisible();
    await canvas.scrollIntoViewIfNeeded();
    const dpr = await page.evaluate(() => window.devicePixelRatio || 1);
    /** Viewport point of a document device-px point on page 0. */
    const viewportAt = async (x: number, y: number): Promise<{ x: number; y: number }> => {
        const box = await canvas.boundingBox();
        if (!box) throw new Error('page canvas has no box');
        return { x: box.x + x / dpr, y: box.y + y / dpr };
    };
    const centerOf = (im: Img) =>
        viewportAt(im.rect.x + im.rect.w / 2, im.rect.y + im.rect.h / 2);

    const handles = page.locator('.image-handles');
    /** Click a picture until the shell's rect mirror has caught up and
     *  its handles show. */
    const select = async (rid: string): Promise<void> => {
        await expect(async () => {
            const im = (await pictures(page))[rid]!;
            const c = await centerOf(im);
            await page.mouse.click(c.x, c.y);
            await expect(handles).toBeVisible({ timeout: 1_000 });
            const hb = await handles.boundingBox();
            expect(Math.abs(hb!.width - im.rect.w / dpr)).toBeLessThan(2);
            expect(Math.abs(hb!.height - im.rect.h / dpr)).toBeLessThan(2);
        }).toPass({ timeout: 15_000 });
    };
    /** Press at `from` (viewport px) and drag by (dx, dy) device px. */
    const drag = async (from: { x: number; y: number }, dx: number, dy: number) => {
        await page.mouse.move(from.x, from.y);
        await page.mouse.down();
        await page.mouse.move(from.x + dx / dpr / 2, from.y + dy / dpr / 2, { steps: 4 });
        await page.mouse.move(from.x + dx / dpr, from.y + dy / dpr, { steps: 4 });
        await page.mouse.up();
    };
    const wrapButton = (label: string) =>
        page.locator('.image-toolbar').getByRole('radio', { name: label });

    /* 1 — a real click on the outer story's picture selects it: handles
       over its rect, the wrap picker live with Square checked. */
    await select('1@0');
    await expect(wrapButton('Square')).toHaveAttribute('aria-checked', 'true');
    await expect(wrapButton('Square')).toBeEnabled();

    /* 2 — drag the body 0.5" right, 0.25" down → MOVE_IMAGE with the
       story chain: the picture moves inside the box by the drag. */
    const o0 = start['1@0']!;
    await drag(await centerOf(o0), 0.5 * inch, 0.25 * inch);
    await expect
        .poll(async () => {
            const o = (await pictures(page))['1@0']!;
            return [
                Math.round((o.rect.x - o0.rect.x) / inch * 100) / 100,
                Math.round((o.rect.y - o0.rect.y) / inch * 100) / 100,
            ];
        })
        .toEqual([0.5, 0.25]);

    /* 3 — drag the SE handle 0.25" right → RESIZE_IMAGE (aspect-locked):
       the extent grows from 0.75" to ~1". */
    await select('1@0');
    const se = await page.locator('.image-handle--se').boundingBox();
    await drag({ x: se!.x + se!.width / 2, y: se!.y + se!.height / 2 }, 0.25 * inch, 0);
    await expect
        .poll(async () => (await pictures(page))['1@0']!.width_emu)
        .toBeGreaterThan(Math.round(685_800 * 1.25));
    const o1 = (await pictures(page))['1@0']!;
    expect(o1.width_emu).toBeLessThan(Math.round(685_800 * 1.45));

    /* 4 — the wrap picker re-wraps it → SET_IMAGE_WRAP with the chain. */
    await select('1@0');
    await wrapButton('Top and bottom').click();
    await expect.poll(async () => (await pictures(page))['1@0']!.wrap).toBe('top_and_bottom');

    /* 5 — the picture inside the NESTED box: select, drag 0.25" right,
       0.1" down. */
    const n0 = (await pictures(page))['1@0/1@0']!;
    await select('1@0/1@0');
    await expect(wrapButton('Square')).toHaveAttribute('aria-checked', 'true');
    await drag(await centerOf(n0), 0.25 * inch, 0.1 * inch);
    await expect
        .poll(async () => {
            const n = (await pictures(page))['1@0/1@0']!;
            return [
                Math.round((n.rect.x - n0.rect.x) / inch * 100) / 100,
                Math.round((n.rect.y - n0.rect.y) / inch * 100) / 100,
            ];
        })
        .toEqual([0.25, 0.1]);

    /* 6 — inside the outer box's STORY (a click on its text below the
       nested box), a press on the nested picture still selects it and the
       picker re-wraps it. */
    const box0 = await viewportAt(1.3 * inch, 4.85 * inch);
    await page.mouse.click(box0.x, box0.y);
    await expect
        .poll(async () => {
            const evt = await dispatch(page, { type: 'SELECT_ALL' });
            return (evt.editing_story?.rid as string | undefined) ?? null;
        })
        .toBe('1@0');
    await select('1@0/1@0');
    await wrapButton('Behind text').click();
    await expect
        .poll(async () => (await pictures(page))['1@0/1@0']!.wrap)
        .toBe('behind_text');

    /* 7 — save → reload: every edit survived the .docx round trip. */
    const before = await pictures(page);
    const reloaded = await page.evaluate(async () => {
        const d = (cmd: unknown): Promise<any> => (window as any).__dispatch(cmd);
        await d({ type: 'EXIT_HEADER_FOOTER' });
        const saved = await d({ type: 'SAVE_DOCX' });
        if (saved.type === 'ERROR') return `save: ${saved.message}`;
        const back = await d({ type: 'LOAD_DOCX', bytes: saved.bytes });
        return back.type;
    });
    expect(reloaded).not.toBe('ERROR');
    expect(reloaded).not.toContain('save:');
    const after = await pictures(page);
    for (const rid of ['1@0', '1@0/1@0']) {
        const a = after[rid]!;
        const b = before[rid]!;
        expect(a.width_emu).toBe(b.width_emu);
        expect(a.height_emu).toBe(b.height_emu);
        expect(a.wrap).toBe(b.wrap);
        expect(Math.abs(a.rect.x - a.frame_x - (b.rect.x - b.frame_x))).toBeLessThan(1);
        expect(Math.abs(a.rect.y - a.frame_y - (b.rect.y - b.frame_y))).toBeLessThan(1);
    }
    expect(after['1@0']!.wrap).toBe('top_and_bottom');
    expect(after['1@0/1@0']!.wrap).toBe('behind_text');
});
