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
 * Requires the Vite dev server (`pnpm dev`, http://localhost:5173). Override
 * the origin with the URL env var. Uses Playwright with the system Chrome
 * (channel:'chrome') — no extra browser download.
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

const HERE = dirname(fileURLToPath(import.meta.url));
const PERF_DOC = join(HERE, '..', '..', 'tests', 'perf', '50p.docx');
/* Issue #379 — the table-heavy repaint fixture. */
const TABLES_DOC = join(HERE, '..', '..', 'tests', 'perf', 'tables-30p.docx');
const ORIGIN = process.env.URL ?? 'http://localhost:5173';

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
/* Issue #379 — body edits timed on the table-heavy fixture. */
const TABLE_REPAINT_KEYSTROKES = 40;

function percentile(samples, p) {
    const sorted = [...samples].sort((a, b) => a - b);
    const rank = Math.ceil((p / 100) * sorted.length) - 1;
    return sorted[Math.min(sorted.length - 1, Math.max(0, rank))];
}

console.log(`[perf] origin=${ORIGIN} hardware=${hardware} mode=${strict ? 'strict' : 'report'}`);

const browser = await chromium.launch({ headless: true, channel: 'chrome' });
let coldStartMs = 0;
let workerBootMs = null;
let sab = null;
let samples = [];
let openDocMs = null;
let tables = null;
try {
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
        openDocMs = await page.evaluate(async (bytes) => {
            const start = performance.now();
            const evt = await window.__dispatch({
                type: 'LOAD_DOCX',
                bytes: new Uint8Array(bytes),
            });
            const elapsed = performance.now() - start;
            return evt.type === 'DOCUMENT_LOADED' ? elapsed : -1;
        }, docxBytes);
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
                if (evt.type !== 'DOCUMENT_LOADED') return { openMs: -1, samples: [] };
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
                return { openMs, samples };
            },
            { bytes: docxBytes, n: TABLE_REPAINT_KEYSTROKES },
        );
    } else {
        console.warn(`[perf] ${TABLES_DOC} missing — skipping table repaint metric (run tools/perf-fixtures)`);
    }
} finally {
    await browser.close();
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
    console.log(`[perf] open 50-page document     : ${shown} (budget ${budget.openDocMs} ms)`);
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
                `p95 ${tp95.toFixed(2)} ms (report only)`,
        );
    }
}

/* Gated budgets: cold start + insert p95 — the achievable §6 numbers a
   regression must fail on. */
const breaches = [];
if (coldStartMs >= budget.coldStartMs) breaches.push(`cold start ${coldStartMs}ms`);
if (workerBootMs === null) breaches.push('worker boot never reported (__bootMs)');
else if (workerBootMs >= budget.workerBootMs) breaches.push(`worker boot ${workerBootMs.toFixed(1)}ms`);
if (sab === null || !sab.ok) breaches.push('SAB transfer not zero-copy / not reported');
else if (sab.ms >= budget.sabTransferMs) breaches.push(`SAB transfer ${sab.ms.toFixed(1)}ms`);
if (p95 >= budget.insertP95Ms) breaches.push(`insert p95 ${p95.toFixed(2)}ms`);

/* Open-doc on a multi-page document is dominated by the engine's
   whole-document relayout — incremental relayout is deferred Phase-5 work
   (CLAUDE.md / BACKLOG). It is measured and reported, but kept out of the
   --strict gate so a known, tracked cost cannot mask a real regression in
   the other budgets. */
if (openDocMs !== null && openDocMs >= 0 && openDocMs >= budget.openDocMs) {
    console.warn(
        `[perf] note: open-doc ${openDocMs.toFixed(0)} ms exceeds the ` +
            `${budget.openDocMs} ms budget — known deferred cost (incremental relayout)`,
    );
}

if (breaches.length) {
    console.error(`[perf] OVER ${hardware} BUDGET — ${breaches.join('; ')}`);
    process.exit(strict ? 1 : 0);
}
console.log(`[perf] PASS — cold start + insert p95 within ${hardware} budgets`);
process.exit(0);
