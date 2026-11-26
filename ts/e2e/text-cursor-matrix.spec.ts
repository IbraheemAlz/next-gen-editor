import { test, expect, type Page } from '@playwright/test';
import { boot, documentText, settle } from './helpers/editor';
import { caretBox } from './helpers/images';
import {
    REL,
    docxPackage,
    floatingPictureRun,
    pictureDocx,
    pictureRun,
    png,
    sectPr,
} from './helpers/picture-docx';

/* Epic #555 — the TEXT / OBJECT rows of the cursor-behaviour matrix, each
 * pinned through the REAL shell: Playwright mouse events on a page canvas
 * (`pointer.ts`), keys and composition events on the hidden textarea
 * (`HiddenInput`). `__dispatch` only builds the document and parks the
 * caret before the action under test (setup); the evidence is engine truth
 * (every `SELECTION_CHANGED` the client receives, the document text read
 * back through the worker queue, the worker-side pixel probe) — headless
 * Chrome never composites the interactive canvas (CLAUDE.md), so nothing
 * here reads a screenshot.
 *
 * Expected behaviour: the clean-room methods memo
 * `plans/cleanroom/memos/onlyoffice-cursor-memo.md` (section numbers in
 * each test). */

test.use({ viewport: { width: 1280, height: 900 } });

type Step = { kind: 'BLOCK'; idx: number } | { kind: 'CELL'; row: number; col: number };
interface Pos {
    path: { steps: Step[] };
    offset: number;
}
interface Rect {
    x: number;
    y: number;
    w: number;
    h: number;
}

const pos = (block: number, offset: number): Pos => ({
    path: { steps: [{ kind: 'BLOCK', idx: block }] },
    offset,
});
const utf8 = (s: string): number => new TextEncoder().encode(s).length;

const dispatch = (page: Page, cmd: unknown): Promise<any> =>
    page.evaluate((c) => (window as any).__dispatch(c), cmd);

/** Record every `SELECTION_CHANGED` the client sees (replies to the shell's
 *  own commands included). */
async function watchSelection(page: Page): Promise<void> {
    await page.evaluate(() => {
        const log: any[] = [];
        (window as any).__selLog = log;
        (window as any).__engineClient.subscribe((e: any) => {
            if (e.type === 'SELECTION_CHANGED') log.push(e);
        });
    });
}

interface Sel {
    start: { block: number; offset: number };
    end: { block: number; offset: number };
    story: string | null;
}

/** The engine's latest selection (ordered range + the story it edits),
 *  after every command already posted has been answered. */
async function selection(page: Page): Promise<Sel> {
    await settle(page);
    return page.evaluate(() => {
        const log = (window as any).__selLog as any[];
        const e = log[log.length - 1];
        const at = (p: any) => ({ block: p.path.steps[0].idx as number, offset: p.offset as number });
        return {
            start: at(e.range.start),
            end: at(e.range.end),
            story: (e.editing_story?.area as string | undefined) ?? null,
        };
    });
}

/** The collapsed caret's offset (asserts the selection IS collapsed). */
async function caretOffset(page: Page): Promise<{ block: number; offset: number }> {
    const s = await selection(page);
    expect(s.start, 'a collapsed caret').toEqual(s.end);
    return s.start;
}

/** Replace the document with plain text (one paragraph per line). */
async function openText(page: Page, text: string): Promise<void> {
    const evt = await dispatch(page, {
        type: 'OPEN_DOCUMENT',
        bytes: new TextEncoder().encode(text),
        format: 'plain_text',
        name: 'matrix.txt',
    });
    expect(evt.type).toBe('DOCUMENT_LOADED');
    await settle(page);
}

async function openDocx(page: Page, bytes: Uint8Array): Promise<void> {
    const type = await page.evaluate(async (arr: number[]) => {
        const evt = await (window as any).__dispatch({
            type: 'OPEN_DOCUMENT',
            bytes: new Uint8Array(arr),
            format: 'docx',
            name: 'matrix.docx',
        });
        return evt.type as string;
    }, Array.from(bytes));
    expect(type).toBe('DOCUMENT_LOADED');
    await settle(page);
}

/** Setup: a collapsed caret at `(block, offset)`; returns its rect (page-0
 *  local device px). */
async function caretAt(page: Page, block: number, offset: number): Promise<Rect> {
    const p = pos(block, offset);
    const evt = await dispatch(page, { type: 'SET_SELECTION', range: { start: p, end: p }, caret: p });
    expect(evt.type).toBe('SELECTION_CHANGED');
    return evt.caret as Rect;
}

/** The paragraphs' texts as the engine holds them. */
async function paragraphs(page: Page): Promise<string[]> {
    return (await documentText(page)).split('\n');
}

/** Viewport (mouse) coordinates of a page-0 device-px point. */
async function client(page: Page, x: number, y: number): Promise<{ x: number; y: number }> {
    const canvas = page.locator('.editor-page[data-page-index="0"] canvas').first();
    const box = await canvas.boundingBox();
    if (!box) throw new Error('page 0 canvas has no box');
    const dpr = await page.evaluate(() => window.devicePixelRatio || 1);
    return { x: box.x + x / dpr, y: box.y + y / dpr };
}

async function clickAt(page: Page, x: number, y: number): Promise<void> {
    const c = await client(page, x, y);
    await page.mouse.click(c.x, c.y);
}

/** UAX #29 extended grapheme cluster boundaries of `text`, in UTF-8 bytes. */
function graphemeBoundaries(text: string): number[] {
    const seg = new Intl.Segmenter('en', { granularity: 'grapheme' });
    const out: number[] = [];
    for (const s of seg.segment(text)) out.push(utf8(text.slice(0, s.index)));
    out.push(utf8(text));
    return out;
}

test.beforeEach(async ({ page }) => {
    await boot(page);
    await watchSelection(page);
    const canvas = page.locator('.editor-page[data-page-index="0"] canvas').first();
    await canvas.scrollIntoViewIfNeeded();
});

/* ------------------------------------------------------------------ */
/* Row 2 — memo §1 (normalisation: never inside a base + combining-mark
 * cluster) and §3 (one grapheme cluster per press). */
test('row 02: Left/Right step one grapheme cluster and a click never lands inside one', async ({
    page,
}) => {
    test.setTimeout(90_000);
    /* An RTL paragraph of letters carrying harakat (kaf / teh / beh, each
       with a fatha; the beh also a shadda), an LTR paragraph of decomposed
       accents, an LTR paragraph around an emoji ZWJ family. */
    await openText(page, 'كَتَبَّ\nété\na👩‍👩‍👧b');
    const texts = await paragraphs(page);
    expect(texts).toHaveLength(3);
    const cases = [
        { block: 0, forward: 'ArrowLeft', back: 'ArrowRight' }, // RTL: Left = logical forward
        { block: 1, forward: 'ArrowRight', back: 'ArrowLeft' },
        { block: 2, forward: 'ArrowRight', back: 'ArrowLeft' },
    ];
    for (const c of cases) {
        const text = texts[c.block]!;
        const stops = graphemeBoundaries(text);
        /* Sanity: every case really has multi-scalar clusters. */
        expect(stops.length - 1, `block ${c.block} clusters`).toBeLessThan([...text].length);

        await caretAt(page, c.block, 0);
        const forward: number[] = [];
        for (let i = 1; i < stops.length; i++) {
            await page.keyboard.press(c.forward);
            const at = await caretOffset(page);
            expect(at.block).toBe(c.block);
            forward.push(at.offset);
        }
        expect(forward, `block ${c.block}: ${c.forward} walks clusters`).toEqual(stops.slice(1));

        const back: number[] = [];
        for (let i = 1; i < stops.length; i++) {
            await page.keyboard.press(c.back);
            back.push((await caretOffset(page)).offset);
        }
        expect(back, `block ${c.block}: ${c.back} walks clusters back`).toEqual(
            stops.slice(0, -1).reverse(),
        );

        /* Clicks across the whole line (and a little past its ink): every
           resolved caret is a cluster boundary. */
        const a = await caretAt(page, c.block, 0);
        const b = await caretAt(page, c.block, utf8(text));
        const lo = Math.min(a.x, b.x) - 6;
        const hi = Math.max(a.x, b.x) + 6;
        const y = a.y + a.h / 2;
        const landed = new Set<number>();
        for (let x = lo; x <= hi; x += 1.5) {
            await clickAt(page, x, y);
            const at = await caretOffset(page);
            expect(at.block).toBe(c.block);
            expect(stops, `block ${c.block}: click at x=${x.toFixed(1)} -> ${at.offset}`).toContain(
                at.offset,
            );
            landed.add(at.offset);
        }
        /* The sweep reached more than the two ends. */
        expect(landed.size, `block ${c.block}: distinct click stops`).toBeGreaterThan(2);
    }
});

/* ------------------------------------------------------------------ */
/* Row 3 — memo §2 "Paragraph": the line under (or nearest to) the pointer;
 * an x outside the line's ink clamps to its logical start or end,
 * mirrored for an RTL paragraph. Pinned where "nearest line" and the
 * memo's paragraph-owns-its-spacing rule agree; where they part (the lower
 * half of a space-after gap) see #572. */
test('row 03: a click picks the nearest line and clamps outside the ink to its logical start / end (LTR and RTL)', async ({
    page,
}) => {
    const LTR = 'Hello world';
    const RTL = 'مرحبا بالعالم';
    /* 24 pt after the LTR paragraph and 24 pt before the RTL one: a 48 pt
       gap whose upper half belongs to the LTR paragraph, its lower half to
       the RTL one (the nearest line, and the paragraph whose spacing it
       is). */
    const para = (ppr: string, run: string): string => `<w:p><w:pPr>${ppr}</w:pPr>${run}</w:p>`;
    await openDocx(
        page,
        docxPackage({
            body:
                para(
                    '<w:spacing w:before="0" w:after="480"/>',
                    `<w:r><w:t xml:space="preserve">${LTR}</w:t></w:r>`,
                ) +
                para(
                    '<w:bidi/><w:spacing w:before="480" w:after="0"/>',
                    `<w:r><w:rPr><w:rtl/></w:rPr><w:t xml:space="preserve">${RTL}</w:t></w:r>`,
                ) +
                para('<w:spacing w:before="0" w:after="0"/>', '<w:r><w:t>third line</w:t></w:r>') +
                sectPr(1440),
        }),
    );
    const box = await page.locator('.editor-page[data-page-index="0"] canvas').first().boundingBox();
    const dpr = await page.evaluate(() => window.devicePixelRatio || 1);
    const pageW = box!.width * dpr;

    const l0 = await caretAt(page, 0, 0);
    const l1 = await caretAt(page, 0, utf8(LTR));
    const r0 = await caretAt(page, 1, 0);
    const r1 = await caretAt(page, 1, utf8(RTL));
    /* The RTL paragraph's logical start is its RIGHT edge. */
    expect(r0.x).toBeGreaterThan(r1.x);
    const mid = (r: Rect) => r.y + r.h / 2;
    const rightOfLtrInk = l1.x + 80 * dpr;
    const leftMargin = 10 * dpr;
    const rightMargin = pageW - 10 * dpr;

    /* LTR: right of the ink -> logical end; in the left margin -> start. */
    await caretAt(page, 2, 0);
    await clickAt(page, rightOfLtrInk, mid(l0));
    expect(await caretOffset(page)).toEqual({ block: 0, offset: utf8(LTR) });
    await clickAt(page, leftMargin, mid(l0));
    expect(await caretOffset(page)).toEqual({ block: 0, offset: 0 });

    /* RTL, mirrored: left of the ink -> logical END; in the right margin ->
       logical START. */
    await clickAt(page, r1.x - 80 * dpr, mid(r0));
    expect(await caretOffset(page)).toEqual({ block: 1, offset: utf8(RTL) });
    await clickAt(page, rightMargin, mid(r0));
    expect(await caretOffset(page)).toEqual({ block: 1, offset: 0 });
    /* And what the user sees: typing there lands at the logical start. */
    await page.keyboard.type('|');
    expect((await paragraphs(page))[1]).toBe(`|${RTL}`);
    await page.keyboard.press('Control+z');
    await expect.poll(async () => (await paragraphs(page))[1]).toBe(RTL);

    /* In the gap between the two lines: the nearer line, clamped the same
       way — the upper quarter goes to the LTR line (its logical end, right
       of the ink), the lower quarter to the RTL line (its logical start,
       in the right margin). */
    const gapTop = l0.y + l0.h;
    const gapBottom = r0.y;
    expect(gapBottom - gapTop, 'a real gap').toBeGreaterThan(20);
    await clickAt(page, rightOfLtrInk, gapTop + (gapBottom - gapTop) * 0.25);
    expect(await caretOffset(page)).toEqual({ block: 0, offset: utf8(LTR) });
    await clickAt(page, rightMargin, gapBottom - (gapBottom - gapTop) * 0.25);
    expect(await caretOffset(page)).toEqual({ block: 1, offset: 0 });
});

/* ------------------------------------------------------------------ */
/* Row 11 — memo §2 "Drawings are layer-scoped": only the active story's
 * drawing layer is hit-tested. The inert halves first; then the
 * active-story half (#573: a picture of the header being edited is an
 * object — click selects it, the first Esc only drops it (§8), Delete
 * removes it, a handle resizes it). */

test("row 11: only the active story's pictures are hit-testable (header picture inert in the body, body picture inert in the header)", async ({
    page,
}) => {
    test.setTimeout(60_000);
    await openDocx(page, pictureDocx());
    const handles = page.locator('.image-handles');

    /* Device px per pt: the body's first caret sits on the 1 in margin. */
    const scale = (await caretAt(page, 0, 0)).x / 72;

    /* Find the header picture on the page: the only ink in the header band
       (its bitmap, or the placeholder before the bitmap is decoded). */
    const grid: Array<{ x: number; y: number }> = [];
    for (let xpt = 76; xpt <= 140; xpt += 8) {
        for (let ypt = 20; ypt <= 110; ypt += 3) grid.push({ x: xpt * scale, y: ypt * scale });
    }
    const ink = (p: number[] | null): boolean =>
        !!p && p[3] === 255 && (p[0]! < 240 || p[1]! < 240 || p[2]! < 240);
    let hits: Array<{ x: number; y: number }> = [];
    await expect
        .poll(
            async () => {
                const px: Array<number[] | null> = await page.evaluate(
                    (pts) => (window as any).__engineClient.probePagePixels(0, pts),
                    grid,
                );
                hits = grid.filter((_, i) => ink(px[i]!));
                return hits.length;
            },
            { message: 'the header picture is on the page', timeout: 15_000 },
        )
        .toBeGreaterThan(8);
    const ys = hits.map((p) => p.y).sort((a, b) => a - b);
    const headerPic = {
        x: hits.reduce((s, p) => s + p.x, 0) / hits.length,
        y: ys[Math.floor(ys.length / 2)]!,
    };
    /* It IS in the header band: above the body's first line. */
    expect(headerPic.y).toBeLessThan((await caretAt(page, 0, 0)).y);

    /* Editing the BODY: a click on the header picture selects nothing; it is
       an ordinary body press (nearest body line), the body stays active. */
    await caretAt(page, 2, 3);
    await clickAt(page, headerPic.x, headerPic.y);
    await settle(page);
    await expect(handles).toHaveCount(0);
    const inBody = await selection(page);
    expect(inBody.story).toBeNull();
    /* The first body line is the nearest one to a point in the header
       band (the caret parked in block 2 moved there). */
    expect(inBody.start.block).toBe(0);
    expect(inBody.start).toEqual(inBody.end);

    /* Enter the header (a double-click on a blank stretch of its band). */
    const band = await client(page, 400 * scale, 54 * scale);
    await page.mouse.dblclick(band.x, band.y);
    await expect.poll(async () => (await selection(page)).story).toBe('Header');

    /* Editing the HEADER: a click on the body picture selects nothing and
       leaves the header active (a single body click is ignored). */
    const imgs = await dispatch(page, { type: 'GET_IMAGE_RECTS' });
    expect(imgs.type).toBe('IMAGE_RECTS');
    const bodyImgs = (imgs.images as Array<{ rect: Rect; story_area?: string }>).filter(
        (im) => !im.story_area,
    );
    expect(bodyImgs, 'only the body picture is an object of the body').toHaveLength(1);
    const body = bodyImgs[0]!.rect;
    await clickAt(page, body.x + body.w / 2, body.y + body.h / 2);
    await settle(page);
    await expect(handles).toHaveCount(0);
    expect((await selection(page)).story).toBe('Header');

    /* Sanity: back in the body, the same click on the body picture DOES
       select it (it is hit-testable in its own story). */
    await page.keyboard.press('Escape');
    await expect.poll(async () => (await selection(page)).story).toBeNull();
    await clickAt(page, body.x + body.w / 2, body.y + body.h / 2);
    await expect(handles).toHaveCount(1);
});

interface Pic {
    rect: Rect;
    at: number;
    floating: boolean;
    width_emu: number;
    story_area?: string;
    story_rid?: string;
}

/** The pictures `GET_IMAGE_RECTS` lists for the header (a query). */
async function headerPictures(page: Page): Promise<Pic[]> {
    const evt = await dispatch(page, { type: 'GET_IMAGE_RECTS' });
    expect(evt.type).toBe('IMAGE_RECTS');
    return (evt.images as Pic[]).filter((im) => im.story_area === 'Header');
}

/** A body picture (green) between two lines of text; a header with an
 *  inline picture (red) in its first paragraph and, anchored in its second,
 *  a FLOATING picture (blue) 4 in from the page's left edge, 0.4 .. 2.4 in
 *  from its top — past the 2 in top margin, so its lower part lies where
 *  the body zone begins. */
function headerPicturesDocx(): Uint8Array {
    const IN = 914_400;
    return docxPackage({
        body:
            '<w:p><w:r><w:t xml:space="preserve">Body text above the picture</w:t></w:r></w:p>' +
            `<w:p>${pictureRun('rId5')}</w:p>` +
            '<w:p><w:r><w:t xml:space="preserve">Body text below</w:t></w:r></w:p>' +
            sectPr(2880, '<w:headerReference w:type="default" r:id="rId7"/>'),
        docRels: [
            ['rId5', `${REL}image`, 'media/image1.png'],
            ['rId7', `${REL}header`, 'header1.xml'],
        ],
        header: {
            xml:
                `<w:p>${pictureRun('rId5')}</w:p>` +
                `<w:p>${floatingPictureRun('rId6', { x: 4 * IN, y: 0.4 * IN, cx: IN, cy: 2 * IN, id: 9 })}</w:p>`,
            rels: [
                ['rId5', `${REL}image`, 'media/image2.png'],
                ['rId6', `${REL}image`, 'media/image3.png'],
            ],
        },
        media: [
            ['word/media/image1.png', png(64, 32, [32, 176, 64])],
            ['word/media/image2.png', png(64, 32, [208, 32, 32])],
            ['word/media/image3.png', png(32, 64, [32, 64, 208])],
        ],
    });
}

/** Enter the header with a double-click on a blank stretch of its band. */
async function enterHeader(page: Page): Promise<void> {
    const scale = (await caretAt(page, 0, 0)).x / 72;
    const band = await client(page, 450 * scale, 54 * scale);
    await page.mouse.dblclick(band.x, band.y);
    await expect.poll(async () => (await selection(page)).story).toBe('Header');
}

const centre = (r: Rect) => ({ x: r.x + r.w / 2, y: r.y + r.h / 2 });

test("row 11 (#573): a picture of the header being edited is an object — click, Esc, Delete, undo, resize, the float's lower part", async ({
    page,
}) => {
    test.setTimeout(90_000);
    await openDocx(page, headerPicturesDocx());
    const handles = page.locator('.image-handles');
    expect(await headerPictures(page), 'inert while the body is edited').toEqual([]);

    await enterHeader(page);
    await expect.poll(async () => (await headerPictures(page)).length).toBe(2);
    const listed = await headerPictures(page);
    const inline = listed.find((im) => !im.floating)!;
    const float = listed.find((im) => im.floating)!;
    expect(inline.at).toBe(0);
    const pic = centre(inline.rect);

    /* A click on the header's inline picture object-selects it: handles,
       no caret, the header still the active story. */
    await clickAt(page, pic.x, pic.y);
    await expect(handles).toHaveCount(1);
    expect(await caretBox(page), 'object selection replaces the caret').toBeNull();
    expect((await selection(page)).story).toBe('Header');

    /* The FIRST Esc only drops the object selection (memo §8): the caret
       goes before the picture, in the header. */
    await page.keyboard.press('Escape');
    await expect(handles).toHaveCount(0);
    const afterEsc = await selection(page);
    expect(afterEsc.story).toBe('Header');
    expect(afterEsc.start).toEqual({ block: 0, offset: 0 });
    expect(await caretBox(page)).not.toBeNull();

    /* Delete removes it, in ONE undo step. */
    await clickAt(page, pic.x, pic.y);
    await expect(handles).toHaveCount(1);
    await page.keyboard.press('Delete');
    await expect
        .poll(async () => (await headerPictures(page)).filter((im) => !im.floating).length)
        .toBe(0);
    await expect(handles).toHaveCount(0);
    expect((await selection(page)).story).toBe('Header');
    await page.keyboard.press('Control+z');
    await expect
        .poll(async () => (await headerPictures(page)).filter((im) => !im.floating).length)
        .toBe(1);

    /* A handle resizes it (RESIZE_IMAGE addressed in the part). */
    await clickAt(page, pic.x, pic.y);
    await expect(handles).toHaveCount(1);
    const se = await page.locator('.image-handle--se').boundingBox();
    expect(se).not.toBeNull();
    const from = { x: se!.x + se!.width / 2, y: se!.y + se!.height / 2 };
    await page.mouse.move(from.x, from.y);
    await page.mouse.down();
    await page.mouse.move(from.x + 20, from.y + 10, { steps: 4 });
    await page.mouse.move(from.x + 40, from.y + 20, { steps: 4 });
    await page.mouse.up();
    await expect
        .poll(async () => (await headerPictures(page)).find((im) => !im.floating)?.width_emu ?? 0)
        .toBeGreaterThan(inline.width_emu);
    expect((await selection(page)).story).toBe('Header');

    /* The float reaches below the band: a point inside a header drawing
       counts as that band (memo §2), so its lower part selects it too. */
    const lowY = float.rect.y + float.rect.h * 0.9;
    /* Device px per pt: the inline picture is 1 in (72 pt) wide. */
    const scale = inline.rect.w / 72;
    expect(lowY, 'below the 2 in top margin').toBeGreaterThan(144 * scale);
    await page.keyboard.press('Escape');
    await expect(handles).toHaveCount(0);
    await clickAt(page, centre(float.rect).x, lowY);
    await expect(handles).toHaveCount(1);
    expect((await selection(page)).story).toBe('Header');

    /* Esc drops the object, a second Esc leaves the header; in the body
       the header's pictures are inert again. */
    await page.keyboard.press('Escape');
    await expect(handles).toHaveCount(0);
    expect((await selection(page)).story).toBe('Header');
    await page.keyboard.press('Escape');
    await expect.poll(async () => (await selection(page)).story).toBeNull();
    await expect.poll(async () => (await headerPictures(page)).length).toBe(0);
    await clickAt(page, centre(float.rect).x, centre(float.rect).y);
    await settle(page);
    await expect(handles).toHaveCount(0);
});

test('row 11 (#573): a double-click on a body picture from the header leaves the header and selects it', async ({
    page,
}) => {
    test.setTimeout(60_000);
    await openDocx(page, headerPicturesDocx());
    const handles = page.locator('.image-handles');
    await enterHeader(page);
    const evt = await dispatch(page, { type: 'GET_IMAGE_RECTS' });
    const body = (evt.images as Pic[]).find((im) => !im.story_area)!;
    const c = centre(body.rect);

    /* A single click on it is ignored (memo §2): nothing selected, still
       the header. */
    await clickAt(page, c.x, c.y);
    await settle(page);
    await expect(handles).toHaveCount(0);
    expect((await selection(page)).story).toBe('Header');

    /* A double-click leaves the header (memo §2) and, the press being on
       an object, selects the picture instead of placing a caret. */
    const at = await client(page, c.x, c.y);
    await page.mouse.dblclick(at.x, at.y);
    await expect.poll(async () => (await selection(page)).story).toBeNull();
    await expect(handles).toHaveCount(1);
    expect(await caretBox(page), 'object selection replaces the caret').toBeNull();
    /* It is the body's picture: Delete removes it from the body. */
    await page.keyboard.press('Delete');
    await expect
        .poll(async () => {
            const e = await dispatch(page, { type: 'GET_IMAGE_RECTS' });
            return (e.images as Pic[]).filter((im) => !im.story_area).length;
        })
        .toBe(0);
    expect((await paragraphs(page))[1]).toBe('');
});

/* ------------------------------------------------------------------ */
/* Row 14 — memo §3 "Fields": a selection whose ends sit in different
 * fields grows in the direction of travel to cover them whole — fields
 * are atomic for selection. Not for the caret (Left / Right enter the
 * result): ours is atomic for the caret too since #77 — #570. */
test('row 14: a field is atomic for selection — Shift+Right and a drag cover it whole', async ({
    page,
}) => {
    await dispatch(page, { type: 'SET_RENDER_DATE', year: 2026, month: 10, day: 23 });
    await openText(page, 'Due  done');
    const evt = await dispatch(page, { type: 'INSERT_FIELD', at: pos(0, 4), kind: 'Date' });
    expect(evt.type).not.toBe('ERROR');
    const text = (await paragraphs(page))[0]!;
    expect(text).toBe('Due 10/23/2026 done');
    const fStart = 4;
    const fEnd = 4 + utf8('10/23/2026');

    /* Shift+Right from the field's start covers it whole in one press. */
    await caretAt(page, 0, fStart);
    await page.keyboard.press('Shift+ArrowRight');
    let s = await selection(page);
    expect([s.start.offset, s.end.offset]).toEqual([fStart, fEnd]);
    /* Shift+Left from the field's end, the same backwards. */
    await caretAt(page, 0, fEnd);
    await page.keyboard.press('Shift+ArrowLeft');
    s = await selection(page);
    expect([s.start.offset, s.end.offset]).toEqual([fStart, fEnd]);

    /* A drag that stops in the middle of the field covers the field whole,
       in the direction of travel. */
    const a = await caretAt(page, 0, 2);
    const fs = await caretAt(page, 0, fStart);
    const fe = await caretAt(page, 0, fEnd);
    const z = await caretAt(page, 0, utf8(text) - 2);
    const y = a.y + a.h / 2;
    const midField = (fs.x + fe.x) / 2;
    const drag = async (fromX: number, toX: number): Promise<void> => {
        const from = await client(page, fromX, y);
        const to = await client(page, toX, y);
        await page.mouse.move(from.x, from.y);
        await page.mouse.down();
        await page.mouse.move(to.x, to.y, { steps: 6 });
        await page.mouse.up();
    };
    await drag(a.x, midField);
    s = await selection(page);
    expect([s.start.offset, s.end.offset], 'forward drag into the field').toEqual([2, fEnd]);
    await drag(z.x, midField);
    s = await selection(page);
    expect([s.start.offset, s.end.offset], 'backward drag into the field').toEqual([
        fStart,
        utf8(text) - 2,
    ]);
});

/* ------------------------------------------------------------------ */
/* Row 20 — memo §5: Ctrl+Left/Right are logical, flip in RTL paragraphs,
 * and use three classes — text, punctuation (a class of its own),
 * separators — with no Arabic-specific rule. Ctrl+Right skips a run of
 * one class, then the separators after it; Ctrl+Left skips separators,
 * then a run of one class. */
test('row 20: Ctrl+Left/Right stop at text / punctuation / separator classes, Arabic tatweel included', async ({
    page,
}) => {
    const walk = async (key: string, presses: number): Promise<number[]> => {
        const out: number[] = [];
        for (let i = 0; i < presses; i++) {
            await page.keyboard.press(key);
            out.push((await caretOffset(page)).offset);
        }
        return out;
    };

    /* `word, next`: the comma is a word stop of its own. */
    await openText(page, 'word, next');
    await caretAt(page, 0, 0);
    expect(await walk('Control+ArrowRight', 3)).toEqual([4, 6, 10]);
    expect(await walk('Control+ArrowLeft', 3)).toEqual([6, 4, 0]);

    /* Arabic words with a tatweel (and a shadda) inside an LTR paragraph:
       the tatweel is part of its word; the Arabic comma is punctuation. */
    const W1 = 'كتـــاب';
    const W2 = 'جدّيد';
    const LTR = `go ${W1}، ${W2} end`;
    await openText(page, LTR);
    const b = (s: string) => utf8(s);
    const ltrStops = [
        b('go '),
        b(`go ${W1}`),
        b(`go ${W1}، `),
        b(`go ${W1}، ${W2} `),
        b(LTR),
    ];
    await caretAt(page, 0, 0);
    expect(await walk('Control+ArrowRight', ltrStops.length)).toEqual(ltrStops);
    expect(await walk('Control+ArrowLeft', ltrStops.length)).toEqual([
        ...ltrStops.slice(0, -1).reverse(),
        0,
    ]);

    /* An all-Arabic (RTL) paragraph: the keys flip — Ctrl+Left walks
       logically forward, Ctrl+Right back. */
    const RTL = `${W1} ${W2}`;
    await openText(page, RTL);
    await caretAt(page, 0, 0);
    expect(await walk('Control+ArrowLeft', 2)).toEqual([b(`${W1} `), b(RTL)]);
    expect(await walk('Control+ArrowRight', 2)).toEqual([b(`${W1} `), 0]);
});

/* ------------------------------------------------------------------ */
/* Row 34 — memo §10: Backspace / Delete across a paragraph boundary merge
 * with the caret at the join. */
test('row 34: Backspace / Delete across a paragraph boundary leave the caret at the join (LTR, RTL, list + plain)', async ({
    page,
}) => {
    const join = async (
        first: string,
        second: string,
        key: 'Backspace' | 'Delete',
        list = false,
    ): Promise<void> => {
        await openText(page, `${first}\n${second}`);
        if (list) {
            const t = await dispatch(page, {
                type: 'TOGGLE_LIST',
                range: { start: pos(0, 0), end: pos(0, 0) },
                kind: 'Bullet',
            });
            expect(t.type).not.toBe('ERROR');
        }
        if (key === 'Backspace') await caretAt(page, 1, 0);
        else await caretAt(page, 0, utf8(first));
        await page.keyboard.press(key);
        expect(await caretOffset(page), `${key}: caret at the join`).toEqual({
            block: 0,
            offset: utf8(first),
        });
        await page.keyboard.type('|');
        expect(await paragraphs(page), `${key}: one paragraph, marker at the join`).toEqual([
            `${first}|${second}`,
        ]);
    };
    for (const key of ['Backspace', 'Delete'] as const) {
        await join('abc', 'def', key);
        await join('مرحبا', 'بالعالم', key);
        await join('item', 'plain', key, true);
    }
});

/* ------------------------------------------------------------------ */
/* Row 37 — memo §10 "IME": composition replaces the selection and works
 * inside the run at the caret; an empty commit undoes back to the state
 * before the composition. Driven through `CompositionEvent`s on the hidden
 * textarea (the real `HiddenInput` handlers). The commit and the empty /
 * cancelled commit are pinned; the preview while composing over a selection
 * still shows the selected text — #571. */
async function compose(page: Page, events: Array<[string, string]>): Promise<void> {
    await page.evaluate((evs) => {
        const ta = document.querySelector<HTMLTextAreaElement>('textarea[data-nge-hidden-input]');
        if (!ta) throw new Error('editor hidden input missing');
        for (const [type, data] of evs) {
            ta.dispatchEvent(new CompositionEvent(type, { data, bubbles: true }));
        }
    }, events);
    await settle(page);
}

test('row 37: IME composition replaces the selection inside its run; an empty or cancelled commit restores the text', async ({
    page,
}) => {
    await openText(page, 'Hello world!');
    const selectWorld = async (): Promise<void> => {
        await dispatch(page, {
            type: 'SET_SELECTION',
            range: { start: pos(0, 6), end: pos(0, 11) },
            caret: pos(0, 11),
        });
    };
    /* `world` bold, so "inside its run" is visible in the committed style. */
    await selectWorld();
    await dispatch(page, { type: 'TOGGLE_FORMATTING', attr: 'Bold' });

    /* A commit replaces the selection, in the selection's run style. */
    await selectWorld();
    await compose(page, [
        ['compositionstart', ''],
        ['compositionupdate', 'に'],
        ['compositionupdate', '日本'],
        ['compositionend', '日本'],
    ]);
    expect(await paragraphs(page)).toEqual(['Hello 日本!']);
    const committed = await dispatch(page, {
        type: 'SET_SELECTION',
        range: { start: pos(0, 6), end: pos(0, 6 + utf8('日本')) },
        caret: pos(0, 6 + utf8('日本')),
    });
    expect(committed.attrs_at_caret.bold, 'the composed text continues the run').toBe(true);
    await page.keyboard.press('Control+z');
    expect(await paragraphs(page)).toEqual(['Hello world!']);

    /* An EMPTY commit (the composed text deleted before ending) restores
       the selected text, still selected. */
    await selectWorld();
    await compose(page, [
        ['compositionstart', ''],
        ['compositionupdate', 'に'],
        ['compositionupdate', ''],
        ['compositionend', ''],
    ]);
    expect(await paragraphs(page)).toEqual(['Hello world!']);

    /* A CANCELLED composition (the browser ends with empty data and no
       final update, as Chrome does on Escape) restores it too. */
    await selectWorld();
    await compose(page, [
        ['compositionstart', ''],
        ['compositionupdate', 'に'],
        ['compositionend', ''],
    ]);
    let s = await selection(page);
    expect([s.start.offset, s.end.offset], 'the selection survives the cancel').toEqual([6, 11]);
    expect(await paragraphs(page)).toEqual(['Hello world!']);

    /* A collapsed caret: composition inserts at the caret inside the run. */
    await caretAt(page, 0, 8);
    await compose(page, [
        ['compositionstart', ''],
        ['compositionupdate', 'か'],
        ['compositionend', 'か'],
    ]);
    s = await selection(page);
    expect([s.start.offset, s.end.offset], 'the caret after the composed text').toEqual([
        8 + utf8('か'),
        8 + utf8('か'),
    ]);
    expect(await paragraphs(page)).toEqual(['Hello woかrld!']);
});
