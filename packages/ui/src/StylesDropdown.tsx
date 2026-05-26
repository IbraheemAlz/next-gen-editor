/**
 * StylesDropdown — paragraph styles picker.
 *
 * **Faux styles** (Sprint 5 UI Edition): each entry dispatches
 * `APPLY_FORMATTING` with a preset `TextAttrsPatch` (font size +
 * bold weight). It does NOT write `<w:pStyle>` — the engine has
 * no live style table to mutate today. See the project backlog
 * issue "Core: Implement Live Style Table and Cascade Re-application".
 *
 * Visual presets are exported from `@nge/core` (`STYLE_PRESETS`) so
 * downstream UIs can extend or override them without duplicating
 * the canonical hierarchy.
 */
import { createSignal, For, Show, type Component } from 'solid-js';
import {
    createEditorCommands,
    createEditorState,
    STYLE_PRESETS,
    type ParagraphStyleId,
} from '@nge/core';
import './StylesDropdown.css';

const STYLES: { id: ParagraphStyleId; label: string }[] = [
    { id: 'Normal', label: 'Normal' },
    { id: 'Title', label: 'Title' },
    { id: 'Heading1', label: 'Heading 1' },
    { id: 'Heading2', label: 'Heading 2' },
    { id: 'Heading3', label: 'Heading 3' },
];

export const StylesDropdown: Component = () => {
    const cmd = createEditorCommands();
    const state = createEditorState();
    const [open, setOpen] = createSignal(false);
    const [active, setActive] = createSignal<ParagraphStyleId>('Normal');

    const ready = () => state.selection() !== undefined;

    const apply = async (id: ParagraphStyleId) => {
        setActive(id);
        setOpen(false);
        await cmd.applyStyle(id);
    };

    const previewStyle = (id: ParagraphStyleId): string => {
        const p = STYLE_PRESETS[id];
        const parts: string[] = [];
        if (p.font_size) parts.push(`font-size: ${p.font_size}px`);
        if (p.bold) parts.push('font-weight: 700');
        return parts.join('; ');
    };

    return (
        <div class="nge-styles">
            <button
                class="nge-btn nge-styles__trigger"
                type="button"
                aria-haspopup="menu"
                aria-expanded={open()}
                aria-label="Paragraph style"
                disabled={!ready()}
                title="Paragraph style"
                onClick={() => setOpen((v) => !v)}
            >
                <span>{STYLES.find((s) => s.id === active())?.label}</span>
                <span class="nge-styles__chevron" aria-hidden="true">▾</span>
            </button>
            <Show when={open()}>
                <ul
                    class="nge-styles__menu"
                    role="menu"
                    onMouseLeave={() => setOpen(false)}
                >
                    <For each={STYLES}>
                        {(s) => (
                            <li role="none">
                                <button
                                    class="nge-styles__item"
                                    type="button"
                                    role="menuitemradio"
                                    aria-checked={active() === s.id}
                                    onClick={() => void apply(s.id)}
                                >
                                    <span class="nge-styles__preview" style={previewStyle(s.id)}>
                                        {s.label}
                                    </span>
                                </button>
                            </li>
                        )}
                    </For>
                    <li role="none" class="nge-styles__note">
                        Faux styles — visual only. No <code>&lt;w:pStyle&gt;</code> yet.
                    </li>
                </ul>
            </Show>
        </div>
    );
};
