import { defineConfig } from '@playwright/test';

/* Phase 2 exit-gate e2e suite (PHASE_2_BRIDGE_MEMORY.md §11).
 *
 * Runs against the Vite dev server, which serves the COOP/COEP headers that
 * SharedArrayBuffer + cross-origin isolation require. Uses the system Chrome
 * (`channel: 'chrome'`) so no Playwright browser download is needed —
 * consistent with tools/visual-diff. */
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
    webServer: {
        command: 'pnpm dev',
        url: 'http://localhost:5173',
        reuseExistingServer: true,
        timeout: 60_000,
    },
});
