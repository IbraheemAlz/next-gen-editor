#!/usr/bin/env node
/**
 * Failure bucketer + top-N report — issue #88 Scope §3.
 *
 * Reads the JSONL `corpus-native` produces (one record per document — see
 * `tools/corpus-native/src/pipeline.rs` for the schema) and:
 *   - counts outcomes (ok / error / panic / crash / timeout),
 *   - flags a timeout only when it is CONFIRMED (issue #418): corpus-native
 *     retries a timed-out document once, alone, and records `timeout` only
 *     when the retry times out too; a first-attempt timeout the retry
 *     cleared is reported as load noise, never as a failure,
 *   - buckets every non-`ok` record into a stable *signature* (mirrors
 *     `corpus-native`'s own `panics::normalize_signature`: digit runs
 *     collapse to `N` so the same underlying bug at different byte offsets
 *     lands in one bucket),
 *   - for each bucket, surfaces the SMALLEST reproducing file as a stand-in
 *     "minimized reproducer" (true delta-debug minimization is its own
 *     tool; smallest-known-input is the pragmatic proxy here),
 *   - flags every "unchanged" `document.xml` / round-trip anomaly even on
 *     an `ok` document (a document can pass "no panic" and still have
 *     drifted `document.xml` on a no-op save — that's a real bug even
 *     though `corpus-native` doesn't hard-fail on it),
 *   - lists layout-time outliers (the "layout-time outliers" bucket the
 *     issue calls for),
 *   - issue #380: a production-layout section - p50/p95/max
 *     `engine_layout_ms` and CPU ms (#418), documents over the layout budget,
 *     a histogram of `engine_degradations`, and a page-count diff against the
 *     previous nightly artifact (`--prev`). A document that laid out in the
 *     previous artifact and now has a CONFIRMED timeout (the #418 retry also
 *     failed) is a layout REGRESSION: it is printed, emitted as a GitHub
 *     `::warning::` annotation and listed in the JSON summary. A first-attempt
 *     timeout the retry cleared is never flagged. `n/a` is reserved for files
 *     that `read_docx` itself rejects (the engine-layout stage runs for every
 *     document that reads, whatever the round-trip outcome).
 *
 * Usage:
 *   node report.mjs [--in /data/corpus/results.jsonl] [--json out.json] [--top 20]
 *                   [--prev previous/results.jsonl] [--layout-budget-ms 10000]
 *
 * Exit code is always 0 — this is a reporting tool, not a gate (the issue's
 * nightly workflow is explicitly non-blocking, matching `qa-harness` in
 * `ci.yml`).
 */
import { readFileSync, writeFileSync, existsSync } from 'node:fs';

const argv = process.argv.slice(2);
const argValue = (flag, fallback) => {
    const i = argv.indexOf(flag);
    return i >= 0 && i + 1 < argv.length ? argv[i + 1] : fallback;
};
const IN_PATH = argValue('--in', '/data/corpus/results.jsonl');
const JSON_OUT = argValue('--json', null);
const TOP_N = Number(argValue('--top', '20'));
const PREV_PATH = argValue('--prev', null);
/* Mirrors corpus-native's DEFAULT_LAYOUT_BUDGET_MS. */
const LAYOUT_BUDGET_MS = Number(argValue('--layout-budget-ms', '10000'));

function percentile(sortedAsc, p) {
    if (sortedAsc.length === 0) return null;
    const rank = Math.ceil((p / 100) * sortedAsc.length) - 1;
    return sortedAsc[Math.min(sortedAsc.length - 1, Math.max(0, rank))];
}

/** Compute the production-layout summary (issue #380) - exported shape is the JSON `layout` block. */
function layoutSummary(records, prevRecords) {
    const laidOut = records.filter((r) => typeof r.engine_layout_ms === 'number');
    const ms = laidOut.map((r) => r.engine_layout_ms).sort((a, b) => a - b);
    const cpu = records
        .filter((r) => typeof r.engine_layout_cpu_ms === 'number')
        .map((r) => r.engine_layout_cpu_ms)
        .sort((a, b) => a - b);
    const isEngineTimeout = (r) => r.outcome === 'timeout' && r.stage === 'engine_layout';
    const overBudget = records.filter(
        (r) => isEngineTimeout(r) || (typeof r.engine_layout_ms === 'number' && r.engine_layout_ms > LAYOUT_BUDGET_MS),
    );
    const degradations = new Map();
    for (const r of records) {
        for (const d of new Set(r.engine_degradations || [])) degradations.set(d, (degradations.get(d) || 0) + 1);
    }
    /* Documents the stage never reached = the file did not read. */
    const notAvailable = records.filter(
        (r) => typeof r.engine_layout_ms !== 'number' && typeof r.engine_layout_wall_ms !== 'number',
    );
    const pageDiffs = [];
    const regressions = [];
    if (prevRecords) {
        const prev = new Map(prevRecords.map((r) => [r.path, r]));
        for (const r of records) {
            const p = prev.get(r.path);
            if (!p) continue;
            const prevLaidOut = typeof p.engine_layout_ms === 'number';
            if (prevLaidOut && typeof r.engine_page_count === 'number' && p.engine_page_count !== r.engine_page_count) {
                pageDiffs.push({ path: r.path, before: p.engine_page_count, after: r.engine_page_count });
            }
            const confirmed = r.outcome === 'timeout' && r.timeout_retry && r.timeout_retry.recovered === false;
            if (prevLaidOut && confirmed && r.stage === 'engine_layout') {
                regressions.push({ path: r.path, previous_ms: p.engine_layout_ms });
            }
        }
    }
    return { laidOut, ms, cpu, overBudget, degradations, notAvailable, pageDiffs, regressions };
}

function normalizeSignature(outcome, stage, message) {
    const normalized = (message || '')
        .replace(/\d+/g, 'N')
        .slice(0, 200);
    return `${outcome}@${stage || '?'}: ${normalized}`;
}

function loadRecords(path) {
    if (!existsSync(path)) {
        console.error(`[report] no such file: ${path}`);
        console.error('[report] run `corpus-native` first (see tools/corpus-native/src/main.rs)');
        process.exit(1);
    }
    const lines = readFileSync(path, 'utf8').split('\n').filter((l) => l.trim().length > 0);
    const records = [];
    for (const line of lines) {
        try {
            records.push(JSON.parse(line));
        } catch {
            console.warn(`[report] skipping malformed JSONL line: ${line.slice(0, 100)}`);
        }
    }
    return records;
}

function main() {
    const records = loadRecords(IN_PATH);
    const prevRecords = PREV_PATH && existsSync(PREV_PATH) ? loadRecords(PREV_PATH) : null;
    if (PREV_PATH && !prevRecords) console.warn(`[report] --prev ${PREV_PATH} not found; no page-count diff`);
    console.log(`[report] ${records.length} documents from ${IN_PATH}`);

    /* --- Outcome counts. --- */
    const outcomeCounts = {};
    for (const r of records) {
        outcomeCounts[r.outcome] = (outcomeCounts[r.outcome] || 0) + 1;
    }
    console.log('\n=== Outcome counts ===');
    for (const [outcome, count] of Object.entries(outcomeCounts).sort((a, b) => b[1] - a[1])) {
        console.log(`  ${outcome.padEnd(10)} ${count}`);
    }

    /* --- Failure buckets (panic signature when present, else outcome@stage). --- */
    /* Issue #418 - a `timeout` record is CONFIRMED when the lone retry also
    timed out (`timeout_retry.recovered === false`). A record from before
    the retry existed carries no `timeout_retry`: it is UNVERIFIED (it may
    be load noise) and is listed, not bucketed as a failure. */
    const isTimeout = (r) => r.outcome === 'timeout';
    const isConfirmedTimeout = (r) => isTimeout(r) && r.timeout_retry && r.timeout_retry.recovered === false;
    const confirmedTimeouts = records.filter(isConfirmedTimeout);
    const unverifiedTimeouts = records.filter((r) => isTimeout(r) && !r.timeout_retry);
    const clearedTimeouts = records.filter((r) => r.timeout_retry && r.timeout_retry.recovered === true);
    console.log('\n=== Timeouts (#418) ===');
    console.log(`  confirmed (the retry, alone, also timed out): ${confirmedTimeouts.length}`);
    for (const r of confirmedTimeouts) {
        console.log(
            `    ${r.path} (${r.stage ?? 'whole document'}; cpu ${r.engine_layout_cpu_ms ?? '?'} ms, wall ${r.engine_layout_wall_ms ?? '?'} ms)`,
        );
    }
    console.log(`  cleared by the lone retry (load noise, not flagged): ${clearedTimeouts.length}`);
    for (const r of clearedTimeouts) {
        const t = r.timeout_retry;
        console.log(`    ${r.path} (first attempt: cpu ${t.first_cpu_ms ?? '?'} ms, wall ${t.first_wall_ms ?? '?'} ms)`);
    }
    if (unverifiedTimeouts.length > 0) {
        console.log(`  unverified (record predates the retry; re-run to confirm): ${unverifiedTimeouts.length}`);
        for (const r of unverifiedTimeouts) console.log(`    ${r.path}`);
    }

    /* --- Issue #379 - warm repaint: a second full production layout with
    every cross-paint layout cache warm must reproduce the cold layout. A
    mismatch is a cache bug (a stale or colliding entry served). --- */
    const withRepaint = records.filter((r) => typeof r.engine_repaint_consistent === 'boolean');
    const repaintInconsistent = withRepaint.filter((r) => r.engine_repaint_consistent === false);
    const sumMs = (rs, key) => rs.reduce((acc, r) => acc + (r[key] ?? 0), 0);
    console.log('\n=== Warm repaint (#379) ===');
    console.log(
        `  ${withRepaint.length} documents: cold layout ${sumMs(withRepaint, 'engine_layout_ms')} ms total, ` +
            `warm repaint ${sumMs(withRepaint, 'engine_repaint_ms')} ms total`,
    );
    console.log(`  inconsistent (warm != cold - a layout-cache bug): ${repaintInconsistent.length}`);
    for (const r of repaintInconsistent) console.log(`    ${r.path}`);

    const buckets = new Map(); // signature -> { count, examplePath, exampleBytes, message, outcome, stage }
    for (const r of records) {
        if (r.outcome === 'ok') continue;
        if (isTimeout(r) && !isConfirmedTimeout(r)) continue; // #418: only confirmed timeouts are failures
        const sig = r.panic_signature || normalizeSignature(r.outcome, r.stage, r.message);
        const existing = buckets.get(sig);
        if (!existing) {
            buckets.set(sig, {
                signature: sig,
                outcome: r.outcome,
                stage: r.stage ?? null,
                count: 1,
                examplePath: r.path,
                exampleBytes: r.size_bytes,
                message: r.message,
            });
        } else {
            existing.count += 1;
            /* Smallest known reproducer — the pragmatic "minimized" proxy
            (see module docs). */
            if (r.size_bytes < existing.exampleBytes) {
                existing.examplePath = r.path;
                existing.exampleBytes = r.size_bytes;
            }
        }
    }
    const sortedBuckets = [...buckets.values()].sort((a, b) => b.count - a.count);

    console.log(`\n=== Failure buckets (${sortedBuckets.length} distinct signatures) ===`);
    for (const b of sortedBuckets.slice(0, TOP_N)) {
        console.log(`  [${b.count}x] ${b.signature}`);
        console.log(`        reproducer: ${b.examplePath} (${b.exampleBytes} B)`);
    }
    if (sortedBuckets.length > TOP_N) {
        console.log(`  ... and ${sortedBuckets.length - TOP_N} more signatures (raise --top to see them)`);
    }

    /* --- Round-trip anomalies even on `ok` documents. --- */
    const xmlDrifted = records.filter((r) => r.document_xml_unchanged === false);
    const siblingDrifted = records.filter((r) => r.sibling_bytes_identical === false);
    const textLost = records.filter((r) => r.plain_text_equal === false);
    const pageUnstable = records.filter((r) => r.page_count_stable === false);
    /* Issue #251 — the PRIMARY edit-drift bound: no ORIGINAL byte rewritten.
    `within_bound` (the old ≤2×N size-only check) is kept below as an
    INFORMATIONAL column only — see CLAUDE.md's ".docx round-trip
    invariants" for why it can't distinguish fidelity from loss. */
    const editChecked = records.filter((r) => r.edit_check);
    const fidelityViolations = records.filter((r) => r.edit_check && r.edit_check.fidelity_ok === false);
    const secondaryBoundViolations = records.filter(
        (r) => r.edit_check && r.edit_check.within_secondary_bound === false,
    );
    const editOutOfBoundInformational = records.filter(
        (r) => r.edit_check && r.edit_check.within_bound === false,
    );

    console.log('\n=== Round-trip invariant violations (may overlap with failure buckets above) ===');
    console.log(`  document.xml changed on a no-op resave: ${xmlDrifted.length}`);
    for (const r of xmlDrifted.slice(0, TOP_N)) {
        console.log(`    ${r.path} (Δ ${r.document_xml_delta_noedit_bytes} B)`);
    }
    console.log(`  sibling entries not byte-identical:     ${siblingDrifted.length}`);
    for (const r of siblingDrifted.slice(0, TOP_N)) {
        console.log(`    ${r.path} (Δ ${r.sibling_drift_bytes} B)`);
    }
    console.log(`  plain-text mismatch after reopen:       ${textLost.length}`);
    for (const r of textLost.slice(0, TOP_N)) {
        console.log(`    ${r.path}`);
    }
    console.log(`  page count unstable across reopen:      ${pageUnstable.length}`);
    for (const r of pageUnstable.slice(0, TOP_N)) {
        console.log(`    ${r.path} (${r.page_count_before} -> ${r.page_count_after})`);
    }
    console.log(
        `  scripted-edit fidelity bound violated (#251, primary): ${fidelityViolations.length}/${editChecked.length}`,
    );
    for (const r of fidelityViolations.slice(0, TOP_N)) {
        const cause = r.edit_check.rewrite_cause ?? '<unclassified>';
        console.log(`    ${r.path} (rewrote ${r.edit_check.source_bytes_rewritten} B, cause: ${cause})`);
    }
    console.log(
        `  scripted-edit secondary size bound violated (#251, advisory): ${secondaryBoundViolations.length}/${editChecked.length}`,
    );
    for (const r of secondaryBoundViolations.slice(0, TOP_N)) {
        console.log(
            `    ${r.path} (Δ ${r.edit_check.document_xml_delta_bytes} B > secondary bound ${r.edit_check.secondary_bound_bytes} B)`,
        );
    }
    console.log(
        `  [informational] old size-only ≤2×N bound exceeded:   ${editOutOfBoundInformational.length}/${editChecked.length}`,
    );
    for (const r of editOutOfBoundInformational.slice(0, TOP_N)) {
        console.log(`    ${r.path} (Δ ${r.edit_check.document_xml_delta_bytes} B > bound ${r.edit_check.bound_bytes} B)`);
    }

    /* --- Rewrite root-cause histogram (issues #242-#249), #251 Scope §2. --- */
    const causeCounts = new Map();
    for (const r of fidelityViolations) {
        const cause = r.edit_check.rewrite_cause ?? '<unclassified>';
        causeCounts.set(cause, (causeCounts.get(cause) || 0) + 1);
    }
    const sortedCauses = [...causeCounts.entries()].sort((a, b) => b[1] - a[1]);
    if (sortedCauses.length > 0) {
        console.log('\n=== Rewrite root-cause histogram (docs still rewriting source bytes) ===');
        for (const [cause, count] of sortedCauses) {
            console.log(`  ${String(count).padStart(4)}  ${cause}`);
        }
    }

    /* --- Issue #384 — `--regen-check`: clean paragraphs that do not
    regenerate byte-identically, by class. --- */
    const regenChecked = records.filter((r) => r.regen_check);
    const regenClasses = new Map();
    let regenParagraphs = 0;
    let regenMismatched = 0;
    for (const r of regenChecked) {
        regenParagraphs += r.regen_check.checked;
        regenMismatched += r.regen_check.mismatched;
        for (const [cls, n] of Object.entries(r.regen_check.classes ?? {})) {
            regenClasses.set(cls, (regenClasses.get(cls) || 0) + n);
        }
    }
    const sortedRegenClasses = [...regenClasses.entries()].sort((a, b) => b[1] - a[1]);
    if (regenChecked.length > 0) {
        console.log(
            `\n=== Paragraph regeneration (#384): ${regenMismatched}/${regenParagraphs} clean paragraphs differ, ` +
                `${regenChecked.filter((r) => r.regen_check.mismatched > 0).length}/${regenChecked.length} documents ===`,
        );
        for (const [cls, count] of sortedRegenClasses) {
            console.log(`  ${String(count).padStart(4)}  ${cls}`);
        }
    }

    /* --- Layout-time outliers. --- */
    const withLayoutTime = records.filter((r) => typeof r.layout_ms === 'number').sort((a, b) => b.layout_ms - a.layout_ms);
    console.log(`\n=== Layout-time outliers (top ${Math.min(10, withLayoutTime.length)}) ===`);
    for (const r of withLayoutTime.slice(0, 10)) {
        console.log(
            `  ${r.layout_ms} ms  ${r.path}  (${r.size_bytes} B, ${r.page_count_before ?? '?'} pages, ${r.paragraph_count ?? '?'} paragraphs)`,
        );
    }

    /* --- Production layout (issues #318 / #380 / #418). --- */
    const L = layoutSummary(records, prevRecords);
    const fmt = (v) => (v === null ? 'n/a' : `${v} ms`);
    console.log(`\n=== Production layout (engine-wasm; budget ${LAYOUT_BUDGET_MS} ms CPU) ===`);
    console.log(`  laid out: ${L.laidOut.length}/${records.length} (n/a - file does not read: ${L.notAvailable.length})`);
    console.log(
        `  engine_layout_ms     p50 ${fmt(percentile(L.ms, 50))} - p95 ${fmt(percentile(L.ms, 95))} - max ${fmt(L.ms.at(-1) ?? null)}`,
    );
    console.log(
        `  engine_layout_cpu_ms p50 ${fmt(percentile(L.cpu, 50))} - p95 ${fmt(percentile(L.cpu, 95))} - max ${fmt(L.cpu.at(-1) ?? null)}`,
    );
    console.log(`  documents over budget: ${L.overBudget.length}`);
    for (const r of L.overBudget.slice(0, TOP_N)) {
        console.log(`    ${r.path} (cpu ${r.engine_layout_cpu_ms ?? '?'} ms, wall ${r.engine_layout_wall_ms ?? '?'} ms)`);
    }
    const sortedDeg = [...L.degradations.entries()].sort((a, b) => b[1] - a[1]);
    console.log(`  degradation reasons (documents): ${sortedDeg.length === 0 ? 'none' : ''}`);
    for (const [reason, count] of sortedDeg) console.log(`    ${String(count).padStart(4)}  ${reason}`);
    if (prevRecords) {
        console.log(`  page-count changes vs previous artifact: ${L.pageDiffs.length}`);
        for (const d of L.pageDiffs.slice(0, TOP_N)) console.log(`    ${d.path} (${d.before} -> ${d.after})`);
        console.log(`  LAYOUT REGRESSIONS (laid out before, confirmed timeout now): ${L.regressions.length}`);
        for (const d of L.regressions) {
            console.log(`    ${d.path} (previously ${d.previous_ms} ms)`);
            /* GitHub Actions annotation - flags the run without failing it. */
            console.log(`::warning title=corpus layout regression::${d.path} laid out in ${d.previous_ms} ms last night, now times out (confirmed by the lone retry)`);
        }
    } else {
        console.log('  (no --prev artifact: page-count diff and regression flag skipped)');
    }

    const summary = {
        total_documents: records.length,
        outcome_counts: outcomeCounts,
        failure_buckets: sortedBuckets,
        timeouts: {
            confirmed: confirmedTimeouts.map((r) => r.path),
            cleared_by_retry: clearedTimeouts.map((r) => r.path),
            unverified: unverifiedTimeouts.map((r) => r.path),
        },
        round_trip_violations: {
            document_xml_drifted: xmlDrifted.map((r) => r.path),
            sibling_drifted: siblingDrifted.map((r) => r.path),
            text_lost: textLost.map((r) => r.path),
            page_count_unstable: pageUnstable.map((r) => r.path),
            /* Issue #251 — primary bound + advisory secondary bound, plus
            the old size-only number kept as an informational column. */
            edit_fidelity_violated: fidelityViolations.map((r) => ({
                path: r.path,
                source_bytes_rewritten: r.edit_check.source_bytes_rewritten,
                rewrite_cause: r.edit_check.rewrite_cause ?? null,
            })),
            edit_secondary_bound_exceeded: secondaryBoundViolations.map((r) => r.path),
            edit_bound_exceeded_informational: editOutOfBoundInformational.map((r) => r.path),
        },
        edit_rewrite_root_causes: Object.fromEntries(sortedCauses),
        warm_repaint_inconsistent: repaintInconsistent.map((r) => r.path),
        /* Issue #384 — `--regen-check` mismatch classes (paragraphs). */
        regen_check: {
            documents: regenChecked.length,
            paragraphs: regenParagraphs,
            mismatched: regenMismatched,
            classes: Object.fromEntries(sortedRegenClasses),
        },
        layout: {
            budget_ms: LAYOUT_BUDGET_MS,
            laid_out: L.laidOut.length,
            not_available: L.notAvailable.map((r) => r.path),
            engine_layout_ms: { p50: percentile(L.ms, 50), p95: percentile(L.ms, 95), max: L.ms.at(-1) ?? null },
            engine_layout_cpu_ms: { p50: percentile(L.cpu, 50), p95: percentile(L.cpu, 95), max: L.cpu.at(-1) ?? null },
            over_budget: L.overBudget.map((r) => r.path),
            degradations: Object.fromEntries(L.degradations),
            page_count_changes: L.pageDiffs,
            regressions: L.regressions,
        },
        layout_time_outliers: withLayoutTime.slice(0, 10).map((r) => ({
            path: r.path,
            layout_ms: r.layout_ms,
            size_bytes: r.size_bytes,
            page_count: r.page_count_before,
        })),
    };
    if (JSON_OUT) {
        writeFileSync(JSON_OUT, `${JSON.stringify(summary, null, 2)}\n`);
        console.log(`\n[report] structured summary written to ${JSON_OUT}`);
    }
}

main();
