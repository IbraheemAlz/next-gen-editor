#!/usr/bin/env node
/**
 * Interactive-typing smoke test.
 *
 * Loads the default route (no ?test= → interactive A4 editor), types Arabic
 * via real keyboard events, then undoes into the visible region and asserts
 * the canvas actually changed. Also exercises redo.
 *
 * Usage: node typing-test.mjs   (vite must be running on :5173)
 */
import { chromium } from 'playwright';
import { mkdirSync, readFileSync } from 'node:fs';
import { PNG } from 'pngjs';
import pixelmatch from 'pixelmatch';

const OUT = '/tmp/visual-diff';
mkdirSync(OUT, { recursive: true });

function pixelDelta(aPath, bPath) {
    const a = PNG.sync.read(readFileSync(aPath));
    const b = PNG.sync.read(readFileSync(bPath));
    const diff = new PNG({ width: a.width, height: a.height });
    return pixelmatch(a.data, b.data, diff.data, a.width, a.height, { threshold: 0.1 });
}

const browser = await chromium.launch({ headless: true, channel: 'chrome' });
let failed = false;
try {
    const ctx = await browser.newContext({ viewport: { width: 595, height: 842 } });
    const page = await ctx.newPage();
    const errors = [];
    page.on('pageerror', (e) => errors.push(`pageerror: ${e.message}`));
    page.on('console', (m) => {
        if (m.type() === 'error' && !m.text().startsWith('Failed to load resource')) {
            errors.push(`console.error: ${m.text()}`);
        }
    });

    await page.goto('http://localhost:5173/', { waitUntil: 'load' });
    await page.waitForFunction(() => window.__paintIdle === true, { timeout: 15000 });
    console.log('[typing-test] editor booted, blank A4 ready');
    await page.screenshot({ path: `${OUT}/typing-0-blank.png` });

    /* Focus the page like a user would. */
    await page.locator('#doc').click({ position: { x: 300, y: 120 } });

    /* Type pure Arabic so every glyph is visible (NotoNaskhArabic has no
       Latin — mixed text would leave .notdef gaps). */
    const sample = 'السلام عليكم ورحمة';
    await page.keyboard.type(sample, { delay: 20 });
    await page.waitForTimeout(400);
    await page.screenshot({ path: `${OUT}/typing-1-typed.png` });
    console.log(`[typing-test] typed "${sample}"`);

    const typedDelta = pixelDelta(`${OUT}/typing-0-blank.png`, `${OUT}/typing-1-typed.png`);
    console.log(`[typing-test] blank→typed pixel delta: ${typedDelta}`);
    if (typedDelta < 100) {
        console.error('[typing-test] FAIL — typing did not change the canvas');
        failed = true;
    }

    /* Undo 4× — removes "ورحم" (4 visible Arabic chars). Canvas MUST change. */
    for (let i = 0; i < 4; i++) {
        await page.keyboard.press('Control+z');
        await page.waitForTimeout(120);
    }
    await page.screenshot({ path: `${OUT}/typing-2-undone.png` });
    const undoDelta = pixelDelta(`${OUT}/typing-1-typed.png`, `${OUT}/typing-2-undone.png`);
    console.log(`[typing-test] typed→undo×4 pixel delta: ${undoDelta}`);
    if (undoDelta < 100) {
        console.error('[typing-test] FAIL — undo did not change the canvas');
        failed = true;
    }

    /* Redo 4× — restores the text. Canvas must match the typed screenshot. */
    for (let i = 0; i < 4; i++) {
        await page.keyboard.press('Control+y');
        await page.waitForTimeout(120);
    }
    await page.screenshot({ path: `${OUT}/typing-3-redone.png` });
    const redoDelta = pixelDelta(`${OUT}/typing-1-typed.png`, `${OUT}/typing-3-redone.png`);
    console.log(`[typing-test] redo×4 vs original-typed pixel delta: ${redoDelta}`);
    if (redoDelta > 50) {
        console.error('[typing-test] FAIL — redo did not restore the typed state');
        failed = true;
    }

    if (errors.length) {
        console.error('[typing-test] page errors:');
        for (const e of errors) console.error('  ' + e);
        failed = true;
    }

    console.log(failed ? '[typing-test] FAIL' : '[typing-test] PASS');
} finally {
    await browser.close();
}
process.exit(failed ? 1 : 0);
