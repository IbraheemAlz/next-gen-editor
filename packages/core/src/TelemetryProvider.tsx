/**
 * TelemetryProvider — Solid context that supplies a single `TelemetryConfig`
 * instance to every descendant (Issue #86). Built once at the app root
 * (alongside `EngineProvider` / `FontRegistryProvider`) so the boot
 * sequence's `startTelemetry(client, { enabled: config.enabled, ... })` and
 * `@nge/ui`'s `SettingsMenu` toggle share one opt-in flag.
 *
 * Mirrors `FontRegistryProvider`'s shape exactly: `useTelemetryConfig()`
 * throws if used outside the provider, since that is a wiring error, not a
 * recoverable runtime state.
 */
import { createContext, useContext, type ParentComponent } from 'solid-js';
import type { TelemetryConfig } from './createTelemetryConfig';

const TelemetryContext = createContext<TelemetryConfig | undefined>(undefined);

export interface TelemetryProviderProps {
    config: TelemetryConfig;
}

export const TelemetryProvider: ParentComponent<TelemetryProviderProps> = (props) => {
    return (
        <TelemetryContext.Provider value={props.config}>{props.children}</TelemetryContext.Provider>
    );
};

export function useTelemetryConfig(): TelemetryConfig {
    const ctx = useContext(TelemetryContext);
    if (!ctx) {
        throw new Error(
            '[@nge/core] useTelemetryConfig() called outside <TelemetryProvider>. Wrap your UI in <TelemetryProvider config={...}>.',
        );
    }
    return ctx;
}
