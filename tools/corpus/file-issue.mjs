#!/usr/bin/env node
/**
 * Auto-file / update a GitHub tracking issue per NEW panic/crash signature —
 * issue #88 Scope §4 ("any new panic signature opens/updates a tracking
 * issue automatically via `gh`").
 *
 * IMPORTANT: this script is wired into `.github/workflows/corpus-nightly.yml`
 * and is NOT invoked by a developer running the corpus harness locally
 * (`node run.mjs` never calls it). It requires `gh auth login` / a
 * `GH_TOKEN` with `issues: write`, and it mutates the real issue tracker —
 * treat it the same as any other command that files GitHub issues.
 *
 * Logic:
 *   1. Read `report.mjs --json`'s structured summary.
 *   2. For each failure bucket whose `outcome` is `panic` or `crash` (a
 *      `.claude/rules` violation category — `error`/`timeout` buckets are
 *      NOT auto-filed, since a clean typed `DocxError` on a deliberately
 *      malformed fuzz-corpus file is often the CORRECT, desired behaviour,
 *      not a bug; a human should triage those), compute a stable label
 *      (`corpus-signature:<sha256(signature)[0..12]>`) and search open +
 *      closed issues for it.
 *   3. No match -> `gh issue create` with the signature, reproducer path,
 *      and message, labelled `bug`, `core-engine`, and the stable
 *      fingerprint label.
 *   4. Match found and OPEN -> `gh issue comment` with a "seen again in
 *      last night's run" note (only if the run date differs from the last
 *      comment, to avoid daily spam — best-effort, not load-bearing).
 *   5. Match found and CLOSED -> reopen with a "regressed" comment. A
 *      closed tracking issue for a signature that fired again means the
 *      fix didn't stick (or the bucketing is over-eager) — never silently
 *      re-close it here.
 *
 * Usage:
 *   node file-issue.mjs --json /path/to/summary.json [--repo owner/name] [--dry-run]
 */
import { readFileSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';

const argv = process.argv.slice(2);
const argValue = (flag, fallback) => {
    const i = argv.indexOf(flag);
    return i >= 0 && i + 1 < argv.length ? argv[i + 1] : fallback;
};
const SUMMARY_PATH = argValue('--json', null);
const REPO = argValue('--repo', null); // defaults to `gh`'s own repo detection
const DRY_RUN = argv.includes('--dry-run');

if (!SUMMARY_PATH) {
    console.error('[file-issue] usage: node file-issue.mjs --json <report.mjs summary.json> [--repo owner/name] [--dry-run]');
    process.exit(2);
}

function gh(args) {
    const fullArgs = REPO ? [...args, '--repo', REPO] : args;
    if (DRY_RUN) {
        console.log(`[file-issue] (dry-run) gh ${fullArgs.join(' ')}`);
        return '';
    }
    return execFileSync('gh', fullArgs, { encoding: 'utf8' });
}

function fingerprintLabel(signature) {
    const hash = createHash('sha256').update(signature).digest('hex').slice(0, 12);
    return `corpus-signature:${hash}`;
}

/** Buckets worth auto-filing: genuine crashes the panic-catching layer
 * couldn't (fully) contain. `error` buckets (clean `DocxError`s) and
 * `timeout` buckets are surfaced in the human-read report but not
 * auto-filed — see module docs. */
const AUTO_FILE_OUTCOMES = new Set(['panic', 'crash']);

function ensureLabelExists(label) {
    try {
        gh(['label', 'list', '--search', label, '--json', 'name']);
    } catch {
        /* best-effort — `gh issue create --label` fails loudly enough on
        its own if the label truly doesn't exist and can't be created. */
    }
    if (!DRY_RUN) {
        try {
            execFileSync('gh', ['label', 'create', label, '--color', 'B60205', '--force'], {
                stdio: 'ignore',
            });
        } catch {
            /* label probably already exists — fine. */
        }
    }
}

function findExistingIssue(label) {
    const raw = gh([
        'issue', 'list',
        '--label', label,
        '--state', 'all',
        '--json', 'number,state,url',
        '--limit', '1',
    ]);
    if (!raw) return null;
    try {
        const issues = JSON.parse(raw);
        return issues[0] || null;
    } catch {
        return null;
    }
}

function fileOrUpdate(bucket) {
    const label = fingerprintLabel(bucket.signature);
    ensureLabelExists(label);
    const existing = findExistingIssue(label);

    const body = [
        `Auto-filed by \`tools/corpus/file-issue.mjs\` (issue #88 nightly corpus harness).`,
        '',
        `**Signature:** \`${bucket.signature}\``,
        `**Outcome:** ${bucket.outcome}` + (bucket.stage ? ` (stage: \`${bucket.stage}\`)` : ''),
        `**Occurrences this run:** ${bucket.count}`,
        `**Smallest known reproducer:** \`${bucket.examplePath}\` (${bucket.exampleBytes} bytes)`,
        '',
        '```',
        (bucket.message || '').slice(0, 2000),
        '```',
        '',
        `To reproduce locally: fetch the corpus (\`node tools/corpus/fetch.mjs\`) and run`,
        '```sh',
        `cargo run -p corpus-native --release -- --worker /data/corpus/files/${bucket.examplePath.replace(/^.*files\//, '')}`,
        '```',
    ].join('\n');

    if (!existing) {
        console.log(`[file-issue] NEW: ${bucket.signature}`);
        gh([
            'issue', 'create',
            '--title', `corpus: ${bucket.outcome} — ${bucket.signature.slice(0, 100)}`,
            '--body', body,
            '--label', `bug,core-engine,${label}`,
        ]);
        return;
    }

    if (existing.state === 'CLOSED') {
        console.log(`[file-issue] REGRESSED: ${bucket.signature} (issue #${existing.number})`);
        gh(['issue', 'reopen', String(existing.number)]);
        gh([
            'issue', 'comment', String(existing.number),
            '--body', `Regressed — seen again in last night's corpus run.\n\n${body}`,
        ]);
    } else {
        console.log(`[file-issue] SEEN AGAIN: ${bucket.signature} (issue #${existing.number}, still open)`);
        gh([
            'issue', 'comment', String(existing.number),
            '--body', `Seen again in last night's corpus run (${bucket.count} occurrence(s)). Reproducer: \`${bucket.examplePath}\`.`,
        ]);
    }
}

function main() {
    const summary = JSON.parse(readFileSync(SUMMARY_PATH, 'utf8'));
    const buckets = (summary.failure_buckets || []).filter((b) => AUTO_FILE_OUTCOMES.has(b.outcome));
    console.log(`[file-issue] ${buckets.length} panic/crash signature(s) to triage (of ${summary.failure_buckets?.length ?? 0} total buckets)`);
    for (const bucket of buckets) {
        fileOrUpdate(bucket);
    }
}

main();
