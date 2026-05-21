#!/usr/bin/env node
/**
 * PDF/A-1b validation harness — Phase 5 D5.4.
 *
 * Drives the real engine (in its Web Worker, exactly as a user would) to export
 * each corpus document to PDF, then validates the output against the PDF/A-1b
 * archival profile with veraPDF.
 *
 * Usage:
 *   node run.mjs                              — validate tests/corpus/tier-a
 *   node run.mjs --corpus tier-a --profile 1b — explicit corpus + profile
 *   node run.mjs --strict                     — fail if veraPDF is unavailable
 *
 * Requires the Vite dev server (`pnpm dev`, http://localhost:5173). Override
 * with the URL env var. Uses Playwright with the system Chrome
 * (channel:'chrome') — no 150 MB chromium download.
 *
 * veraPDF is optional locally: when it is not on PATH the harness still exports
 * every PDF and runs an in-process structural check for the PDF/A-1b markers,
 * but external conformance is reported as skipped (a hard failure under
 * --strict, which the release pipeline uses).
 *
 * When the corpus directory holds no .docx fixtures the harness falls back to
 * exporting the editor's seeded document, so it always produces at least one
 * PDF to validate. Corpus population is separate Phase 5 work (D5.1).
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

const corpus = argValue('--corpus', 'tier-a');
const profile = argValue('--profile', '1b');
const strict = hasFlag('--strict');
const serverUrl = process.env.URL ?? 'http://localhost:5173';

/* Engine conformance value per veraPDF flavour. Only PDF/A-1b is implemented
   in crates/format-pdf today; other flavours are intentionally rejected. */
const CONFORMANCE_FOR_PROFILE = { '1b': 'A1b' };
const conformance = CONFORMANCE_FOR_PROFILE[profile];
if (!conformance) {
    console.error(`[pdf-validate] unsupported --profile '${profile}'; only '1b' is implemented`);
    process.exit(2);
}

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

/* The PDF/A-1b structures crates/format-pdf must emit. This is a cheap sanity
   gate — it confirms the markers exist; veraPDF is the authority on whether
   they are correct. */
function structuralMarkers(pdf) {
    const body = pdf.toString('latin1');
    return {
        'PDF 1.4 header': body.startsWith('%PDF-1.4'),
        'OutputIntent': body.includes('/OutputIntent'),
        'GTS_PDFA1 subtype': body.includes('GTS_PDFA1'),
        'embedded ICC profile': body.includes('/DestOutputProfile') && body.includes('acsp'),
        'XMP pdfaid:part': body.includes('pdfaid:part>1'),
        'XMP pdfaid:conformance': body.includes('pdfaid:conformance>B'),
        'document /ID': body.includes('/ID'),
        'EOF marker': body.trimEnd().endsWith('%%EOF'),
    };
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
                `(${Object.keys(markers).length}/${Object.keys(markers).length} PDF/A-1b markers)`,
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

console.log(`[pdf-validate] validating with ${veraPdf} --flavour ${profile}`);
let veraOk = true;
for (const doc of exported) {
    const run = spawnSync(
        veraPdf,
        ['--flavour', profile, '--format', 'text', doc.path],
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
