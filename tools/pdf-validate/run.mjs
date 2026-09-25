#!/usr/bin/env node
/**
 * PDF conformance validation harness — Phase 5 D5.4 (+ issue #28 flavours).
 *
 * Drives the real engine (in its Web Worker, exactly as a user would) to export
 * each corpus document to PDF, then validates the output against the selected
 * conformance profile: PDF/A-1b, PDF/A-2u or PDF/X-3.
 *
 * Usage:
 *   node run.mjs                              — validate tests/corpus/tier-a
 *   node run.mjs --corpus tier-a --profile 1b — explicit corpus + profile
 *   node run.mjs --profile 2u                 — PDF/A-2u flavour
 *   node run.mjs --profile x3                 — PDF/X-3 (structural check only)
 *   node run.mjs --strict                     — fail if veraPDF is unavailable
 *   node run.mjs --regen                      — regenerate tests/corpus/tier-a
 *   node run.mjs --check-fixtures             — CI gate: fail if stale
 *
 * Requires the Vite dev server (`pnpm dev`, http://localhost:5173). Override
 * with the URL env var. Uses Playwright with the system Chrome
 * (channel:'chrome') — no 150 MB chromium download.
 *
 * veraPDF is optional locally: when it is not on PATH the harness still exports
 * every PDF and runs an in-process structural check for the profile's markers,
 * but external conformance is reported as skipped (a hard failure under
 * --strict, which the release pipeline uses). veraPDF validates PDF/A only —
 * it has no PDF/X flavour, so `--profile x3` always stops at the structural
 * check and says so.
 *
 * When the corpus directory holds no .docx fixtures the harness falls back to
 * exporting the editor's seeded document, so it always produces at least one
 * PDF to validate. Corpus population is separate Phase 5 work (D5.1).
 *
 * Issue #258 — `--regen` / `--check-fixtures` never touch the browser. Every
 * `tests/corpus/tier-a/*.docx` fixture is generated in Rust by an
 * `#[ignore]`d `engine-wasm` test named `generate_*_pdf_validate_fixture`
 * (`crates/engine-wasm/src/{toc_pdf_export_tests,pdf_validate_fixtures_tests}.rs`)
 * that writes straight into the working tree — the same idiom
 * `tools/visual-diff`'s `UPDATE=1` goldens and `tools/perf-fixtures` use for
 * "committed, regeneratable fixture". `--regen` runs every one of them via a
 * single substring-filtered `cargo test` (any test whose name contains
 * `pdf_validate_fixture` — no hard-coded list, so a future generator
 * following the naming convention is picked up automatically) and leaves the
 * regenerated files for review/`git add`. `--check-fixtures` does the same
 * regeneration into the working tree, diffs it against HEAD with `git
 * status --porcelain`, restores the working tree via `git checkout --`
 * either way (this check must never leave a dirty tree behind), and fails
 * when the regeneration produced a difference — i.e. a committed fixture is
 * stale. `build_minimal_docx` (`crates/format-docx/src/writer.rs`) is
 * deterministic (issue #258 also fixed 3 unsorted `HashMap` walks that made
 * multi-image documents serialize in a different byte order every run), so
 * two regenerations of an unchanged generator produce byte-identical files
 * and `--check-fixtures` never false-fails on its own re-run.
 */
import { readFileSync, writeFileSync, mkdirSync, readdirSync, existsSync } from 'node:fs';
import { dirname, join, basename } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';

const HERE = dirname(fileURLToPath(import.meta.url));
const REPO = join(HERE, '..', '..');

const argValue = (flag, fallback) => {
    const i = process.argv.indexOf(flag);
    return i >= 0 && i + 1 < process.argv.length ? process.argv[i + 1] : fallback;
};
const hasFlag = (flag) => process.argv.includes(flag);

/* ---- --regen / --check-fixtures — no browser, no corpus/profile flags -- */

if (hasFlag('--regen') || hasFlag('--check-fixtures')) {
    const checkOnly = hasFlag('--check-fixtures');
    const corpusRel = join('tests', 'corpus', 'tier-a');

    console.log(
        `[pdf-validate] ${checkOnly ? 'checking' : 'regenerating'} ${corpusRel} via ` +
            "'cargo test -p engine-wasm --lib -- --ignored pdf_validate_fixture'",
    );
    const build = spawnSync(
        'cargo',
        ['test', '-p', 'engine-wasm', '--lib', '--', '--ignored', 'pdf_validate_fixture', '--test-threads=1'],
        { cwd: REPO, stdio: 'inherit' },
    );
    if (build.status !== 0) {
        console.error('[pdf-validate] FAIL: fixture regeneration did not complete cleanly');
        process.exit(build.status ?? 1);
    }

    if (!checkOnly) {
        console.log(
            `[pdf-validate] regenerated ${corpusRel}. Review with ` +
                `\`git diff --stat -- ${corpusRel}\` and stage intentionally.`,
        );
        process.exit(0);
    }

    /* `git diff HEAD` (a single commit argument) compares the working tree
       directly to HEAD regardless of the index, and — unlike `git status`
       — never reports untracked files: a fixture generator that is new in
       this very change (working tree ahead of HEAD, nothing to compare
       against yet) is not "stale", so it must not fail this gate. Only a
       HEAD-tracked fixture whose regenerated bytes differ counts. */
    const diff = spawnSync('git', ['diff', '--stat', '--exit-code', 'HEAD', '--', corpusRel], {
        cwd: REPO,
        encoding: 'utf8',
    });
    const stale = diff.status !== 0;
    /* This check must never leave the working tree modified, pass or
       fail — `checkout HEAD --` restores every HEAD-tracked path
       regeneration touched; it is a no-op for paths HEAD doesn't have. */
    spawnSync('git', ['checkout', 'HEAD', '--', corpusRel], { cwd: REPO });

    if (stale) {
        console.error(`[pdf-validate] FAIL: ${corpusRel} is stale — regeneration changed:`);
        console.error((diff.stdout ?? '').trim());
        console.error('[pdf-validate] run `node tools/pdf-validate/run.mjs --regen` and commit the result');
        process.exit(1);
    }
    console.log(`[pdf-validate] PASS — ${corpusRel} fixtures are up to date`);
    process.exit(0);
}

const corpus = argValue('--corpus', 'tier-a');
const profile = argValue('--profile', '1b');
const strict = hasFlag('--strict');
const serverUrl = process.env.URL ?? 'http://localhost:5173';

/* Engine `PdfConformance` value per harness profile (issue #28 added the
   PDF/A-2u + PDF/X-3 targets). `veraFlavour` is the veraPDF `--flavour`
   argument; null means veraPDF cannot validate the profile (it is a PDF/A +
   PDF/UA validator — no PDF/X flavour exists) and the structural check is
   the harness's last word. */
const PROFILES = {
    '1b': { conformance: 'A1b', veraFlavour: '1b' },
    '2u': { conformance: 'A2u', veraFlavour: '2u' },
    'x3': { conformance: 'X3', veraFlavour: null },
};
const profileSpec = PROFILES[profile];
if (!profileSpec) {
    console.error(
        `[pdf-validate] unsupported --profile '${profile}'; ` +
            `expected one of: ${Object.keys(PROFILES).join(', ')}`,
    );
    process.exit(2);
}
const { conformance, veraFlavour } = profileSpec;

const OUT_DIR = join(REPO, 'tmp', 'pdf-validate');
mkdirSync(OUT_DIR, { recursive: true });

let chromium;
try {
    ({ chromium } = await import('playwright'));
} catch {
    console.error('[pdf-validate] playwright is not installed.');
    console.error('[pdf-validate] run: pnpm install --dir tools/pdf-validate');
    process.exit(2);
}

/* Build the case list. Each case is a name + an optional .docx to load first;
   a null docx means "export whatever the editor already shows". */
const corpusDir = join(REPO, 'tests', 'corpus', corpus);
const fixtures = existsSync(corpusDir)
    ? readdirSync(corpusDir).filter((f) => f.toLowerCase().endsWith('.docx'))
    : [];
const cases = fixtures.length
    ? fixtures.map((f) => ({ name: basename(f, '.docx'), docx: join(corpusDir, f) }))
    : [{ name: 'seeded-default', docx: null }];
if (!fixtures.length) {
    console.warn(`[pdf-validate] no .docx fixtures in ${corpusDir}`);
    console.warn("[pdf-validate] falling back to the editor's seeded document");
}

console.log(
    `[pdf-validate] profile=${profile} conformance=${conformance} ` +
        `url=${serverUrl} cases=${cases.length}`,
);

/* ---- Export every case through the live engine ----------------------- */

const exported = [];
const browser = await chromium.launch({ headless: true, channel: 'chrome' });
try {
    const page = await (await browser.newContext()).newPage();

    try {
        await page.goto(serverUrl, { waitUntil: 'load', timeout: 20000 });
        await page.waitForFunction(() => window.__paintIdle === true, { timeout: 30000 });
    } catch (e) {
        console.error(`[pdf-validate] cannot reach a ready editor at ${serverUrl}`);
        console.error(`[pdf-validate] is \`pnpm dev\` running? (${e.message})`);
        await browser.close();
        process.exit(2);
    }

    for (const testCase of cases) {
        if (testCase.docx) {
            const docxBytes = Array.from(readFileSync(testCase.docx));
            const loaded = await page.evaluate(async (bytes) => {
                const evt = await window.__dispatch({
                    type: 'LOAD_DOCX',
                    bytes: new Uint8Array(bytes),
                });
                return evt.type;
            }, docxBytes);
            if (loaded === 'ERROR') {
                console.error(`[pdf-validate] ${testCase.name}: LOAD_DOCX failed — skipping`);
                continue;
            }
        }

        const result = await page.evaluate(async (conf) => {
            const evt = await window.__dispatch({ type: 'EXPORT_PDF', conformance: conf });
            if (evt.type !== 'PDF_EXPORTED') {
                return { ok: false, error: evt.type === 'ERROR' ? evt.message : evt.type };
            }
            return { ok: true, bytes: Array.from(evt.bytes), pages: evt.pages };
        }, conformance);

        if (!result.ok) {
            console.error(`[pdf-validate] ${testCase.name}: EXPORT_PDF failed — ${result.error}`);
            continue;
        }

        const pdfPath = join(OUT_DIR, `${testCase.name}.pdf`);
        writeFileSync(pdfPath, Buffer.from(result.bytes));
        exported.push({ name: testCase.name, path: pdfPath, size: result.bytes.length });
        console.log(
            `[pdf-validate] exported ${testCase.name} -> ${pdfPath} ` +
                `(${result.bytes.length} B, ${result.pages}p)`,
        );
    }
} finally {
    await browser.close();
}

if (!exported.length) {
    console.error('[pdf-validate] FAIL: no documents were exported');
    process.exit(1);
}

/* ---- Structural self-check (runs with or without veraPDF) ------------- */

/* The per-profile structures crates/format-pdf must emit. This is a cheap
   sanity gate — it confirms the markers exist; veraPDF (where it supports the
   flavour) is the authority on whether they are correct. */
function structuralMarkers(pdf) {
    const body = pdf.toString('latin1');
    const common = {
        'OutputIntent': body.includes('/OutputIntent'),
        'embedded ICC profile': body.includes('/DestOutputProfile') && body.includes('acsp'),
        'document /ID': body.includes('/ID'),
        'EOF marker': body.trimEnd().endsWith('%%EOF'),
    };
    /* Issue #121 — image XObjects. PDF/A-1b (ISO 19005-1 §6.4) and
       PDF/X-3:2003 forbid transparency, so format-pdf flattens PNG alpha
       onto white there and must never emit an /SMask; PDF/A-1 also bans
       JPEG 2000 (/JPXDecode). Vacuously true for image-free documents. */
    const noTransparency = {
        'no image /SMask (transparency)': !body.includes('/SMask'),
    };
    switch (profile) {
        case '1b':
            return {
                'PDF 1.4 header': body.startsWith('%PDF-1.4'),
                'GTS_PDFA1 subtype': body.includes('GTS_PDFA1'),
                'XMP pdfaid:part': body.includes('pdfaid:part>1'),
                'XMP pdfaid:conformance': body.includes('pdfaid:conformance>B'),
                'no JPEG 2000 (/JPXDecode)': !body.includes('/JPXDecode'),
                ...noTransparency,
                ...common,
            };
        case '2u':
            return {
                'PDF 1.7 header': body.startsWith('%PDF-1.7'),
                'GTS_PDFA1 subtype': body.includes('GTS_PDFA1'),
                'XMP pdfaid:part': body.includes('pdfaid:part>2'),
                'XMP pdfaid:conformance': body.includes('pdfaid:conformance>U'),
                'ToUnicode CMaps': body.includes('/ToUnicode'),
                ...common,
            };
        case 'x3':
            return {
                'PDF 1.4 header': body.startsWith('%PDF-1.4'),
                'GTS_PDFX subtype': body.includes('/S /GTS_PDFX'),
                'GTS_PDFXVersion': body.includes('PDF/X-3:2003'),
                'Info /Title': body.includes('/Title'),
                'Info /Trapped': body.includes('/Trapped /False'),
                'Info dates': body.includes('/CreationDate') && body.includes('/ModDate'),
                'per-page TrimBox': body.includes('/TrimBox'),
                ...noTransparency,
                ...common,
            };
        /* Unreachable — PROFILES gates the flag upfront. */
        default:
            return common;
    }
}

let structuralOk = true;
for (const doc of exported) {
    const markers = structuralMarkers(readFileSync(doc.path));
    const missing = Object.entries(markers)
        .filter(([, present]) => !present)
        .map(([name]) => name);
    if (missing.length) {
        structuralOk = false;
        console.error(`[pdf-validate] ${doc.name}: structural FAIL — missing: ${missing.join(', ')}`);
    } else {
        console.log(
            `[pdf-validate] ${doc.name}: structural PASS ` +
                `(${Object.keys(markers).length}/${Object.keys(markers).length} '${profile}' markers)`,
        );
    }
}

/* ---- External veraPDF validation ------------------------------------- */

function findVeraPdf() {
    for (const bin of ['verapdf', 'veraPDF']) {
        const probe = spawnSync(bin, ['--version'], { encoding: 'utf8' });
        if (!probe.error) return bin;
    }
    return null;
}

if (veraFlavour === null) {
    console.log(
        `[pdf-validate] veraPDF has no '${profile}' flavour (it validates PDF/A + ` +
            'PDF/UA only) — the structural check above is the final result.',
    );
    process.exit(structuralOk ? 0 : 1);
}

const veraPdf = findVeraPdf();
if (!veraPdf) {
    console.warn('[pdf-validate] veraPDF not found on PATH — external validation skipped.');
    console.warn(`[pdf-validate] install veraPDF to validate; PDFs are in ${OUT_DIR}`);
    if (strict) {
        console.error('[pdf-validate] FAIL: --strict set and veraPDF is unavailable');
        process.exit(1);
    }
    process.exit(structuralOk ? 0 : 1);
}

console.log(`[pdf-validate] validating with ${veraPdf} --flavour ${veraFlavour}`);
let veraOk = true;
for (const doc of exported) {
    const run = spawnSync(
        veraPdf,
        ['--flavour', veraFlavour, '--format', 'text', doc.path],
        { encoding: 'utf8' },
    );
    const report = `${run.stdout ?? ''}${run.stderr ?? ''}`.trim();
    const pass = run.status === 0 && !/\bnon-compliant\b/i.test(report);
    if (!pass) veraOk = false;
    console.log(`[pdf-validate] ${doc.name}: veraPDF ${pass ? 'PASS' : 'FAIL'}`);
    if (report) {
        console.log(report.split('\n').map((line) => `    ${line}`).join('\n'));
    }
}

if (!structuralOk || !veraOk) {
    console.error('[pdf-validate] FAIL');
    process.exit(1);
}
console.log(`[pdf-validate] PASS — ${exported.length} document(s) conform to PDF/A-${profile}`);
process.exit(0);
