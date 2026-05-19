# Phase 5 — Defect Hardening, Fidelity Regression & Release Validation

> **Parent:** [`MASTER_PLAN.md`](./MASTER_PLAN.md) §6.
> **Owning tracks:** G (QA), F (DevOps); all engineering tracks on-call for bug fixes.
> **Calendar:** Months 18–24 (continuous, with final 6 months primarily hardening).
> **Exit gate:** §10 = MVP release.

---

## 1. Objective

Convert a feature-complete internal beta into a release-quality MVP:

1. **Visual fidelity** — 200-document corpus passing Tier-A 100 %, Tier-B ≥80 %.
2. **Memory under budget** on 50, 100, 250, 500-page documents.
3. **Performance budgets** hit on Tier-2 hardware (mid-tier Android Chrome, low-spec Windows laptop).
4. **PDF/A-1b conformance** validated externally (veraPDF).
5. **Fuzz harness** clean for 24 h continuous run on `.docx` reader + RPC schema.
6. **Security** review signed off by independent third party.
7. **Telemetry** in place (paint p95, error rate, engine stats).
8. **Release pipeline** — versioned WASM artifact, SBOM, signed releases, staged rollout.

---

## 2. Deliverables

| ID | Deliverable | Acceptance signal |
| --- | --- | --- |
| D5.1 | Playwright visual-diff farm | Daily CI run against full corpus |
| D5.2 | Memory snapshot harness | 50/100/250/500 docs profiled with budgets enforced |
| D5.3 | Performance harness | Cold start, paint p95, scroll FPS gated in CI |
| D5.4 | PDF/A-1b validator integrated | veraPDF gate in release pipeline |
| D5.5 | Fuzz harness (cargo-fuzz) | Nightly run; crashes auto-filed |
| D5.6 | Security audit | External report + remediation tickets resolved |
| D5.7 | Telemetry pipeline | Paint p95 + error rate + engine stats dashboards |
| D5.8 | Release pipeline | Signed WASM + SBOM + GitHub Release |
| D5.9 | Operator runbook | Docs for incident response, rollback, hot-fix |
| D5.10 | Arabic typography sign-off | Native Arabic typographer accepts release |

---

## 3. Visual diff harness

`tools/visual-diff/run.mjs`:

```js
import { chromium } from 'playwright';
import { PNG } from 'pngjs';
import pixelmatch from 'pixelmatch';
import fs from 'node:fs/promises';
import path from 'node:path';

const TIERS = {
    A: { tol: 0.005, threshold: 1.00 },   /* our spec; deterministic */
    B: { tol: 0.020, threshold: 0.80 },   /* LibreOffice parity */
    C: { tol: 0.050, threshold: 0.60 },   /* Word parity, best-effort */
};

async function runCase(browser, fixturePath, goldenPath, opts) {
    const ctx  = await browser.newContext({ viewport: { width: 1280, height: 1024 } });
    const page = await ctx.newPage();
    await page.goto('http://localhost:8080/?ci=1');
    await page.waitForFunction(() => window.__engineReady === true, { timeout: 30000 });
    await page.evaluate(async (p) => window.__loadFixture(p), fixturePath);
    await page.waitForFunction(() => window.__paintIdle === true);

    const actualBuf = await page.locator('.editor-canvas').screenshot();
    const golden    = PNG.sync.read(await fs.readFile(goldenPath));
    const actual    = PNG.sync.read(actualBuf);
    const diff      = new PNG({ width: golden.width, height: golden.height });

    const numDiffPx = pixelmatch(
        golden.data, actual.data, diff.data,
        golden.width, golden.height,
        { threshold: opts.tol },
    );
    const pct = numDiffPx / (golden.width * golden.height);

    if (pct >= opts.tol) {
        await fs.writeFile(`tmp/diffs/${path.basename(fixturePath)}.diff.png`, PNG.sync.write(diff));
    }

    await ctx.close();
    return pct;
}

async function main() {
    const tier = arg('--tier', 'A');
    const { tol, threshold } = TIERS[tier];
    const fixtures = await fs.readdir(`tests/corpus/tier-${tier.toLowerCase()}`);
    const browser  = await chromium.launch();
    let passes = 0;

    for (const f of fixtures) {
        const fp = `tests/corpus/tier-${tier.toLowerCase()}/${f}`;
        const gp = `tests/corpus/goldens/tier-${tier.toLowerCase()}/${f.replace(/\.docx$/, '.png')}`;
        const pct = await runCase(browser, fp, gp, { tol });
        console.log(`${f}: diff=${(pct*100).toFixed(3)}%`);
        if (pct < tol) passes++;
    }

    const rate = passes / fixtures.length;
    console.log(`Tier-${tier} pass rate: ${(rate*100).toFixed(2)}% (threshold ${(threshold*100).toFixed(0)}%)`);
    if (rate < threshold) process.exit(1);
    await browser.close();
}

main();
```

Golden capture script (regenerate goldens after intentional renderer changes):

```bash
node tools/visual-diff/capture.mjs --corpus tier-a --out tests/corpus/goldens/tier-a
git diff tests/corpus/goldens/tier-a/                  # review every pixel change
```

---

## 4. Document corpus

200 documents across three tiers. Per-doc manifest entry:

```json
{
    "id": "ar-mixed-001",
    "tier": "A",
    "features": ["bidi", "kashida", "table", "footnote"],
    "lang": ["ar", "en"],
    "pages": 5,
    "kashida_required": true,
    "source": "synthetic",
    "notes": "Mixed paragraph alignment to validate BiDi cursor at LTR/RTL boundary"
}
```

Layout:

```
tests/corpus/
├── tier-a/             (50 docs;  our spec)
├── tier-b/             (100 docs; LibreOffice parity)
├── tier-c/             (50 docs;  Word parity best-effort)
├── manifest.json
└── goldens/
    ├── tier-a/         (.png per fixture page)
    ├── tier-b/
    └── tier-c/
```

Golden refresh policy:

- Renderer change must update goldens.
- PR reviewer eyeballs every pixel change.
- Auto-generated goldens never merged without human inspection.

---

## 5. Memory snapshot suite

`tools/memory-profile/run.mjs`:

```js
import { chromium } from 'playwright';

const BUDGETS = {
    '50p':  { engine: 128*1024*1024, jsHeap: 64*1024*1024 },
    '100p': { engine: 192*1024*1024, jsHeap: 96*1024*1024 },
    '250p': { engine: 384*1024*1024, jsHeap: 128*1024*1024 },
    '500p': { engine: 640*1024*1024, jsHeap: 192*1024*1024 },
};

async function profile(label, fixture) {
    const browser = await chromium.launch({
        args: ['--enable-precise-memory-info', '--js-flags=--expose-gc'],
    });
    const page = await (await browser.newContext()).newPage();
    await page.goto('http://localhost:8080/?ci=1');
    await page.waitForFunction(() => window.__engineReady);
    await page.evaluate(async (p) => window.__loadFixture(p), fixture);
    await page.waitForFunction(() => window.__paintIdle);

    /* Force GC and re-measure */
    await page.evaluate(() => (window as any).gc?.());

    const stats = await page.evaluate(() => window.__engineStats());
    const jsHeap = await page.evaluate(() => (performance as any).memory?.usedJSHeapSize ?? 0);

    const b = BUDGETS[label];
    const ok = stats.wasm_heap_bytes < b.engine && jsHeap < b.jsHeap;
    console.log(JSON.stringify({ label, ok, stats, jsHeap, budget: b }, null, 2));
    if (!ok) process.exit(1);
    await browser.close();
}

for (const label of ['50p', '100p', '250p', '500p']) {
    await profile(label, `tests/perf/${label}.docx`);
}
```

CI invocation:

```bash
node tools/memory-profile/run.mjs --budgets
```

---

## 6. Performance budgets

| Metric | Tier-1 (M2 / desktop) | Tier-2 (mid-tier Android Chrome) |
| --- | --- | --- |
| Cold start to first paint | <3 s | <8 s |
| Open 50-page doc | <1 s | <2.5 s |
| Insert char @ caret p95 | <8 ms (120 fps) | <16 ms (60 fps) |
| Scroll 50p sustained | ≥60 fps | ≥30 fps |
| Save `.docx` | <800 ms | <2 s |
| PDF export 50p | <2.5 s | <6 s |
| Undo / Redo p95 | <20 ms | <40 ms |

Measured via Playwright `page.evaluate()` calling `performance.mark/measure` instrumentation already in the engine and UI. Each metric runs as a separate test; budget violations fail CI.

---

## 7. PDF/A-1b validation

Embedded in CI release pipeline:

```bash
# 1. Export each Tier-A doc to PDF
for f in tests/corpus/tier-a/*.docx; do
    node tools/pdf-export/run.mjs --in "$f" --out "tmp/pdf/$(basename "$f" .docx).pdf" --profile 1b
done

# 2. Validate
veraPDF --format text --profile 1b --recurse tmp/pdf/ > tmp/verapdf.txt
grep -q '"validationResult"\s*:\s*\[\]' tmp/verapdf.txt || (cat tmp/verapdf.txt; exit 1)
```

Conformance checklist:

- [x] All fonts embedded + subsetted (Type 0 CIDFonts).
- [x] No transparency.
- [x] No external content references.
- [x] XMP metadata present.
- [x] No JavaScript.
- [x] No encryption.
- [x] Document structure (PDF tags) present for accessibility.

---

## 8. Fuzz harness

`fuzz/Cargo.toml`:

```toml
[package]
name = "engine-fuzz"
edition = "2024"
publish = false

[dependencies]
libfuzzer-sys = "0.4"
format-docx = { path = "../crates/format-docx" }
bridge      = { path = "../crates/bridge" }

[[bin]]
name = "docx_reader"
path = "fuzz_targets/docx_reader.rs"
test = false
doc  = false

[[bin]]
name = "rpc_command"
path = "fuzz_targets/rpc_command.rs"
test = false
doc  = false
```

`fuzz/fuzz_targets/docx_reader.rs`:

```rust
#![no_main]
use libfuzzer_sys::fuzz_target;
use format_docx::Reader;

fuzz_target!(|data: &[u8]| {
    if data.len() < 4 || data.len() > 50 * 1024 * 1024 { return; }
    let _ = Reader::new(data).and_then(|r| r.read_document());
});
```

`fuzz/fuzz_targets/rpc_command.rs`:

```rust
#![no_main]
use libfuzzer_sys::fuzz_target;
use bridge::Command;

fuzz_target!(|data: &[u8]| {
    let _ = serde_json::from_slice::<Command>(data);
});
```

Nightly CI:

```bash
cargo +nightly fuzz run docx_reader -- -max_total_time=86400
cargo +nightly fuzz run rpc_command -- -max_total_time=86400
```

Crashes auto-filed as issues with corpus minimization (`cargo fuzz tmin`).

---

## 9. Security review checklist

External auditor (recommended: Trail of Bits, NCC Group, or Cure53):

- [ ] WASM artifact integrity verified via SRI on the loading script.
- [ ] All RPC `Command` payloads bounds-checked inside engine (no trust of TS callers).
- [ ] `.docx` reader sandboxed: no path traversal in ZIP entries (`zip-slip`); per-entry size cap 50 MB; total uncompressed cap 200 MB; zip-bomb detection.
- [ ] Font loading: per-font size cap 20 MB; format whitelist (TTF / OTF / WOFF2); reject embedded scripts.
- [ ] No `eval`, `Function()`, or `setTimeout(string)` in TS.
- [ ] CSP enforced: `default-src 'self'; worker-src 'self'; connect-src 'self'; img-src 'self' data: blob:; style-src 'self' 'unsafe-inline'; font-src 'self'`.
- [ ] PII not leaked in error messages or telemetry payloads.
- [ ] Clipboard write gated by user gesture.
- [ ] No `unsafe` Rust in `format-docx` or `bridge` crates (other crates: case-by-case).
- [ ] All `Vec::with_capacity` calls from untrusted input clamped.
- [ ] `serde_wasm_bindgen` deserialization limited to 100 MB per call.
- [ ] Audit signed off before MVP launch.

---

## 10. Exit gate (MVP release)

All commands must exit 0; all sign-offs must be documented.

```bash
# 1. Full visual-diff Tier-A 100%
node tools/visual-diff/run.mjs --tier A --threshold 1.00

# 2. Tier-B ≥80%
node tools/visual-diff/run.mjs --tier B --threshold 0.80

# 3. Memory budgets all sizes
node tools/memory-profile/run.mjs --budgets

# 4. Performance Tier-1 + Tier-2
node tools/perf/run.mjs --hardware tier-1 --strict
node tools/perf/run.mjs --hardware tier-2 --strict

# 5. PDF/A-1b across Tier-A
node tools/pdf-validate/run.mjs --corpus tier-a --profile 1b

# 6. Fuzz: 24h clean run (no new crashes)
cargo +nightly fuzz run docx_reader -- -max_total_time=86400
cargo +nightly fuzz run rpc_command -- -max_total_time=86400

# 7. A11y E2E + 3 screen-readers (manual sign-off)
playwright test ts/e2e/a11y.spec.ts
# NVDA, VoiceOver, Orca manual reports filed

# 8. Security audit report received + remediations closed
gh issue list --label security-audit --state open  # must be empty

# 9. Arabic typography sign-off recorded
test -f docs/signoffs/arabic-typography-mvp.md

# 10. Telemetry dashboards green for 7 consecutive days
node tools/telemetry/check.mjs --window 7d
```

Plus durable conditions:

- Zero P0 bugs in tracker for **14 consecutive days**.
- No P1 bugs older than 30 days.
- Release SBOM + signed WASM artifact published to release storage.

---

## 11. Telemetry pipeline

Engine emits (sampled) telemetry events to a self-hosted collector:

```rust
pub struct TelemetryEvent {
    pub doc_id: Anonymized,
    pub event: TelemetryKind,
    pub timestamp_ms: f64,
}

pub enum TelemetryKind {
    PaintTiming { p50: f32, p95: f32, p99: f32 },
    CommandTiming { kind: &'static str, p50: f32, p95: f32 },
    EngineStats(EngineStats),
    Error { code: ErrorCode, recoverable: bool },
    FontFallback { script: Script, requested: String, fallback: String },
}
```

UI batches and POSTs every 60 s to the collector. No PII; document IDs anonymized; opt-in by default with admin override.

Dashboards (Grafana or self-hosted equivalent):

- Paint p95 / p99 by browser × OS.
- Error rate by `ErrorCode` over time.
- Memory utilization distribution.
- Font fallback frequency (signals missing font packages).
- Crash recovery invocations.

---

## 12. Release pipeline

`.github/workflows/release.yml`:

```yaml
name: release
on:
  push: { tags: [ 'v*' ] }

jobs:
  release:
    runs-on: ubuntu-24.04
    steps:
      - uses: actions/checkout@v6
      - uses: dtolnay/rust-toolchain@stable
        with: { targets: wasm32-unknown-unknown }
      - uses: jetli/wasm-pack-action@v0.4.0
      - run: wasm-pack build --target web --release crates/engine-wasm
      - run: pnpm install --frozen-lockfile
      - run: pnpm build
      - name: SBOM
        run: cargo cyclonedx --format json --output sbom.json
      - name: Sign WASM
        run: cosign sign-blob --yes --output-signature engine.wasm.sig crates/engine-wasm/pkg/engine_wasm_bg.wasm
      - name: Visual diff Tier-A
        run: node tools/visual-diff/run.mjs --tier A --threshold 1.00
      - name: PDF/A-1b
        run: node tools/pdf-validate/run.mjs --corpus tier-a --profile 1b
      - uses: softprops/action-gh-release@v3
        with:
          files: |
            crates/engine-wasm/pkg/engine_wasm_bg.wasm
            engine.wasm.sig
            sbom.json
            dist/**
          generate_release_notes: true
```

Staged rollout:

- **Internal alpha** (0–100 users): tag `v0.1.0-alpha.N` weekly.
- **Closed beta** (100–1 000): tag `v0.1.0-beta.N` biweekly; telemetry on by default.
- **MVP release** (`v0.1.0`): public release after all exit-gate conditions met.

---

## 13. Operator runbook (`docs/RUNBOOK.md` outline)

- Incident severity matrix (P0–P3).
- WASM rollback procedure (serve previous artifact via CDN edge config).
- Telemetry alert thresholds + on-call rotation.
- Hot-fix process (cherry-pick to release branch, re-run pipeline, sign, deploy).
- Font outage handling (missing font event spike).
- "Save failure" user-visible flows + recovery.

---

## 14. Risk register (Phase 5 specific)

| # | Risk | Likelihood | Detection | Mitigation |
| --- | --- | --- | --- | --- |
| 1 | Long-tail OOXML feature regressions during hardening | High | Tier-B diff drop | Quarantine, defer to v1.1; document known-unsupported list |
| 2 | Fuzzer finds critical `.docx` parser issue late | Med | Nightly fuzz | Reserve 4-week buffer pre-release for fuzzer triage |
| 3 | External security audit findings block release | Med | Audit report | Schedule audit at month 20; reserve months 22–24 for remediation |
| 4 | Telemetry reveals unexpected user environments | Med | Beta dashboards | Tier-2 hardware list updated based on real distribution |
| 5 | Memory regression in v0.0.x-beta.N | Med | Memory CI | Bisect via per-commit CI memory snapshots |
| 6 | Arabic typography sign-off withheld | High | Phase-5 review | Iterate Kashida policy + font stack with consultant until accepted |

---

## 15. Definition of "MVP done"

The MVP ships when **all** the following are true, recorded, and dated:

1. Exit gate (§10) passing on green main for 7 consecutive days.
2. Telemetry dashboards green for 7 consecutive days on beta cohort.
3. External security audit report filed; all critical + high remediations closed.
4. Arabic typography sign-off filed at `docs/signoffs/arabic-typography-mvp.md`.
5. Accessibility sign-off filed (NVDA + VoiceOver + Orca + axe-core score ≥95).
6. Operator runbook published.
7. SBOM + signed WASM artifact published as `v0.1.0` GitHub Release.
8. Internal post-mortem on alpha/beta incidents archived.
9. Customer-facing documentation site live (install, usage, RTL guide, troubleshooting).
10. Engineering leadership sign-off in writing.
