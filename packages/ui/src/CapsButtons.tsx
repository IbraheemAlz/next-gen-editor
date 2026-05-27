/**
 * CapsButtons — Small Caps + All Caps toggles.
 *
 * Bridge gap closed in this sprint: `TextAttrsPatch` grew `caps` +
 * `small_caps` fields (engine `SpanStyle` already carried them, but
 * the wire schema didn't surface a toggle). These buttons round-trip
 * through `cmd.setCaps` / `cmd.setSmallCaps`; the engine resolves
 * caps over small_caps per OOXML §17.3.2.7 when both are armed on
 * the same run.
 */
import { type Component } from 'solid-js';
import { createEditorCommands, createEditorState } from '@nge/core';
import './CapsButtons.css';

export const CapsButtons: Component = () => {
    const cmd = createEditorCommands();
    const state = createEditorState();

    const ready = () => state.selection() !== undefined;
    const isCaps = () => state.attrsAtCaret()?.caps === true;
    const isSmallCaps = () => state.attrsAtCaret()?.small_caps === true;

    return (
        <div class="nge-caps" role="group" aria-label="Caps">
            <button
                class="nge-btn nge-caps__btn"
                type="button"
                aria-label="All caps"
                aria-pressed={isCaps()}
                data-active={isCaps()}
                disabled={!ready()}
                title="All caps"
                onClick={() => void cmd.setCaps(!isCaps())}
            >
                AB
            </button>
            <button
                class="nge-btn nge-caps__btn"
                type="button"
                aria-label="Small caps"
                aria-pressed={isSmallCaps()}
                data-active={isSmallCaps()}
                disabled={!ready()}
                title="Small caps"
                onClick={() => void cmd.setSmallCaps(!isSmallCaps())}
            >
                A<span class="nge-caps__small">B</span>
            </button>
        </div>
    );
};
