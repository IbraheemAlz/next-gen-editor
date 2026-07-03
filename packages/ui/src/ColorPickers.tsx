/**
 * ColorPickers — discrete-swatch text + highlight pickers.
 *
 * The earlier `<input type="color">` continuous wheel dispatched
 * `cmd.setColor` on every `input` event — a single drag could fire
 * 30+ APPLY_FORMATTING commands per second and flooded the worker
 * bridge into a stall.
 *
 * This rewrite mirrors MS Word: a popover with a fixed palette of
 * preset swatches plus a hex `<input>` that only commits on Enter
 * or blur. Each click / commit fires exactly one bridge dispatch.
 * Text colour reflects the resolved value at the caret;
 * highlight is local because the engine's `bg_color` is sparse and
 * may be `undefined` on most spans.
 */
import {
    createMemo,
    createSignal,
    For,
    Show,
    onCleanup,
    onMount,
    type Component,
} from 'solid-js';
import { createEditorCommands, createEditorState } from '@nge/core';
import { focusEditorInput } from './focus';
import './ColorPickers.css';

const TEXT_SWATCHES: string[] = [
    '#000000', '#434343', '#666666', '#999999', '#b7b7b7', '#cccccc', '#d9d9d9', '#ffffff',
    '#980000', '#ff0000', '#ff9900', '#ffff00', '#00ff00', '#00ffff', '#4a86e8', '#0000ff',
    '#9900ff', '#ff00ff', '#e6b8af', '#f4cccc', '#fce5cd', '#fff2cc', '#d9ead3', '#d0e0e3',
    '#cfe2f3', '#d9d2e9', '#ead1dc', '#dd7e6b', '#ea9999', '#f9cb9c', '#ffe599', '#b6d7a8',
];

const HIGHLIGHT_SWATCHES: string[] = [
    '#ffff00', '#00ff00', '#00ffff', '#ff00ff', '#0000ff', '#ff0000', '#000080', '#008000',
    '#800080', '#800000', '#808000', '#008080', '#c0c0c0', '#808080', '#ffffff', '#000000',
];

function hexToRgb(hex: string): [number, number, number] | null {
    const m = /^#?([0-9a-f]{3}|[0-9a-f]{6})$/i.exec(hex.trim());
    if (!m) return null;
    let h = m[1]!;
    if (h.length === 3) h = h.split('').map((c) => c + c).join('');
    const v = parseInt(h, 16);
    return [(v >> 16) & 0xff, (v >> 8) & 0xff, v & 0xff];
}

function rgbToHex(c: { r: number; g: number; b: number } | undefined): string {
    if (!c) return '#000000';
    const h = (n: number) => n.toString(16).padStart(2, '0');
    return `#${h(c.r)}${h(c.g)}${h(c.b)}`;
}

type Kind = 'text' | 'highlight';

const PickerPopover: Component<{
    kind: Kind;
    swatches: string[];
    current: string;
    onPick: (hex: string) => void;
    onClear: () => void;
    onClose: () => void;
}> = (props) => {
    const [draft, setDraft] = createSignal(props.current);

    /* Close on outside click. Captured at document level so a click
     * outside the popover dismisses without bubbling into the editor. */
    let root: HTMLDivElement | undefined;
    const onDocPointer = (e: PointerEvent) => {
        if (root && !root.contains(e.target as Node)) props.onClose();
    };
    onMount(() => document.addEventListener('pointerdown', onDocPointer, true));
    onCleanup(() => document.removeEventListener('pointerdown', onDocPointer, true));

    const commit = () => {
        const hex = draft().trim();
        if (hexToRgb(hex)) props.onPick(hex);
    };

    return (
        <div class="nge-color__popover" role="dialog" ref={root}>
            <div class="nge-color__grid" role="grid">
                <For each={props.swatches}>
                    {(sw) => (
                        <button
                            class="nge-color__cell"
                            type="button"
                            role="gridcell"
                            aria-label={sw}
                            title={sw}
                            style={{ background: sw }}
                            data-active={sw.toLowerCase() === props.current.toLowerCase()}
                            onClick={() => props.onPick(sw)}
                        />
                    )}
                </For>
            </div>
            <div class="nge-color__hexrow">
                <label class="nge-color__hexlabel">
                    Hex
                    <input
                        class="nge-color__hexinput"
                        type="text"
                        spellcheck={false}
                        autocomplete="off"
                        value={draft()}
                        onInput={(e) => setDraft(e.currentTarget.value)}
                        onKeyDown={(e) => {
                            if (e.key === 'Enter') {
                                e.preventDefault();
                                commit();
                            }
                        }}
                        onBlur={commit}
                    />
                </label>
                <button
                    class="nge-btn nge-color__clear"
                    type="button"
                    onClick={() => props.onClear()}
                >
                    {props.kind === 'text' ? 'Reset' : 'Clear'}
                </button>
            </div>
        </div>
    );
};

export const ColorPickers: Component = () => {
    const cmd = createEditorCommands();
    const state = createEditorState();

    const ready = createMemo(() => state.selection() !== undefined);
    const [openTextPicker, setOpenTextPicker] = createSignal(false);
    const [openHighlightPicker, setOpenHighlightPicker] = createSignal(false);
    const [lastHighlightHex, setLastHighlightHex] = createSignal('#ffff00');

    const currentTextHex = () => rgbToHex(state.attrsAtCaret()?.color);
    const currentHighlightHex = () => {
        const c = state.attrsAtCaret()?.bg_color;
        return c ? rgbToHex(c) : lastHighlightHex();
    };

    /* Every close path hands focus back to the editor (issues #35/#49).
       The popover is role="dialog", so the hidden input's pointerup
       re-grab deliberately skips clicks inside it — without an explicit
       hand-back, picking a swatch (or committing the hex input) unmounts
       the popover with the focused node and strands focus on <body>. */
    const pickText = async (hex: string) => {
        if (!ready()) return;
        const rgb = hexToRgb(hex);
        if (!rgb) return;
        setOpenTextPicker(false);
        focusEditorInput();
        await cmd.setColor(rgb[0], rgb[1], rgb[2]);
    };
    const clearText = async () => {
        if (!ready()) return;
        setOpenTextPicker(false);
        focusEditorInput();
        await cmd.setColor(0, 0, 0);
    };
    const pickHighlight = async (hex: string) => {
        if (!ready()) return;
        const rgb = hexToRgb(hex);
        if (!rgb) return;
        setLastHighlightHex(hex);
        setOpenHighlightPicker(false);
        focusEditorInput();
        await cmd.setHighlight(rgb[0], rgb[1], rgb[2]);
    };
    const clearHighlight = async () => {
        if (!ready()) return;
        setOpenHighlightPicker(false);
        focusEditorInput();
        await cmd.setHighlight(0, 0, 0, 0);
    };

    return (
        <div class="nge-color" role="group" aria-label="Colour">
            <div class="nge-color__wrap">
                <button
                    class="nge-btn nge-color__trigger"
                    type="button"
                    aria-haspopup="dialog"
                    aria-expanded={openTextPicker()}
                    aria-label="Text colour"
                    title="Text colour"
                    disabled={!ready()}
                    onClick={() => {
                        setOpenHighlightPicker(false);
                        setOpenTextPicker((v) => !v);
                    }}
                >
                    <span class="nge-color__triglyph">A</span>
                    <span
                        class="nge-color__bar"
                        style={{ background: currentTextHex() }}
                        aria-hidden="true"
                    />
                    <span class="nge-color__chev" aria-hidden="true">▾</span>
                </button>
                <Show when={openTextPicker()}>
                    <PickerPopover
                        kind="text"
                        swatches={TEXT_SWATCHES}
                        current={currentTextHex()}
                        onPick={(h) => void pickText(h)}
                        onClear={() => void clearText()}
                        onClose={() => setOpenTextPicker(false)}
                    />
                </Show>
            </div>
            <div class="nge-color__wrap">
                <button
                    class="nge-btn nge-color__trigger"
                    type="button"
                    aria-haspopup="dialog"
                    aria-expanded={openHighlightPicker()}
                    aria-label="Highlight colour"
                    title="Highlight colour"
                    disabled={!ready()}
                    onClick={() => {
                        setOpenTextPicker(false);
                        setOpenHighlightPicker((v) => !v);
                    }}
                >
                    <span class="nge-color__triglyph">▣</span>
                    <span
                        class="nge-color__bar"
                        style={{ background: currentHighlightHex() }}
                        aria-hidden="true"
                    />
                    <span class="nge-color__chev" aria-hidden="true">▾</span>
                </button>
                <Show when={openHighlightPicker()}>
                    <PickerPopover
                        kind="highlight"
                        swatches={HIGHLIGHT_SWATCHES}
                        current={currentHighlightHex()}
                        onPick={(h) => void pickHighlight(h)}
                        onClear={() => void clearHighlight()}
                        onClose={() => setOpenHighlightPicker(false)}
                    />
                </Show>
            </div>
        </div>
    );
};
