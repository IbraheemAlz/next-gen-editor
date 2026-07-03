#!/usr/bin/env node
/**
 * Memory snapshot harness — Phase 5 D5.2 (reworked for issue #33).
 *
 * Loads perf documents into the engine and records the engine's WASM
 * heap and the page's JS heap after a forced GC, against the §5 budgets.
 *
 * Usage:
 *   node run.mjs                 — profile every tests/perf/*.docx (report only)
 *   node run.mjs 50p             — profile just one document (any of: 50p 100p 250p 500p)
 *   node run.mjs 50p 250p        — profile a subset
 *   node run.mjs --budgets       — all docs, budget breach exits non-zero (CI contract)
 *   node run.mjs --lazy 50p      — measure the LAZY cold-open footprint instead
 *
 * WHAT IS MEASURED (issue #33): documents load through the interactive
 * OPEN_DOCUMENT path (viewport-culled lazy pagination — the code path
 * the product actually ships), then a deliberate EXPAND_LAYOUT forces
 * the full document layout so the recorded heap keeps the §5 budget
 * semantics ("whole document resident"). `--lazy` skips the expand and
 * measures the true cold-open footprint; the output labels which mode
 * produced each number.
 *
 * Requires the Vite dev server (`pnpm dev`, http://localhost:5173).
 * Override the origin with the URL env var. Uses Playwright with the
 * system Chrome (channel:'chrome'), launched with
 * --enable-precise-memory-info and --js-flags=--expose-gc.
 *
 * Ctrl+C / SIGTERM closes the active browser before exiting — an
 * interrupted run must not leave node + Chrome orphans squatting on the
 * machine (the 2026-07 audit lost 2.5 h of RAM to exactly that).
 */
import { readFileSync, existsSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright';

const HERE = dirname(fileURLToPath(import.meta.url));
const PERF_DIR = join(HERE, '..', '..', 'tests', 'perf');
const ORIGIN = process.env.URL ?? 'http://localhost:5173';

const MIB = 1024 * 1024;

/* Per-document-size budgets — Phase 5 §5. */
const BUDGETS = {
    '50p': { engine: 128 * MIB, jsHeap: 64 * MIB },
    '100p': { engine: 192 * MIB, jsHeap: 96 * MIB },
    '250p': { engine: 384 * MIB, jsHeap: 128 * MIB },
    '500p': { engine: 640 * MIB, jsHeap: 192 * MIB },
};

/* Issue #33 — the old hardcoded 1_800_000 ms stats race stalled an
   interrupted run for 30 minutes with zero output. */
const STATS_TIMEOUT_MS = 120_000;
const HEARTBEAT_MS = 10_000;

const argv = process.argv.slice(2);
const enforce = argv.includes('--budgets');
const lazyMode = argv.includes('--lazy');
const requested = argv.filter((a) => !a.startsWith('--'));
for (const label of requested) {
    if (!(label in BUDGETS)) {
        console.error(
            `[memory-profile] unknown doc label '${label}' (expected: ${Object.keys(BUDGETS).join(' | ')})`,
        );
        process.exit(2);
    }
}
const labels = requested.length ? requested : Object.keys(BUDGETS);

const fmtMiB = (b) => `${(b / MIB).toFixed(1)} MiB`;

/* Issue #33 — SIGINT/SIGTERM must tear the browser down. */
let activeBrowser = null;
let shuttingDown = false;
async function shutdown(signal, code) {
    if (shuttingDown) return;
    shuttingDown = true;
    console.error(`[memory-profile] ${signal} — closing browser and exiting`);
    try {
        await activeBrowser?.close();
    } catch {
        /* already gone */
    }
    process.exit(code);
}
process.on('SIGINT', () => void shutdown('SIGINT', 130));
process.on('SIGTERM', () => void shutdown('SIGTERM', 143));

/** Race `promise` against a timeout, with a progress heartbeat. Both
 *  timers are cleared when the race settles so a finished run exits
 *  promptly instead of idling on live timers. */
async function withHeartbeat(promise, timeoutMs, onTick, timeoutValue) {
    let timeoutHandle;
    let heartbeatHandle;
    const started = Date.now();
    try {
        return await Promise.race([
            promise,
            new Promise((resolve) => {
                heartbeatHandle = setInterval(() => onTick(Date.now() - started), HEARTBEAT_MS);
                timeoutHandle = setTimeout(() => resolve(timeoutValue), timeoutMs);
            }),
        ]);
    } finally {
        clearTimeout(timeoutHandle);
        clearInterval(heartbeatHandle);
    }
}

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
        page.on('pageerror', (e) =>
            console.log(`[memory-profile] ${label} pageerror: ${e.message}`),
        );
        await page.goto(ORIGIN, { waitUntil: 'load', timeout: 60000 });
        await page.waitForFunction(() => window.__paintIdle === true, { timeout: 120000 });
        console.log(`[memory-profile] ${label}: page loaded ${Date.now() - t0}ms`);

        const docxBytes = Array.from(readFileSync(docxPath));
        console.log(
            `[memory-profile] ${label}: dispatching OPEN_DOCUMENT (lazy) (${docxBytes.length}B)`,
        );
        const loadStart = Date.now();
        const loaded = await withHeartbeat(
            page.evaluate(async (bytes) => {
                const evt = await window.__dispatch({
                    type: 'OPEN_DOCUMENT',
                    bytes: new Uint8Array(bytes),
                    format: 'docx',
                    name: undefined,
                });
                return evt.type === 'ERROR' ? evt.message : evt.type;
            }, docxBytes),
            300_000,
            (ms) =>
                console.log(
                    `[memory-profile] ${label}: waiting for OPEN_DOCUMENT... ${Math.round(ms / 1000)}s`,
                ),
            'TIMEOUT_300S',
        );
        console.log(
            `[memory-profile] ${label}: OPEN_DOCUMENT (lazy) -> ${loaded} in ${Date.now() - loadStart}ms`,
        );
        if (loaded !== 'DOCUMENT_LOADED') {
            console.error(`[memory-profile] ${label}: OPEN_DOCUMENT failed — ${loaded}`);
            return { label, ok: false };
        }

        /* Issue #33 — the §5 budgets mean "whole document resident", so
           force the full layout DELIBERATELY (and say so); `--lazy`
           keeps the product's cold-open footprint instead. One retry:
           a first expand can report a still-partial layout when the
           runway estimate undershoots the real document height. */
        if (!lazyMode) {
            const expandStart = Date.now();
            let full = false;
            for (let attempt = 1; attempt <= 2 && !full; attempt++) {
                full = await withHeartbeat(
                    page.evaluate(async () => {
                        const evt = await window.__dispatch({
                            type: 'EXPAND_LAYOUT',
                            target_y: 1e9,
                        });
                        return evt.type === 'PAINTED' ? evt.is_full_layout === true : false;
                    }),
                    300_000,
                    (ms) =>
                        console.log(
                            `[memory-profile] ${label}: waiting for EXPAND_LAYOUT... ${Math.round(ms / 1000)}s`,
                        ),
                    false,
                );
            }
            console.log(
                `[memory-profile] ${label}: EXPAND_LAYOUT forced full layout ` +
                    `(${full ? 'complete' : 'INCOMPLETE'}) in ${Date.now() - expandStart}ms`,
            );
        }

        /* Brief settle window, then GC. */
        await page.waitForTimeout(500);
        await page.evaluate(() => window.gc?.());
        await page.waitForTimeout(100);

        console.log(`[memory-profile] ${label}: requesting stats`);
        const statsStart = Date.now();
        const stats = await withHeartbeat(
            page.evaluate(async () => {
                const evt = await window.__dispatch({ type: 'REQUEST_STATS' });
                return evt.type === 'STATS' ? evt : null;
            }),
            STATS_TIMEOUT_MS,
            (ms) =>
                console.log(
                    `[memory-profile] ${label}: waiting for REQUEST_STATS... ${Math.round(ms / 1000)}s`,
                ),
            null,
        );
        console.log(`[memory-profile] ${label}: stats returned in ${Date.now() - statsStart}ms`);
        const jsHeap = await page.evaluate(() => performance.memory?.usedJSHeapSize ?? 0);

        if (!stats) {
            console.error(
                `[memory-profile] ${label}: REQUEST_STATS did not return within ` +
                    `${STATS_TIMEOUT_MS / 1000}s — worker busy past timeout`,
            );
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

console.log(
    `[memory-profile] origin=${ORIGIN} docs=${labels.join(', ')} ` +
        `mode=${enforce ? 'enforce' : 'report'} layout=${lazyMode ? 'lazy cold-open' : 'forced full'}`,
);

const results = [];
for (const label of labels) {
    if (shuttingDown) break;
    /* Fresh browser per document — a clean heap baseline. */
    const browser = await chromium.launch({
        headless: true,
        channel: 'chrome',
        args: ['--enable-precise-memory-info', '--js-flags=--expose-gc'],
    });
    activeBrowser = browser;
    try {
        results.push(await profile(browser, label));
    } catch (e) {
        console.error(`[memory-profile] ${label}: ERROR ${e.message}`);
        results.push({ label, ok: false, error: e.message });
    } finally {
        activeBrowser = null;
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
