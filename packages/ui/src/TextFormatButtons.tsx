/**
 * TextFormatButtons — Bold / Italic / Strike toolbar toggles.
 *
 * Underline lives in its own dropdown (`UnderlineStyleDropdown`) so the
 * five engine-supported variants (Single / Double / Dotted / Dashed /
 * Wavy / None) stay reachable. These three are pure booleans.
 *
 * State source: `attrsAtCaret()` for the resolved value at the caret.
 * `attrsMixed()` flips the visual to "mixed" (partial highlight) when
 * the selection straddles a boundary — Word's convention.
 */
import { type Component } from 'solid-js';
import { createEditorCommands, createEditorState } from '@nge/core';
import './TextFormatButtons.css';

export const TextFormatButtons: Component = () => {
    const cmd = createEditorCommands();
    const state = createEditorState();

    const ready = () => state.selection() !== undefined;
    const isBold = () => state.attrsAtCaret()?.bold === true;
    const isItalic = () => state.attrsAtCaret()?.italic === true;
    const isStrike = () => state.attrsAtCaret()?.strike === true;
    const mixedBold = () => state.attrsMixed()?.bold === true;
    const mixedItalic = () => state.attrsMixed()?.italic === true;
    const mixedStrike = () => state.attrsMixed()?.strike === true;

    return (
        <div class="nge-tfmt" role="group" aria-label="Text formatting">
            <button
                class="nge-btn nge-tfmt__btn nge-tfmt__btn--bold"
                type="button"
                aria-label="Bold"
                aria-pressed={isBold()}
                data-active={isBold()}
                data-mixed={mixedBold()}
                disabled={!ready()}
                title="Bold (Ctrl+B)"
                onClick={() => void cmd.setBold(!isBold())}
            >
                B
            </button>
            <button
                class="nge-btn nge-tfmt__btn nge-tfmt__btn--italic"
                type="button"
                aria-label="Italic"
                aria-pressed={isItalic()}
                data-active={isItalic()}
                data-mixed={mixedItalic()}
                disabled={!ready()}
                title="Italic (Ctrl+I)"
                onClick={() => void cmd.setItalic(!isItalic())}
            >
                I
            </button>
            <button
                class="nge-btn nge-tfmt__btn nge-tfmt__btn--strike"
                type="button"
                aria-label="Strikethrough"
                aria-pressed={isStrike()}
                data-active={isStrike()}
                data-mixed={mixedStrike()}
                disabled={!ready()}
                title="Strikethrough"
                onClick={() => void cmd.setStrike(!isStrike())}
            >
                S
            </button>
        </div>
    );
};
