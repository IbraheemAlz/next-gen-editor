/**
 * TextBoxButton — text box authoring (issue #83).
 *
 *   - Body mode: "Text Box" dispatches `Command::InsertTextBox` at the
 *     caret (2" × 1", Word's "Draw Text Box" defaults: white fill,
 *     0.75 pt outline, square wrap). The engine ENTERS the new box, so
 *     typing lands in it immediately. Disabled inside table cells (the
 *     engine rejects them — text boxes in cells are a follow-up) and
 *     inside any other story (text boxes cannot nest).
 *   - Text-box mode (`state.editingStory()?.area === 'TextBox'`): a
 *     "Close Text Box" button returns to the body right after the box's
 *     anchor (`Command::ExitHeaderFooter`); a click outside every box
 *     does the same, and a click inside any box enters it.
 *
 * Every path here is real engine behaviour — no "Engine pending" gate.
 */
import { Show, createMemo, type Component } from 'solid-js';
import { createEditorCommands, createEditorState } from '@nge/core';
import { focusEditorInput } from './focus';
import './TextBoxButton.css';

export const TextBoxButton: Component = () => {
    const cmd = createEditorCommands();
    const state = createEditorState();

    const ready = createMemo(() => state.selection() !== undefined);
    const inCell = createMemo(() => state.cellProperties() !== undefined);
    const story = () => state.editingStory();
    const inTextBox = createMemo(() => story()?.area === 'TextBox');
    const canInsert = createMemo(() => ready() && !inCell() && story() === undefined);

    const why = (): string | undefined => {
        if (story() !== undefined) return 'Return to the document body first';
        if (inCell()) return 'Text boxes inside table cells aren’t supported yet';
        return undefined;
    };

    const insert = async () => {
        if (!canInsert()) return;
        await cmd.insertTextBox();
        focusEditorInput();
    };

    const close = async () => {
        await cmd.exitHeaderFooter();
        focusEditorInput();
    };

    return (
        <div class="nge-text-box" role="group" aria-label="Text box">
            <Show
                when={inTextBox()}
                fallback={
                    <button
                        class="nge-btn nge-text-box__btn"
                        type="button"
                        aria-label="Insert text box"
                        title={why() ?? 'Insert a text box at the caret'}
                        disabled={!canInsert()}
                        onClick={() => void insert()}
                    >
                        <span class="nge-text-box__icon" aria-hidden="true">
                            ▭
                        </span>
                        <span>Text Box</span>
                    </button>
                }
            >
                <button
                    class="nge-btn nge-text-box__btn nge-text-box__btn--close"
                    type="button"
                    aria-label="Close text box editing"
                    title="Return to the document body (Esc)"
                    onClick={() => void close()}
                >
                    <span aria-hidden="true">✕</span>
                    <span>Close Text Box</span>
                </button>
            </Show>
        </div>
    );
};
