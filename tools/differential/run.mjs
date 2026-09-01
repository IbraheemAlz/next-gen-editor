#!/usr/bin/env node
/**
 * Issue #89 — differential oracle harness.
 *
 * Compares our engine's PDF export (`differential-native`, native
 * `read_docx` → layout → `format-pdf`, no wasm/browser) against LibreOffice
 * headless as a black-box baseline for pagination + line-break + rendering.
 * Word 365 is the intended second oracle (see `README.md` / `_manifest.json`
 * in each fixture directory) — every fixture's `word_pdf` is PENDING, no
 * Word available in this environment; the comparison path below already
 * treats an oracle PDF generically, so wiring in a real `word_pdf` later is
 * a manifest change, not a code change (see `wordPdfFor()`).
 *
 * CLEAN-ROOM (plans/cleanroom/PROTOCOL.md): this is the *black-box* channel
 * — it shells out to `soffice`/`pdftotext`/`pdfinfo`/`pdftoppm` and diffs
 * PDF bytes/text/pixels only. It never reads anything under
 * `/data/code/reference/` and never inspects LibreOffice/Word source or
 * internals. See this directory's README.md.
 *
 * Usage:
 *   node run.mjs                        — the Arabic/RTL corpus +
 *                                          crates/format-docx's existing
 *                                          .docx fixtures
 *   node run.mjs <dir-or-file.docx> ...  — explicit corpus paths
 *   node run.mjs --out tmp/differential  — output dir (PDFs/PNGs/report;
 *                                          gitignored, never committed)
 *   node run.mjs --raster-dpi 100        — low-DPI raster compare setting
 *   node run.mjs --json                  — also dump the full ranked report
 *                                          as JSON to stdout
 *
 * Exit code: 0 whenever at least one fixture was compared (this is a
 * *report*, not a pass/fail gate — see the nightly workflow, which is
 * non-blocking). Non-zero only on total infra failure (no soffice, no
 * poppler-utils, the differential-native binary missing, or every single
 * fixture failing to convert on both sides).
 */
import { spawnSync } from 'node:child_process';
import {
    existsSync,
    mkdirSync,
    mkdtempSync,
    readFileSync,
    readdirSync,
    renameSync,
    statSync,
    writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { basename, dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
const REPO = resolve(HERE, '..', '..');

/* ---------------------------------------------------------------- */
/* CLI                                                                */
/* ---------------------------------------------------------------- */

const argv = process.argv.slice(2);
const opts = { out: 'tmp/differential', rasterDpi: 100, json: false };
const positional = [];
for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === '--out') opts.out = argv[++i];
    else if (a === '--raster-dpi') opts.rasterDpi = parseInt(argv[++i], 10);
    else if (a === '--json') opts.json = true;
    else positional.push(a);
}
const OUT_DIR = resolve(REPO, opts.out);
const RASTER_DPI = Number.isFinite(opts.rasterDpi) ? opts.rasterDpi : 100;
mkdirSync(OUT_DIR, { recursive: true });

const DEFAULT_CORPUS_DIRS = [
    join(REPO, 'tools/differential/fixtures/arabic'),
    join(REPO, 'crates/format-docx/tests/fixtures'),
];

function findDocx(paths) {
    const out = [];
    for (const p of paths) {
        if (!existsSync(p)) {
            console.error(`[differential] WARN: corpus path not found: ${p}`);
            continue;
        }
        const st = statSync(p);
        if (st.isDirectory()) {
            for (const f of readdirSync(p)) {
                if (f.endsWith('.docx')) out.push(join(p, f));
            }
        } else if (p.endsWith('.docx')) {
            out.push(p);
        }
    }
    return [...new Set(out)].sort();
}

const corpus = findDocx(positional.length ? positional : DEFAULT_CORPUS_DIRS);
if (corpus.length === 0) {
    console.error('[differential] no .docx fixtures found in the corpus');
    process.exit(1);
}

/* ---------------------------------------------------------------- */
/* Tool discovery                                                     */
/* ---------------------------------------------------------------- */

const CARGO_TARGET_DIR = process.env.CARGO_TARGET_DIR || join(REPO, 'target');
const NATIVE_BIN = join(CARGO_TARGET_DIR, 'release', 'differential-native');
if (!existsSync(NATIVE_BIN)) {
    console.error(
        `[differential] missing ${NATIVE_BIN} — run: cargo build -p differential-native --release`,
    );
    process.exit(1);
}

function haveTool(bin, args) {
    const r = spawnSync(bin, args, { stdio: 'ignore' });
    return !r.error;
}
const haveSoffice = haveTool('soffice', ['--version']);
const havePdftotext = haveTool('pdftotext', ['-v']);
const havePdfinfo = haveTool('pdfinfo', ['-v']);
const havePdftoppm = haveTool('pdftoppm', ['-v']);

if (!haveSoffice) {
    console.error('[differential] soffice not found on PATH — cannot run the LibreOffice oracle');
    process.exit(1);
}
if (!havePdftotext || !havePdfinfo) {
    console.error('[differential] poppler-utils (pdftotext/pdfinfo) not found on PATH');
    process.exit(1);
}
if (!havePdftoppm) {
    console.error(
        '[differential] pdftoppm not found on PATH — raster-similarity comparison will be SKIPPED for every fixture',
    );
}

let pixelmatch, PNG;
if (havePdftoppm) {
    ({ default: pixelmatch } = await import('pixelmatch'));
    ({ PNG } = await import('pngjs'));
}

/* ---------------------------------------------------------------- */
/* Conversion                                                         */
/* ---------------------------------------------------------------- */

function convertOurEngine(docxPath, outPdf) {
    const r = spawnSync(NATIVE_BIN, [docxPath, outPdf], { encoding: 'utf8' });
    if (r.status !== 0 || !existsSync(outPdf)) {
        return { ok: false, error: (r.stderr || r.stdout || 'unknown error').trim() };
    }
    return { ok: true };
}

/** Per-process `-env:UserInstallation` profile so parallel/back-to-back
 * `soffice` invocations never collide on the same user profile lock. */
function convertLibreOffice(docxPath, outDir) {
    const profileDir = mkdtempSync(join(tmpdir(), 'differential-lo-'));
    const r = spawnSync(
        'soffice',
        [
            '--headless',
            '--norestore',
            `-env:UserInstallation=file://${profileDir}`,
            '--convert-to',
            'pdf',
            '--outdir',
            outDir,
            docxPath,
        ],
        { encoding: 'utf8', timeout: 60_000 },
    );
    if (r.status !== 0 || r.error) {
        return { ok: false, error: (r.stderr || r.stdout || String(r.error) || 'unknown error').trim() };
    }
    return { ok: true };
}

/* ---------------------------------------------------------------- */
/* Extraction                                                         */
/* ---------------------------------------------------------------- */

function pdfInfo(pdfPath) {
    const r = spawnSync('pdfinfo', [pdfPath], { encoding: 'utf8' });
    return r.stdout || '';
}

function pdfPageCount(info) {
    const m = /Pages:\s+(\d+)/.exec(info);
    return m ? parseInt(m[1], 10) : 0;
}

/** First page's `Page size: W x H pts (label)` from `pdfinfo`. Resolution-
 * independent (unlike diffing rasterized pixel dimensions, which conflates
 * true page-size differences with DPI rounding) — the right signal for
 * "did the two renderers agree on the sheet size", found to matter during
 * development: without an explicit `<w:pgSz>`, our engine defaults to A4
 * while this harness's LibreOffice defaults to US Letter (its locale). */
function pdfPageSizePt(info) {
    const m = /Page size:\s+([\d.]+) x ([\d.]+) pts/.exec(info);
    return m ? { width: parseFloat(m[1]), height: parseFloat(m[2]) } : null;
}

/** Per-page display-order text (`-layout`, NOT `-bbox`: bbox mode does not
 * apply the visual bidi remap, so RTL words come out character-reversed —
 * confirmed against this harness's own fixtures during development).
 * Bidi control marks (LRM/RLM/ALM) are stripped before comparison; they are
 * an invisible extraction artifact, not a content difference. */
function pdfPagesText(pdfPath, pageCount) {
    const r = spawnSync('pdftotext', ['-layout', pdfPath, '-'], {
        encoding: 'utf8',
        maxBuffer: 64 * 1024 * 1024,
    });
    const raw = r.stdout || '';
    let parts = raw.split('\f');
    if (parts.length > pageCount && pageCount > 0) parts = parts.slice(0, pageCount);
    return parts.map((s) => s.replace(/[‎‏؜]/g, '').trim());
}

/** Per-page `<line>` geometry from `-bbox-layout`. Poppler's `<block>`
 * grouping is NOT used for paragraph detection: on this harness's own PDFs
 * poppler emits one block per line, while on LibreOffice's PDFs it merges
 * a whole paragraph into one block — an asymmetry that would make `<block>`
 * useless for a like-for-like comparison. Line geometry has no such
 * asymmetry (confirmed during development), so paragraph grouping is
 * re-derived uniformly in `groupIntoParagraphs`. */
function pdfBBoxLines(pdfPath) {
    const r = spawnSync('pdftotext', ['-bbox-layout', pdfPath, '-'], {
        encoding: 'utf8',
        maxBuffer: 64 * 1024 * 1024,
    });
    const xml = r.stdout || '';
    const pages = [];
    const pageRe = /<page width="([\d.-]+)" height="([\d.-]+)">([\s\S]*?)<\/page>/g;
    let pm;
    while ((pm = pageRe.exec(xml))) {
        const body = pm[3];
        const lines = [];
        const lineRe = /<line xMin="([\d.-]+)" yMin="([\d.-]+)" xMax="([\d.-]+)" yMax="([\d.-]+)">/g;
        let lm;
        while ((lm = lineRe.exec(body))) {
            lines.push({ xMin: +lm[1], yMin: +lm[2], xMax: +lm[3], yMax: +lm[4] });
        }
        pages.push(lines);
    }
    return pages;
}

/** Group a flat, in-order line sequence into paragraph-like chunks by
 * vertical gap. The fixture corpus stamps every top-level paragraph with
 * `<w:spacing w:after="120"/>` (6pt) specifically so this heuristic has a
 * real, renderer-independent signal to key off — intra-paragraph line
 * leading measured ~0pt on both this harness's own PDFs and LibreOffice's
 * during development, so a small fixed threshold cleanly separates the
 * two. Documents with zero paragraph spacing (most of the pre-existing
 * `crates/format-docx/tests/fixtures/*.docx`) fall back to one paragraph
 * per contiguous page region — reported as a page-level `paragraphCount`
 * of 1, not a crash. */
const PARAGRAPH_GAP_THRESHOLD_PT = 2.0;

function groupIntoParagraphs(lines) {
    if (lines.length === 0) return [];
    const groups = [[lines[0]]];
    for (let i = 1; i < lines.length; i++) {
        const gap = lines[i].yMin - lines[i - 1].yMax;
        if (gap > PARAGRAPH_GAP_THRESHOLD_PT) groups.push([]);
        groups[groups.length - 1].push(lines[i]);
    }
    return groups;
}

/* ---------------------------------------------------------------- */
/* Text similarity — normalized Levenshtein.                         */
/* Exact-match is the wrong bar: known-acceptable extraction noise    */
/* (e.g. a duplicated combining mark around Arabic tanween, observed  */
/* during development) must not drown a real ordering disagreement.   */
/* ---------------------------------------------------------------- */

function levenshtein(a, b) {
    if (a === b) return 0;
    const al = a.length;
    const bl = b.length;
    if (al === 0) return bl;
    if (bl === 0) return al;
    let prev = new Array(bl + 1);
    let cur = new Array(bl + 1);
    for (let j = 0; j <= bl; j++) prev[j] = j;
    for (let i = 1; i <= al; i++) {
        cur[0] = i;
        const ca = a.charCodeAt(i - 1);
        for (let j = 1; j <= bl; j++) {
            const cost = ca === b.charCodeAt(j - 1) ? 0 : 1;
            cur[j] = Math.min(prev[j] + 1, cur[j - 1] + 1, prev[j - 1] + cost);
        }
        [prev, cur] = [cur, prev];
    }
    return prev[bl];
}

function normalizedSimilarity(a, b) {
    const na = a.replace(/\s+/g, ' ').trim();
    const nb = b.replace(/\s+/g, ' ').trim();
    const maxLen = Math.max(na.length, nb.length);
    if (maxLen === 0) return 1;
    return 1 - levenshtein(na, nb) / maxLen;
}

/* ---------------------------------------------------------------- */
/* Raster similarity                                                  */
/* ---------------------------------------------------------------- */

function renderToPng(pdfPath, prefix, dpi) {
    spawnSync('pdftoppm', ['-png', '-r', String(dpi), pdfPath, prefix], { encoding: 'utf8' });
    const dir = dirname(prefix);
    const base = basename(prefix);
    return readdirSync(dir)
        .filter((f) => f.startsWith(`${base}-`) && f.endsWith('.png'))
        .map((f) => ({ f, n: parseInt(f.slice(base.length + 1, -4), 10) || 0 }))
        .sort((a, b) => a.n - b.n)
        .map((x) => join(dir, x.f));
}

function comparePng(pathA, pathB) {
    const a = PNG.sync.read(readFileSync(pathA));
    const b = PNG.sync.read(readFileSync(pathB));
    if (a.width !== b.width || a.height !== b.height) {
        return {
            comparable: false,
            reason: `dimension mismatch ${a.width}x${a.height} vs ${b.width}x${b.height}`,
        };
    }
    const diff = new PNG({ width: a.width, height: a.height });
    const diffPixels = pixelmatch(a.data, b.data, diff.data, a.width, a.height, {
        threshold: 0.15,
    });
    return { comparable: true, diffPct: (diffPixels / (a.width * a.height)) * 100 };
}

/** Cross-renderer raster tolerance bands. These are NOT the tight,
 * same-renderer visual-diff tiers (`.claude/rules/visual-diff.md`) — font
 * substitution + hinting differences between our embedded Amiri/Liberation
 * faces and whatever LibreOffice substitutes are expected "by design" (see
 * the issue's Out-of-scope). Bands are for triage, not pass/fail. */
function rasterBand(pct) {
    if (pct <= 5) return 'close';
    if (pct <= 15) return 'moderate';
    return 'divergent';
}

/* ---------------------------------------------------------------- */
/* Per-fixture comparison                                            */
/* ---------------------------------------------------------------- */

const results = [];
for (const docxPath of corpus) {
    const name = basename(docxPath, '.docx');
    console.log(`\n=== ${name} ===`);

    const ourPdf = join(OUT_DIR, `${name}.ours.pdf`);
    const rOurs = convertOurEngine(docxPath, ourPdf);
    if (!rOurs.ok) {
        console.error(`  our engine FAILED: ${rOurs.error}`);
        results.push({ name, error: `our engine: ${rOurs.error}` });
        continue;
    }

    const rLo = convertLibreOffice(docxPath, OUT_DIR);
    const loDefaultOut = join(OUT_DIR, `${name}.pdf`);
    const loPdf = join(OUT_DIR, `${name}.lo.pdf`);
    if (!rLo.ok || !existsSync(loDefaultOut)) {
        console.error(`  LibreOffice FAILED: ${rLo.error || 'no output produced'}`);
        results.push({ name, error: `libreoffice: ${rLo.error || 'no output produced'}` });
        continue;
    }
    renameSync(loDefaultOut, loPdf);

    const ourInfo = pdfInfo(ourPdf);
    const loInfo = pdfInfo(loPdf);
    const ourPages = pdfPageCount(ourInfo);
    const loPages = pdfPageCount(loInfo);
    const ourPageSizePt = pdfPageSizePt(ourInfo);
    const loPageSizePt = pdfPageSizePt(loInfo);
    const pageSizeMismatch =
        ourPageSizePt != null &&
        loPageSizePt != null &&
        (Math.abs(ourPageSizePt.width - loPageSizePt.width) > 1 ||
            Math.abs(ourPageSizePt.height - loPageSizePt.height) > 1);

    const ourText = pdfPagesText(ourPdf, ourPages);
    const loText = pdfPagesText(loPdf, loPages);
    const textPageCount = Math.min(ourText.length, loText.length);
    let textSimSum = 0;
    for (let i = 0; i < textPageCount; i++) textSimSum += normalizedSimilarity(ourText[i], loText[i]);
    const avgTextSimilarity = textPageCount ? textSimSum / textPageCount : null;

    const ourLines = pdfBBoxLines(ourPdf).flat();
    const loLines = pdfBBoxLines(loPdf).flat();
    const ourParas = groupIntoParagraphs(ourLines);
    const loParas = groupIntoParagraphs(loLines);
    const minParas = Math.min(ourParas.length, loParas.length);
    const paragraphLineBreakDiffs = [];
    for (let i = 0; i < minParas; i++) {
        if (ourParas[i].length !== loParas[i].length) {
            paragraphLineBreakDiffs.push({
                paragraphIndex: i,
                ourLines: ourParas[i].length,
                loLines: loParas[i].length,
            });
        }
    }

    let raster = null;
    if (havePdftoppm) {
        const ourPngs = renderToPng(ourPdf, join(OUT_DIR, `${name}.ours`), RASTER_DPI);
        const loPngs = renderToPng(loPdf, join(OUT_DIR, `${name}.lo`), RASTER_DPI);
        const n = Math.min(ourPngs.length, loPngs.length);
        const perPage = [];
        for (let i = 0; i < n; i++) perPage.push({ page: i + 1, ...comparePng(ourPngs[i], loPngs[i]) });
        const comparablePages = perPage.filter((p) => p.comparable);
        const avgDiffPct = comparablePages.length
            ? comparablePages.reduce((s, p) => s + p.diffPct, 0) / comparablePages.length
            : null;
        raster = { perPage, avgDiffPct, band: avgDiffPct == null ? 'n/a' : rasterBand(avgDiffPct) };
    }

    const record = {
        name,
        ourPages,
        loPages,
        pageCountDelta: ourPages - loPages,
        ourPageSizePt,
        loPageSizePt,
        pageSizeMismatch,
        avgTextSimilarity,
        ourLineCount: ourLines.length,
        loLineCount: loLines.length,
        ourParagraphCount: ourParas.length,
        loParagraphCount: loParas.length,
        paragraphCountDelta: ourParas.length - loParas.length,
        paragraphLineBreakDiffs,
        raster,
    };
    results.push(record);

    console.log(`  pages: ours=${ourPages} lo=${loPages} (delta ${record.pageCountDelta})`);
    if (pageSizeMismatch) {
        console.log(
            `  PAGE SIZE MISMATCH: ours=${ourPageSizePt.width}x${ourPageSizePt.height}pt lo=${loPageSizePt.width}x${loPageSizePt.height}pt`,
        );
    }
    console.log(
        `  paragraphs (gap-heuristic, threshold ${PARAGRAPH_GAP_THRESHOLD_PT}pt): ours=${ourParas.length} lo=${loParas.length} (delta ${record.paragraphCountDelta}); line-count diffs: ${paragraphLineBreakDiffs.length}`,
    );
    console.log(
        `  text similarity (avg over ${textPageCount} page(s)): ${avgTextSimilarity == null ? 'n/a' : avgTextSimilarity.toFixed(4)}`,
    );
    if (raster) {
        console.log(
            `  raster @ ${RASTER_DPI}dpi: avg diff ${raster.avgDiffPct == null ? 'n/a' : `${raster.avgDiffPct.toFixed(2)}%`} (${raster.band})`,
        );
    }
}

/* ---------------------------------------------------------------- */
/* Ranked disagreement report                                        */
/* ---------------------------------------------------------------- */

function severity(r) {
    if (r.error) return 1000;
    let s = 0;
    if (r.pageSizeMismatch) s += 500;
    s += Math.abs(r.pageCountDelta || 0) * 100;
    s += Math.abs(r.paragraphCountDelta || 0) * 20;
    s += (r.paragraphLineBreakDiffs || []).length * 10;
    s += r.avgTextSimilarity != null ? (1 - r.avgTextSimilarity) * 30 : 0;
    s += r.raster?.avgDiffPct != null ? r.raster.avgDiffPct * 0.2 : 0;
    return s;
}

const ranked = [...results].sort((a, b) => severity(b) - severity(a));

console.log('\n=== ranked disagreement report (worst first) ===');
for (const r of ranked) {
    if (r.error) {
        console.log(`  [ERROR] ${r.name}: ${r.error}`);
        continue;
    }
    const textSim = r.avgTextSimilarity == null ? 'n/a' : r.avgTextSimilarity.toFixed(3);
    const rasterPct = r.raster?.avgDiffPct != null ? `${r.raster.avgDiffPct.toFixed(1)}%` : 'n/a';
    const pageSize = r.pageSizeMismatch
        ? ` PAGE-SIZE-MISMATCH(${r.ourPageSizePt?.width}x${r.ourPageSizePt?.height} vs ${r.loPageSizePt?.width}x${r.loPageSizePt?.height}pt)`
        : '';
    console.log(
        `  ${r.name}: pageDelta=${r.pageCountDelta} paraDelta=${r.paragraphCountDelta} ` +
            `lineBreakDiffs=${r.paragraphLineBreakDiffs.length} textSim=${textSim} raster=${rasterPct}${pageSize}`,
    );
}

const report = {
    generatedAt: new Date().toISOString(),
    rasterDpi: RASTER_DPI,
    tools: { soffice: haveSoffice, pdftotext: havePdftotext, pdfinfo: havePdfinfo, pdftoppm: havePdftoppm },
    corpus: corpus.map((p) => relative(REPO, p)),
    results: ranked,
};
const reportPath = join(OUT_DIR, 'report.json');
writeFileSync(reportPath, JSON.stringify(report, null, 2));
console.log(`\nfull report: ${reportPath}`);
if (opts.json) console.log(JSON.stringify(ranked, null, 2));

const successCount = results.filter((r) => !r.error).length;
if (successCount === 0) {
    console.error('\n[differential] every fixture failed to convert on at least one side — infra failure');
    process.exit(1);
}
process.exit(0);
