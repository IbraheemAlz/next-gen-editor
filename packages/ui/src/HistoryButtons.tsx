/**
 * HistoryButtons — Undo / Redo toolbar pair.
 *
 * Active gating reads `state.canUndo()` / `state.canRedo()`; the engine
 * emits `UndoStateChanged` on every mutation so the disabled state
 * tracks the real history depth (bounded at 100).
 */
import { type Component } from 'solid-js';
import { createEditorCommands, createEditorState } from '@nge/core';
import './HistoryButtons.css';

export const HistoryButtons: Component = () => {
    const cmd = createEditorCommands();
    const state = createEditorState();

    return (
        <div class="nge-hist" role="group" aria-label="History">
            <button
                class="nge-btn nge-btn--icon nge-hist__btn"
                type="button"
                aria-label="Undo"
                title="Undo (Ctrl+Z)"
                disabled={!state.canUndo()}
                data-nge-command="UNDO"
                onClick={() => void cmd.undo()}
            >
                ↶
            </button>
            <button
                class="nge-btn nge-btn--icon nge-hist__btn"
                type="button"
                aria-label="Redo"
                title="Redo (Ctrl+Y)"
                disabled={!state.canRedo()}
                data-nge-command="REDO"
                onClick={() => void cmd.redo()}
            >
                ↷
            </button>
        </div>
    );
};
