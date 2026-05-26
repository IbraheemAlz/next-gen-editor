/**
 * ParagraphControls — toolbar shelf for paragraph-level formatting.
 *
 * Three controls:
 *   1. Indent +/− buttons → `Command::SetParagraphIndent`. Each click
 *      bumps the `<w:start>` indent by `stepPt` (default 36 pt =
 *      0.5 inch — Word's stock step). Decrease resets to 0 (no
 *      read-back path to compute current - step today; see backlog
 *      issue #10 for the full state read-back fix).
 *   2. Line spacing dropdown → `Command::SetLineSpacing` with a
 *      multiplier (1.0 / 1.15 / 1.5 / 2.0).
 *   3. Paragraph shading swatch → `Command::SetParagraphShading`.
 *      Hex picker + text input; blank clears.
 *
 * Disabled until a SELECTION_CHANGED arrives (engine command paths
 * throw without a caret).
 */
import { createSignal, createMemo, type Component } from 'solid-js';
import {
    createEditorCommands,
    createEditorState,
    type Color,
} from '@nge/core';
import './ParagraphControls.css';

const SPACINGS: { label: string; value: number }[] = [
    { label: 'Single', value: 1.0 },
    { label: '1.15', value: 1.15 },
    { label: '1.5', value: 1.5 },
    { label: 'Double', value: 2.0 },
];

const INDENT_STEP_PT = 36;

function hexToColor(hex: string): Color | undefined {
    const m = /^#?([0-9a-f]{6})$/i.exec(hex.trim());
    if (!m) return undefined;
    const v = parseInt(m[1]!, 16);
    return { r: (v >> 16) & 0xff, g: (v >> 8) & 0xff, b: v & 0xff, a: 255 };
}

export const ParagraphControls: Component = () => {
    const cmd = createEditorCommands();
    const state = createEditorState();
    const [spacing, setSpacing] = createSignal(1.0);
    const [shadingHex, setShadingHex] = createSignal('');

    const ready = createMemo(() => state.selection() !== undefined);

    const inc = async () => {
        if (!ready()) return;
        await cmd.increaseIndent(INDENT_STEP_PT);
    };
    const dec = async () => {
        if (!ready()) return;
        await cmd.decreaseIndent(INDENT_STEP_PT);
    };
    const applySpacing = async (v: number) => {
        if (!ready()) return;
        setSpacing(v);
        await cmd.setLineSpacing(v);
    };
    const applyShading = async (hex: string) => {
        if (!ready()) return;
        setShadingHex(hex);
        const color = hex.trim() === '' ? undefined : hexToColor(hex);
        await cmd.setParagraphShading(color);
    };
    const clearShading = async () => {
        if (!ready()) return;
        setShadingHex('');
        await cmd.setParagraphShading(undefined);
    };

    return (
        <div class="nge-pcontrols" role="group" aria-label="Paragraph">
            <div class="nge-pcontrols__group" role="group" aria-label="Indent">
                <button
                    class="nge-btn nge-btn--icon nge-pcontrols__icon"
                    type="button"
                    aria-label="Decrease indent"
                    title="Decrease indent"
                    disabled={!ready()}
                    onClick={() => void dec()}
                >
                    ⇤
                </button>
                <button
                    class="nge-btn nge-btn--icon nge-pcontrols__icon"
                    type="button"
                    aria-label="Increase indent"
                    title={`Increase indent (${INDENT_STEP_PT} pt step)`}
                    disabled={!ready()}
                    onClick={() => void inc()}
                >
                    ⇥
                </button>
            </div>

            <label class="nge-pcontrols__field">
                <span class="nge-pcontrols__label" aria-hidden="true">↕</span>
                <select
                    class="nge-pcontrols__select"
                    aria-label="Line spacing"
                    disabled={!ready()}
                    value={spacing().toString()}
                    onChange={(e) =>
                        void applySpacing(parseFloat(e.currentTarget.value))
                    }
                >
                    {SPACINGS.map((s) => (
                        <option value={s.value.toString()}>{s.label}</option>
                    ))}
                </select>
            </label>

            <div class="nge-pcontrols__group" role="group" aria-label="Paragraph shading">
                <input
                    class="nge-pcontrols__color"
                    type="color"
                    aria-label="Paragraph shading colour"
                    disabled={!ready()}
                    value={shadingHex() || '#ffffff'}
                    onInput={(e) => void applyShading(e.currentTarget.value)}
                />
                <button
                    class="nge-btn nge-btn--icon nge-pcontrols__icon"
                    type="button"
                    aria-label="Clear paragraph shading"
                    title="Clear paragraph shading"
                    disabled={!ready() || shadingHex() === ''}
                    onClick={() => void clearShading()}
                >
                    ⌫
                </button>
            </div>
        </div>
    );
};
