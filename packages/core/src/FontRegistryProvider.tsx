/**
 * FontRegistryProvider — Solid context that supplies a single
 * `FontRegistry` instance to every descendant. Built once at the app
 * root (where the engine handle lives) so boot-time `loadDefaults` and
 * the toolbar's JIT `ensureFont` share one resident-font cache — a font
 * pushed by either path is never fetched or re-pushed by the other.
 *
 * Mirrors EngineProvider's shape: `useFontRegistry()` throws if used
 * outside the provider, since that is a wiring error, not a recoverable
 * runtime state.
 */
import { createContext, useContext, type ParentComponent } from 'solid-js';
import type { FontRegistry } from './createFontRegistry';

const FontRegistryContext = createContext<FontRegistry | undefined>(undefined);

export interface FontRegistryProviderProps {
    registry: FontRegistry;
}

export const FontRegistryProvider: ParentComponent<FontRegistryProviderProps> = (
    props,
) => {
    return (
        <FontRegistryContext.Provider value={props.registry}>
            {props.children}
        </FontRegistryContext.Provider>
    );
};

export function useFontRegistry(): FontRegistry {
    const ctx = useContext(FontRegistryContext);
    if (!ctx) {
        throw new Error(
            '[@nge/core] useFontRegistry() called outside <FontRegistryProvider>. Wrap your UI in <FontRegistryProvider registry={...}>.',
        );
    }
    return ctx;
}
