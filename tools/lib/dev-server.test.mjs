import { createServer } from 'node:http';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { afterEach, describe, expect, it } from 'vitest';
import { REPO_ROOT, ensureDevServer, ensureDevServerOrExit, probeServer } from './dev-server.mjs';
import { devServerPort } from './port.mjs';

const FAKE = join(dirname(fileURLToPath(import.meta.url)), 'test-fixtures', 'fake-dev-server.mjs');
const quiet = () => {};

/** A port nothing is listening on (bind to 0, read it, release it). */
async function freePort() {
    const s = createServer();
    await new Promise((r) => s.listen(0, 'localhost', r));
    const { port } = s.address();
    await new Promise((r) => s.close(r));
    return port;
}

const cleanups = [];
afterEach(async () => {
    while (cleanups.length) await cleanups.pop()();
});

describe('probeServer', () => {
    it('reports a refused connection as down', async () => {
        const port = await freePort();
        expect(await probeServer(`http://localhost:${port}`, { timeoutMs: 500 })).toEqual({
            up: false,
        });
    });

    it('reports status and the COOP header of whatever answers', async () => {
        const s = createServer((_req, res) => {
            res.writeHead(404, { 'Cross-Origin-Opener-Policy': 'same-origin' });
            res.end();
        });
        await new Promise((r) => s.listen(0, 'localhost', r));
        cleanups.push(() => new Promise((r) => s.close(r)));
        const { port } = s.address();
        expect(await probeServer(`http://localhost:${port}`)).toEqual({
            up: true,
            status: 404,
            coop: 'same-origin',
        });
    });
});

describe('ensureDevServer', () => {
    it('reuses a server that already answers and never spawns', async () => {
        const s = createServer((_req, res) => {
            res.writeHead(200, { 'Cross-Origin-Opener-Policy': 'same-origin' });
            res.end('ok');
        });
        await new Promise((r) => s.listen(0, 'localhost', r));
        cleanups.push(() => new Promise((r) => s.close(r)));
        const { port } = s.address();
        const lines = [];
        const server = await ensureDevServer({
            env: { URL: `http://localhost:${port}/` },
            command: '/nonexistent/would-fail-if-spawned',
            log: (l) => lines.push(l),
        });
        expect(server).toMatchObject({
            origin: `http://localhost:${port}`,
            port,
            source: 'URL',
            booted: false,
            pid: null,
        });
        expect(server.describe()).toContain(`http://localhost:${port}`);
        expect(server.describe()).toContain('reused');
        expect(lines.join('\n')).toContain('reusing');
        expect(lines.join('\n')).not.toContain('WARNING');
        await server.stop(); // a no-op, must not throw
    });

    it('warns when the answering server is not the editor dev server (no COOP)', async () => {
        const s = createServer((_req, res) => res.end('not vite'));
        await new Promise((r) => s.listen(0, 'localhost', r));
        cleanups.push(() => new Promise((r) => s.close(r)));
        const { port } = s.address();
        const lines = [];
        await ensureDevServer({
            env: { URL: `http://localhost:${port}` },
            log: (l) => lines.push(l),
        });
        expect(lines.join('\n')).toMatch(/WARNING.*Cross-Origin-Opener-Policy/);
    });

    it('boots when nothing answers, names the origin, and stop() takes it down', async () => {
        const port = await freePort();
        const lines = [];
        const server = await ensureDevServer({
            env: { PW_PORT: String(port) },
            command: process.execPath,
            args: (p) => [FAKE, String(p)],
            cwd: dirname(FAKE),
            log: (l) => lines.push(l),
        });
        cleanups.push(() => server.stop());
        expect(server).toMatchObject({
            origin: `http://localhost:${port}`,
            port,
            source: 'PW_PORT',
            booted: true,
            coop: 'same-origin',
        });
        expect(server.pid).toBeGreaterThan(0);
        expect(server.describe()).toContain('booted by this run');
        expect((await probeServer(server.origin)).up).toBe(true);
        expect(lines.join('\n')).toContain('nothing answers');

        await server.stop();
        expect((await probeServer(server.origin, { timeoutMs: 500 })).up).toBe(false);
    });

    it('a second call reuses the server the first one booted', async () => {
        const port = await freePort();
        const env = { PW_PORT: String(port) };
        const first = await ensureDevServer({
            env,
            command: process.execPath,
            args: (p) => [FAKE, String(p)],
            log: quiet,
        });
        cleanups.push(() => first.stop());
        const second = await ensureDevServer({
            env,
            command: '/nonexistent/would-fail-if-spawned',
            log: quiet,
        });
        expect(second.booted).toBe(false);
        expect(second.origin).toBe(first.origin);
    });

    it('defaults to the path-derived port of the repo root', async () => {
        /* Not booted here (no server): just prove the resolution. The fake
           root's derived port is occupied by nothing in the 5200-5999 range
           we use, so the call must reach the boot attempt and fail on the
           bogus command - naming that port. */
        let root = '';
        let expected = 0;
        for (let i = 0; ; i++) {
            root = `/definitely/not/a/checkout/for-the-test-${i}`;
            expected = devServerPort(root, {});
            /* skip a root whose derived port a sibling checkout's server holds */
            if (!(await probeServer(`http://localhost:${expected}`, { timeoutMs: 500 })).up) break;
        }
        await expect(
            ensureDevServer({
                repoRoot: root,
                env: {},
                command: '/nonexistent/boot-command',
                cwd: dirname(FAKE),
                log: quiet,
            }),
        ).rejects.toThrow(new RegExp(`localhost:${expected}.*cannot run`, 's'));
        expect(REPO_ROOT).toBe(join(dirname(fileURLToPath(import.meta.url)), '..', '..'));
    });

    it('a boot that exits early fails with the exit code and the process output', async () => {
        const port = await freePort();
        await expect(
            ensureDevServer({
                env: { PW_PORT: String(port) },
                command: process.execPath,
                args: (p) => [FAKE, String(p), '--exit', '3'],
                log: quiet,
            }),
        ).rejects.toThrow(/exited \(code 3.*\) before answering[\s\S]*boot failure on port/);
    });

    it('a boot that never answers times out, kills the process, and says so', async () => {
        const port = await freePort();
        await expect(
            ensureDevServer({
                env: { PW_PORT: String(port) },
                command: process.execPath,
                /* a process that stays alive but never listens */
                args: () => ['-e', 'setInterval(() => {}, 1000)'],
                bootTimeoutMs: 1500,
                log: quiet,
            }),
        ).rejects.toThrow(/no answer within 2 s|no answer within 1 s/);
    });

    it('refuses to boot for a URL that is not a local http origin with a port', async () => {
        await expect(
            ensureDevServer({
                /* connection refused instantly: nothing listens on the discard port */
                env: { URL: 'https://127.0.0.1:9' },
                command: '/nonexistent/would-fail-if-spawned',
                log: quiet,
            }),
        ).rejects.toThrow(/not auto-booted/);
    });

    it('ensureDevServerOrExit prints one readable line and exits 2 instead of throwing', async () => {
        const lines = [];
        const exits = [];
        const port = await freePort();
        await ensureDevServerOrExit({
            label: 'perf',
            env: { PW_PORT: String(port) },
            command: '/nonexistent/boot-command',
            cwd: dirname(FAKE),
            log: quiet,
            error: (l) => lines.push(l),
            exit: (code) => {
                exits.push(code);
                return undefined;
            },
        });
        expect(exits).toEqual([2]);
        expect(lines[0]).toMatch(/^\[perf\] no editor dev server: could not boot the dev server at/);
    });

    it('an explicit local URL with nothing behind it is booted at exactly that port', async () => {
        const port = await freePort();
        const server = await ensureDevServer({
            env: { URL: `http://localhost:${port}` },
            command: process.execPath,
            args: (p) => [FAKE, String(p)],
            log: quiet,
        });
        cleanups.push(() => server.stop());
        expect(server).toMatchObject({ origin: `http://localhost:${port}`, source: 'URL', booted: true });
    });
});
