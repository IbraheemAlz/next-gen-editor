import { defineConfig } from '@playwright/test';
import { fileURLToPath } from 'node:url';
import { join, dirname, resolve } from 'node:path';
import { createHash } from 'node:crypto';

/* Phase 2 exit-gate e2e suite (PHASE_2_BRIDGE_MEMORY.md §11).
 *
 * Runs against the Vite dev server, which serves the COOP/COEP headers that
 * SharedArrayBuffer + cross-origin isolation require. Uses the system Chrome
 * (`channel: 'chrome'`) so no Playwright browser download is needed —
 * consistent with tools/visual-diff. */

const HERE = dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = resolve(HERE, '..');

/* Issue #205 — parallel-worktree port trap. A fixed port 5173 +
 * `reuseExistingServer: true` meant a *different* worktree's (or the main
 * checkout's) already-running Vite server silently satisfied this config's
 * health check, so specs ran against the wrong tree's code and passed/failed
 * for the wrong reason — a false-green hazard for the parallel-agent
 * workflow (CLAUDE.md "Parallel agents in git worktrees").
 *
 * Fix: derive the port from a stable hash of the absolute repo root (+ a
 * salt, so different services never collide with each other even though
 * they hash the same root), so the main checkout and every worktree land on
 * their own port. `PW_PORT` overrides the main dev-server port explicitly
 * (e.g. to pin a port in a script). `reuseExistingServer` now defaults to
 * `false` for every webServer entry below — a stale server from a
 * *previous* run of the same worktree is no longer silently trusted either;
 * set `PW_REUSE_SERVER=1` to opt back into reuse for fast local iteration
 * (this also covers the telemetry sink below, which has the exact same
 * cross-worktree collision risk as the dev server: it is a stateful HTTP
 * service on a fixed port, and `telemetry.spec.ts` mutates that state
 * — `POST /reset` — mid-test, so two worktrees silently sharing one sink
 * process doesn't just misattribute samples, it makes both suites flaky). */
function stablePortFor(seed: string, rangeStart: number, rangeSize: number): number {
    const digest = createHash('sha256').update(seed).digest();
    return rangeStart + (digest.readUInt32BE(0) % rangeSize);
}

// 5200-5999 (800-wide): plenty of headroom for concurrent checkouts, well
// clear of the well-known-port range and of the fixed 5173/4173 Vite
// defaults.
const PORT = process.env.PW_PORT
    ? Number.parseInt(process.env.PW_PORT, 10)
    : stablePortFor(REPO_ROOT, 5200, 800);
const REUSE_SERVER = process.env.PW_REUSE_SERVER === '1';
const BASE_URL = `http://localhost:${PORT}`;

/* Issue #86 — D5.7 real telemetry: the tiny Node receiver `telemetry.spec.ts`
 * points `?telemetryEndpoint=` at. Issue #205 extends the same per-checkout
 * derivation here: a distinct salt (`::telemetry-sink`) on the same
 * REPO_ROOT seed, in a disjoint range (4200-4999) so it can never collide
 * with the dev-server port above even for a checkout path that hashes to
 * the same offset in both ranges. `telemetry.spec.ts` computes this exact
 * same port independently (same algorithm + salt + range, see the comment
 * there) — the two processes have no shared state to pass it through, so
 * the derivation itself is the contract. If you change the algorithm, salt,
 * or range here, change it there too. */
const TELEMETRY_SINK_ENTRY = join(HERE, '..', 'tools', 'telemetry-sink', 'run.mjs');
const TELEMETRY_SINK_PORT = stablePortFor(`${REPO_ROOT}::telemetry-sink`, 4200, 800);
const TELEMETRY_SINK_OUT = join(HERE, '..', 'tools', 'telemetry-sink', '.e2e-received.jsonl');

export default defineConfig({
    testDir: './e2e',
    timeout: 30_000,
    /* Serial: the throughput + boot tests are timing-sensitive. */
    workers: 1,
    reporter: 'list',
    use: {
        baseURL: BASE_URL,
        channel: 'chrome',
        headless: true,
    },
    webServer: [
        {
            /* `pnpm exec vite` (not the `pnpm dev` alias) so the port can be
             * passed on the CLI — `--strictPort` fails fast instead of
             * silently shifting to a free port if this one is somehow
             * still taken (see the `printCheckoutBanner` Vite plugin in
             * `vite.config.ts`, which logs the served absolute path + short
             * commit SHA on every boot so a mismatch is visible at a glance). */
            command: `pnpm exec vite --port ${PORT} --strictPort`,
            url: BASE_URL,
            reuseExistingServer: REUSE_SERVER,
            timeout: 60_000,
        },
        /* Issue #86 — D5.7 real telemetry: `telemetry.spec.ts` points the
           collector at this sink via `?telemetryEndpoint=` and asserts
           against its `/received` list. */
        {
            command: `node ${TELEMETRY_SINK_ENTRY} --port ${TELEMETRY_SINK_PORT} --out ${TELEMETRY_SINK_OUT}`,
            url: `http://localhost:${TELEMETRY_SINK_PORT}/health`,
            reuseExistingServer: REUSE_SERVER,
            timeout: 30_000,
        },
    ],
});
