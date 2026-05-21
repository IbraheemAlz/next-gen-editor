/* Phase 4 §4 — Solid application entry point.
 *
 * Two modes share this entry:
 *   - `?test=<case>` present → boot the visual-diff harness and never mount
 *     Solid, so the golden screenshot is the bare engine canvas.
 *   - otherwise → mount the Solid editor app on #root. */
import { render } from 'solid-js/web';
import { App } from './App';
import type { Command, Event } from './engine/types';
import type { EngineClient } from './engine/engine-client';

declare global {
    interface Window {
        /** Set true once the engine completes its seed paint (e2e + visual-diff hook). */
        __paintIdle?: boolean;
        /** Set true once the engine worker has booted (e2e hook). */
        __engineReady?: boolean;
        /** Most recent harness paint event (visual-diff debugging hook). */
        __lastEvent?: unknown;
        /** Dispatch a Command to the engine; resolves with the Event (e2e hook). */
        __dispatch?: (cmd: Command) => Promise<Event>;
        /** The interactive editor's EngineClient (e2e + debugging hook). */
        __engineClient?: EngineClient;
        /** Most recent EngineStats event (debugging hook). */
        __lastStats?: unknown;
        /** Cold-boot duration in ms — EngineClient.init() (e2e hook, boot.spec). */
        __bootMs?: number;
        /** Set true after a successful crash recovery (e2e hook). */
        __recovered?: boolean;
        /** Force a telemetry batch flush now (D5.7 debug + e2e hook). */
        __telemetryFlush?: () => Promise<void>;
    }
}

/* SAB depends on cross-origin isolation; the Vite COOP/COEP headers supply
   it. Warn rather than fail so the harness still runs in odd environments. */
if (!self.crossOriginIsolated) {
    console.warn('crossOriginIsolated is false — SAB unavailable');
}

const testCase = new URLSearchParams(window.location.search).get('test') ?? '';

if (testCase === '') {
    const root = document.getElementById('root');
    if (!root) throw new Error('#root mount point missing');
    render(() => <App />, root);
} else {
    /* Visual-diff harness — dynamically imported so it never weighs down the
       interactive app bundle. */
    void import('./harness/visual-diff').then((m) => m.runVisualDiffHarness(testCase));
}
