import { readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';
import {
    DEV_SERVER_PORT_RANGE,
    TELEMETRY_SINK_PORT_RANGE,
    devServerPort,
    parsePort,
    resolveOrigin,
    stablePortFor,
    telemetrySinkPort,
} from './port.mjs';

const MAIN = '/data/code/next-gen-editor';
const WORKTREE = '/data/code/next-gen-editor/.claude/worktrees/agent-ab5b421a57d60e9eb';
const CI = '/home/ci/work/next-gen-editor';

describe('stablePortFor / devServerPort / telemetrySinkPort (issue #205)', () => {
    /* PINNED: these are the values the pre-#470 `ts/playwright.config.ts`
       produced for these checkout paths. Every consumer derives its port
       independently from this one function, so a change here strands every
       running server - update the numbers only with a deliberate decision. */
    it.each([
        [MAIN, 5842, 4867],
        [WORKTREE, 5397, 4836],
        [CI, 5739, 4334],
    ])('%s -> dev %i, telemetry sink %i', (root, dev, sink) => {
        expect(devServerPort(root, {})).toBe(dev);
        expect(telemetrySinkPort(root)).toBe(sink);
    });

    it('is deterministic and path-sensitive', () => {
        expect(devServerPort(MAIN, {})).toBe(devServerPort(MAIN, {}));
        expect(devServerPort(MAIN, {})).not.toBe(devServerPort(WORKTREE, {}));
    });

    it('always lands inside its range, and the two ranges are disjoint', () => {
        const dev = DEV_SERVER_PORT_RANGE;
        const sink = TELEMETRY_SINK_PORT_RANGE;
        expect(sink.start + sink.size).toBeLessThanOrEqual(dev.start);
        expect(dev.start + dev.size).toBeLessThanOrEqual(65536);
        for (let i = 0; i < 500; i++) {
            const root = `/work/checkout-${i}`;
            const d = devServerPort(root, {});
            const s = telemetrySinkPort(root);
            expect(d).toBeGreaterThanOrEqual(dev.start);
            expect(d).toBeLessThan(dev.start + dev.size);
            expect(s).toBeGreaterThanOrEqual(sink.start);
            expect(s).toBeLessThan(sink.start + sink.size);
            expect(d).not.toBe(s);
        }
    });

    it('stablePortFor is rangeStart + sha256(seed)[0..4] mod size', () => {
        expect(stablePortFor('', 1000, 1)).toBe(1000);
        expect(stablePortFor('anything', 7000, 1)).toBe(7000);
    });

    it('PW_PORT overrides the derivation; an empty PW_PORT does not', () => {
        expect(devServerPort(WORKTREE, { PW_PORT: '6123' })).toBe(6123);
        expect(devServerPort(WORKTREE, { PW_PORT: '' })).toBe(5397);
        /* the sink port is never overridden by PW_PORT */
        expect(telemetrySinkPort(WORKTREE)).toBe(4836);
    });

    it.each(['abc', '0', '65536', '12.5', '-3', '5 173x'])(
        'rejects the invalid PW_PORT %j instead of yielding NaN',
        (bad) => {
            expect(() => devServerPort(WORKTREE, { PW_PORT: bad })).toThrow(/PW_PORT/);
        },
    );

    it('parsePort accepts the full port range', () => {
        expect(parsePort('1', 'X')).toBe(1);
        expect(parsePort(' 65535 ', 'X')).toBe(65535);
    });
});

describe('resolveOrigin (issue #470)', () => {
    it('defaults to the path-derived port, bootable', () => {
        expect(resolveOrigin({ repoRoot: WORKTREE, env: {} })).toEqual({
            origin: 'http://localhost:5397',
            port: 5397,
            source: 'derived',
            bootable: true,
        });
    });

    it('never falls back to a fixed 5173', () => {
        for (const root of [MAIN, WORKTREE, CI]) {
            expect(resolveOrigin({ repoRoot: root, env: {} }).port).not.toBe(5173);
        }
    });

    it('PW_PORT beats the hash', () => {
        expect(resolveOrigin({ repoRoot: WORKTREE, env: { PW_PORT: '5871' } })).toEqual({
            origin: 'http://localhost:5871',
            port: 5871,
            source: 'PW_PORT',
            bootable: true,
        });
    });

    it('URL beats PW_PORT and the hash', () => {
        const r = resolveOrigin({
            repoRoot: WORKTREE,
            env: { URL: 'http://localhost:5173', PW_PORT: '5871' },
        });
        expect(r).toMatchObject({ origin: 'http://localhost:5173', port: 5173, source: 'URL' });
        expect(r.bootable).toBe(true);
    });

    it('normalises a trailing slash, a query and a hash', () => {
        for (const url of [
            'http://localhost:5173/',
            'http://localhost:5173//',
            'http://localhost:5173/?test=glyph-a',
            'http://localhost:5173/#x',
        ]) {
            expect(resolveOrigin({ repoRoot: MAIN, env: { URL: url } }).origin).toBe(
                'http://localhost:5173',
            );
        }
    });

    it('keeps a path prefix (a site served under a subpath)', () => {
        expect(
            resolveOrigin({ repoRoot: MAIN, env: { URL: 'https://x.github.io/next-gen-editor/' } })
                .origin,
        ).toBe('https://x.github.io/next-gen-editor');
    });

    it('only a local http URL with an explicit port may be auto-booted', () => {
        const b = (url) => resolveOrigin({ repoRoot: MAIN, env: { URL: url } }).bootable;
        expect(b('http://localhost:5871')).toBe(true);
        expect(b('http://127.0.0.1:5871')).toBe(true);
        expect(b('http://localhost')).toBe(false); // port 80: not ours to boot
        expect(b('https://localhost:5871')).toBe(false);
        expect(b('http://staging.example.com:5871')).toBe(false);
    });

    it('rejects a URL that is not an absolute http(s) URL', () => {
        expect(() => resolveOrigin({ repoRoot: MAIN, env: { URL: 'localhost:5173' } })).toThrow(/URL/);
        expect(() => resolveOrigin({ repoRoot: MAIN, env: { URL: 'not a url' } })).toThrow(/URL/);
        expect(() => resolveOrigin({ repoRoot: MAIN, env: { URL: 'file:///etc/passwd' } })).toThrow(
            /URL/,
        );
    });

    it('an empty URL is treated as unset', () => {
        expect(resolveOrigin({ repoRoot: WORKTREE, env: { URL: '' } }).source).toBe('derived');
    });
});

describe('no consumer re-forks the derivation', () => {
    const TS_DIR = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..', 'ts');
    it.each(['playwright.config.ts', 'e2e/telemetry.spec.ts', 'e2e/open-warnings.spec.ts'])(
        '%s imports tools/lib/port.mjs and has no private hash',
        (rel) => {
            const src = readFileSync(join(TS_DIR, rel), 'utf8');
            expect(src).toMatch(/tools\/lib\/port\.mjs/);
            expect(src).not.toMatch(/createHash/);
            expect(src).not.toMatch(/function stablePortFor/);
        },
    );
});
