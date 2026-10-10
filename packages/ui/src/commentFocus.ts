/**
 * Issue #387 — the comment the user is looking at, shared between the
 * comments rail and the on-canvas highlight overlay: clicking a highlight
 * selects its card in the rail, "Go to comment" in the rail emphasizes its
 * highlight. UI state only (the engine has no notion of it), one signal
 * per engine handle — like `ZoomControls`' fit-width mode — so every page
 * overlay and the rail agree whenever each was mounted.
 */
import { createRoot, createSignal, type Accessor } from 'solid-js';
import type { EngineHandle } from '@nge/core';

export interface CommentFocus {
    /** The active comment thread's root id, `null` for none. */
    activeId: Accessor<number | null>;
    setActiveId: (id: number | null) => void;
}

const focusStates = new WeakMap<EngineHandle, CommentFocus>();

export function commentFocusFor(engine: EngineHandle): CommentFocus {
    const existing = focusStates.get(engine);
    if (existing) return existing;
    const state = createRoot(() => {
        const [activeId, setActive] = createSignal<number | null>(null);
        return { activeId, setActiveId: (id: number | null) => setActive(id) };
    });
    focusStates.set(engine, state);
    return state;
}
