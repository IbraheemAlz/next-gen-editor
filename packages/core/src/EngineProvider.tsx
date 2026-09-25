/**
 * EngineProvider — Solid context that supplies the EngineClient handle to
 * every descendant. Downstream UI accesses the engine exclusively through
 * `useEngine()` so the concrete client can be swapped (mock in tests,
 * real worker in prod) without changing component code.
 */
import { createContext, useContext, type ParentComponent } from 'solid-js';
import type { EngineClientLike, EngineClientSnapshots } from './types';
import type { DocumentDefaults } from './types';

export type EngineHandle = EngineClientLike & Partial<EngineClientSnapshots>;

const EngineContext = createContext<EngineHandle | undefined>(undefined);

/**
 * Issue #221 — a host-configured fallback for `openDocument`'s `defaults`
 * option (page size / widow control), set once at the app root instead of
 * threading it through every `cmd.openDocument(...)` call site (a
 * Letter-locale product, or one that wants the strict ECMA-376 widow-
 * control reading, sets it here). Separate from `EngineContext` itself so
 * `useEngine()`'s return type is unaffected — `undefined` (no
 * `<EngineProvider documentDefaults={...}>`, or no provider at all) means
 * "no host default", and `createEditorCommands().openDocument` then omits
 * `defaults` entirely: the pre-#221 wire shape.
 */
const DocumentDefaultsContext = createContext<DocumentDefaults | undefined>(undefined);

export interface EngineProviderProps {
    client: EngineHandle;
    /** Issue #221 — see [`DocumentDefaultsContext`]'s doc comment above. */
    documentDefaults?: DocumentDefaults;
}

export const EngineProvider: ParentComponent<EngineProviderProps> = (props) => {
    return (
        <EngineContext.Provider value={props.client}>
            <DocumentDefaultsContext.Provider value={props.documentDefaults}>
                {props.children}
            </DocumentDefaultsContext.Provider>
        </EngineContext.Provider>
    );
};

/**
 * Resolve the EngineClient from context. Throws if used outside
 * EngineProvider — that misuse is a programming error, not a runtime
 * recoverable state.
 */
export function useEngine(): EngineHandle {
    const ctx = useContext(EngineContext);
    if (!ctx) {
        throw new Error(
            '[@nge/core] useEngine() called outside <EngineProvider>. Wrap your app in <EngineProvider client={...}>.',
        );
    }
    return ctx;
}

/**
 * Issue #221 — the host's configured `DocumentDefaults`
 * (`<EngineProvider documentDefaults={...}>`), or `undefined` if none was
 * set. Unlike `useEngine`, this never throws — no configured default
 * (including outside any `<EngineProvider>`, e.g. in a unit test) is a
 * normal, common state, not a wiring error.
 */
export function useDocumentDefaults(): DocumentDefaults | undefined {
    return useContext(DocumentDefaultsContext);
}
