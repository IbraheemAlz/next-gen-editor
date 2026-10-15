/**
 * Per-checkout port derivation + dev-server origin resolution (issues #205,
 * #470).
 *
 * ONE definition, shared by `ts/playwright.config.ts`, the e2e specs that need
 * the telemetry sink's port, and every `tools/*` harness that drives the
 * editor in a browser (`perf`, `memory-profile`, `pdf-validate`,
 * `visual-diff`). Pure and side-effect free (no I/O) so a Playwright config
 * can import it; the booting half lives in `./dev-server.mjs`.
 *
 * Why a derived port (issue #205): several worktrees of this repo, plus the
 * main checkout, run Vite side by side. A fixed port let one tree's
 * already-running server answer for another, so specs and harnesses measured
 * the wrong code. The port is a stable hash of the checkout's absolute path,
 * so every checkout lands on its own port. If you change the algorithm, the
 * salt or a range, `port.test.mjs` pins the old values and fails on purpose:
 * every consumer derives the port independently from this one function, so a
 * silent change would strand a running server on the old port.
 */
import { createHash } from 'node:crypto';

/** 5200-5999: clear of the well-known range and of Vite's fixed 5173/4173. */
export const DEV_SERVER_PORT_RANGE = Object.freeze({ start: 5200, size: 800 });
/** 4200-4999: disjoint from the dev-server range, so the two never collide. */
export const TELEMETRY_SINK_PORT_RANGE = Object.freeze({ start: 4200, size: 800 });

/** `rangeStart + (sha256(seed)[0..4] as big-endian u32) % rangeSize`. */
export function stablePortFor(seed, rangeStart, rangeSize) {
    const digest = createHash('sha256').update(seed).digest();
    return rangeStart + (digest.readUInt32BE(0) % rangeSize);
}

/** Parse an explicit port override; throws a precise error instead of
 *  silently producing `NaN` (which `vite --port NaN` turns into a random
 *  port). */
export function parsePort(value, label) {
    const text = String(value).trim();
    const port = /^\d+$/.test(text) ? Number.parseInt(text, 10) : Number.NaN;
    if (!Number.isInteger(port) || port < 1 || port > 65535) {
        throw new Error(`${label} must be an integer in 1-65535, got '${value}'`);
    }
    return port;
}

/** The checkout's editor dev-server port: `PW_PORT` when set (and
 *  non-empty), else the path-derived one. */
export function devServerPort(repoRoot, env = process.env) {
    if (env.PW_PORT) return parsePort(env.PW_PORT, 'PW_PORT');
    return stablePortFor(repoRoot, DEV_SERVER_PORT_RANGE.start, DEV_SERVER_PORT_RANGE.size);
}

/** The checkout's telemetry-sink port (`tools/telemetry-sink`). */
export function telemetrySinkPort(repoRoot) {
    return stablePortFor(
        `${repoRoot}::telemetry-sink`,
        TELEMETRY_SINK_PORT_RANGE.start,
        TELEMETRY_SINK_PORT_RANGE.size,
    );
}

const LOCAL_HOSTS = new Set(['localhost', '127.0.0.1', '[::1]']);

/**
 * Where a harness should point its browser.
 *
 * Precedence: `URL` (an explicit origin; a path suffix is kept, a query or
 * hash is dropped, so `http://localhost:5173/?test=x` still works) >
 * `PW_PORT` > the path-derived port. `bootable` says whether a harness may
 * start Vite itself if nothing answers there: only for a local `http://`
 * origin with an explicit port, because a remote or port-less URL is
 * somebody else's server and booting one on it would be a lie.
 *
 * @returns {{ origin: string, port: number, source: 'URL'|'PW_PORT'|'derived', bootable: boolean }}
 */
export function resolveOrigin({ repoRoot, env = process.env }) {
    if (env.URL) {
        let url;
        try {
            url = new URL(env.URL);
        } catch {
            throw new Error(`URL must be an absolute http(s) URL, got '${env.URL}'`);
        }
        if (url.protocol !== 'http:' && url.protocol !== 'https:') {
            throw new Error(`URL must be an absolute http(s) URL, got '${env.URL}'`);
        }
        const path = url.pathname.replace(/\/+$/, '');
        const port =
            url.port !== '' ? Number.parseInt(url.port, 10) : url.protocol === 'https:' ? 443 : 80;
        return {
            origin: `${url.protocol}//${url.host}${path}`,
            port,
            source: 'URL',
            bootable:
                url.protocol === 'http:' && url.port !== '' && LOCAL_HOSTS.has(url.hostname),
        };
    }
    const port = devServerPort(repoRoot, env);
    return {
        origin: `http://localhost:${port}`,
        port,
        source: env.PW_PORT ? 'PW_PORT' : 'derived',
        bootable: true,
    };
}
