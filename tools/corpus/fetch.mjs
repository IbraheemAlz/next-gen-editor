#!/usr/bin/env node
/**
 * Corpus fetch script — issue #88 Scope §1.
 *
 * Reads `sources.json` (committed, hand-curated) and materializes every
 * source's `.docx` files into `--out-dir` (default `/data/corpus/files`,
 * OUTSIDE the repo — never committed). Writes `manifest.json` (sha256,
 * source, license per file) next to this script, which IS committed.
 *
 * Two source kinds:
 *   - `local-dir`   — copies `*.docx` out of a directory already in this
 *                     repo (our own round-trip / perf fixtures).
 *   - `github-dir`  — lists a directory in a public GitHub repo via the
 *                     Contents API (pinned to `ref`) and downloads every
 *                     `.docx` entry from `raw.githubusercontent.com`.
 *
 * Deliberately NOT a general web crawler: the issue's DISK BUDGET note
 * ("a modest public sample -- a few hundred documents") and this being a
 * from-scratch harness favour a small, reviewable, licensed source list
 * over crawler machinery this PR doesn't have budget to make safe
 * (robots.txt, license detection, dedup at scale). `sources.json` is easy
 * to extend with more `github-dir` entries later (nightly CI has a much
 * larger time/disk budget than this PR's local 1 GB cap) toward the
 * issue's "≥1,000 real documents" nightly acceptance bar.
 *
 * Reproducibility note: `github-dir` sources pin a `ref` (branch or tag),
 * not an immutable commit SHA. A branch ref (e.g. Apache POI's `trunk`) can
 * gain/lose/modify files between runs — deliberately: a nightly corpus
 * harness benefits from picking up new upstream regression fixtures over
 * time. `manifest.json` records exactly what a given run fetched (sha256
 * per file), so drift is always visible in `git diff`, but a later re-fetch
 * is not guaranteed byte-identical to what's currently committed. Pin a
 * commit SHA as `ref` instead of a branch name if exact reproducibility is
 * ever required.
 *
 * Usage:
 *   node fetch.mjs [--out-dir /data/corpus/files] [--max-bytes N] [--force]
 *
 * `GITHUB_TOKEN` (or `GH_TOKEN`), if set, is sent as a Bearer token to
 * raise the anonymous 60 req/hr GitHub API limit — only the directory
 * LISTING call counts against it (one per `github-dir` source); the actual
 * file downloads go through `raw.githubusercontent.com`, which is not
 * API-rate-limited the same way.
 */
import { createHash } from 'node:crypto';
import { existsSync, mkdirSync, readFileSync, readdirSync, writeFileSync, copyFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = join(HERE, '..', '..');

const argv = process.argv.slice(2);
const argValue = (flag, fallback) => {
    const i = argv.indexOf(flag);
    return i >= 0 && i + 1 < argv.length ? argv[i + 1] : fallback;
};
const OUT_DIR = argValue('--out-dir', '/data/corpus/files');
const MAX_BYTES = Number(argValue('--max-bytes', String(1_000_000_000))); // 1 GB default budget
const FORCE = argv.includes('--force');

const GH_TOKEN = process.env.GITHUB_TOKEN || process.env.GH_TOKEN || '';

function sha256Hex(buf) {
    return createHash('sha256').update(buf).digest('hex');
}

function githubHeaders() {
    const headers = { 'User-Agent': 'next-gen-editor-corpus-fetch', Accept: 'application/vnd.github+json' };
    if (GH_TOKEN) headers.Authorization = `Bearer ${GH_TOKEN}`;
    return headers;
}

async function listGithubDir(source) {
    const url = `https://api.github.com/repos/${source.repo}/contents/${source.path}?ref=${encodeURIComponent(source.ref)}`;
    const res = await fetch(url, { headers: githubHeaders() });
    if (!res.ok) {
        throw new Error(`GitHub contents API ${url} -> HTTP ${res.status}`);
    }
    const entries = await res.json();
    if (!Array.isArray(entries)) {
        throw new Error(`GitHub contents API ${url} did not return a directory listing`);
    }
    return entries.filter((e) => e.type === 'file' && e.name.toLowerCase().endsWith('.docx'));
}

function listLocalDocx(dir) {
    const abs = join(REPO_ROOT, dir);
    if (!existsSync(abs)) return [];
    return readdirSync(abs).filter((f) => f.toLowerCase().endsWith('.docx'));
}

async function fetchSource(source, budget) {
    const destDir = join(OUT_DIR, source.id);
    mkdirSync(destDir, { recursive: true });
    const files = [];

    if (source.kind === 'local-dir') {
        for (const name of listLocalDocx(source.dir)) {
            const srcPath = join(REPO_ROOT, source.dir, name);
            const destPath = join(destDir, name);
            const bytes = readFileSync(srcPath);
            if (budget.used + bytes.length > budget.max) {
                console.warn(`[fetch] budget exhausted, skipping remaining files in ${source.id}`);
                break;
            }
            copyFileSync(srcPath, destPath);
            budget.used += bytes.length;
            files.push({
                path: `${source.id}/${name}`,
                sha256: sha256Hex(bytes),
                bytes: bytes.length,
                source_id: source.id,
                source_url: `repo:${source.dir}/${name}`,
                license: source.license,
                project: source.project,
            });
        }
    } else if (source.kind === 'github-dir') {
        const entries = await listGithubDir(source);
        for (const entry of entries) {
            const destPath = join(destDir, entry.name);
            if (!FORCE && existsSync(destPath)) {
                const bytes = readFileSync(destPath);
                budget.used += bytes.length;
                files.push({
                    path: `${source.id}/${entry.name}`,
                    sha256: sha256Hex(bytes),
                    bytes: bytes.length,
                    source_id: source.id,
                    source_url: entry.html_url,
                    license: source.license,
                    project: source.project,
                });
                continue;
            }
            if (budget.used + entry.size > budget.max) {
                console.warn(`[fetch] budget exhausted, skipping remaining files in ${source.id}`);
                break;
            }
            const res = await fetch(entry.download_url);
            if (!res.ok) {
                console.warn(`[fetch] HTTP ${res.status} for ${entry.download_url} — skipping`);
                continue;
            }
            const bytes = Buffer.from(await res.arrayBuffer());
            writeFileSync(destPath, bytes);
            budget.used += bytes.length;
            files.push({
                path: `${source.id}/${entry.name}`,
                sha256: sha256Hex(bytes),
                bytes: bytes.length,
                source_id: source.id,
                source_url: entry.html_url,
                license: source.license,
                project: source.project,
            });
        }
    } else {
        throw new Error(`unknown source kind '${source.kind}' for ${source.id}`);
    }
    return files;
}

async function main() {
    const { sources } = JSON.parse(readFileSync(join(HERE, 'sources.json'), 'utf8'));
    mkdirSync(OUT_DIR, { recursive: true });
    const budget = { used: 0, max: MAX_BYTES };
    const allFiles = [];

    for (const source of sources) {
        console.log(`[fetch] ${source.id} (${source.kind}) ...`);
        try {
            const files = await fetchSource(source, budget);
            console.log(`[fetch]   ${files.length} files, ${(budget.used / 1e6).toFixed(1)} MB used so far`);
            allFiles.push(...files);
        } catch (err) {
            console.error(`[fetch]   FAILED: ${err.message}`);
            console.error('[fetch]   continuing with remaining sources');
        }
        if (budget.used >= budget.max) {
            console.warn('[fetch] total byte budget reached — stopping early');
            break;
        }
    }

    allFiles.sort((a, b) => (a.path < b.path ? -1 : a.path > b.path ? 1 : 0));
    const manifest = {
        $comment:
            'Committed provenance record for /data/corpus/files (never committed itself). Regenerate with `node tools/corpus/fetch.mjs`.',
        sources,
        total_files: allFiles.length,
        total_bytes: allFiles.reduce((n, f) => n + f.bytes, 0),
        files: allFiles,
    };
    writeFileSync(join(HERE, 'manifest.json'), `${JSON.stringify(manifest, null, 2)}\n`);
    console.log(
        `[fetch] done: ${allFiles.length} files, ${(manifest.total_bytes / 1e6).toFixed(1)} MB -> ${OUT_DIR}`,
    );
    console.log(`[fetch] manifest written to ${join(HERE, 'manifest.json')}`);
}

main().catch((err) => {
    console.error(`[fetch] fatal: ${err.stack || err.message}`);
    process.exit(1);
});
