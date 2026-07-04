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
    /* Phase 3 (#39) — every control in this component (indent, line
     * spacing, shading) maps to a `Command::SetParagraph*` variant, and
     * ALL of those are engine-gated while a header/footer story is
     * active (`story_gate` in engine-wasm rejects them with
     * `Event::Error`). Disable the whole shelf rather than let that
     * error surface silently. */
    const inStory = () => state.editingStory() !== undefined;
    const gatedTitle = (label: string) =>
        inStory() ? 'Not available while editing a header or footer' : label;

    const inc = async () => {
        if (!ready() || inStory()) return;
        await cmd.increaseIndent(INDENT_STEP_PT);
    };
    const dec = async () => {
        if (!ready() || inStory()) return;
        await cmd.decreaseIndent(INDENT_STEP_PT);
    };
    const applySpacing = async (v: number) => {
        if (!ready() || inStory()) return;
        setSpacing(v);
        await cmd.setLineSpacing(v);
    };
    const applyShading = async (hex: string) => {
        if (!ready() || inStory()) return;
        setShadingHex(hex);
        const color = hex.trim() === '' ? undefined : hexToColor(hex);
        await cmd.setParagraphShading(color);
    };
    const clearShading = async () => {
        if (!ready() || inStory()) return;
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
                    title={gatedTitle('Decrease indent')}
                    disabled={!ready() || inStory()}
                    onClick={() => void dec()}
                >
                    ⇤
                </button>
                <button
                    class="nge-btn nge-btn--icon nge-pcontrols__icon"
                    type="button"
                    aria-label="Increase indent"
                    title={gatedTitle(`Increase indent (${INDENT_STEP_PT} pt step)`)}
                    disabled={!ready() || inStory()}
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
                    title={gatedTitle('Line spacing')}
                    disabled={!ready() || inStory()}
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
                    title={gatedTitle('Paragraph shading colour')}
                    disabled={!ready() || inStory()}
                    value={shadingHex() || '#ffffff'}
                    /* `onChange` fires once on commit; `onInput` would
                     * dispatch a SetParagraphShading on every cursor
                     * tick of the continuous picker and flood the
                     * bridge (the same anti-pattern the text/highlight
                     * pickers replaced with a discrete swatch grid). */
                    onChange={(e) => void applyShading(e.currentTarget.value)}
                />
                <button
                    class="nge-btn nge-btn--icon nge-pcontrols__icon"
                    type="button"
                    aria-label="Clear paragraph shading"
                    title={gatedTitle('Clear paragraph shading')}
                    disabled={!ready() || inStory() || shadingHex() === ''}
                    onClick={() => void clearShading()}
                >
                    ⌫
                </button>
            </div>
        </div>
    );
};
