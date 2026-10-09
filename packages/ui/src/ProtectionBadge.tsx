/**
 * ProtectionBadge — issue #345. Shows the open document's enforced
 * editing restriction (`<w:documentProtection w:enforcement="1">`,
 * reported on `SELECTION_CHANGED.protection`) as a status-bar chip, so a
 * user knows up front why some edits will not land:
 *
 *   - `readOnly` → "Read-only"
 *   - `comments` → "Comments only"
 *   - `trackedChanges` → "Tracked changes only" (review mode is forced on)
 *   - `forms` → "Filling in forms" (only content controls and text form
 *     fields take edits)
 *
 * When the engine refuses a command (`ERROR { kind: 'Protected' }`, read
 * through `state.protectionRefusal()`), a short toast above the chip says
 * why (`role="status"`; the engine also queues an assertive `aria-live`
 * announcement). The FileMenu's global error banner skips these refusals
 * so the user sees one message, here.
 *
 * Renders nothing for an unrestricted document. There is no "stop
 * protection" action: lifting a restriction (and verifying its password
 * hash) is not offered by the engine.
 */
import { createEffect, createSignal, on, onCleanup, Show, type Component } from 'solid-js';
import { createEditorState, type ProtectionMode } from '@nge/core';
import './ProtectionBadge.css';

/** Label + tooltip for each enforced mode. */
export const PROTECTION_LABELS: Record<ProtectionMode, { label: string; hint: string }> = {
    readOnly: {
        label: 'Read-only',
        hint: 'This document is protected: it can be read but not edited.',
    },
    comments: {
        label: 'Comments only',
        hint: 'This document is protected: you can add comments, but not change its text.',
    },
    trackedChanges: {
        label: 'Tracked changes only',
        hint: 'This document is protected: every edit is recorded as a tracked change, and Track Changes cannot be turned off.',
    },
    forms: {
        label: 'Filling in forms',
        hint: 'This document is protected: only its form fields and content controls can be edited.',
    },
};

/** How long a refusal toast stays up, in ms. */
const TOAST_MS = 4000;

export const ProtectionBadge: Component = () => {
    const state = createEditorState();
    const [toast, setToast] = createSignal<string | null>(null);
    let timer: number | undefined;

    createEffect(
        on(
            () => state.protectionRefusal(),
            (refusal) => {
                if (!refusal) return;
                setToast(refusal.message);
                window.clearTimeout(timer);
                timer = window.setTimeout(() => setToast(null), TOAST_MS);
            },
        ),
    );
    onCleanup(() => window.clearTimeout(timer));

    return (
        <Show when={state.protection()}>
            {(mode) => (
                <span class="nge-protection-wrap">
                    <span
                        class="nge-protection"
                        data-nge-protection={mode()}
                        title={PROTECTION_LABELS[mode()].hint}
                        aria-label={`Protected document: ${PROTECTION_LABELS[mode()].label}`}
                    >
                        <span class="nge-protection__icon" aria-hidden="true">
                            🔒
                        </span>
                        {PROTECTION_LABELS[mode()].label}
                    </span>
                    <Show when={toast()}>
                        <span class="nge-protection__toast" role="status">
                            {toast()}
                        </span>
                    </Show>
                </span>
            )}
        </Show>
    );
};
