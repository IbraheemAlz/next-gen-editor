/**
 * EngineProvider — Solid context that supplies the EngineClient handle to
 * every descendant. Downstream UI accesses the engine exclusively through
 * `useEngine()` so the concrete client can be swapped (mock in tests,
 * real worker in prod) without changing component code.
 */
import { createContext, useContext, type ParentComponent } from 'solid-js';
import type { EngineClientLike, EngineClientSnapshots } from './types';

export type EngineHandle = EngineClientLike & Partial<EngineClientSnapshots>;

const EngineContext = createContext<EngineHandle | undefined>(undefined);

export interface EngineProviderProps {
    client: EngineHandle;
}

export const EngineProvider: ParentComponent<EngineProviderProps> = (props) => {
    return (
        <EngineContext.Provider value={props.client}>
            {props.children}
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
