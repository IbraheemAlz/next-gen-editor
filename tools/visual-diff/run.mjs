#!/usr/bin/env node
/**
 * Visual-diff harness for the engine's canvas output — Phase 5 D5.1 farm.
 *
 * Modes:
 *   node run.mjs                — FARM: every golden case at Tier-A
 *   node run.mjs --tier B       — FARM at the given tier (A | B | C)
 *   node run.mjs <case> [tol]   — single case (back-compat)
 *   UPDATE=1 node run.mjs ...   — regenerate goldens instead of diffing
 *
 * Assumes the Vite dev server is running on http://localhost:5173/. Override
 * the origin with the URL env var. Uses Playwright with the system Chrome
 * (channel:'chrome') — no extra browser download. Waits for
 * `window.__paintIdle === true` before capturing.
 *
 * Phase 5 §3 specifies a 200-document tier corpus driven by a `__loadFixture`
 * hook; neither the corpus nor the hook exists yet (corpus population is D5.1
 * follow-up work). This farm runs the committed Phase-3 goldens in
 * tools/visual-diff/golden/ — the real, reproducible suite — at the §3 tier
 * tolerances.
 */
import { readFileSync, writeFileSync, existsSync, mkdirSync, readdirSync } from 'node:fs';
import { dirname, join, basename } from 'node:path';
import { fileURLToPath } from 'node:url';
import { PNG } from 'pngjs';
import pixelmatch from 'pixelmatch';
import { chromium } from 'playwright';

const HERE = dirname(fileURLToPath(import.meta.url));
/* PR #19 cherry-pick — dual-tier goldens. Canvas2D goldens live at
   `golden/`; Vello goldens at `golden/vello/`. CLI `--renderer vello`
   switches both the golden dir and the harness `?renderer=vello`
   query param so the worker boots Vello. */
const ROOT_GOLDEN_DIR = join(HERE, 'golden');
const VELLO_GOLDEN_DIR = join(ROOT_GOLDEN_DIR, 'vello');
const TMP_DIR = '/tmp/visual-diff';
mkdirSync(ROOT_GOLDEN_DIR, { recursive: true });
mkdirSync(TMP_DIR, { recursive: true });

/* Per-tier pass criteria (Phase 5 §3). `tol` is the maximum diff fraction a
   case may show; `threshold` is the share of cases that must pass. The
   per-pixel colour sensitivity is fixed — it is not the diff cutoff. */
const TIERS = {
    A: { tol: 0.005, threshold: 1.0 },
    B: { tol: 0.02, threshold: 0.8 },
    C: { tol: 0.05, threshold: 0.6 },
};
const PIXELMATCH_THRESHOLD = 0.1;

const ORIGIN = process.env.URL ?? 'http://localhost:5173';
const update = process.env.UPDATE === '1';

/* Per-case viewport. A4 cases render a full 595x842 pt page 1:1; single-glyph
   and single-word cases stay 400x400. VIEWPORT=WxH overrides. */
const VIEWPORTS = {
    'a4-justified-mixed': { width: 595, height: 842 },
    'rich-text': { width: 595, height: 842 },
    'editing-arabic': { width: 595, height: 842 },
    'docx-round-trip': { width: 595, height: 842 },
    'tab-stops-center-kind-ltr': { width: 595, height: 842 },
    'tab-stops-right-kind-ltr': { width: 595, height: 842 },
    'tab-stops-decimal-kind-ltr': { width: 595, height: 842 },
    'autofit-long-url-overflow': { width: 595, height: 842 },
};
function viewportFor(name) {
    if (process.env.VIEWPORT) {
        const [w, h] = process.env.VIEWPORT.split('x').map((n) => parseInt(n, 10));
        return { width: w, height: h };
    }
    return VIEWPORTS[name] ?? { width: 400, height: 400 };
}

/** Screenshot one `?test=<name>` case; returns the PNG buffer. Throws on a
 *  page error. `renderer === 'vello'` adds `&renderer=vello` so the harness
 *  boots Vello against the parallel `golden/vello/` corpus. */
async function capture(browser, name, renderer) {
    const viewport = viewportFor(name);
    const ctx = await browser.newContext({ viewport });
    try {
        const page = await ctx.newPage();
        const errors = [];
        page.on('pageerror', (e) => errors.push(`pageerror: ${e.message}`));
        page.on('console', (m) => {
            /* Browser auto-fetches (favicon.ico) are not part of the contract. */
            if (m.type() === 'error' && !m.text().startsWith('Failed to load resource')) {
                errors.push(`console.error: ${m.text()}`);
            }
        });
        const url =
            renderer === 'vello'
                ? `${ORIGIN}/?test=${name}&renderer=vello`
                : `${ORIGIN}/?test=${name}`;
        await page.goto(url, { waitUntil: 'load' });
        await page.waitForFunction(() => window.__paintIdle === true, { timeout: 15000 });
        if (errors.length) throw new Error(`page errors:\n  ${errors.join('\n  ')}`);
        return await page.screenshot({
            clip: { x: 0, y: 0, width: viewport.width, height: viewport.height },
        });
    } finally {
        await ctx.close();
    }
}

/** Diff an actual capture against the committed golden in `goldenDir`. */
function diffGolden(name, actualBuf, tol, goldenDir) {
    const goldenPath = join(goldenDir, `${name}.png`);
    if (!existsSync(goldenPath)) return { status: 'no-golden', pct: 1 };
    const golden = PNG.sync.read(readFileSync(goldenPath));
    const actual = PNG.sync.read(actualBuf);
    if (golden.width !== actual.width || golden.height !== actual.height) {
        return { status: 'size-mismatch', pct: 1 };
    }
    const out = new PNG({ width: golden.width, height: golden.height });
    const diffPx = pixelmatch(golden.data, actual.data, out.data, golden.width, golden.height, {
        threshold: PIXELMATCH_THRESHOLD,
    });
    const pct = diffPx / (golden.width * golden.height);
    if (pct > tol) writeFileSync(join(TMP_DIR, `${name}.diff.png`), PNG.sync.write(out));
    return { status: pct <= tol ? 'pass' : 'fail', pct };
}

/* ---- mode dispatch --------------------------------------------------- */

const args = process.argv.slice(2);
const tierIdx = args.indexOf('--tier');
const rendererIdx = args.indexOf('--renderer');
/* `--tier` + `--renderer` are flag pairs; a positional case is anything
   not preceded by either flag. */
const flagValuePositions = new Set([
    tierIdx >= 0 ? tierIdx + 1 : -1,
    rendererIdx >= 0 ? rendererIdx + 1 : -1,
]);
const firstPositional = args.find(
    (a, i) => !a.startsWith('--') && !flagValuePositions.has(i),
);
const singleCase = tierIdx < 0 && firstPositional !== undefined;

const tier = (tierIdx >= 0 ? args[tierIdx + 1] : 'A')?.toUpperCase() ?? 'A';
const renderer =
    rendererIdx >= 0 ? args[rendererIdx + 1]?.toLowerCase() : 'canvas2d';
if (renderer !== 'canvas2d' && renderer !== 'vello') {
    console.error(
        `[visual-diff] unknown renderer '${renderer}' (expected canvas2d or vello)`,
    );
    process.exit(2);
}
const GOLDEN_DIR = renderer === 'vello' ? VELLO_GOLDEN_DIR : ROOT_GOLDEN_DIR;
mkdirSync(GOLDEN_DIR, { recursive: true });
if (!singleCase && !TIERS[tier]) {
    console.error(`[visual-diff] unknown tier '${tier}' (expected A, B or C)`);
    process.exit(2);
}

const browser = await chromium.launch({ headless: true, channel: 'chrome' });
let exitCode = 0;
try {
    if (singleCase) {
        const name = firstPositional;
        const tol = parseFloat(args[args.indexOf(name) + 1] ?? '0.02');
        console.log(
            `[visual-diff] single case=${name} tol=${(tol * 100).toFixed(2)}% ` +
                `renderer=${renderer}`,
        );
        const buf = await capture(browser, name, renderer);
        if (update || !existsSync(join(GOLDEN_DIR, `${name}.png`))) {
            writeFileSync(join(GOLDEN_DIR, `${name}.png`), buf);
            console.log(`[visual-diff] golden ${update ? 'updated' : 'created'}: ${name}`);
        } else {
            const r = diffGolden(name, buf, tol, GOLDEN_DIR);
            console.log(`[visual-diff] ${name}: ${r.status} (diff ${(r.pct * 100).toFixed(3)}%)`);
            if (r.status !== 'pass') exitCode = 1;
        }
    } else {
        const cfg = TIERS[tier];
        const cases = readdirSync(GOLDEN_DIR, { withFileTypes: true })
            .filter((d) => d.isFile() && d.name.endsWith('.png'))
            .map((d) => basename(d.name, '.png'))
            .sort();
        if (!cases.length) {
            console.warn(`[visual-diff] no goldens in ${GOLDEN_DIR} — nothing to run`);
            process.exit(0);
        }
        console.log(
            `[visual-diff] FARM tier=${tier} renderer=${renderer} ` +
                `tol=${(cfg.tol * 100).toFixed(2)}% ` +
                `cases=${cases.length}${update ? ' (UPDATE)' : ''}`,
        );
        let passes = 0;
        for (const name of cases) {
            try {
                const buf = await capture(browser, name, renderer);
                if (update) {
                    writeFileSync(join(GOLDEN_DIR, `${name}.png`), buf);
                    console.log(`[visual-diff]   ${name}: golden updated`);
                    passes++;
                    continue;
                }
                const r = diffGolden(name, buf, cfg.tol, GOLDEN_DIR);
                if (r.status === 'pass') passes++;
                console.log(`[visual-diff]   ${name}: ${r.status} (diff ${(r.pct * 100).toFixed(3)}%)`);
            } catch (e) {
                console.error(`[visual-diff]   ${name}: ERROR — ${e.message}`);
            }
        }
        const rate = passes / cases.length;
        console.log(
            `[visual-diff] tier-${tier} renderer=${renderer} ` +
                `pass rate ${(rate * 100).toFixed(1)}% ` +
                `(${passes}/${cases.length}, threshold ${(cfg.threshold * 100).toFixed(0)}%)`,
        );
        if (update) {
            console.log('[visual-diff] goldens regenerated — eyeball every change before commit');
        } else if (rate < cfg.threshold) {
            exitCode = 1;
        }
    }
} finally {
    await browser.close();
}
process.exit(exitCode);
