/**
 * ListButtons — Bullet + Numbered list toggle group.
 *
 * Sprint 13 (#12) — wired through the real engine path. Each
 * Bullet / Number / Remove dispatch goes to `Command::ToggleList`;
 * the engine's idempotent `synth_list_definition` reuses an
 * existing matching template when one is present so repeated
 * toggles never inflate `numbering.xml`. Markers are recomputed
 * document-wide after every toggle so visible bullets / numbers
 * refresh immediately.
 */
import { type Component } from 'solid-js';
import { createEditorCommands, createEditorState } from '@nge/core';
import './ListButtons.css';

export const ListButtons: Component = () => {
    const cmd = createEditorCommands();
    const state = createEditorState();

    const ready = () => state.selection() !== undefined;
    const inStory = () => state.editingStory() !== undefined;
    const gatedTitle = (label: string) =>
        inStory() ? 'Not available while editing a header or footer' : label;

    const removeList = async () => {
        await cmd.toggleList('Off');
    };

    const bulletOn = async () => {
        await cmd.toggleList('Bullet');
    };

    const numberOn = async () => {
        await cmd.toggleList('Number');
    };

    return (
        <div class="nge-lists" role="group" aria-label="List">
            <button
                class="nge-btn nge-lists__btn"
                type="button"
                aria-label="Bulleted list"
                title={gatedTitle('Bulleted list')}
                disabled={!ready() || inStory()}
                onClick={() => void bulletOn()}
            >
                <span class="nge-lists__icon" aria-hidden="true">•≡</span>
                <span>Bullet</span>
            </button>
            <button
                class="nge-btn nge-lists__btn"
                type="button"
                aria-label="Numbered list"
                title={gatedTitle('Numbered list')}
                disabled={!ready() || inStory()}
                onClick={() => void numberOn()}
            >
                <span class="nge-lists__icon" aria-hidden="true">1≡</span>
                <span>Number</span>
            </button>
            <button
                class="nge-btn nge-lists__btn"
                type="button"
                aria-label="Remove list"
                title={gatedTitle('Remove list membership from selected paragraphs')}
                disabled={!ready() || inStory()}
                onClick={() => void removeList()}
            >
                <span class="nge-lists__icon" aria-hidden="true">⌫≡</span>
                <span>Remove</span>
            </button>
        </div>
    );
};
