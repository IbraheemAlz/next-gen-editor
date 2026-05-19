#!/usr/bin/env node
/**
 * Visual-diff harness for the engine's canvas output.
 *
 * Usage:
 *   node run.mjs                 — diff against golden, fail if >2 %
 *   UPDATE=1 node run.mjs        — regenerate the golden
 *   node run.mjs <name> <tol>    — override case name and tolerance
 *
 * Assumes the vite dev server is running on http://localhost:5173/.
 * Uses playwright with the system Chrome (channel:'chrome') so no extra
 * browser download is needed. Waits for `window.__paintIdle === true`
 * before capturing.
 */
import { readFileSync, writeFileSync, existsSync, mkdirSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { PNG } from 'pngjs';
import pixelmatch from 'pixelmatch';
import { chromium } from 'playwright';

const HERE = dirname(fileURLToPath(import.meta.url));
const GOLDEN_DIR = join(HERE, 'golden');
const TMP_DIR = '/tmp/visual-diff';
mkdirSync(GOLDEN_DIR, { recursive: true });
mkdirSync(TMP_DIR, { recursive: true });

const NAME = process.argv[2] ?? 'glyph-a';
const TOL = parseFloat(process.argv[3] ?? '0.02');
const URL = process.env.URL ?? 'http://localhost:5173/?test=1';

const actualPath = join(TMP_DIR, `${NAME}.actual.png`);
const goldenPath = join(GOLDEN_DIR, `${NAME}.png`);
const diffPath = join(TMP_DIR, `${NAME}.diff.png`);

console.log(`[visual-diff] case=${NAME} tol=${(TOL * 100).toFixed(2)}% url=${URL}`);

/* Per-case viewport. A4 case is 595x842 pt so the engine's A4Page::a4()
   matches the drawing surface 1:1. Override via VIEWPORT=WxH if needed. */
const VIEWPORTS = {
    'a4-justified-mixed': { width: 595, height: 842 },
};
const viewport = process.env.VIEWPORT
    ? Object.fromEntries(
        ['width', 'height'].map((k, i) => [k, parseInt(process.env.VIEWPORT.split('x')[i], 10)]),
    )
    : (VIEWPORTS[NAME] ?? { width: 400, height: 400 });

const browser = await chromium.launch({ headless: true, channel: 'chrome' });
try {
    const ctx = await browser.newContext({ viewport });
    const page = await ctx.newPage();

    const errors = [];
    page.on('pageerror', (e) => errors.push(`pageerror: ${e.message}`));
    page.on('console', (m) => {
        if (m.type() !== 'error') return;
        const t = m.text();
        // Ignore browser auto-fetches that aren't part of the page contract
        // (e.g. favicon.ico when we haven't shipped one).
        if (t.startsWith('Failed to load resource')) return;
        errors.push(`console.error: ${t}`);
    });

    await page.goto(URL, { waitUntil: 'load' });

    // Engine logs "[PoC] worker idle" via console + sets window.__paintIdle = true.
    await page.waitForFunction(() => window.__paintIdle === true, { timeout: 15000 });

    if (errors.length) {
        console.error('[visual-diff] page errors:');
        for (const e of errors) console.error('  ' + e);
        process.exit(1);
    }

    await page.screenshot({
        path: actualPath,
        clip: { x: 0, y: 0, width: viewport.width, height: viewport.height },
    });
} finally {
    await browser.close();
}

const update = process.env.UPDATE === '1';
if (!existsSync(goldenPath) || update) {
    writeFileSync(goldenPath, readFileSync(actualPath));
    console.log(`[visual-diff] golden ${update ? 'updated' : 'created'} -> ${goldenPath}`);
    console.log(`[visual-diff] PASS (golden ${update ? 'refresh' : 'init'})`);
    process.exit(0);
}

const actual = PNG.sync.read(readFileSync(actualPath));
const golden = PNG.sync.read(readFileSync(goldenPath));

if (actual.width !== golden.width || actual.height !== golden.height) {
    console.error(
        `[visual-diff] FAIL: size mismatch — actual ${actual.width}x${actual.height} ` +
        `vs golden ${golden.width}x${golden.height}`,
    );
    process.exit(1);
}

const diff = new PNG({ width: golden.width, height: golden.height });
const numDiffPx = pixelmatch(
    golden.data, actual.data, diff.data,
    golden.width, golden.height,
    { threshold: 0.1 },
);
const pct = numDiffPx / (golden.width * golden.height);
writeFileSync(diffPath, PNG.sync.write(diff));

console.log(
    `[visual-diff] diff: ${numDiffPx} pixels (${(pct * 100).toFixed(3)}%) ` +
    `vs tolerance ${(TOL * 100).toFixed(2)}%`,
);
console.log(`[visual-diff] diff image: ${diffPath}`);

if (pct > TOL) {
    console.error(`[visual-diff] FAIL: exceeds tolerance`);
    process.exit(1);
}
console.log(`[visual-diff] PASS`);
process.exit(0);
