/**
 * FontPickers — font-family `<select>` + font-size number input.
 *
 * Family list is the editor's loaded font set (`App.tsx` loads Amiri,
 * Liberation, Noto Naskh) plus a few common stacks for direct
 * formatting. Dispatches `cmd.setFontFamily` / `cmd.setFontSize`.
 *
 * Both controls reflect the resolved value at the caret on selection
 * change so the indicator matches Word / Google Docs behaviour.
 */
import { createMemo, type Component } from 'solid-js';
import { createEditorCommands, createEditorState } from '@nge/core';
import './FontPickers.css';

const FONT_FAMILIES: { label: string; value: string }[] = [
    { label: 'Amiri',          value: 'amiri' },
    { label: 'Liberation Sans', value: 'liberation' },
    { label: 'Noto Naskh',     value: 'noto-naskh' },
    { label: 'System UI',      value: 'system-ui' },
    { label: 'Serif',          value: 'serif' },
    { label: 'Monospace',      value: 'monospace' },
];

const FONT_SIZES = [8, 9, 10, 11, 12, 14, 16, 18, 20, 24, 28, 32, 36, 48, 72];

export const FontPickers: Component = () => {
    const cmd = createEditorCommands();
    const state = createEditorState();

    const ready = createMemo(() => state.selection() !== undefined);
    const currentFamily = () => state.attrsAtCaret()?.font_family ?? '';
    const currentSize = () => state.attrsAtCaret()?.font_size ?? 12;

    const applyFamily = async (family: string) => {
        if (!ready() || !family) return;
        await cmd.setFontFamily(family);
    };
    const applySize = async (pt: number) => {
        if (!ready() || !Number.isFinite(pt) || pt <= 0) return;
        await cmd.setFontSize(pt);
    };

    return (
        <div class="nge-font" role="group" aria-label="Font">
            <select
                class="nge-font__family"
                aria-label="Font family"
                disabled={!ready()}
                value={currentFamily()}
                onChange={(e) => void applyFamily(e.currentTarget.value)}
            >
                {FONT_FAMILIES.map((f) => (
                    <option value={f.value}>{f.label}</option>
                ))}
            </select>
            <input
                class="nge-font__size"
                type="number"
                min="4"
                max="400"
                step="1"
                aria-label="Font size (pt)"
                disabled={!ready()}
                value={currentSize()}
                title="Font size in points"
                onChange={(e) => void applySize(parseInt(e.currentTarget.value, 10))}
                list="nge-font-sizes"
            />
            <datalist id="nge-font-sizes">
                {FONT_SIZES.map((s) => <option value={s.toString()} />)}
            </datalist>
        </div>
    );
};
