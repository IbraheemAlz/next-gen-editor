/**
 * Issue #449 — which shelf controls the open document's protection would
 * refuse, so a control can render `disabled` (with the reason) instead of
 * dispatching into a typed `Protected` refusal and a toast.
 *
 * The engine's `protection_gate` (`crates/engine-wasm/src/protection_gate.rs`)
 * is the rule; this module only ASKS it, it never restates it:
 *
 * - which commands a mode admits is `protectionAdmits`, generated from
 *   `bridge::ProtectionClass::admitted_by` (`commandMeta.generated.ts`);
 * - which commands a facade method dispatches is `FACADE_METHOD_COMMANDS`
 *   (checked by `tsc` against the command table), so a control names the
 *   facade method it calls and cannot drift from what it really sends;
 * - the one refinement the gate adds on top of the class table that a
 *   control can see: while a header / footer / note / text-box story is
 *   being edited every non-exempt command is refused (story content is
 *   neither a tracked change nor form content).
 *
 * What stays with the engine, on purpose: a `forms` document admits the
 * `text` class only inside form-field content, and `trackedChanges`
 * refuses `TOGGLE_TRACK_CHANGES { enabled: false }`. Both depend on the
 * caret / the payload, not on the control, so a `text`-class control is
 * not locked by them (no shelf control dispatches one — typing arrives
 * through the hidden textarea) and the Track toggle gates itself on
 * "any protection" (`ReviewControls`).
 *
 * DOM-free and Solid-free: unit-tested in `protection.test.ts`.
 */
import { commandMeta, protectionAdmits } from './commandMeta.generated';
import type { EditorCommands } from './createEditorCommands';
import { FACADE_METHOD_COMMANDS } from './facadeMap';
import type { ProtectionMode } from './types';

/** What a refusal is derived from: the enforced mode (`undefined` for an
 *  unrestricted document) and whether a story is being edited. */
export interface ProtectionContext {
    readonly mode: ProtectionMode | undefined;
    readonly inStory: boolean;
}

/** Why a command would be refused. */
export type ProtectionRefusalKind =
    /** The mode does not admit the command's class. */
    | 'mode'
    /** The mode admits the class, but a story is being edited. */
    | 'story';

export interface ProtectionRefusal {
    readonly kind: ProtectionRefusalKind;
    readonly mode: ProtectionMode;
    /** The command the refusal was computed for (a facade method that
     *  dispatches several reports the first one refused). */
    readonly command: string;
    /** Tooltip text: starts "Not available:" and names the reason. */
    readonly title: string;
}

/** The mode's phrase in a refusal sentence — the engine's own wording
 *  (`protection_label` in `protection_gate.rs`). */
export const PROTECTION_MODE_PHRASE: Readonly<Record<ProtectionMode, string>> = {
    readOnly: 'read-only',
    comments: 'comments only',
    trackedChanges: 'tracked changes only',
    forms: 'filling in forms',
};

/** Would the engine refuse command `type` in this context? `undefined`
 *  when it would not (or when the answer depends on the caret / payload,
 *  see the module docs). */
export function commandRefusal(
    context: ProtectionContext,
    type: string,
): ProtectionRefusal | undefined {
    const { mode } = context;
    if (mode === undefined) return undefined;
    if (commandMeta(type).protection === 'exempt') return undefined;
    const phrase = PROTECTION_MODE_PHRASE[mode];
    if (!protectionAdmits(mode, type)) {
        return {
            kind: 'mode',
            mode,
            command: type,
            title: `Not available: this document is protected (${phrase})`,
        };
    }
    if (context.inStory) {
        return {
            kind: 'story',
            mode,
            command: type,
            title: `Not available: headers, footers, notes and text boxes cannot be edited in a protected document (${phrase})`,
        };
    }
    return undefined;
}

/** The first refusal among the commands the facade `methods` dispatch
 *  (`FACADE_METHOD_COMMANDS`): a control that calls several members is
 *  locked when any of them would be refused. A `'host'` member (no engine
 *  command) and the `raw` escape hatch are never refused here. */
export function methodRefusal(
    context: ProtectionContext,
    ...methods: readonly (keyof EditorCommands)[]
): ProtectionRefusal | undefined {
    if (context.mode === undefined) return undefined;
    for (const method of methods) {
        const target = FACADE_METHOD_COMMANDS[method];
        if (typeof target === 'string') continue;
        for (const type of target) {
            const refusal = commandRefusal(context, type);
            if (refusal !== undefined) return refusal;
        }
    }
    return undefined;
}
