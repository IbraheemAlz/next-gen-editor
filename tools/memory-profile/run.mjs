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

    const ctx = await browser.newContext();
    try {
        const page = await ctx.newPage();
        await page.goto(ORIGIN, { waitUntil: 'load', timeout: 20000 });
        await page.waitForFunction(() => window.__paintIdle === true, { timeout: 30000 });

        const docxBytes = Array.from(readFileSync(docxPath));
        const loaded = await page.evaluate(async (bytes) => {
            const evt = await window.__dispatch({
                type: 'LOAD_DOCX',
                bytes: new Uint8Array(bytes),
            });
            return evt.type === 'ERROR' ? evt.message : evt.type;
        }, docxBytes);
        if (loaded !== 'DOCUMENT_LOADED') {
            console.error(`[memory-profile] ${label}: LOAD_DOCX failed — ${loaded}`);
            return { label, ok: false };
        }

        /* Let layout + paint settle, then force a full GC so the reading is
           steady state, not transient allocation. */
        await page.waitForTimeout(300);
        await page.evaluate(() => window.gc?.());
        await page.waitForTimeout(100);

        const stats = await page.evaluate(async () => {
            const evt = await window.__dispatch({ type: 'REQUEST_STATS' });
            return evt.type === 'STATS' ? evt : null;
        });
        const jsHeap = await page.evaluate(() => performance.memory?.usedJSHeapSize ?? 0);

        const engine = stats?.wasm_heap_bytes ?? 0;
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

const browser = await chromium.launch({
    headless: true,
    channel: 'chrome',
    args: ['--enable-precise-memory-info', '--js-flags=--expose-gc'],
});
const results = [];
try {
    for (const label of labels) {
        results.push(await profile(browser, label));
    }
} finally {
    await browser.close();
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
