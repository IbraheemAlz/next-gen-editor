/**
 * UnderlineStyleDropdown — toolbar combo: U button + style picker chevron.
 *
 * Click the U button → toggle Single underline.
 * Click the chevron → popover with 5 styles: Single / Double / Dotted /
 * Dashed / Wavy. All five round-trip through `Command::ApplyFormatting`
 * with the `UnderlineStyle` enum (`common.rs:174`).
 *
 * Before this component lived, only `Single` was reachable from the UI.
 */
import { createSignal, For, Show, type Component } from 'solid-js';
import {
    createEditorCommands,
    createEditorState,
    type UnderlineStyle,
} from '@nge/core';
import './UnderlineStyleDropdown.css';

const STYLES: { value: UnderlineStyle; label: string; preview: string }[] = [
    { value: 'Single', label: 'Single', preview: '────────' },
    { value: 'Double', label: 'Double', preview: '════════' },
    { value: 'Dotted', label: 'Dotted', preview: '········' },
    { value: 'Dashed', label: 'Dashed', preview: '╴ ╴ ╴ ╴' },
    { value: 'Wavy', label: 'Wavy', preview: '∿∿∿∿∿∿∿' },
];

export interface UnderlineStyleDropdownProps {
    /** Default style applied when the U button is clicked directly. */
    defaultStyle?: UnderlineStyle;
}

export const UnderlineStyleDropdown: Component<UnderlineStyleDropdownProps> = (props) => {
    const cmd = createEditorCommands();
    const state = createEditorState();
    const [open, setOpen] = createSignal(false);
    const [lastStyle, setLastStyle] = createSignal<UnderlineStyle>(
        props.defaultStyle ?? 'Single',
    );

    const isActive = () => {
        const attrs = state.attrsAtCaret();
        return attrs ? attrs.underline !== 'None' : false;
    };

    const applyToggle = async () => {
        await cmd.setUnderline(isActive() ? 'None' : lastStyle());
    };

    const choose = async (style: UnderlineStyle) => {
        setLastStyle(style);
        setOpen(false);
        await cmd.setUnderline(style);
    };

    return (
        <div class="nge-underline">
            <button
                class="nge-btn nge-underline__main"
                type="button"
                aria-label="Underline"
                aria-pressed={isActive()}
                data-active={isActive()}
                title="Underline (Ctrl+U)"
                onClick={() => void applyToggle()}
            >
                <span class="nge-underline__glyph">U</span>
            </button>
            <button
                class="nge-btn nge-underline__chevron"
                type="button"
                aria-haspopup="menu"
                aria-expanded={open()}
                aria-label="Underline style"
                title="Underline style"
                onClick={() => setOpen((v) => !v)}
            >
                ▾
            </button>
            <Show when={open()}>
                <ul
                    class="nge-underline__menu"
                    role="menu"
                    onMouseLeave={() => setOpen(false)}
                >
                    <For each={STYLES}>
                        {(s) => (
                            <li role="none">
                                <button
                                    role="menuitemradio"
                                    aria-checked={lastStyle() === s.value}
                                    class="nge-underline__item"
                                    type="button"
                                    onClick={() => void choose(s.value)}
                                >
                                    <span class="nge-underline__label">{s.label}</span>
                                    <span class="nge-underline__preview">{s.preview}</span>
                                </button>
                            </li>
                        )}
                    </For>
                    <li role="none">
                        <button
                            role="menuitem"
                            class="nge-underline__item nge-underline__item--clear"
                            type="button"
                            onClick={() => void choose('None')}
                        >
                            Clear underline
                        </button>
                    </li>
                </ul>
            </Show>
        </div>
    );
};
