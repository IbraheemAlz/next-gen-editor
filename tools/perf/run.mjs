#!/usr/bin/env node
/**
 * Performance harness — Phase 5 D5.3.
 *
 * Measures the §6 budgets (+ the D2.1 worker-boot gate, issue #425) against the live editor:
 *   - cold start to first paint
 *   - insert char @ caret, p95 over 100 keystrokes (one-page seeded document)
 *   - open a 50-page .docx
 *
 * plus, issue #379, the table-heavy repaint (report only, ungated): open
 * `tests/perf/tables-30p.docx` (22 three-deep nested autofit tables,
 * `tools/perf-fixtures`) and time one-character body edits — each one a
 * repaint that re-lays (or, with the cross-paint table cache, reuses)
 * every table in the viewport band.
 *
 * Usage:
 *   node run.mjs                    — report only, Tier-2 budgets
 *   node run.mjs --hardware tier-1  — Tier-1 (stricter) budgets
 *   node run.mjs --strict           — a budget breach exits non-zero
 *
 * Issue #470 — needs the Vite dev server (the `window.__dispatch` hooks), and
 * finds it the way `ts/playwright.config.ts` does (`tools/lib/port.mjs`): the
 * `URL` env var, else `PW_PORT`, else a port hashed from this checkout's
 * path (so a worktree never measures a sibling's server). When nothing
 * answers there the harness boots `vite` itself, warms it with one
 * throw-away load, and stops it on exit. The measured URL is the first line
 * of the report. Uses Playwright with the system Chrome (channel:'chrome') —
 * no extra browser download.
 *
 * Issue #476 - fixture honesty: after the timed open, each fixture is laid
 * out in full (`EXPAND_LAYOUT`) and its page count is reported; --strict
 * fails when `50p.docx` is more than NOMINAL_PAGE_TOLERANCE away from 50
 * pages. The page count is a calibration against the font stack and line
 * pitch (it drifted to 167 after #329 / #475), so a layout change that moves
 * it must re-calibrate `tools/perf-fixtures` (see its module docs) instead of
 * silently turning "open a 50-page document" into something else.
 *
 * Insert latency is measured on the one-page seeded document — the case the
 * §6 <8/16 ms budget targets. Per-keystroke cost on a multi-page document is
 * bounded by the deferred incremental-relayout work (see CLAUDE.md / BACKLOG):
 * the engine relays out the whole document on every edit.
 */
import { readFileSync, existsSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright';
import { ensureDevServerOrExit } from '../lib/dev-server.mjs';

const HERE = dirname(fileURLToPath(import.meta.url));
const PERF_DOC = join(HERE, '..', '..', 'tests', 'perf', '50p.docx');
/* Issue #379 — the table-heavy repaint fixture. */
const TABLES_DOC = join(HERE, '..', '..', 'tests', 'perf', 'tables-30p.docx');

const argv = process.argv.slice(2);
const argValue = (flag, fallback) => {
    const i = argv.indexOf(flag);
    return i >= 0 && i + 1 < argv.length ? argv[i + 1] : fallback;
};
const hardware = argValue('--hardware', 'tier-2');
const strict = argv.includes('--strict');

/* Performance budgets — Phase 5 §6. */
const BUDGETS = {
    /* `workerBootMs` = D2.1 exit gate (issue #425): the engine worker boots
       cold (spawn + WASM load + engine construction) in < 500 ms. It lives
       here, on a quiet runner, and NOT in the blocking e2e suite
       (`ts/e2e/boot.spec.ts` is only a smoke) - a wall-clock budget there
       fails under machine load with the code unchanged. */
    /* `sabTransferMs` = D2.4 exit gate (issue #452): a 50 MB ArrayBuffer
       round-trips through a worker via Transferable in < 50 ms. Moved out of
       `ts/e2e/sab-transfer.spec.ts` (now a functional smoke) for the same
       reason as `workerBootMs`. */
    'tier-1': { coldStartMs: 3000, workerBootMs: 500, sabTransferMs: 50, insertP95Ms: 8, openDocMs: 1000 },
    'tier-2': { coldStartMs: 8000, workerBootMs: 500, sabTransferMs: 50, insertP95Ms: 16, openDocMs: 2500 },
};
const budget = BUDGETS[hardware];
if (!budget) {
    console.error(`[perf] unknown --hardware '${hardware}' (tier-1 | tier-2)`);
    process.exit(2);
}

const INSERT_KEYSTROKES = 100;
/* Issue #476 - the pages `tests/perf/50p.docx` must lay out to, and how far
   it may drift before the "open a 50-page document" metric stops meaning
   that. */
const NOMINAL_PAGES = 50;
const NOMINAL_PAGE_TOLERANCE = 0.1;
/* Issue #379 — body edits timed on the table-heavy fixture. */
const TABLE_REPAINT_KEYSTROKES = 40;

function percentile(samples, p) {
    const sorted = [...samples].sort((a, b) => a - b);
    const rank = Math.ceil((p / 100) * sorted.length) - 1;
    return sorted[Math.min(sorted.length - 1, Math.max(0, rank))];
}

const devServer = await ensureDevServerOrExit({ label: 'perf' });
const ORIGIN = devServer.origin;
console.log(
    `[perf] origin=${devServer.describe()} hardware=${hardware} mode=${strict ? 'strict' : 'report'}`,
);

const browser = await chromium.launch({ headless: true, channel: 'chrome' });
let coldStartMs = 0;
let workerBootMs = null;
let sab = null;
let samples = [];
let openDocMs = null;
let openDocPages = null;
let tables = null;
try {
    /* A server this run just booted has transformed nothing yet: its first
       page load pays Vite's one-off module transforms (and the dependency
       optimizer's reload), which is the dev server's warm-up, not the
       editor's cold start. One throw-away load pays it, so the measured
       cold start is the same quantity as against a server that was already
       running. */
    if (devServer.booted) {
        const t = Date.now();
        const warm = await (await browser.newContext()).newPage();
        await warm.goto(ORIGIN, { waitUntil: 'load', timeout: 60000 });
        await warm.waitForFunction(() => window.__paintIdle === true, { timeout: 60000 });
        await warm.context().close();
        console.log(`[perf] warm-up load of the freshly booted dev server: ${Date.now() - t} ms (not measured)`);
    }
    const page = await (await browser.newContext()).newPage();

    /* --- Cold start to first paint --- */
    const t0 = Date.now();
    await page.goto(ORIGIN, { waitUntil: 'load', timeout: 30000 });
    await page.waitForFunction(() => window.__paintIdle === true, { timeout: 30000 });
    coldStartMs = Date.now() - t0;
    workerBootMs = await page.evaluate(() =>
        typeof window.__bootMs === 'number' ? window.__bootMs : null,
    );

    /* --- D2.4: 50 MB zero-copy worker round-trip (issue #452) --- */
    sab = await page.evaluate(async () => {
        const SIZE = 50 * 1024 * 1024;
        const src = 'self.onmessage = (e) => { const b = e.data; self.postMessage(b, [b]); };';
        const worker = new Worker(URL.createObjectURL(new Blob([src], { type: 'text/javascript' })));
        try {
            const buf = new ArrayBuffer(SIZE);
            const t0 = performance.now();
            const echoed = await new Promise((resolve) => {
                worker.onmessage = (e) => resolve(e.data);
                worker.postMessage(buf, [buf]);
            });
            return {
                ms: performance.now() - t0,
                ok: buf.byteLength === 0 && echoed.byteLength === SIZE,
            };
        } finally {
            worker.terminate();
        }
    });

    /* --- Insert char @ caret, p95 (one-page seeded document) --- */
    samples = await page.evaluate(async (n) => {
        const timings = [];
        for (let i = 0; i < n; i++) {
            const start = performance.now();
            /* `at` is honoured only when the engine holds no selection; the
               seeded caret means every insert lands at the live caret and
               advances it — a faithful "type one character" keystroke. */
            await window.__dispatch({
                type: 'INSERT_TEXT',
                at: { path: { steps: [{ kind: 'BLOCK', idx: 0 }] }, offset: i },
                text: 'x',
            });
            timings.push(performance.now() - start);
        }
        return timings;
    }, INSERT_KEYSTROKES);

    /* --- Open a 50-page document --- */
    if (existsSync(PERF_DOC)) {
        const docxBytes = Array.from(readFileSync(PERF_DOC));
        ({ openMs: openDocMs, pages: openDocPages } = await page.evaluate(async (bytes) => {
            const start = performance.now();
            const evt = await window.__dispatch({
                type: 'LOAD_DOCX',
                bytes: new Uint8Array(bytes),
            });
            const elapsed = performance.now() - start;
            if (evt.type !== 'DOCUMENT_LOADED') return { openMs: -1, pages: null };
            /* Issue #476 - not timed: the laid-out page count, so the fixture
               cannot drift away from "50 pages" unnoticed. */
            const full = await window.__dispatch({ type: 'EXPAND_LAYOUT', target_y: 1e9 });
            return {
                openMs: elapsed,
                pages: full.type === 'PAINTED' && full.is_full_layout ? full.page_count : null,
            };
        }, docxBytes));
    } else {
        console.warn(`[perf] ${PERF_DOC} missing — skipping open-doc metric`);
    }

    /* --- Issue #379: repaint on a table-heavy document --- */
    if (existsSync(TABLES_DOC)) {
        const docxBytes = Array.from(readFileSync(TABLES_DOC));
        tables = await page.evaluate(
            async ({ bytes, n }) => {
                const start = performance.now();
                const evt = await window.__dispatch({
                    type: 'LOAD_DOCX',
                    bytes: new Uint8Array(bytes),
                });
                const openMs = performance.now() - start;
                if (evt.type !== 'DOCUMENT_LOADED') return { openMs: -1, samples: [], pages: null };
                const full = await window.__dispatch({ type: 'EXPAND_LAYOUT', target_y: 1e9 });
                const pages =
                    full.type === 'PAINTED' && full.is_full_layout ? full.page_count : null;
                /* One-character edits in the intro paragraph, above every
                   table: the engine's reply follows its auto-repaint, so the
                   round trip is the repaint an edit elsewhere triggers. */
                const samples = [];
                for (let i = 0; i < n; i++) {
                    const t = performance.now();
                    await window.__dispatch({
                        type: 'INSERT_TEXT',
                        at: { path: { steps: [{ kind: 'BLOCK', idx: 0 }] }, offset: 0 },
                        text: 'x',
                    });
                    samples.push(performance.now() - t);
                }
                return { openMs, samples, pages };
            },
            { bytes: docxBytes, n: TABLE_REPAINT_KEYSTROKES },
        );
    } else {
        console.warn(`[perf] ${TABLES_DOC} missing — skipping table repaint metric (run tools/perf-fixtures)`);
    }
} finally {
    await browser.close();
    await devServer.stop();
}

const p50 = percentile(samples, 50);
const p95 = percentile(samples, 95);
const maxInsert = Math.max(...samples);

console.log(`[perf] cold start to first paint : ${coldStartMs} ms (budget ${budget.coldStartMs} ms)`);
console.log(
    `[perf] worker boot (__bootMs)    : ` +
        `${workerBootMs === null ? 'NOT REPORTED' : `${workerBootMs.toFixed(1)} ms`} ` +
        `(budget ${budget.workerBootMs} ms)`,
);
console.log(
    `[perf] SAB 50 MB transfer        : ` +
        `${sab === null ? 'NOT REPORTED' : `${sab.ms.toFixed(1)} ms${sab.ok ? '' : ' (NOT ZERO-COPY)'}`} ` +
        `(budget ${budget.sabTransferMs} ms)`,
);
console.log(
    `[perf] insert @ caret x${INSERT_KEYSTROKES}     : ` +
        `p50 ${p50.toFixed(2)} ms · p95 ${p95.toFixed(2)} ms · max ${maxInsert.toFixed(2)} ms ` +
        `(p95 budget ${budget.insertP95Ms} ms)`,
);
if (openDocMs !== null) {
    const shown = openDocMs < 0 ? 'LOAD_DOCX failed' : `${openDocMs.toFixed(0)} ms`;
    console.log(
        `[perf] open 50-page document     : ${shown} (budget ${budget.openDocMs} ms) · ` +
            `lays out to ${openDocPages === null ? 'UNKNOWN' : openDocPages} pages ` +
            `(nominal ${NOMINAL_PAGES} +/- ${NOMINAL_PAGE_TOLERANCE * 100}%)`,
    );
}

if (tables !== null) {
    if (tables.openMs < 0) {
        console.log('[perf] table-heavy document      : LOAD_DOCX failed');
    } else {
        const tp50 = percentile(tables.samples, 50);
        const tp95 = percentile(tables.samples, 95);
        console.log(
            `[perf] table-heavy doc (#379)    : open ${tables.openMs.toFixed(0)} ms · ` +
                `repaint after body edit x${tables.samples.length} p50 ${tp50.toFixed(2)} ms · ` +
                `p95 ${tp95.toFixed(2)} ms (report only) · ` +
                `${tables.pages === null ? 'UNKNOWN' : tables.pages} pages`,
        );
    }
}

/* Gated budgets: cold start, worker boot, SAB transfer, insert p95 and (issue
   #476) open-50p - the achievable §6 numbers a regression must fail on. */
const breaches = [];
if (coldStartMs >= budget.coldStartMs) breaches.push(`cold start ${coldStartMs}ms`);
if (workerBootMs === null) breaches.push('worker boot never reported (__bootMs)');
else if (workerBootMs >= budget.workerBootMs) breaches.push(`worker boot ${workerBootMs.toFixed(1)}ms`);
if (sab === null || !sab.ok) breaches.push('SAB transfer not zero-copy / not reported');
else if (sab.ms >= budget.sabTransferMs) breaches.push(`SAB transfer ${sab.ms.toFixed(1)}ms`);
if (p95 >= budget.insertP95Ms) breaches.push(`insert p95 ${p95.toFixed(2)}ms`);

/* Issue #476 - open-doc is gated now. It was reported only while a
   multi-page open was dominated by whole-document relayout; incremental,
   viewport-culled layout shipped (CLAUDE.md: LazyLayoutState), and the
   50-page fixture opens in tens of milliseconds, far inside the budget, so a
   breach is a real regression. A failed load, or a fixture that no longer
   lays out to ~50 pages (the measurement would silently describe another
   document), fails the run as well. */
if (openDocMs !== null) {
    if (openDocMs < 0) breaches.push('open-doc LOAD_DOCX failed');
    else if (openDocMs >= budget.openDocMs) breaches.push(`open-doc ${openDocMs.toFixed(0)}ms`);
    if (openDocPages === null) {
        breaches.push('open-doc page count not reported (EXPAND_LAYOUT did not complete)');
    } else if (Math.abs(openDocPages - NOMINAL_PAGES) > NOMINAL_PAGES * NOMINAL_PAGE_TOLERANCE) {
        breaches.push(
            `50p.docx lays out to ${openDocPages} pages, not ~${NOMINAL_PAGES} ` +
                '(re-calibrate tools/perf-fixtures PARAS_PER_PAGE)',
        );
    }
}

if (breaches.length) {
    console.error(`[perf] OVER ${hardware} BUDGET — ${breaches.join('; ')}`);
    process.exit(strict ? 1 : 0);
}
console.log(`[perf] PASS — cold start, worker boot, SAB, insert p95 and open-doc within ${hardware} budgets`);
process.exit(0);
