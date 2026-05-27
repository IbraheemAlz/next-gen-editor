/**
 * ColorPickers — text colour + highlight colour swatches.
 *
 * Two native `<input type="color">` pickers, each backed by a clear
 * button (`⌫`). Text colour dispatches `cmd.setColor`; highlight
 * dispatches `cmd.setHighlight`. Both are selection-aware: gated on
 * SELECTION_CHANGED having fired.
 *
 * Highlight clear sends `(0,0,0,0)` — fully transparent, which the
 * engine treats as "remove background fill". Text colour clear resets
 * to the engine default black `(0,0,0,255)`.
 */
import { createSignal, createMemo, type Component } from 'solid-js';
import { createEditorCommands, createEditorState } from '@nge/core';
import './ColorPickers.css';

function hexToRgb(hex: string): [number, number, number] | null {
    const m = /^#?([0-9a-f]{6})$/i.exec(hex.trim());
    if (!m) return null;
    const v = parseInt(m[1]!, 16);
    return [(v >> 16) & 0xff, (v >> 8) & 0xff, v & 0xff];
}

function rgbToHex(c: { r: number; g: number; b: number } | undefined): string {
    if (!c) return '#000000';
    const h = (n: number) => n.toString(16).padStart(2, '0');
    return `#${h(c.r)}${h(c.g)}${h(c.b)}`;
}

export const ColorPickers: Component = () => {
    const cmd = createEditorCommands();
    const state = createEditorState();

    const ready = createMemo(() => state.selection() !== undefined);
    const [highlightHex, setHighlightHex] = createSignal('#ffff00');

    const currentTextHex = () => rgbToHex(state.attrsAtCaret()?.color);
    const currentHighlightHex = () => {
        const c = state.attrsAtCaret()?.bg_color;
        return c ? rgbToHex(c) : highlightHex();
    };

    const applyText = async (hex: string) => {
        if (!ready()) return;
        const rgb = hexToRgb(hex);
        if (!rgb) return;
        await cmd.setColor(rgb[0], rgb[1], rgb[2]);
    };
    const clearText = async () => {
        if (!ready()) return;
        await cmd.setColor(0, 0, 0);
    };
    const applyHighlight = async (hex: string) => {
        if (!ready()) return;
        const rgb = hexToRgb(hex);
        if (!rgb) return;
        setHighlightHex(hex);
        await cmd.setHighlight(rgb[0], rgb[1], rgb[2]);
    };
    const clearHighlight = async () => {
        if (!ready()) return;
        await cmd.setHighlight(0, 0, 0, 0);
    };

    return (
        <div class="nge-color" role="group" aria-label="Colour">
            <label class="nge-color__field" title="Text colour">
                <span class="nge-color__icon nge-color__icon--text" aria-hidden="true">A</span>
                <input
                    class="nge-color__swatch"
                    type="color"
                    aria-label="Text colour"
                    disabled={!ready()}
                    value={currentTextHex()}
                    onInput={(e) => void applyText(e.currentTarget.value)}
                />
            </label>
            <button
                class="nge-btn nge-btn--icon nge-color__clear"
                type="button"
                aria-label="Reset text colour"
                title="Reset text colour to black"
                disabled={!ready()}
                onClick={() => void clearText()}
            >
                ⌫
            </button>
            <label class="nge-color__field" title="Highlight colour">
                <span class="nge-color__icon nge-color__icon--hl" aria-hidden="true">▣</span>
                <input
                    class="nge-color__swatch"
                    type="color"
                    aria-label="Highlight colour"
                    disabled={!ready()}
                    value={currentHighlightHex()}
                    onInput={(e) => void applyHighlight(e.currentTarget.value)}
                />
            </label>
            <button
                class="nge-btn nge-btn--icon nge-color__clear"
                type="button"
                aria-label="Clear highlight"
                title="Clear highlight"
                disabled={!ready()}
                onClick={() => void clearHighlight()}
            >
                ⌫
            </button>
        </div>
    );
};
