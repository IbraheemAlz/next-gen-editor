import { test, expect, type Page } from '@playwright/test';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { boot, burst, documentText, settle } from './helpers/editor';
import {
    RED,
    caretBox,
    clickRect,
    dispatch,
    expectPainted,
    images,
    insertPng,
    waitForImages,
    type Img,
} from './helpers/images';

/* Issue #506 (the residue after 6726cc5 / #554) — the flows around a
 * picture's OBJECT selection (clean-room memo `onlyoffice-cursor-memo.md`
 * §2 / §9), driven through the real UI:
 *
 *   1. after INSERT_IMAGE the picture is object-selected exactly as if it
 *      had been clicked (handles, no caret; epic #555 row 31), and typing
 *      leaves the selection with the caret before the picture, so the text
 *      goes in FRONT of it (memo §9) — also for a key typed in the very
 *      task the insert's reply lands in;
 *   2. Ctrl+C / Ctrl+X act on the picture, not on the stale text selection
 *      the click left behind: the copy carries the picture, the cut removes
 *      it in ONE undo step, and pasting that payload re-inserts it;
 *   3. no frame shows the caret and the handles together, and a key that
 *      leaves the selection never flashes the stale caret: a
 *      MutationObserver + rAF sampler records the overlay DOM in every
 *      mutation batch and every frame;
 *   4. a picture of ANOTHER story — a text-box picture clicked from the
 *      body, a body picture clicked from inside a box (#220) — is entered
 *      and selected by one click (memo §2: only the active story's drawings
 *      are hit-testable), and a Delete fired in the same task as that click
 *      still removes it. */

test.use({ viewport: { width: 1280, height: 1400 } });

/** `above` and an empty paragraph with the caret in it (no table, so the
 *  picture is the only thing below the text). */
async function seed(page: Page): Promise<void> {
    await page.evaluate(async () => {
        const d = (c: unknown): Promise<any> => (window as any).__dispatch(c);
        await d({ type: 'SELECT_ALL' });
        await d({ type: 'INSERT_TEXT', at: undefined, text: 'above' });
        await d({ type: 'SPLIT_PARAGRAPH', at: undefined });
    });
}

const handles = (page: Page) => page.locator('.image-handles');

/** Start recording the overlay DOM: one sample per MutationObserver batch
 *  (every task that changed the DOM) and one per animation frame. */
async function startRecorder(page: Page): Promise<void> {
    await page.evaluate(() => {
        const w = window as any;
        const samples: Array<{
            src: string;
            caret: { x: number; y: number } | null;
            handles: boolean;
        }> = [];
        w.__samples = samples;
        const snap = (src: string): void => {
            const c = document.querySelector<HTMLElement>('.editor-page[data-page-index="0"] .caret');
            samples.push({
                src,
                caret: c ? { x: parseFloat(c.style.left), y: parseFloat(c.style.top) } : null,
                handles: document.querySelector('.image-handles') !== null,
            });
        };
        snap('start');
        const mo = new MutationObserver(() => snap('mutation'));
        mo.observe(document.body, {
            subtree: true,
            childList: true,
            attributes: true,
            attributeFilter: ['style'],
        });
        let raf = requestAnimationFrame(function tick() {
            snap('frame');
            raf = requestAnimationFrame(tick);
        });
        w.__stopRecorder = () => {
            mo.disconnect();
            cancelAnimationFrame(raf);
        };
    });
}

type Sample = { src: string; caret: { x: number; y: number } | null; handles: boolean };

async function stopRecorder(page: Page): Promise<Sample[]> {
    await settle(page);
    /* Two more frames, so a late flash would be sampled. */
    await page.evaluate(
        () => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))),
    );
    return page.evaluate(() => {
        const w = window as any;
        w.__stopRecorder();
        return w.__samples as Sample[];
    });
}

const both = (samples: Sample[]): Sample[] => samples.filter((s) => s.caret && s.handles);

test.describe('the picture right after INSERT_IMAGE (#506 item 1, #555 row 31)', () => {
    test.beforeEach(async ({ page }) => {
        await boot(page);
        await seed(page);
    });

    test('is object-selected: handles, no caret, no text highlight — then typing goes in front of it', async ({
        page,
    }) => {
        await startRecorder(page);
        await insertPng(page, RED);
        const [img] = await waitForImages(page, 1);
        await expect(handles(page)).toHaveCount(1);
        expect(await caretBox(page), 'object selection replaces the caret').toBeNull();
        await expect(page.locator('.selection-rect')).toHaveCount(0);
        const samples = await stopRecorder(page);
        expect(both(samples), 'never the caret and the handles together').toEqual([]);
        /* The reply moved the engine caret past the picture: that caret is
           never drawn. */
        const r = img!.rect;
        const dpr = await page.evaluate(() => window.devicePixelRatio || 1);
        const behind = samples.filter((s) => s.caret && s.caret.x > (r.x + r.w / 2) / dpr);
        expect(behind, 'the caret behind the new picture never shows').toEqual([]);

        /* Memo §9: typing leaves the object selection with the caret just
           BEFORE the picture's anchor - the picture stays, the text goes in
           front of it. */
        await page.keyboard.type('xy');
        await settle(page);
        const [after] = await waitForImages(page, 1);
        expect(after!.at, 'two bytes went in front of the sentinel').toBe(2);
        await expect(handles(page)).toHaveCount(0);
        expect(await caretBox(page)).not.toBeNull();
        await expectPainted(page, after!.rect, RED);
    });

    test('Enter right after the insert splits in front of the picture and keeps it', async ({ page }) => {
        /* Enter leaves the object selection with the caret before the
           picture, then splits there: the picture moves down with the
           right half. `Paragraph::split_at` used to drop the right half's
           inline objects - the picture vanished, an orphan U+FFFC stayed. */
        await insertPng(page, RED);
        await waitForImages(page, 1);
        await expect(handles(page)).toHaveCount(1);
        await page.keyboard.press('Enter');
        await settle(page);
        const [moved] = await waitForImages(page, 1);
        expect(moved!.at, 'at the start of the new paragraph').toBe(0);
        await expectPainted(page, moved!.rect, RED);
        await page.keyboard.press('Control+z');
        await settle(page);
        expect(await waitForImages(page, 1)).toHaveLength(1);
    });

    test('Delete right after the insert removes the picture in one undo step', async ({ page }) => {
        await insertPng(page, RED);
        await waitForImages(page, 1);
        await expect(handles(page)).toHaveCount(1);
        await page.keyboard.press('Delete');
        await waitForImages(page, 0);
        expect(await documentText(page)).toContain('above');
        await page.keyboard.press('Control+z');
        await waitForImages(page, 1);
    });

    test('a key typed in the same task as the insert\'s reply still resolves against the new picture', async ({
        page,
    }) => {
        /* The picture's rect only arrives with the NEXT reply (the paint's
           GET_IMAGE_RECTS); a key that lands before it must not fall through
           to the engine caret behind the picture. The key is fired from the
           client's own reply fan-out, i.e. in the reply's task. */
        await page.evaluate(async () => {
            const w = window as any;
            const sel = await w.__dispatch({ type: 'MOVE_CARET', direction: 'DocEnd', extend: false });
            const baseline = sel.undo_depth as number;
            const off = w.__engineClient.subscribe((e: any) => {
                if (e.type !== 'SELECTION_CHANGED' || e.undo_depth <= baseline) return;
                off();
                const ta = document.querySelector<HTMLTextAreaElement>('textarea[data-nge-hidden-input]')!;
                ta.dispatchEvent(new KeyboardEvent('keydown', { key: 'x', bubbles: true, cancelable: true }));
                ta.dispatchEvent(
                    new InputEvent('beforeinput', {
                        inputType: 'insertText',
                        data: 'x',
                        bubbles: true,
                        cancelable: true,
                    }),
                );
            });
        });
        await insertPng(page, RED);
        await expect.poll(async () => (await images(page))[0]?.at, { timeout: 15_000 }).toBe(1);
        expect(await documentText(page)).toContain('x\u{FFFC}');
    });
});

test.describe('copy / cut with a picture selected (#506 item 2)', () => {
    test.beforeEach(async ({ page, context }) => {
        await context.grantPermissions(['clipboard-read', 'clipboard-write']);
        await boot(page);
        await seed(page);
    });

    /** Every MIME type on the system clipboard, as text. */
    const clipboard = (page: Page): Promise<Record<string, string>> =>
        page.evaluate(async () => {
            const out: Record<string, string> = {};
            for (const item of await navigator.clipboard.read()) {
                for (const t of item.types) out[t] = await (await item.getType(t)).text();
            }
            return out;
        });

    /** Insert the picture, then leave a TEXT selection over `above` in the
     *  engine (setup) and click the picture: the engine's selection is the
     *  stale text range, the shell shows the object selection. */
    async function selectPictureOverStaleText(page: Page): Promise<Img> {
        await insertPng(page, RED);
        const [img] = await waitForImages(page, 1);
        await expectPainted(page, img!.rect, RED);
        const p = (offset: number) => ({ path: { steps: [{ kind: 'BLOCK', idx: 0 }] }, offset });
        await dispatch(page, { type: 'SET_SELECTION', range: { start: p(0), end: p(5) }, caret: p(5) });
        await page.evaluate(() => navigator.clipboard.writeText('before'));
        await clickRect(page, img!.rect);
        await expect(handles(page)).toHaveCount(1);
        /* Object selection replaces the text highlight too. */
        await expect(page.locator('.selection-rect')).toHaveCount(0);
        return img!;
    }

    test('Ctrl+C copies the picture, not the stale text selection, and keeps it selected', async ({
        page,
    }) => {
        await selectPictureOverStaleText(page);
        await page.keyboard.press('Control+c');
        await expect
            .poll(async () => (await clipboard(page))['text/html'] ?? '', { timeout: 10_000 })
            .toContain('<img');
        const clip = await clipboard(page);
        expect(clip['text/html']).not.toContain('above');
        expect(clip['text/plain']).toBe('\u{FFFC}');
        /* Copy does not leave the object selection. */
        await expect(handles(page)).toHaveCount(1);
        expect(await caretBox(page)).toBeNull();
        await expect(page.locator('.selection-rect')).toHaveCount(0);
        expect(await images(page)).toHaveLength(1);
    });

    test('Ctrl+X removes the picture in ONE undo step and Ctrl+V puts it back, painted', async ({
        page,
    }) => {
        await selectPictureOverStaleText(page);
        await page.keyboard.press('Control+x');
        await waitForImages(page, 0);
        await expect(handles(page)).toHaveCount(0);
        expect(await caretBox(page), 'the caret is back at the old anchor').not.toBeNull();
        const text = await documentText(page);
        expect(text, 'the stale text selection was not cut').toContain('above');
        expect(text).not.toContain('\u{FFFC}');
        expect((await clipboard(page))['text/html']).toContain('<img');

        /* The caret sits at the picture's old anchor: paste puts it back. */
        const p = (offset: number) => ({ path: { steps: [{ kind: 'BLOCK', idx: 1 }] }, offset });
        await dispatch(page, { type: 'SET_SELECTION', range: { start: p(0), end: p(0) }, caret: p(0) });
        await page.keyboard.press('Control+v');
        const [back] = await waitForImages(page, 1);
        await expectPainted(page, back!.rect, RED);
        expect(await documentText(page)).toContain('above');

        /* One undo takes the paste back, ONE more undoes the cut. */
        await page.keyboard.press('Control+z');
        await waitForImages(page, 0);
        await page.keyboard.press('Control+z');
        await waitForImages(page, 1);
    });
});

test.describe('no caret flash around the object selection (#506 item 3)', () => {
    test.beforeEach(async ({ page }) => {
        await boot(page);
        await seed(page);
    });

    /** Insert, park the engine caret at the top (far from the picture) and
     *  return the picture plus the parked caret's CSS y. */
    async function pictureAndParkedCaret(page: Page): Promise<{ img: Img; parkedY: number }> {
        await insertPng(page, RED);
        const [img] = await waitForImages(page, 1);
        await page.keyboard.press('Escape');
        await expect(handles(page)).toHaveCount(0);
        await dispatch(page, { type: 'MOVE_CARET', direction: 'DocHome', extend: false });
        const parked = await caretBox(page);
        expect(parked).not.toBeNull();
        return { img: img!, parkedY: parked!.y };
    }

    test('a click hides the caret in the same task the handles appear', async ({ page }) => {
        const { img } = await pictureAndParkedCaret(page);
        await startRecorder(page);
        await clickRect(page, img.rect);
        await expect(handles(page)).toHaveCount(1);
        const samples = await stopRecorder(page);
        expect(both(samples)).toEqual([]);
        /* The first sample with handles is also the first without a caret:
           the two DOM changes land in one batch. */
        const first = samples.findIndex((s) => s.handles);
        expect(first).toBeGreaterThan(0);
        expect(samples[first]!.caret).toBeNull();
        expect(samples.slice(0, first).every((s) => s.caret !== null)).toBe(true);
    });

    for (const key of ['Delete', 'ArrowLeft', 'Escape'] as const) {
        test(`${key} on the selected picture never draws the stale caret`, async ({ page }) => {
            const { img, parkedY } = await pictureAndParkedCaret(page);
            await clickRect(page, img.rect);
            await expect(handles(page)).toHaveCount(1);
            await startRecorder(page);
            await page.keyboard.press(key);
            await expect(handles(page)).toHaveCount(0);
            await expect.poll(() => caretBox(page).then((c) => c !== null)).toBe(true);
            const samples = await stopRecorder(page);
            expect(both(samples)).toEqual([]);
            const stale = samples.filter((s) => s.caret && Math.abs(s.caret.y - parkedY) < 2);
            expect(stale, `the caret parked at y=${parkedY} never reappears`).toEqual([]);
            /* And it does land at the picture's line. */
            const end = await caretBox(page);
            expect(end!.y).toBeGreaterThan(parkedY + 5);
        });
    }
});

test.describe('a picture of another story: one click enters it and selects (#506 item 4, #220)', () => {
    const FIXTURE = fileURLToPath(
        new URL('../../crates/format-docx/tests/fixtures/text_box_pictures.docx', import.meta.url),
    );

    test.beforeEach(async ({ page }) => {
        test.setTimeout(90_000);
        await boot(page);
        const bytes = Array.from(readFileSync(FIXTURE));
        await page.evaluate(async (arr: number[]) => {
            const d = (cmd: unknown): Promise<any> => (window as any).__dispatch(cmd);
            const top = { path: { steps: [{ kind: 'BLOCK', idx: 0 }] }, offset: 0 };
            const loaded = await d({ type: 'LOAD_DOCX', bytes: new Uint8Array(arr) });
            if (loaded.type === 'ERROR') throw new Error(loaded.message);
            await d({ type: 'SET_SELECTION', range: { start: top, end: top }, caret: top });
            const w = window as any;
            w.__editing = null;
            w.__engineClient.subscribe((e: any) => {
                if (e.type === 'SELECTION_CHANGED') w.__editing = e.editing_story?.rid ?? null;
            });
        }, bytes);
        await page.locator('.editor-page[data-page-index="0"] canvas').first().scrollIntoViewIfNeeded();
    });

    const byRid = async (page: Page): Promise<Record<string, Img>> => {
        const out: Record<string, Img> = {};
        for (const im of await images(page)) out[im.story_rid ?? ''] = im;
        return out;
    };
    const rids = async (page: Page): Promise<string[]> => Object.keys(await byRid(page)).sort();
    const editing = (page: Page): Promise<string | null> =>
        settle(page).then(() => page.evaluate(() => (window as any).__editing as string | null));

    test('from the body, a click + Delete in ONE synchronous burst removes a nested box\'s picture', async ({
        page,
    }) => {
        await expect.poll(() => rids(page)).toEqual(['1@0', '1@0/1@0']);
        expect(await editing(page)).toBeNull();
        const r = (await byRid(page))['1@0/1@0']!.rect;
        /* The Delete is handled before the click's entry into the box has
           answered: the key path must already count the box as entered. */
        await burst(page, [
            { pointerdown: { x: r.x + r.w / 2, y: r.y + r.h / 2 } },
            { pointerup: true },
            { key: 'Delete' },
        ]);
        await expect.poll(() => rids(page)).toEqual(['1@0']);
        expect(await editing(page), 'the click entered the innermost box').toBe('1@0/1@0');
        await page.keyboard.press('Control+z');
        await expect.poll(() => rids(page)).toEqual(['1@0', '1@0/1@0']);
    });

    test('from inside a box, one click on a body picture leaves the box and selects it (#220)', async ({
        page,
    }) => {
        /* A body picture above the box (Original size keeps it clear of it). */
        await page.locator('select[aria-label="Image fit mode"]').selectOption('Original');
        await insertPng(page, RED);
        await expect.poll(() => rids(page)).toEqual(['', '1@0', '1@0/1@0']);
        await page.keyboard.press('Escape');

        /* Into the outer box: one click on its picture enters + selects. */
        await clickRect(page, (await byRid(page))['1@0']!.rect);
        await expect(handles(page)).toHaveCount(1);
        expect(await editing(page)).toBe('1@0');

        /* One click on the body picture: back in the body, picture selected. */
        await clickRect(page, (await byRid(page))['']!.rect);
        await expect(handles(page)).toHaveCount(1);
        expect(await editing(page), 'the click left the box').toBeNull();
        await page.keyboard.press('Delete');
        await expect.poll(() => rids(page)).toEqual(['1@0', '1@0/1@0']);
    });
});
