/* Issue #332 - the #340 / #389 dev-hook gate and the telemetry endpoint
 * precedence. `import.meta.env` is stubbed per test: a production page
 * must never let the URL choose where telemetry goes. */
import { afterEach, describe, expect, it, vi } from 'vitest';
import { devHooksEnabled, installDevHook, resolveTelemetryEndpoint } from './dev-hooks';

/** The environment of a release build: not DEV, no hooks flag, no endpoint. */
function production(): void {
    vi.stubEnv('DEV', false);
    vi.stubEnv('VITE_NGE_DEV_HOOKS', '');
    vi.stubEnv('VITE_NGE_TELEMETRY_ENDPOINT', '');
}
function at(search: string): void {
    vi.stubGlobal('location', { search });
}

afterEach(() => {
    vi.unstubAllEnvs();
    vi.unstubAllGlobals();
});

describe('devHooksEnabled (#340)', () => {
    it('is off in a release build', () => {
        production();
        at('');
        expect(devHooksEnabled()).toBe(false);
    });

    it('is off in a release build even with a hostile query string', () => {
        production();
        at('?telemetryEndpoint=https://evil.example&clipboardPrefetch=0');
        expect(devHooksEnabled()).toBe(false);
    });

    it('is on under the Vite dev server', () => {
        production();
        vi.stubEnv('DEV', true);
        at('');
        expect(devHooksEnabled()).toBe(true);
    });

    it('is on in a build made with VITE_NGE_DEV_HOOKS=1, and only exactly "1"', () => {
        production();
        at('');
        vi.stubEnv('VITE_NGE_DEV_HOOKS', '1');
        expect(devHooksEnabled()).toBe(true);
        vi.stubEnv('VITE_NGE_DEV_HOOKS', 'true');
        expect(devHooksEnabled()).toBe(false);
        vi.stubEnv('VITE_NGE_DEV_HOOKS', '0');
        expect(devHooksEnabled()).toBe(false);
    });

    it('is on for a ?test= page (the visual-diff harness), whatever its value', () => {
        production();
        at('?test=glyph-a');
        expect(devHooksEnabled()).toBe(true);
        at('?x=1&test');
        expect(devHooksEnabled()).toBe(true);
    });

    it('does not mistake a similarly named parameter for ?test', () => {
        production();
        at('?testing=1&latest=2');
        expect(devHooksEnabled()).toBe(false);
    });

    it('is off when there is no location at all (a worker / SSR context)', () => {
        production();
        vi.stubGlobal('location', undefined);
        expect(devHooksEnabled()).toBe(false);
    });
});

describe('installDevHook (#340)', () => {
    it('installs nothing in a release build and assigns in a dev build', () => {
        const win: Record<string, unknown> = {};
        vi.stubGlobal('window', win);
        production();
        at('');
        installDevHook('__dispatch' as never, (() => 1) as never);
        expect(win).toEqual({});
        vi.stubEnv('DEV', true);
        installDevHook('__dispatch' as never, (() => 1) as never);
        expect(Object.keys(win)).toEqual(['__dispatch']);
    });
});

describe('resolveTelemetryEndpoint (#340 / #389)', () => {
    it('is undefined (console only, never the network) with nothing configured', () => {
        production();
        at('');
        expect(resolveTelemetryEndpoint()).toBeUndefined();
    });

    it('a production page never lets the URL choose', () => {
        production();
        at('?telemetryEndpoint=https://evil.example/collect');
        expect(resolveTelemetryEndpoint()).toBeUndefined();
        expect(resolveTelemetryEndpoint('https://host.example/t')).toBe('https://host.example/t');
    });

    it('the host prop beats the build constant', () => {
        production();
        at('');
        vi.stubEnv('VITE_NGE_TELEMETRY_ENDPOINT', 'https://built.example/t');
        expect(resolveTelemetryEndpoint('https://prop.example/t')).toBe('https://prop.example/t');
    });

    it('the build constant is the fallback; an empty one is no endpoint', () => {
        production();
        at('');
        vi.stubEnv('VITE_NGE_TELEMETRY_ENDPOINT', 'https://built.example/t');
        expect(resolveTelemetryEndpoint()).toBe('https://built.example/t');
        expect(resolveTelemetryEndpoint('')).toBe('https://built.example/t');
        vi.stubEnv('VITE_NGE_TELEMETRY_ENDPOINT', '');
        expect(resolveTelemetryEndpoint()).toBeUndefined();
    });

    it('under the dev-hooks flag the URL parameter beats both the prop and the constant', () => {
        production();
        vi.stubEnv('VITE_NGE_DEV_HOOKS', '1');
        vi.stubEnv('VITE_NGE_TELEMETRY_ENDPOINT', 'https://built.example/t');
        at('?telemetryEndpoint=http://localhost:9999/c');
        expect(resolveTelemetryEndpoint('https://prop.example/t')).toBe('http://localhost:9999/c');
    });

    it('under the flag, an absent or empty parameter falls through to prop then constant', () => {
        production();
        vi.stubEnv('VITE_NGE_DEV_HOOKS', '1');
        vi.stubEnv('VITE_NGE_TELEMETRY_ENDPOINT', 'https://built.example/t');
        at('?telemetryEndpoint=');
        expect(resolveTelemetryEndpoint('https://prop.example/t')).toBe('https://prop.example/t');
        at('');
        expect(resolveTelemetryEndpoint()).toBe('https://built.example/t');
    });

    it('a ?test= page honours the URL parameter too', () => {
        production();
        at('?test=case&telemetryEndpoint=http://localhost:1/c');
        expect(resolveTelemetryEndpoint()).toBe('http://localhost:1/c');
    });
});
