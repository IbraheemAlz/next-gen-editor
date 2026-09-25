/**
 * ImageWrapPicker — the image toolbar's text-wrap picker (issue #82).
 *
 * Word's "Wrap Text" menu for the selected picture: Square, Tight,
 * Through, Top and bottom, Behind text, In front of text. Each entry
 * dispatches `Command::SetImageWrap` (`cmd.setImageWrap`); the engine
 * re-runs its anchor → position → wrap → reflow loop and the body text
 * flows around the picture. The checked entry mirrors the engine's
 * `ImageRect.wrap` for the selected image — the picker never keeps its
 * own copy of the mode.
 *
 * Image selection is shell-side (the canvas overlay owns it), so the
 * host passes the selected image in through `image`. An INLINE picture
 * has no wrap mode: converting inline ↔ floating is not implemented in
 * the engine (it answers `ERROR`), so for an inline image the picker is
 * visibly gated — disabled, with the amber "Engine pending" badge (Honest
 * UX; the conversion is a tracked gap of the #82 follow-up).
 */
import { For, Show, createMemo, type Component } from 'solid-js';
import { createEditorCommands, type BlockPath, type ImageWrapMode } from '@nge/core';
import { focusEditorInput } from './focus';
import './ImageWrapPicker.css';

/** The picture the picker acts on. */
export interface ImageWrapTarget {
    path: BlockPath;
    at: number;
    /** `true` for a floating (`<wp:anchor>`) picture. */
    floating: boolean;
    /** The engine-reported mode (`ImageRect.wrap`); `undefined` inline. */
    wrap: ImageWrapMode | undefined;
}

export interface ImageWrapPickerProps {
    /** The selected picture, or `undefined` when none is selected. */
    image: () => ImageWrapTarget | undefined;
}

const MODES: { mode: ImageWrapMode; label: string; glyph: string; hint: string }[] = [
    { mode: 'square', label: 'Square', glyph: '▤', hint: 'Text wraps around the picture’s box' },
    { mode: 'tight', label: 'Tight', glyph: '◧', hint: 'Text wraps close to the picture’s outline' },
    { mode: 'through', label: 'Through', glyph: '◨', hint: 'Text flows through the picture’s outline' },
    {
        mode: 'top_and_bottom',
        label: 'Top and bottom',
        glyph: '☰',
        hint: 'Text stays above and below the picture',
    },
    { mode: 'behind_text', label: 'Behind text', glyph: '▢', hint: 'The picture sits behind the text' },
    {
        mode: 'in_front_of_text',
        label: 'In front of text',
        glyph: '▣',
        hint: 'The picture sits over the text',
    },
];

export const ImageWrapPicker: Component<ImageWrapPickerProps> = (props) => {
    const cmd = createEditorCommands();
    const target = createMemo(() => props.image());
    const floating = createMemo(() => target()?.floating === true);

    const choose = async (mode: ImageWrapMode) => {
        const im = target();
        if (!im || !im.floating || im.wrap === mode) return;
        await cmd.setImageWrap(im.path, im.at, mode);
        focusEditorInput();
    };

    return (
        <Show when={target()}>
            <div
                class="nge-image-wrap"
                role="radiogroup"
                aria-label="Wrap text around the picture"
                onPointerDown={(e) => e.stopPropagation()}
            >
                <span class="nge-image-wrap__label">Wrap</span>
                <For each={MODES}>
                    {(m) => (
                        <button
                            class="nge-btn nge-image-wrap__btn"
                            classList={{ 'nge-image-wrap__btn--on': target()?.wrap === m.mode }}
                            type="button"
                            role="radio"
                            aria-checked={target()?.wrap === m.mode}
                            aria-label={m.label}
                            title={
                                floating()
                                    ? m.hint
                                    : 'Wrap modes: engine pending for inline pictures (inline ↔ floating conversion)'
                            }
                            disabled={!floating()}
                            onClick={() => void choose(m.mode)}
                        >
                            <span class="nge-image-wrap__glyph" aria-hidden="true">
                                {m.glyph}
                            </span>
                        </button>
                    )}
                </For>
                <Show when={!floating()}>
                    <span class="nge-image-wrap__badge">Engine pending</span>
                </Show>
            </div>
        </Show>
    );
};
