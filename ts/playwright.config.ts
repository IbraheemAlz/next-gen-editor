import { defineConfig } from '@playwright/test';
import { fileURLToPath } from 'node:url';
import { join, dirname } from 'node:path';

/* Phase 2 exit-gate e2e suite (PHASE_2_BRIDGE_MEMORY.md §11).
 *
 * Runs against the Vite dev server, which serves the COOP/COEP headers that
 * SharedArrayBuffer + cross-origin isolation require. Uses the system Chrome
 * (`channel: 'chrome'`) so no Playwright browser download is needed —
 * consistent with tools/visual-diff. */

const HERE = dirname(fileURLToPath(import.meta.url));
/* Issue #86 — D5.7 real telemetry: the tiny Node receiver `telemetry.spec.ts`
 * points `?telemetryEndpoint=` at. */
const TELEMETRY_SINK_ENTRY = join(HERE, '..', 'tools', 'telemetry-sink', 'run.mjs');
const TELEMETRY_SINK_PORT = 4319;
const TELEMETRY_SINK_OUT = join(HERE, '..', 'tools', 'telemetry-sink', '.e2e-received.jsonl');

export default defineConfig({
    testDir: './e2e',
    timeout: 30_000,
    /* Serial: the throughput + boot tests are timing-sensitive. */
    workers: 1,
    reporter: 'list',
    use: {
        baseURL: 'http://localhost:5173',
        channel: 'chrome',
        headless: true,
    },
    webServer: [
        {
            command: 'pnpm dev',
            url: 'http://localhost:5173',
            reuseExistingServer: true,
            timeout: 60_000,
        },
        /* Issue #86 — D5.7 real telemetry: `telemetry.spec.ts` points the
           collector at this sink via `?telemetryEndpoint=` and asserts
           against its `/received` list. */
        {
            command: `node ${TELEMETRY_SINK_ENTRY} --port ${TELEMETRY_SINK_PORT} --out ${TELEMETRY_SINK_OUT}`,
            url: `http://localhost:${TELEMETRY_SINK_PORT}/health`,
            reuseExistingServer: true,
            timeout: 30_000,
        },
    ],
});
