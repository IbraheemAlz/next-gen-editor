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
 *
 * Issue #423 — the family shown is the one the caret's script slot
 * RESOLVES to (`state.slotFormats()` / `resolvedFontLatin()` /
 * `resolvedFontCs()`: the run's name → its theme binding → the style
 * chain → docDefaults → the layout default), for the slot the caret's
 * own text reads (`caretFontSlot()` — the complex-script slot inside
 * Arabic text). A theme font carries a subtle "(theme)" marker; a family
 * the registry does not ship (Word's Calibri) still shows by name, as a
 * disabled placeholder option, instead of silently showing the first
 * registry font. Picks stay "Both" slots (Word's ribbon); per-slot picks
 * live in the Font dialog (issue #420).
 */
import { createSignal, createMemo, createEffect, For, Show, type Component } from 'solid-js';
import {
    createEditorCommands,
    createEditorState,
    useFontRegistry,
} from '@nge/core';
import { focusEditorInput } from './focus';
import './FontPickers.css';

const FONT_SIZES = [8, 9, 10, 11, 12, 14, 16, 18, 20, 24, 28, 32, 36, 48, 72];

export const FontPickers: Component = () => {
    const cmd = createEditorCommands();
    const state = createEditorState();
    const registry = useFontRegistry();
    const [pending, setPending] = createSignal(false);

    const ready = createMemo(() => state.selection() !== undefined);
    /* Issue #423 — the slot the caret's text reads, and its resolution. */
    const slotKey = () =>
        state.caretFontSlot() === 'ComplexScript' ? 'complex_script' : 'latin';
    const currentFamily = () =>
        state.slotFormats()?.[slotKey()].font_family ||
        state.attrsAtCaret()?.font_family ||
        '';
    const resolvedName = () =>
        (slotKey() === 'complex_script' ? state.resolvedFontCs() : state.resolvedFontLatin()) ??
        currentFamily();
    const fromTheme = () => state.fontSource()?.[slotKey()] === 'Theme';
    const inRegistry = () => registry.fonts().some((f) => f.id === currentFamily());
    const currentSize = () => state.attrsAtCaret()?.font_size ?? 12;

    /* The select's value is synced in a USER effect, which Solid runs
       after every DOM binding of the same update: a `value={…}` binding
       could run before the placeholder option's own `value` changed
       (Arial → Times New Roman, both unshipped) and leave nothing
       selected. */
    let familyEl: HTMLSelectElement | undefined;
    createEffect(() => {
        const id = currentFamily();
        registry.fonts();
        inRegistry();
        if (familyEl && familyEl.value !== id) familyEl.value = id;
    });

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
                    ref={familyEl}
                    class="nge-font__family"
                    aria-label="Font family"
                    aria-busy={pending()}
                    disabled={!ready() || pending()}
                    title={
                        fromTheme()
                            ? `${resolvedName()} — from the document theme`
                            : 'Font family'
                    }
                    data-nge-command="APPLY_FORMATTING"
                    onChange={(e) => void applyFamily(e.currentTarget.value)}
                >
                    {/* Issue #423 — a resolved family the registry does not
                        ship (a theme's Calibri) shows by name; it cannot be
                        picked (no bytes to load), only displayed. */}
                    <Show when={currentFamily() !== '' && !inRegistry()}>
                        <option value={currentFamily()} disabled data-nge-resolved-font="">
                            {resolvedName()}
                        </option>
                    </Show>
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
            <Show when={fromTheme()}>
                <span
                    class="nge-font__source"
                    data-nge-font-source="theme"
                    title="This font comes from the document theme"
                >
                    (theme)
                </span>
            </Show>
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
                onKeyDown={(e) => {
                    /* Enter commits and returns focus to the document
                       (Word behaviour). blur() fires the change handler
                       above exactly once — no duplicate dispatch. */
                    if (e.key === 'Enter') {
                        e.preventDefault();
                        e.currentTarget.blur();
                        focusEditorInput();
                    }
                }}
                list="nge-font-sizes"
            />
            <datalist id="nge-font-sizes">
                <For each={FONT_SIZES}>{(s) => <option value={s.toString()} />}</For>
            </datalist>
        </div>
    );
};
