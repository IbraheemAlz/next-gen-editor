/**
 * Find - or boot - the editor's Vite dev server for a `tools/*` harness
 * (issue #470).
 *
 * The harnesses used to assume `http://localhost:5173`. In a worktree that
 * is wrong twice over: `ts/playwright.config.ts` derives its port from the
 * checkout path (#205), so nothing answered (`ERR_CONNECTION_REFUSED`), or -
 * worse - a SIBLING checkout's server happened to hold 5173 and the harness
 * measured the wrong code. {@link ensureDevServer} resolves the origin the
 * same way Playwright does (`./port.mjs`: `URL` > `PW_PORT` > path hash),
 * reuses whatever already answers there, and otherwise boots
 * `pnpm exec vite --port <port> --strictPort` from `ts/` (the exact command
 * Playwright's `webServer` runs), kills it when the harness ends, and says
 * which of the two happened so the report header can name the measured URL.
 *
 * Only for harnesses that need the dev server (the `?test=` goldens, the
 * `window.__dispatch` hooks). `pdf-validate --native` never calls it.
 */
import { spawn } from 'node:child_process';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { resolveOrigin } from './port.mjs';

const HERE = dirname(fileURLToPath(import.meta.url));
/** The checkout this file lives in - the same string `ts/playwright.config.ts`
 *  hashes (`resolve(<ts dir>, '..')`). */
export const REPO_ROOT = resolve(HERE, '..', '..');

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/**
 * Does anything answer HTTP at `origin`? Any status counts as "up": the
 * question is whether a server holds the port, not whether `/` is healthy.
 * `coop` is the `Cross-Origin-Opener-Policy` header, how the caller tells the
 * editor's own dev server (SAB needs it) from some other service.
 *
 * @returns {Promise<{ up: false } | { up: true, status: number, coop: string | null }>}
 */
export async function probeServer(origin, { timeoutMs = 2000 } = {}) {
    try {
        const res = await fetch(`${origin}/`, {
            signal: AbortSignal.timeout(timeoutMs),
            redirect: 'manual',
        });
        await res.body?.cancel();
        return { up: true, status: res.status, coop: res.headers.get('cross-origin-opener-policy') };
    } catch {
        return { up: false };
    }
}

/**
 * {@link ensureDevServer} for a harness's top level: a failure to find or boot
 * the server prints one readable block and exits 2 (usage / environment
 * error, the code the harnesses already use for "cannot start") instead of an
 * unhandled-rejection stack trace.
 *
 * @param {Parameters<typeof ensureDevServer>[0] & { exit?: (code: number) => never,
 *   error?: (line: string) => void }} [opts]
 */
export async function ensureDevServerOrExit(opts = {}) {
    const { exit = (code) => process.exit(code), error = (line) => console.error(line), ...rest } = opts;
    try {
        return await ensureDevServer(rest);
    } catch (e) {
        error(`[${rest.label ?? 'tools'}] no editor dev server: ${e instanceof Error ? e.message : e}`);
        return exit(2);
    }
}

/**
 * Resolve the dev-server origin, boot Vite there if nothing answers, and
 * return a handle.
 *
 * @param {object} [opts]
 * @param {string} [opts.label]            log prefix, e.g. `perf`
 * @param {string} [opts.repoRoot]         the checkout whose path is hashed
 * @param {Record<string,string|undefined>} [opts.env]
 * @param {string} [opts.command]          boot command (default `pnpm`)
 * @param {(port: number) => string[]} [opts.args]  boot arguments
 * @param {string} [opts.cwd]              boot working directory (default `<repo>/ts`)
 * @param {number} [opts.bootTimeoutMs]    how long to wait for the boot (default 90 s)
 * @param {(line: string) => void} [opts.log]
 * @returns {Promise<{ origin: string, port: number, source: string, booted: boolean,
 *   pid: number | null, coop: string | null, describe: () => string,
 *   stop: () => Promise<void> }>}
 */
export async function ensureDevServer(opts = {}) {
    const {
        label = 'tools',
        repoRoot = REPO_ROOT,
        env = process.env,
        command = 'pnpm',
        args = (port) => ['exec', 'vite', '--port', String(port), '--strictPort'],
        cwd = join(repoRoot, 'ts'),
        bootTimeoutMs = 90_000,
        log = (line) => console.log(line),
    } = opts;

    const target = resolveOrigin({ repoRoot, env });
    const { origin, port, source } = target;
    const sourceNote =
        source === 'URL'
            ? 'from the URL env var'
            : source === 'PW_PORT'
              ? 'from PW_PORT'
              : `derived from ${repoRoot}`;

    const found = await probeServer(origin);
    if (found.up) {
        log(`[${label}] dev server: reusing the server answering at ${origin} (${sourceNote})`);
        if (found.coop !== 'same-origin') {
            log(
                `[${label}] WARNING: ${origin} did not send Cross-Origin-Opener-Policy: same-origin ` +
                    '- it may not be this checkout\'s editor dev server',
            );
        }
        return {
            origin,
            port,
            source,
            booted: false,
            pid: null,
            coop: found.coop,
            describe: () => `${origin} (${sourceNote}; reused a running server)`,
            stop: async () => {},
        };
    }

    if (!target.bootable) {
        throw new Error(
            `nothing answers at ${origin} (${sourceNote}), and it is not a local http URL with ` +
                'an explicit port, so it is not auto-booted - start that server or unset URL',
        );
    }

    const argv = args(port);
    log(
        `[${label}] dev server: nothing answers at ${origin} (${sourceNote}); ` +
            `booting '${[command, ...argv].join(' ')}' in ${cwd}`,
    );
    const started = Date.now();
    const child = spawn(command, argv, {
        cwd,
        env: { ...env, VITE_NGE_DEV_HOOKS: '1' },
        detached: true, // own process group: one signal reaches pnpm AND vite AND esbuild
        stdio: ['ignore', 'pipe', 'pipe'],
    });
    let output = '';
    const keep = (chunk) => {
        output = (output + chunk).slice(-4000);
    };
    child.stdout.on('data', keep);
    child.stderr.on('data', keep);
    let exited = null;
    let spawnError = null;
    const exitPromise = new Promise((resolveExit) => {
        child.once('exit', (code, signal) => {
            exited = { code, signal };
            resolveExit();
        });
    });
    child.once('error', (e) => {
        spawnError = e;
    });

    const killGroup = (signal) => {
        if (child.pid === undefined || exited) return;
        try {
            process.kill(-child.pid, signal);
        } catch {
            /* already gone */
        }
    };

    const onExit = () => killGroup('SIGTERM');
    const signalHandlers = ['SIGINT', 'SIGTERM', 'SIGHUP'].map((sig) => {
        const handler = () => {
            killGroup('SIGTERM');
            /* A harness with its own handler (memory-profile closes its
               browser first) exits itself; only exit here when we are the
               only listener. */
            if (process.listenerCount(sig) <= 1) process.exit(sig === 'SIGINT' ? 130 : 143);
        };
        process.on(sig, handler);
        return [sig, handler];
    });
    process.once('exit', onExit);

    let stopped = false;
    const stop = async () => {
        if (stopped) return;
        stopped = true;
        process.removeListener('exit', onExit);
        for (const [sig, handler] of signalHandlers) process.removeListener(sig, handler);
        if (exited || child.pid === undefined) return; // gone, or never started
        killGroup('SIGTERM');
        await Promise.race([exitPromise, sleep(3000)]);
        killGroup('SIGKILL');
    };

    const fail = async (why) => {
        await stop();
        throw new Error(
            `could not boot the dev server at ${origin}: ${why}\n` +
                `--- last output of '${[command, ...argv].join(' ')}' ---\n${output.trim() || '(none)'}`,
        );
    };

    const deadline = Date.now() + bootTimeoutMs;
    for (;;) {
        if (spawnError) await fail(`cannot run '${command}': ${spawnError.message}`);
        if (exited) {
            await fail(
                `the process exited (code ${exited.code}, signal ${exited.signal}) before answering`,
            );
        }
        const p = await probeServer(origin, { timeoutMs: 1000 });
        if (p.up && !exited) {
            child.unref();
            child.stdout.unref();
            child.stderr.unref();
            log(
                `[${label}] dev server: booted (pid ${child.pid}) at ${origin} ` +
                    `in ${((Date.now() - started) / 1000).toFixed(1)} s`,
            );
            return {
                origin,
                port,
                source,
                booted: true,
                pid: child.pid ?? null,
                coop: p.coop,
                describe: () => `${origin} (${sourceNote}; booted by this run, pid ${child.pid})`,
                stop,
            };
        }
        if (Date.now() >= deadline) {
            await fail(`no answer within ${(bootTimeoutMs / 1000).toFixed(0)} s`);
        }
        await sleep(200);
    }
}
