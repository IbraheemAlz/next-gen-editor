/**
 * protectionGate — issue #449. A shelf control that dispatches a command
 * the open document's protection would refuse must render `disabled`,
 * with the reason as its tooltip, instead of dispatching into a typed
 * `Protected` refusal and a toast.
 *
 * `createEditorState().protectionRefusal(...methods)` is the one query
 * (the engine's class table + the facade map; see `@nge/core`
 * `protection.ts`): a control names the facade members it calls and this
 * wraps the answer in the two things a control reads — whether it is
 * locked, and what its tooltip says.
 *
 * The accessors are reactive (they read `protection()` and
 * `editingStory()`), so call them in JSX / effects.
 */
import type { EditorCommands, EditorState, ProtectionRefusal } from '@nge/core';

export interface ProtectionGate {
    /** The refusal the engine would answer, or `undefined`. */
    refusal: () => ProtectionRefusal | undefined;
    /** `true` while the control must be `disabled` for protection. */
    locked: () => boolean;
    /** `base` normally; the refusal's reason while locked. */
    title: (base: string) => string;
}

export function protectionGate(
    state: Pick<EditorState, 'protectionRefusal'>,
    ...methods: (keyof EditorCommands)[]
): ProtectionGate {
    const refusal = () => state.protectionRefusal(...methods);
    return {
        refusal,
        locked: () => refusal() !== undefined,
        title: (base) => refusal()?.title ?? base,
    };
}
