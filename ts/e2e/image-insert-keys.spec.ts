import { test, expect, type Page } from '@playwright/test';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { boot, burst, documentText, settle } from './helpers/editor';
import {
    GREEN,
    RED,
    caretBox,
    clickRect,
    clientPoint,
    dispatch,
    expectPainted,
    images,
    insertPng,
    pixels,
    waitForImages,
    type Img,
} from './helpers/images';

/* Issues #506 (items 1 and 3) and #554 — a picture inserted through the
 * toolbar's file input, the way a user does it:
 *
 *   1. it PAINTS its pixels (the worker decoded its bytes and registered the
 *      bitmap; before, `INSERT_IMAGE` never triggered the decode and the
 *      renderer drew the flat grey placeholder for the rest of the session);
 *   2. a click selects it (handles, no caret — object selection REPLACES the
 *      caret); Delete / Backspace remove it in one undoable edit, and the
 *      undo brings it back painted;
 *   3. Left / Right collapse the selection to a caret at the picture's own
 *      line (visual left / right edge); Up / Down walk from that line, so Up
 *      lands in the table's LAST row, never its first;
 *   4. typing with the picture selected inserts before it.
 *
 * Evidence is engine truth (`GET_IMAGE_RECTS`, the selection events) plus the
 * worker-side pixel read-back `probePagePixels` — headless Chrome never
 * composites the interactive canvas (CLAUDE.md), so a screenshot would be
 * blank. The click and key presses go through the REAL shell: `pointer.ts`
 * -> `selectImageByPoint` -> `ImageHandlesOverlay`, and the hidden
 * textarea's `onKeyDown`. The flows around the selection (selected after
 * insert, copy / cut, the caret flash, entering a text box) are
 * `image-selection-flows.spec.ts`. */

test.use({ viewport: { width: 1280, height: 1400 } });

/** `above`, a 2 x 3 table, and an empty paragraph with the caret in it. */
async function seed(page: Page): Promise<void> {
    await page.evaluate(async () => {
        const d = (c: unknown): Promise<any> => (window as any).__dispatch(c);
        await d({ type: 'SELECT_ALL' });
        await d({ type: 'INSERT_TEXT', at: undefined, text: 'above' });
        const t = await d({
            type: 'INSERT_TABLE',
            at: { steps: [{ kind: 'BLOCK', idx: 1 }] },
            rows: 2,
            cols: 3,
        });
        if (t.type === 'ERROR') throw new Error(`INSERT_TABLE: ${t.message}`);
        await d({ type: 'MOVE_CARET', direction: 'DocEnd', extend: false });
        /* Every SELECTION_CHANGED, for the caret assertions. */
        const log: any[] = [];
        (window as any).__selLog = log;
        (window as any).__engineClient.subscribe((e: any) => {
            if (e.type === 'SELECTION_CHANGED') log.push(e);
        });
    });
}

/** Park the engine's caret at the very top of the document, far from the
 *  picture. Object selection is shell-side: the engine's caret stays where
 *  it was, and a key that fell through to it (the pre-#554 behaviour) acted
 *  THERE - Delete ate the first letter of `above`, Up / Left stayed in it.
 *  Right after an insert the caret sits just after the picture, which would
 *  make Backspace and the arrows pass by accident. */
async function parkCaretAtTop(page: Page): Promise<void> {
    await dispatch(page, { type: 'MOVE_CARET', direction: 'DocHome', extend: false });
}

/** A real click on the picture's centre (caret parked at the top first). */
async function clickPicture(page: Page, r: Img['rect']): Promise<void> {
    await parkCaretAtTop(page);
    await clickRect(page, r);
    await expect(page.locator('.image-handles')).toHaveCount(1);
}

/** The engine's last reported caret address. */
async function lastCaret(page: Page): Promise<{ block: number; row?: number }> {
    await settle(page);
    return page.evaluate(() => {
        const log = (window as any).__selLog as any[];
        const steps = log[log.length - 1].range.end.path.steps as any[];
        const cell = steps.find((s) => s.kind === 'CELL');
        return { block: steps[0].idx as number, row: cell?.row as number | undefined };
    });
}

test.describe('an inserted picture (#506, #554)', () => {
    test.beforeEach(async ({ page }) => {
        await boot(page);
        await seed(page);
    });

    test('paints its pixels, selects without a caret, deletes with Delete and undoes back painted', async ({
        page,
    }) => {
        await insertPng(page, RED);
        const [img] = await waitForImages(page, 1);
        /* Bytes reached the worker's image cache: the picture is its own
           colour, not the placeholder's flat grey (221, 221, 221). */
        await expectPainted(page, img!.rect, RED);
        const grey = await pixels(page, [{ x: img!.rect.x + 5, y: img!.rect.y + 5 }]);
        expect(grey[0]?.slice(0, 3)).not.toEqual([221, 221, 221]);

        await clickPicture(page, img!.rect);
        /* Object selection replaces the caret. */
        expect(await caretBox(page)).toBeNull();

        await page.keyboard.press('Delete');
        await expect(page.locator('.image-handles')).toHaveCount(0);
        expect(await waitForImages(page, 0)).toEqual([]);
        /* The key did not also eat a letter at the parked caret. */
        expect(await documentText(page)).toContain('above');
        /* The caret is back, at the picture's old anchor. */
        const back = await caretBox(page);
        expect(back).not.toBeNull();

        /* ONE undo brings the picture back, and painted (the bitmap was
           registered once and survives the delete). */
        await page.keyboard.press('Control+z');
        const [again] = await waitForImages(page, 1);
        await expectPainted(page, again!.rect, RED);
    });

    test('Backspace deletes the selected picture too', async ({ page }) => {
        await insertPng(page, RED);
        const [img] = await waitForImages(page, 1);
        await clickPicture(page, img!.rect);
        await page.keyboard.press('Backspace');
        await waitForImages(page, 0);
        expect(await documentText(page)).toContain('above');
    });

    test('a click then Delete in one synchronous burst resolves against the shell-side selection', async ({
        page,
    }) => {
        await insertPng(page, RED);
        const [img] = await waitForImages(page, 1);
        await parkCaretAtTop(page);
        await burst(page, [
            { pointerdown: { x: img!.rect.x + img!.rect.w / 2, y: img!.rect.y + img!.rect.h / 2 } },
            { pointerup: true },
            { key: 'Delete' },
        ]);
        await settle(page);
        await waitForImages(page, 0);
    });

    test('Left / Right collapse to a caret at the picture\'s edges, in its own line', async ({
        page,
    }) => {
        await insertPng(page, RED);
        const [img] = await waitForImages(page, 1);
        const r = img!.rect;
        for (const [key, edgeX] of [
            ['ArrowLeft', r.x],
            ['ArrowRight', r.x + r.w],
        ] as const) {
            await clickPicture(page, r);
            await page.keyboard.press(key);
            await expect(page.locator('.image-handles')).toHaveCount(0);
            const c = await caretBox(page);
            expect(c, `${key}: caret drawn`).not.toBeNull();
            /* In the picture's line - never up in the table above it. */
            expect(c!.y + c!.h, `${key}: caret reaches the picture's line`).toBeGreaterThan(r.y);
            expect(c!.y, `${key}: caret starts inside the picture's line`).toBeLessThan(r.y + r.h);
            /* At the visual edge in the direction of travel, whatever the
               paragraph's direction. */
            expect(Math.abs(c!.x - edgeX), `${key}: caret x ${c!.x} vs edge ${edgeX}`).toBeLessThan(4);
            expect((await lastCaret(page)).block, `${key}: caret in the picture's paragraph`).toBe(2);
        }
    });

    test('Up from the selected picture lands in the table\'s last row, Down stays in its line', async ({
        page,
    }) => {
        await insertPng(page, RED);
        const [img] = await waitForImages(page, 1);
        const r = img!.rect;

        await clickPicture(page, r);
        await page.keyboard.press('ArrowDown');
        /* Nothing below the picture's paragraph: the caret stays in it. */
        expect((await lastCaret(page)).block).toBe(2);
        const down = await caretBox(page);
        expect(down!.y + down!.h).toBeGreaterThan(r.y);

        await clickPicture(page, r);
        await page.keyboard.press('ArrowUp');
        const up = await lastCaret(page);
        expect(up.block, 'the table').toBe(1);
        expect(up.row, 'the table\'s LAST row (2 rows), not its first').toBe(1);
        const c = await caretBox(page);
        expect(c!.y + c!.h, 'above the picture').toBeLessThanOrEqual(r.y + 1);
    });

    test('typing with the picture selected inserts before it', async ({ page }) => {
        await insertPng(page, RED);
        const [img] = await waitForImages(page, 1);
        expect(img!.at).toBe(0);
        await clickPicture(page, img!.rect);
        await page.keyboard.type('x');
        await settle(page);
        const [after] = await waitForImages(page, 1);
        /* 'x' (1 byte) went in front of the picture's sentinel. */
        expect(after!.at).toBe(1);
    });

    test('after an undone insert, a DIFFERENT picture under the re-minted key paints its own pixels', async ({
        page,
    }) => {
        await insertPng(page, RED);
        const [first] = await waitForImages(page, 1);
        await expectPainted(page, first!.rect, RED);
        await page.keyboard.press('Control+z');
        await waitForImages(page, 0);
        await insertPng(page, GREEN);
        const [second] = await waitForImages(page, 1);
        /* The engine mints `nge_img_1` again; the cache still holds the red
           bitmap under that key. */
        expect(second!.media_key).toBe(first!.media_key);
        await expectPainted(page, second!.rect, GREEN);
    });
    /* The recovered engine is a fresh one: its image cache starts empty and
       its document comes back from the event log (the logged INSERT_IMAGE in
       the replayed tail, or the snapshot that holds the picture's bytes). */
    for (const withSnapshot of [false, true]) {
        test(`a picture inserted before a worker trap paints after recovery (${
            withSnapshot ? 'snapshot + tail' : 'tail only'
        })`, async ({ page }) => {
            test.setTimeout(60_000);
            await insertPng(page, RED);
            const [img] = await waitForImages(page, 1);
            await expectPainted(page, img!.rect, RED);
            /* The worker's idle snapshot fires 1.5 s after the last logged
               command. */
            if (withSnapshot) await page.waitForTimeout(2_500);
            await page.evaluate(async () => {
                const w = window as any;
                w.__recovered = false;
                await w.__engineClient.armTrap(1);
                await w.__dispatch({ type: 'PING' }).catch(() => undefined);
                for (let i = 0; i < 600 && w.__recovered !== true; i++) {
                    await new Promise((r) => setTimeout(r, 50));
                }
            });
            expect(await page.evaluate(() => (window as any).__recovered)).toBe(true);
            const info = await page.evaluate(() => (window as any).__engineClient.lastRecovery);
            expect(info.restored, 'base snapshot restored').toBe(withSnapshot);
            const [again] = await waitForImages(page, 1);
            await expectPainted(page, again!.rect, RED);
        });
    }
});

/* A picture inside a text box (#206) is addressed by a story-rooted path, and
 * `DeleteRange` is body-only, so Delete on it goes through the story: select
 * the sentinel there, then delete the selection - only while the engine is
 * EDITING that box (#554). Issue #506: a click on the picture from OUTSIDE its
 * box enters the box and selects the picture in one go, so Delete works from
 * there too (the race and nested cases are in `image-selection-flows.spec.ts`). */
test.describe('a picture inside a text box (#554, #506)', () => {
    const FIXTURE = fileURLToPath(
        new URL('../../crates/format-docx/tests/fixtures/text_box_pictures.docx', import.meta.url),
    );

    test('Delete removes it, whether its box was being edited or entered by the click itself', async ({
        page,
    }) => {
        test.setTimeout(90_000);
        await boot(page);
        const bytes = Array.from(readFileSync(FIXTURE));
        const scale = await page.evaluate(async (arr: number[]) => {
            const d = (cmd: unknown): Promise<any> => (window as any).__dispatch(cmd);
            const top = { path: { steps: [{ kind: 'BLOCK', idx: 0 }] }, offset: 0 };
            const loaded = await d({ type: 'LOAD_DOCX', bytes: new Uint8Array(arr) });
            if (loaded.type === 'ERROR') throw new Error(loaded.message);
            const sel = await d({ type: 'SET_SELECTION', range: { start: top, end: top }, caret: top });
            /* The editing story of every SELECTION_CHANGED (no SELECT_ALL
               probe, which would move the selection). */
            const w = window as any;
            w.__editing = null;
            w.__engineClient.subscribe((e: any) => {
                if (e.type === 'SELECTION_CHANGED') w.__editing = e.editing_story?.rid ?? null;
            });
            return (sel.caret.x as number) / 72;
        }, bytes);
        const inch = 72 * scale;
        const list = async (): Promise<Record<string, Img>> => {
            const out: Record<string, Img> = {};
            for (const im of await images(page)) out[im.story_rid ?? ''] = im;
            return out;
        };
        const rids = async (): Promise<string[]> => Object.keys(await list()).sort();
        expect(await rids()).toEqual(['1@0', '1@0/1@0']);

        const canvas = page.locator('.editor-page[data-page-index="0"] canvas').first();
        await canvas.scrollIntoViewIfNeeded();
        const handles = page.locator('.image-handles');
        const select = async (rid: string): Promise<void> => {
            await expect(async () => {
                const im = (await list())[rid]!;
                await clickRect(page, im.rect);
                await expect(handles).toBeVisible({ timeout: 1_000 });
            }).toPass({ timeout: 15_000 });
        };
        const editing = (): Promise<string | null> =>
            settle(page).then(() => page.evaluate(() => (window as any).__editing as string | null));

        /* From the body: ONE click on the outer picture enters its box and
           selects it; Delete removes it, and no body character at the old
           caret. */
        const before = await documentText(page);
        await dispatch(page, { type: 'MOVE_CARET', direction: 'DocHome', extend: false });
        expect(await editing()).toBeNull();
        await select('1@0');
        expect(await editing(), 'the click entered the picture\'s box').toBe('1@0');
        await page.keyboard.press('Delete');
        await expect.poll(rids).toEqual(['1@0/1@0']);
        await page.keyboard.press('Control+z');
        await expect.poll(rids).toEqual(['1@0', '1@0/1@0']);
        /* Back in the body, its text is what it was. */
        await page.keyboard.press('Escape');
        await expect.poll(editing).toBeNull();
        expect(await documentText(page)).toBe(before);

        /* Inside the outer box's story (a click on its text): Delete removes
           the outer picture, nothing else. */
        const inBox = await clientPoint(page, 1.3 * inch, 4.85 * inch);
        await page.mouse.click(inBox.x, inBox.y);
        await expect.poll(editing).toBe('1@0');
        await select('1@0');
        await page.keyboard.press('Delete');
        await expect.poll(rids).toEqual(['1@0/1@0']);
        /* One undo brings it back. */
        await page.keyboard.press('Control+z');
        await expect.poll(rids).toEqual(['1@0', '1@0/1@0']);
    });
});
