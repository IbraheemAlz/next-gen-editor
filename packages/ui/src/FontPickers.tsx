/**
 * FontPickers — data-driven font-family `<select>` + font-size input.
 *
 * The family list is populated reactively from the `FontRegistry`
 * (`public/fonts.json`), not a hard-coded array — add a font to the
 * manifest and it shows up here with no code change.
 *
 * Selecting a font is just-in-time: `ensureFont(id)` fetches the `.ttf`
 * and pushes it to the engine if it is not already resident (the
 * registry dedups + caches, so a second pick of the same font is
 * instant and never re-fetches), then `cmd.setFontFamily` applies it.
 * While the bytes are in flight the control shows a pending state so
 * the async boundary is visible.
 *
 * Both controls reflect the resolved value at the caret on selection
 * change so the indicator matches Word / Google Docs behaviour.
 */
import { createSignal, createMemo, For, Show, type Component } from 'solid-js';
import {
    createEditorCommands,
    createEditorState,
    useFontRegistry,
} from '@nge/core';
import './FontPickers.css';

const FONT_SIZES = [8, 9, 10, 11, 12, 14, 16, 18, 20, 24, 28, 32, 36, 48, 72];

export const FontPickers: Component = () => {
    const cmd = createEditorCommands();
    const state = createEditorState();
    const registry = useFontRegistry();
    const [pending, setPending] = createSignal(false);

    const ready = createMemo(() => state.selection() !== undefined);
    const currentFamily = () => state.attrsAtCaret()?.font_family ?? '';
    const currentSize = () => state.attrsAtCaret()?.font_size ?? 12;

    const applyFamily = async (id: string) => {
        if (!ready() || !id) return;
        setPending(true);
        try {
            /* JIT: load the bytes into the engine (no-op if already
             * resident — the registry caches + dedups), then apply. */
            await registry.ensureFont(id);
            await cmd.setFontFamily(id);
        } catch (e) {
            /* Surface to the console; the dropdown reverts to the
             * caret's resolved family on the next SELECTION_CHANGED. */
            console.error('[FontPickers] load/apply failed', e);
        } finally {
            setPending(false);
        }
    };
    const applySize = async (pt: number) => {
        if (!ready() || !Number.isFinite(pt) || pt <= 0) return;
        await cmd.setFontSize(pt);
    };

    return (
        <div class="nge-font" role="group" aria-label="Font" data-pending={pending()}>
            <div class="nge-font__familywrap">
                <select
                    class="nge-font__family"
                    aria-label="Font family"
                    aria-busy={pending()}
                    disabled={!ready() || pending()}
                    value={currentFamily()}
                    onChange={(e) => void applyFamily(e.currentTarget.value)}
                >
                    <For each={registry.fonts()}>
                        {(f) => (
                            <option value={f.id}>
                                {f.label}
                                {registry.stateOf(f.id) === 'loaded' ? '' : ' ·'}
                            </option>
                        )}
                    </For>
                </select>
                <Show when={pending()}>
                    <span class="nge-font__spinner" aria-hidden="true" />
                </Show>
            </div>
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
                <For each={FONT_SIZES}>{(s) => <option value={s.toString()} />}</For>
            </datalist>
        </div>
    );
};
