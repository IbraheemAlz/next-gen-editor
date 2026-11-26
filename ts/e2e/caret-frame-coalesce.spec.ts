import { test, expect, type Page } from '@playwright/test';
import { boot, burst, settle } from './helpers/editor';

/* Issue #559 (cursor matrix #555 row 38, clean-room memo §11 — "caret
 * updates are coalesced to the next paint; vertical moves force an
 * immediate update").
 *
 * During a key repeat several `SELECTION_CHANGED` replies land inside one
 * frame; the overlay used to restyle the `.caret` element once per reply.
 * It must write the newest rect ONCE per animation frame, except for the
 * reply to a vertical move (Up / Down / PageUp / PageDown), which is applied
 * in the very task that delivers it.
 *
 * The probe lives in the page: a rAF counter numbers the frames, a
 * `MutationObserver` on the `.caret` element counts its `style` writes per
 * frame, and a `client.subscribe` listener (registered after the store's, so
 * it runs after the overlay's signal has been set) records the overlay's
 * `left` / `top` AT the moment each reply is handled. The keys are fired
 * through `burst` (one synchronous task, through the shell's real key
 * handler) so no engine reply can interleave. */

const DOC = [
    'alpha beta gamma delta epsilon zeta eta theta iota kappa lambda',
    'second line of the document',
    'third line of the document',
].join('\n');

async function open(page: Page): Promise<void> {
    const type = await page.evaluate(async (text) => {
        const evt = await (window as any).__dispatch({
            type: 'OPEN_DOCUMENT',
            bytes: new TextEncoder().encode(text),
            format: 'plain_text',
            name: 'caret-frame.txt',
        });
        return evt.type as string;
    }, DOC);
    expect(type).toBe('DOCUMENT_LOADED');
    await settle(page);
}

/** SETUP: park the caret at the start of paragraph 0. */
async function caretToStart(page: Page): Promise<void> {
    await page.evaluate(() => {
        const p = { path: { steps: [{ kind: 'BLOCK', idx: 0 }] }, offset: 0 };
        return (window as any).__dispatch({
            type: 'SET_SELECTION',
            range: { start: p, end: p },
            caret: p,
        });
    });
    await settle(page);
    await expect(page.locator('.caret')).toHaveCount(1);
}

interface ProbeRead {
    /** `style` writes of the `.caret` element, by frame. */
    writes: Record<number, number>;
    /** `SELECTION_CHANGED` replies handled, by frame. */
    replies: Record<number, number>;
    /** The overlay's `left` / `top` (CSS px) as each reply was handled. */
    atReply: { left: number; top: number }[];
    /** The caret x the engine reported in the newest reply (CSS px). */
    engineLeft: number;
}

/** Install (or reset) the in-page probe on the current `.caret` element. */
async function installProbe(page: Page): Promise<void> {
    await page.evaluate(() => {
        const w = window as any;
        w.__caretProbe?.stop();
        const caretEl = document.querySelector<HTMLElement>('.caret')!;
        const probe: any = {
            frame: 0,
            writes: {},
            replies: {},
            atReply: [],
            engineLeft: NaN,
        };
        let raf = 0;
        const tick = (): void => {
            probe.frame += 1;
            raf = requestAnimationFrame(tick);
        };
        raf = requestAnimationFrame(tick);
        const mo = new MutationObserver((records) => {
            for (const r of records) {
                if (r.target === caretEl && r.attributeName === 'style') {
                    probe.writes[probe.frame] = (probe.writes[probe.frame] ?? 0) + 1;
                }
            }
        });
        mo.observe(caretEl, { attributes: true, attributeFilter: ['style'] });
        const off = w.__engineClient.subscribe((ev: any) => {
            if (ev.type !== 'SELECTION_CHANGED') return;
            probe.replies[probe.frame] = (probe.replies[probe.frame] ?? 0) + 1;
            probe.atReply.push({
                left: parseFloat(caretEl.style.left),
                top: parseFloat(caretEl.style.top),
            });
            probe.engineLeft = ev.caret.x / window.devicePixelRatio;
        });
        probe.stop = (): void => {
            cancelAnimationFrame(raf);
            mo.disconnect();
            off();
        };
        w.__caretProbe = probe;
    });
}

async function readProbe(page: Page): Promise<ProbeRead> {
    return page.evaluate(() => {
        const p = (window as any).__caretProbe;
        return {
            writes: { ...p.writes },
            replies: { ...p.replies },
            atReply: [...p.atReply],
            engineLeft: p.engineLeft,
        };
    });
}

/** Two animation frames: any rAF-coalesced write has been applied. */
async function afterFrames(page: Page): Promise<void> {
    await page.evaluate(
        () =>
            new Promise<void>((resolve) =>
                requestAnimationFrame(() => requestAnimationFrame(() => resolve())),
            ),
    );
}

const overlay = (page: Page) =>
    page.evaluate(() => {
        const el = document.querySelector<HTMLElement>('.caret')!;
        return { left: parseFloat(el.style.left), top: parseFloat(el.style.top) };
    });

const max = (o: Record<number, number>): number => Math.max(0, ...Object.values(o));
const sum = (o: Record<number, number>): number => Object.values(o).reduce((a, b) => a + b, 0);

test.beforeEach(async ({ page }) => {
    await boot(page);
    await open(page);
    await caretToStart(page);
});

test('a burst of Right presses restyles the caret once per frame, not once per reply', async ({
    page,
}) => {
    const presses = 24;
    let read: ProbeRead | undefined;
    /* The burst is one task, but the engine answers one reply per task: on a
       loaded machine each reply can land in a frame of its own, in which case
       there is nothing to coalesce - retry until a frame saw two replies. */
    for (let attempt = 0; attempt < 6 && !read; attempt++) {
        await caretToStart(page);
        await installProbe(page);
        await burst(
            page,
            Array.from({ length: presses }, () => ({ key: 'ArrowRight' })),
        );
        await settle(page);
        await afterFrames(page);
        const r = await readProbe(page);
        if (max(r.replies) >= 2) read = r;
    }
    expect(read, 'no frame ever saw two replies: nothing was coalesced').toBeDefined();
    const r = read!;
    expect(sum(r.replies)).toBe(presses);
    /* Never more than one write per frame ... */
    expect(max(r.writes), `writes per frame ${JSON.stringify(r.writes)}`).toBeLessThanOrEqual(1);
    /* ... and strictly fewer writes than replies: some were coalesced. */
    expect(sum(r.writes)).toBeLessThan(sum(r.replies));
    /* The newest caret is the one drawn. */
    const at = await overlay(page);
    expect(Math.abs(at.left - r.engineLeft)).toBeLessThan(0.5);
});

test('a horizontal reply waits for the frame; a vertical reply moves the caret at once', async ({
    page,
}) => {
    await installProbe(page);

    /* Right: at the moment the reply is handled the overlay has not moved
       yet; the next frame moves it. */
    const start = await overlay(page);
    await burst(page, [{ key: 'ArrowRight' }]);
    await expect.poll(async () => (await readProbe(page)).atReply.length).toBe(1);
    const right = (await readProbe(page)).atReply[0]!;
    expect(right.left, 'the horizontal update is not applied inside the reply task').toBe(
        start.left,
    );
    await afterFrames(page);
    const moved = await overlay(page);
    expect(moved.left).toBeGreaterThan(start.left + 1);
    expect(moved.top).toBe(start.top);

    /* Down: the overlay is on the next line in the very task that handles
       the reply — before any frame ran. */
    await burst(page, [{ key: 'ArrowDown' }]);
    await expect.poll(async () => (await readProbe(page)).atReply.length).toBe(2);
    const down = (await readProbe(page)).atReply[1]!;
    expect(down.top, 'a vertical move is applied immediately').toBeGreaterThan(moved.top + 10);
    await afterFrames(page);
    expect((await overlay(page)).top).toBe(down.top);

    /* Up, too. */
    await burst(page, [{ key: 'ArrowUp' }]);
    await expect.poll(async () => (await readProbe(page)).atReply.length).toBe(3);
    const up = (await readProbe(page)).atReply[2]!;
    expect(up.top, 'Up is applied immediately').toBe(start.top);
});

test('a Right press followed by a Down in one burst leaves the caret on the new line', async ({
    page,
}) => {
    /* The Down reply applies at once and supersedes the Right reply that
       still waited for its frame: the frame must not paint the stale one. */
    await installProbe(page);
    const start = await overlay(page);
    await burst(page, [{ key: 'ArrowRight' }, { key: 'ArrowRight' }, { key: 'ArrowDown' }]);
    await settle(page);
    await afterFrames(page);
    const r = await readProbe(page);
    expect(r.atReply).toHaveLength(3);
    const end = await overlay(page);
    expect(end.top).toBeGreaterThan(start.top + 10);
    expect(Math.abs(end.left - r.engineLeft)).toBeLessThan(0.5);
});
