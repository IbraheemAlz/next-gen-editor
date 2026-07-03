import { test, expect } from '@playwright/test';

/* Issue #54 regression guard — the seed document must be VISIBLE after a
 * fresh load with zero interaction, on whichever renderer the probe
 * picked (Canvas2D in CI-class environments, Vello on WebGPU hardware).
 *
 * This is a PRESENTATION-level assertion, so it needs a real compositor:
 * the headless full-app screenshot is a documented false negative (see
 * CLAUDE.md "Known issue — headless Chrome does not composite the
 * interactive canvas"). Run it explicitly, headed:
 *
 *     HEADFUL=1 pnpm exec playwright test first-paint --headed
 *
 * It self-skips otherwise so the default (headless) e2e sweep stays
 * green and honest — a headless pass here would prove nothing.
 */

const RELOADS = 10;

test.describe('first paint stays visible (#54)', () => {
    test.skip(
        !process.env.HEADFUL,
        'needs a real compositor — run: HEADFUL=1 pnpm exec playwright test first-paint --headed',
    );

    test(`seed text visible on ${RELOADS}/${RELOADS} fresh loads, no interaction`, async ({
        page,
    }) => {
        test.setTimeout(180_000);
        for (let i = 0; i < RELOADS; i++) {
            await page.goto('/');
            await page.waitForFunction(() => (window as any).__paintIdle === true, undefined, {
                timeout: 20_000,
            });
            /* Draw the (transferred) page-0 canvas into a scratch canvas
               and count OPAQUE non-white pixels — `drawImage` samples the
               same composited frame the user sees. The opacity guard is
               load-bearing: a NEVER-presented canvas reads back as
               transparent black, which a plain "non-white" count would
               happily mistake for ink (verified live on the Vello boot
               path, 2026-07-04). A presented seed frame is an opaque
               white page with ~2000 glyph pixels. */
            const painted = await page.evaluate(() => {
                const el = document.querySelector(
                    '.editor-page[data-page-index="0"] canvas',
                ) as HTMLCanvasElement | null;
                if (!el) return -1;
                const rect = el.getBoundingClientRect();
                const c = document.createElement('canvas');
                c.width = Math.max(1, Math.round(rect.width * devicePixelRatio));
                c.height = Math.max(1, Math.round(rect.height * devicePixelRatio));
                const ctx = c.getContext('2d');
                if (!ctx) return -1;
                ctx.drawImage(el, 0, 0, c.width, c.height);
                const d = ctx.getImageData(0, 0, c.width, c.height).data;
                let opaqueInk = 0;
                for (let p = 0; p < d.length; p += 4) {
                    const opaque = (d[p + 3] ?? 0) > 200;
                    const white =
                        (d[p] ?? 255) >= 250 && (d[p + 1] ?? 255) >= 250 && (d[p + 2] ?? 255) >= 250;
                    if (opaque && !white) opaqueInk++;
                }
                return opaqueInk;
            });
            expect(painted, `reload ${i + 1}/${RELOADS}: opaque ink pixel count`).toBeGreaterThan(
                500,
            );
        }
    });
});
