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
 * Renders nothing for an unrestricted document. There is no "stop
 * protection" action: lifting a restriction (and verifying its password
 * hash) is not offered by the engine.
 */
import { Show, type Component } from 'solid-js';
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

export const ProtectionBadge: Component = () => {
    const state = createEditorState();
    return (
        <Show when={state.protection()}>
            {(mode) => (
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
            )}
        </Show>
    );
};
