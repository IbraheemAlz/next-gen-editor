/**
 * SuperSubButtons — Superscript (X²) + Subscript (X₂) toolbar toggles.
 *
 * Dispatches via createEditorCommands().setVerticalScript(...).
 *
 * Bridge note: `TextAttrsPatch.script` holds a `VerticalScript`
 * (Normal | Superscript | Subscript) on the wire — the field is *named*
 * `script` but it carries vertical positioning, not script (Latin/
 * Arabic/etc.). The original Sprint 1 (UI Edition) plan flagged a
 * missing bridge field; once the real types were inspected, the field
 * already exists. No bridge change required.
 *
 * State source: `TextAttrs.script` (resolved at caret) — when the
 * selection straddles a Super↔Normal boundary the engine collapses to
 * one resolved value, which is acceptable for an MVP toggle.
 */
import { type Component } from 'solid-js';
import { createEditorCommands, createEditorState } from '@nge/core';
import './SuperSubButtons.css';

export const SuperSubButtons: Component = () => {
    const cmd = createEditorCommands();
    const state = createEditorState();

    const isSuper = () => state.attrsAtCaret()?.script === 'Superscript';
    const isSub = () => state.attrsAtCaret()?.script === 'Subscript';

    const toggleSuper = async () => {
        await cmd.setVerticalScript(isSuper() ? 'Normal' : 'Superscript');
    };
    const toggleSub = async () => {
        await cmd.setVerticalScript(isSub() ? 'Normal' : 'Subscript');
    };

    return (
        <div class="nge-vscript" role="group" aria-label="Vertical script">
            <button
                class="nge-btn nge-vscript__btn"
                type="button"
                aria-label="Superscript"
                aria-pressed={isSuper()}
                data-active={isSuper()}
                title="Superscript (Ctrl+Shift+=)"
                onClick={() => void toggleSuper()}
            >
                X<sup>2</sup>
            </button>
            <button
                class="nge-btn nge-vscript__btn"
                type="button"
                aria-label="Subscript"
                aria-pressed={isSub()}
                data-active={isSub()}
                title="Subscript (Ctrl+=)"
                onClick={() => void toggleSub()}
            >
                X<sub>2</sub>
            </button>
        </div>
    );
};
