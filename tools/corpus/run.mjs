#!/usr/bin/env node
/**
 * One-shot orchestrator for the issue #88 corpus harness — matches the
 * `run.mjs` convention every other `tools/*` harness uses
 * (`tools/perf/run.mjs`, `tools/visual-diff/run.mjs`, ...).
 *
 * Steps:
 *   1. `node fetch.mjs` (skip with `--skip-fetch` once `/data/corpus/files`
 *      is already populated — nightly CI caches it, see
 *      `.github/workflows/corpus-nightly.yml`).
 *   2. `cargo build -p corpus-native --release`.
 *   3. Run the release binary over the corpus, writing JSONL.
 *   4. `node report.mjs` on that JSONL.
 *
 * Usage:
 *   node run.mjs [--skip-fetch] [--corpus-dir DIR] [--out FILE] [--limit N]
 *                [--no-edit] [--timeout-secs N]
 *
 * Exit code: non-zero only if a SETUP step fails (fetch, build, or the
 * corpus-native process itself erroring out) — matching
 * `corpus-native`'s own "exit reflects whether the run completed, not
 * what it found" contract. A document panicking/crashing/erroring is
 * data, not a run failure.
 */
import { spawnSync } from 'node:child_process';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { existsSync } from 'node:fs';

const HERE = dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = join(HERE, '..', '..');

const argv = process.argv.slice(2);
const argValue = (flag, fallback) => {
    const i = argv.indexOf(flag);
    return i >= 0 && i + 1 < argv.length ? argv[i + 1] : fallback;
};
const SKIP_FETCH = argv.includes('--skip-fetch');
const CORPUS_DIR = argValue('--corpus-dir', '/data/corpus/files');
const OUT = argValue('--out', '/data/corpus/results.jsonl');
const LIMIT = argValue('--limit', null);
const NO_EDIT = argv.includes('--no-edit');
const TIMEOUT_SECS = argValue('--timeout-secs', '60');

function run(label, cmd, args, opts = {}) {
    console.log(`[run] ${label}: ${cmd} ${args.join(' ')}`);
    const res = spawnSync(cmd, args, { stdio: 'inherit', cwd: REPO_ROOT, ...opts });
    if (res.status !== 0) {
        console.error(`[run] ${label} failed (exit ${res.status})`);
        process.exit(res.status ?? 1);
    }
}

if (!SKIP_FETCH) {
    run('fetch corpus', 'node', [join(HERE, 'fetch.mjs'), '--out-dir', CORPUS_DIR]);
} else {
    console.log(`[run] --skip-fetch: using existing ${CORPUS_DIR}`);
}

if (!existsSync(CORPUS_DIR)) {
    console.error(`[run] corpus dir ${CORPUS_DIR} still doesn't exist after fetch — aborting`);
    process.exit(1);
}

run('build corpus-native', 'cargo', ['build', '-p', 'corpus-native', '--release']);

const binArgs = ['run', '-p', 'corpus-native', '--release', '--', '--corpus-dir', CORPUS_DIR, '--out', OUT, '--timeout-secs', TIMEOUT_SECS];
if (LIMIT) binArgs.push('--limit', LIMIT);
if (NO_EDIT) binArgs.push('--no-edit');
run('corpus-native', 'cargo', binArgs);

run('report', 'node', [join(HERE, 'report.mjs'), '--in', OUT]);

console.log(`\n[run] done. JSONL: ${OUT}`);
console.log('[run] filing/updating tracking issues for NEW panic signatures is a separate,');
console.log('[run] nightly-CI-only step (tools/corpus/file-issue.mjs) — not run here.');
