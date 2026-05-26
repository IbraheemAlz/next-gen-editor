/**
 * ListButtons — Bullet + Numbered list toggle group.
 *
 * Sprint 5 (UI Edition) reality:
 *   - "Turn list off" → `Command::ToggleList { kind: Off }`. Works.
 *   - "Turn bullet on" / "Turn number on" → require numbering
 *     synthesis (`numbering.xml` writer + `<w:abstractNum>` /
 *     `<w:num>` generators). Engine returns `Event::Error` and the
 *     UI surfaces this honestly as a disabled button with an
 *     amber "Engine pending" badge.
 *
 * Once the Core Engine ships numbering synthesis (project backlog
 * issue "Core: Implement numbering.xml writer and List Synthesis"),
 * removing the `disabled` attribute + the badge is a 4-line change.
 */
import { type Component } from 'solid-js';
import { createEditorCommands, createEditorState } from '@nge/core';
import './ListButtons.css';

export const ListButtons: Component = () => {
    const cmd = createEditorCommands();
    const state = createEditorState();

    const ready = () => state.selection() !== undefined;

    /* `Off` is the only kind that actually mutates today; binding it
     * to BOTH bullet- and numbered-removal would be misleading.
     * Instead expose a single "Remove list" affordance via the
     * bullet/numbered click-when-active path once read-back tells us
     * the caret is in a list. Until that read-back exists, ship a
     * single explicit "Remove list" button alongside the disabled
     * on-buttons. */
    const removeList = async () => {
        await cmd.toggleList('Off');
    };

    const bulletOn = async () => {
        /* Intentionally dispatches — surfaces the engine's clear
         * error message in the FileMenu's error toast (which
         * subscribes to ERROR events globally). */
        await cmd.toggleList('Bullet');
    };

    const numberOn = async () => {
        await cmd.toggleList('Number');
    };

    return (
        <div class="nge-lists" role="group" aria-label="List">
            <button
                class="nge-btn nge-lists__btn nge-lists__btn--disabled"
                type="button"
                aria-label="Bulleted list"
                title="Bulleted list — engine pending (numbering synthesis)"
                disabled={!ready()}
                onClick={() => void bulletOn()}
            >
                <span class="nge-lists__icon" aria-hidden="true">•≡</span>
                <span>Bullet</span>
                <span class="nge-lists__badge">Engine pending</span>
            </button>
            <button
                class="nge-btn nge-lists__btn nge-lists__btn--disabled"
                type="button"
                aria-label="Numbered list"
                title="Numbered list — engine pending (numbering synthesis)"
                disabled={!ready()}
                onClick={() => void numberOn()}
            >
                <span class="nge-lists__icon" aria-hidden="true">1≡</span>
                <span>Number</span>
                <span class="nge-lists__badge">Engine pending</span>
            </button>
            <button
                class="nge-btn nge-lists__btn"
                type="button"
                aria-label="Remove list"
                title="Remove list membership from selected paragraphs"
                disabled={!ready()}
                onClick={() => void removeList()}
            >
                <span class="nge-lists__icon" aria-hidden="true">⌫≡</span>
                <span>Remove</span>
            </button>
        </div>
    );
};
