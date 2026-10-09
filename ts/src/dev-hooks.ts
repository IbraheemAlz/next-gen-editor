/* Issue #340 - build-time gate for the `window.__*` dev hooks.
 *
 * `window.__dispatch`, `__engineClient`, `__fontRegistry`,
 * `__setTelemetryEnabled`, `__telemetryFlush` and `__clipboardPrefetch`
 * give any same-origin script (an XSS payload included) a typed handle on
 * the engine, and `?telemetryEndpoint=` lets a crafted link choose where
 * telemetry batches go. They exist for Playwright and the live-validation
 * workflow, so they are installed ONLY when:
 *   - the Vite dev server runs (`import.meta.env.DEV`), or
 *   - the visual-diff harness is requested (`?test=<case>`), or
 *   - the build was made with `VITE_NGE_DEV_HOOKS=1` (the Playwright
 *     config sets it; a release build never does).
 * The passive status flags (`__paintIdle`, `__engineReady`, `__renderer`,
 * `__recovered`, `__bootMs`, `__lastStats`) are not capabilities and stay
 * unconditional - they are how a production-build smoke test knows the
 * app booted.
 *
 * The SDK packages (`@nge/core`, `@nge/ui`) install no globals at all. */

/** Whether this page may install the dev hooks / honour the debug URL
 *  parameters. */
export function devHooksEnabled(): boolean {
    if (import.meta.env.DEV) return true;
    if (import.meta.env.VITE_NGE_DEV_HOOKS === '1') return true;
    try {
        return new URLSearchParams(globalThis.location?.search ?? '').has('test');
    } catch {
        return false;
    }
}

/** Install `window[name] = value` only when the dev hooks are enabled. */
export function installDevHook<K extends keyof Window>(name: K, value: Window[K]): void {
    if (!devHooksEnabled()) return;
    const w: Window = window;
    w[name] = value;
}

/**
 * Where telemetry batches go (undefined = console only, never the
 * network). Precedence: under the dev-hooks flag the `?telemetryEndpoint=`
 * URL parameter (the e2e harness); otherwise the host's explicit value
 * (an `EngineProvider` prop); otherwise the build-time constant
 * `VITE_NGE_TELEMETRY_ENDPOINT`. A production page never lets the URL
 * choose.
 */
export function resolveTelemetryEndpoint(explicit?: string): string | undefined {
    if (devHooksEnabled()) {
        const fromUrl = new URLSearchParams(globalThis.location?.search ?? '').get(
            'telemetryEndpoint',
        );
        if (fromUrl) return fromUrl;
    }
    if (explicit) return explicit;
    const built: unknown = import.meta.env.VITE_NGE_TELEMETRY_ENDPOINT;
    return typeof built === 'string' && built !== '' ? built : undefined;
}
