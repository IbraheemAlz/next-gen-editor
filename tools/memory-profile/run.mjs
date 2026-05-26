#!/usr/bin/env node
/**
 * Memory snapshot harness — Phase 5 D5.2.
 *
 * Loads each synthetic perf document into the engine and records the engine's
 * WASM heap and the page's JS heap after a forced GC, against the §5 budgets.
 *
 * Usage:
 *   node run.mjs              — profile every tests/perf/*.docx (report only)
 *   node run.mjs --budgets    — same, but a budget breach exits non-zero
 *
 * Requires the Vite dev server (`pnpm dev`, http://localhost:5173). Override
 * the origin with the URL env var. Uses Playwright with the system Chrome
 * (channel:'chrome'), launched with --enable-precise-memory-info and
 * --js-flags=--expose-gc so performance.memory and window.gc() are available.
 *
 * Phase 5 §5 reads `window.__engineStats()`; that hook does not exist. Engine
 * stats travel over the RPC channel, so the harness dispatches REQUEST_STATS
 * through the existing `window.__dispatch` test hook instead. The perf
 * documents are produced by `cargo run -p perf-fixtures`.
 */
import { readFileSync, existsSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright';

const HERE = dirname(fileURLToPath(import.meta.url));
const PERF_DIR = join(HERE, '..', '..', 'tests', 'perf');
const ORIGIN = process.env.URL ?? 'http://localhost:5173';
const enforce = process.argv.includes('--budgets');

const MIB = 1024 * 1024;

/* Per-document-size budgets — Phase 5 §5. */
const BUDGETS = {
    '50p': { engine: 128 * MIB, jsHeap: 64 * MIB },
    '100p': { engine: 192 * MIB, jsHeap: 96 * MIB },
    '250p': { engine: 384 * MIB, jsHeap: 128 * MIB },
    '500p': { engine: 640 * MIB, jsHeap: 192 * MIB },
};

const fmtMiB = (b) => `${(b / MIB).toFixed(1)} MiB`;

/** Load one perf document and snapshot engine + JS heap after a forced GC. */
async function profile(browser, label) {
    const docxPath = join(PERF_DIR, `${label}.docx`);
    if (!existsSync(docxPath)) {
        console.warn(`[memory-profile] ${label}: ${docxPath} missing — skipped`);
        return null;
    }

    const t0 = Date.now();
    console.log(`[memory-profile] ${label}: starting...`);
    const ctx = await browser.newContext();
    try {
        const page = await ctx.newPage();
        page.on('pageerror', e => console.log(`[memory-profile] ${label} pageerror: ${e.message}`));
        await page.goto(ORIGIN, { waitUntil: 'load', timeout: 60000 });
        await page.waitForFunction(() => window.__paintIdle === true, { timeout: 120000 });
        console.log(`[memory-profile] ${label}: page loaded ${Date.now() - t0}ms`);

        const docxBytes = Array.from(readFileSync(docxPath));
        console.log(`[memory-profile] ${label}: dispatching LOAD_DOCX (${docxBytes.length}B)`);
        const loadStart = Date.now();
        const loaded = await Promise.race([
            page.evaluate(async (bytes) => {
                const evt = await window.__dispatch({
                    type: 'LOAD_DOCX',
                    bytes: new Uint8Array(bytes),
                });
                return evt.type === 'ERROR' ? evt.message : evt.type;
            }, docxBytes),
            new Promise(r => setTimeout(() => r('TIMEOUT_300S'), 300000)),
        ]);
        console.log(`[memory-profile] ${label}: LOAD_DOCX -> ${loaded} in ${Date.now() - loadStart}ms`);
        if (loaded !== 'DOCUMENT_LOADED') {
            console.error(`[memory-profile] ${label}: LOAD_DOCX failed — ${loaded}`);
            return { label, ok: false };
        }

        /* Brief settle window, then GC. LOAD_DOCX already includes the
           full layout + auto-repaint synchronously, so the worker is
           free as soon as DOCUMENT_LOADED returned. */
        await page.waitForTimeout(500);
        await page.evaluate(() => window.gc?.());
        await page.waitForTimeout(100);

        console.log(`[memory-profile] ${label}: requesting stats`);
        const statsStart = Date.now();
        const stats = await Promise.race([
            page.evaluate(async () => {
                const evt = await window.__dispatch({ type: 'REQUEST_STATS' });
                return evt.type === 'STATS' ? evt : null;
            }),
            new Promise(r => setTimeout(() => r(null), 1800000)),
        ]);
        console.log(`[memory-profile] ${label}: stats returned in ${Date.now() - statsStart}ms`);
        const jsHeap = await page.evaluate(() => performance.memory?.usedJSHeapSize ?? 0);

        if (!stats) {
            console.error(`[memory-profile] ${label}: REQUEST_STATS did not return — worker busy past timeout`);
            return { label, ok: false };
        }
        const engine = stats.wasm_heap_bytes ?? 0;
        const budget = BUDGETS[label];
        const ok = engine < budget.engine && jsHeap < budget.jsHeap;
        console.log(
            `[memory-profile] ${label}: engine ${fmtMiB(engine)} / ${fmtMiB(budget.engine)} · ` +
                `jsHeap ${fmtMiB(jsHeap)} / ${fmtMiB(budget.jsHeap)} · ` +
                `tree ${fmtMiB(stats?.document_tree_bytes ?? 0)} · ` +
                `${ok ? 'OK' : 'OVER BUDGET'}`,
        );
        return { label, ok, engine, jsHeap };
    } finally {
        await ctx.close();
    }
}

const labels = Object.keys(BUDGETS);
console.log(
    `[memory-profile] origin=${ORIGIN} docs=${labels.join(', ')} ` +
        `mode=${enforce ? 'enforce' : 'report'}`,
);

const results = [];
for (const label of labels) {
    const browser = await chromium.launch({
        headless: true,
        channel: 'chrome',
        args: ['--enable-precise-memory-info', '--js-flags=--expose-gc'],
    });
    try {
        results.push(await profile(browser, label));
    } catch (e) {
        console.error(`[memory-profile] ${label}: ERROR ${e.message}`);
        results.push({ label, ok: false, error: e.message });
    } finally {
        await browser.close();
    }
}

const profiled = results.filter((r) => r !== null);
if (!profiled.length) {
    console.warn('[memory-profile] no perf documents — run `cargo run -p perf-fixtures`');
    process.exit(0);
}

const breached = profiled.filter((r) => !r.ok);
if (breached.length) {
    console.error(
        `[memory-profile] ${breached.length} document(s) over budget: ` +
            breached.map((r) => r.label).join(', '),
    );
    process.exit(enforce ? 1 : 0);
}
console.log(`[memory-profile] PASS — ${profiled.length} document(s) within budget`);
process.exit(0);
