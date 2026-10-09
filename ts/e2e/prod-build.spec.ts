import { test, expect } from '@playwright/test';
import { spawn, execFile, type ChildProcess } from 'node:child_process';
import { mkdtempSync, rmSync } from 'node:fs';
import { createServer } from 'node:net';
import { tmpdir } from 'node:os';
import { join, dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';

const run = promisify(execFile);
const TS_DIR = resolve(dirname(fileURLToPath(import.meta.url)), '..');

/* Issue #340 - the `window.__*` dev hooks and the `?telemetryEndpoint=` URL
   parameter are build-time gated. This spec builds the app the way a
   release does (`vite build`, NO `VITE_NGE_DEV_HOOKS`), serves it with
   `vite preview` on a free port, and proves a production page exposes no
   engine handle and does not let the URL pick a telemetry sink. A second
   build WITH the flag proves the gate is the flag, not an accident. */

function freePort(): Promise<number> {
    return new Promise((resolvePort, reject) => {
        const srv = createServer();
        srv.once('error', reject);
        srv.listen(0, '127.0.0.1', () => {
            const addr = srv.address();
            const port = typeof addr === 'object' && addr ? addr.port : 0;
            srv.close(() => resolvePort(port));
        });
    });
}

function envWithout(...names: string[]): NodeJS.ProcessEnv {
    const env = { ...process.env };
    for (const n of names) delete env[n];
    return env;
}

async function buildAndServe(
    outDir: string,
    env: NodeJS.ProcessEnv,
): Promise<{ url: string; stop: () => void }> {
    await run(
        'pnpm',
        ['exec', 'vite', 'build', '--outDir', outDir, '--emptyOutDir', '--sourcemap', 'false'],
        { cwd: TS_DIR, env, maxBuffer: 64 * 1024 * 1024 },
    );
    const port = await freePort();
    const child: ChildProcess = spawn(
        'pnpm',
        ['exec', 'vite', 'preview', '--outDir', outDir, '--port', String(port), '--strictPort'],
        { cwd: TS_DIR, env, stdio: 'ignore' },
    );
    const url = `http://localhost:${port}/`;
    for (let i = 0; i < 100; i++) {
        try {
            const res = await fetch(url);
            if (res.ok) return { url, stop: () => child.kill('SIGTERM') };
        } catch {
            /* not listening yet */
        }
        await new Promise((r) => setTimeout(r, 200));
    }
    child.kill('SIGTERM');
    throw new Error('vite preview did not start');
}

const HOOKS = [
    '__dispatch',
    '__engineClient',
    '__setTelemetryEnabled',
    '__fontRegistry',
    '__telemetryFlush',
    '__clipboardPrefetch',
] as const;

test('a production build exposes no engine hooks on window (#340)', async ({ page }) => {
    test.setTimeout(240_000);
    const outDir = mkdtempSync(join(tmpdir(), 'nge-prod-build-'));
    const server = await buildAndServe(outDir, envWithout('VITE_NGE_DEV_HOOKS'));
    try {
        const requests: string[] = [];
        page.on('request', (r) => requests.push(r.url()));
        /* The URL tries to choose a telemetry sink: a production page must
           ignore it (and telemetry is opt-in anyway). */
        await page.goto(`${server.url}?telemetryEndpoint=http://127.0.0.1:9/evil`);
        await page.waitForFunction(() => (window as any).__paintIdle === true, undefined, {
            timeout: 60_000,
        });
        const present = await page.evaluate(
            (names: readonly string[]) =>
                names.filter((n) => (window as unknown as Record<string, unknown>)[n] !== undefined),
            HOOKS,
        );
        expect(present, 'dev hooks present on a production page').toEqual([]);
        expect(await page.evaluate(() => (window as any).__dispatch === undefined)).toBe(true);
        expect(requests.filter((u) => u.startsWith('http://127.0.0.1:9'))).toEqual([]);
        /* The app itself works: the shell mounted. */
        await expect(page.locator('.nge-shell')).toBeVisible();
    } finally {
        server.stop();
        rmSync(outDir, { recursive: true, force: true });
    }
});

test('a build made with VITE_NGE_DEV_HOOKS=1 installs the hooks (#340)', async ({ page }) => {
    test.setTimeout(240_000);
    const outDir = mkdtempSync(join(tmpdir(), 'nge-prod-build-hooks-'));
    const server = await buildAndServe(outDir, { ...process.env, VITE_NGE_DEV_HOOKS: '1' });
    try {
        await page.goto(server.url);
        await page.waitForFunction(() => (window as any).__paintIdle === true, undefined, {
            timeout: 60_000,
        });
        const evt = await page.evaluate(
            () => (window as any).__dispatch({ type: 'PING' }) as Promise<{ type: string }>,
        );
        expect(evt.type).toBe('PONG');
        expect(await page.evaluate(() => typeof (window as any).__engineClient)).toBe('object');
        expect(await page.evaluate(() => typeof (window as any).__setTelemetryEnabled)).toBe(
            'function',
        );
    } finally {
        server.stop();
        rmSync(outDir, { recursive: true, force: true });
    }
});
