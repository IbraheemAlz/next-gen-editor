/* Phase 4 §11 — formatting toolbar.
 *
 * Reads engine state via the store (`attrsAtCaret`, `undoState`) and dispatches
 * `ApplyFormatting` / `Undo` / `Redo`. It holds no document state — the B/I/U
 * pressed state is whatever the engine last reported (§9 invariant).
 *
 * Alignment + font-family pickers from §11 are deferred — neither has engine
 * support yet (see BACKLOG.md). */
import { For } from 'solid-js';
import type { EngineClient } from '../engine/engine-client';
import type { Color, TextAttrsPatch } from '../engine/types';
import type { EngineStore } from '../state/engine-store';

const FONT_SIZES = [12, 14, 16, 18, 20, 24, 32, 48, 64];
const DEFAULT_COLOR: Color = { r: 0, g: 0, b: 0, a: 255 };

/** A `TextAttrsPatch` with every field unset — the base for a sparse patch. */
function emptyPatch(): TextAttrsPatch {
    return {
        bold: undefined,
        italic: undefined,
        underline: undefined,
        strike: undefined,
        font_family: undefined,
        font_size: undefined,
        color: undefined,
        bg_color: undefined,
        script: undefined,
        language: undefined,
    };
}

function colorToHex(c: Color): string {
    const h = (n: number) => n.toString(16).padStart(2, '0');
    return `#${h(c.r)}${h(c.g)}${h(c.b)}`;
}

function hexToColor(hex: string): Color {
    return {
        r: parseInt(hex.slice(1, 3), 16),
        g: parseInt(hex.slice(3, 5), 16),
        b: parseInt(hex.slice(5, 7), 16),
        a: 255,
    };
}

export function Toolbar(props: { client: EngineClient; store: EngineStore }) {
    const attrs = () => props.store.attrsAtCaret();
    const bold = () => attrs()?.bold ?? false;
    const italic = () => attrs()?.italic ?? false;
    const underline = () => (attrs()?.underline ?? 'None') !== 'None';
    const fontSize = () => attrs()?.font_size ?? 24;
    const color = () => attrs()?.color ?? DEFAULT_COLOR;
    const undo = () => props.store.undoState();

    /* ApplyFormatting acts on the current selection; a collapsed caret is a
       no-op in the engine (nothing to format). */
    const apply = (patch: Partial<TextAttrsPatch>): void => {
        void props.client.dispatch({
            type: 'APPLY_FORMATTING',
            range: props.store.selection().range,
            attrs: { ...emptyPatch(), ...patch },
        });
    };

    return (
        <div class="toolbar" role="toolbar" aria-label="Formatting">
            <button
                class="tb-btn"
                onClick={() => void props.client.dispatch({ type: 'UNDO' })}
                disabled={!undo().canUndo}
                aria-label="Undo"
            >
                ↶
            </button>
            <button
                class="tb-btn"
                onClick={() => void props.client.dispatch({ type: 'REDO' })}
                disabled={!undo().canRedo}
                aria-label="Redo"
            >
                ↷
            </button>
            <span class="tb-sep" />
            <button
                class="tb-btn tb-b"
                classList={{ active: bold() }}
                aria-pressed={bold()}
                aria-label="Bold"
                onClick={() => apply({ bold: !bold() })}
            >
                B
            </button>
            <button
                class="tb-btn tb-i"
                classList={{ active: italic() }}
                aria-pressed={italic()}
                aria-label="Italic"
                onClick={() => apply({ italic: !italic() })}
            >
                I
            </button>
            <button
                class="tb-btn tb-u"
                classList={{ active: underline() }}
                aria-pressed={underline()}
                aria-label="Underline"
                onClick={() => apply({ underline: underline() ? 'None' : 'Single' })}
            >
                U
            </button>
            <span class="tb-sep" />
            <label class="tb-field">
                Size
                <select
                    value={String(fontSize())}
                    onChange={(e) => apply({ font_size: Number(e.currentTarget.value) })}
                >
                    <For each={FONT_SIZES}>{(s) => <option value={String(s)}>{s}</option>}</For>
                </select>
            </label>
            <label class="tb-field">
                Color
                <input
                    type="color"
                    value={colorToHex(color())}
                    onChange={(e) => apply({ color: hexToColor(e.currentTarget.value) })}
                />
            </label>
        </div>
    );
}
