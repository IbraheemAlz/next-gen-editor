#!/usr/bin/env node
/**
 * Failure bucketer + top-N report — issue #88 Scope §3.
 *
 * Reads the JSONL `corpus-native` produces (one record per document — see
 * `tools/corpus-native/src/pipeline.rs` for the schema) and:
 *   - counts outcomes (ok / error / panic / crash / timeout),
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
 *     issue calls for).
 *
 * Usage:
 *   node report.mjs [--in /data/corpus/results.jsonl] [--json out.json] [--top 20]
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
    const buckets = new Map(); // signature -> { count, examplePath, exampleBytes, message, outcome, stage }
    for (const r of records) {
        if (r.outcome === 'ok') continue;
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
    const editOutOfBound = records.filter((r) => r.edit_check && r.edit_check.within_bound === false);

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
    console.log(`  scripted-edit ≤2×N bound exceeded:      ${editOutOfBound.length}`);
    for (const r of editOutOfBound.slice(0, TOP_N)) {
        console.log(`    ${r.path} (Δ ${r.edit_check.document_xml_delta_bytes} B > bound ${r.edit_check.bound_bytes} B)`);
    }

    /* --- Layout-time outliers. --- */
    const withLayoutTime = records.filter((r) => typeof r.layout_ms === 'number').sort((a, b) => b.layout_ms - a.layout_ms);
    console.log(`\n=== Layout-time outliers (top ${Math.min(10, withLayoutTime.length)}) ===`);
    for (const r of withLayoutTime.slice(0, 10)) {
        console.log(
            `  ${r.layout_ms} ms  ${r.path}  (${r.size_bytes} B, ${r.page_count_before ?? '?'} pages, ${r.paragraph_count ?? '?'} paragraphs)`,
        );
    }

    const summary = {
        total_documents: records.length,
        outcome_counts: outcomeCounts,
        failure_buckets: sortedBuckets,
        round_trip_violations: {
            document_xml_drifted: xmlDrifted.map((r) => r.path),
            sibling_drifted: siblingDrifted.map((r) => r.path),
            text_lost: textLost.map((r) => r.path),
            page_count_unstable: pageUnstable.map((r) => r.path),
            edit_bound_exceeded: editOutOfBound.map((r) => r.path),
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
