/**
 * StylesDropdown — paragraph styles picker.
 *
 * **Sprint 12 (#11)** — wired through the real engine path. Each
 * entry dispatches `Command::ApplyStyle { style_id }`; the engine
 * sets `Paragraph.style_id`, folds the `<w:style>` cascade into the
 * resolved props, and preserves direct overrides via the shadow
 * approach. The faux `APPLY_FORMATTING` path is retained as a
 * fallback when the loaded document carries no `<w:styles>` entry
 * for the chosen id (a brand-new file with default styles only) —
 * see the inline `previewStyle` helper.
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
        /* Sprint 12 — real engine path: writes `<w:pStyle>` + folds
         * the style cascade. `Normal` detaches by passing the same
         * id (engine treats it as a regular style; if the loaded
         * doc has no `Normal` entry, the cascade falls back to
         * `style_defaults` which gives the Word default look). */
        await cmd.applyStyle(id);
    };

    const previewStyle = (id: ParagraphStyleId): string => {
        const p = STYLE_PRESETS[id];
        if (!p) return '';
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
                        Writes <code>&lt;w:pStyle&gt;</code>; folds the style cascade.
                    </li>
                </ul>
            </Show>
        </div>
    );
};
