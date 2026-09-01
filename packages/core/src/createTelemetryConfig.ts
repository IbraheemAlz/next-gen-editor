/**
 * createTelemetryConfig — the opt-in flag for D5.7 telemetry (Issue #86).
 *
 * A tiny Solid-signal-backed config, persisted per-browser via
 * `localStorage`, shared between the app's boot sequence (which starts or
 * stops `ts/src/state/telemetry.ts`'s pipeline) and `@nge/ui`'s
 * `SettingsMenu` (which shows + toggles the opt-in state). Telemetry is
 * **opt-in only** — `enabled` defaults to `false` until a person explicitly
 * turns it on; no document content or personal data is ever sampled
 * regardless of this flag (see the collector's own doc comment).
 *
 * Mirrors `createFontRegistry` + `FontRegistryProvider`'s shape: built once
 * at the app root, threaded down via `TelemetryProvider`.
 */
import { createSignal, type Accessor } from 'solid-js';

const STORAGE_KEY = 'nge-telemetry-enabled';

export interface TelemetryConfig {
    /** Whether telemetry sampling + transport is opted in. */
    enabled: Accessor<boolean>;
    setEnabled: (v: boolean) => void;
}

function readStored(): boolean {
    try {
        return localStorage.getItem(STORAGE_KEY) === '1';
    } catch {
        /* Storage unavailable (private browsing / disabled) — fall back to
           the safe opt-in default. */
        return false;
    }
}

export function createTelemetryConfig(): TelemetryConfig {
    const [enabled, setEnabledSignal] = createSignal<boolean>(readStored());

    const setEnabled = (v: boolean): void => {
        setEnabledSignal(v);
        try {
            localStorage.setItem(STORAGE_KEY, v ? '1' : '0');
        } catch {
            /* The in-memory signal still drives the current session
               correctly even if the preference can't persist. */
        }
    };

    return { enabled, setEnabled };
}
