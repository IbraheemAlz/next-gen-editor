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

    /* UI-side mutual exclusivity: clicking the inactive button clears
     * the other flag in the same patch so the user never sees both
     * "pressed" simultaneously. The engine's render path (caps wins
     * over small_caps per OOXML §17.3.2.7) would also handle it, but
     * pressing one and seeing the other stay highlighted is confusing
     * — Word's Format > Font dialog allows both flags to coexist;
     * our toolbar's two-button shelf does not. */
    const toggleCaps = () => {
        if (isCaps()) {
            void cmd.applyAttrs({ caps: false });
        } else {
            void cmd.applyAttrs({ caps: true, small_caps: false });
        }
    };
    const toggleSmallCaps = () => {
        if (isSmallCaps()) {
            void cmd.applyAttrs({ small_caps: false });
        } else {
            void cmd.applyAttrs({ small_caps: true, caps: false });
        }
    };

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
                onClick={toggleCaps}
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
                onClick={toggleSmallCaps}
            >
                A<span class="nge-caps__small">B</span>
            </button>
        </div>
    );
};
