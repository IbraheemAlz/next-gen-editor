/**
 * AlignmentButtons — Start / Center / End / Justify + Ltr / Rtl
 * toggles. Dispatches `cmd.setParagraphAlign` and
 * `cmd.setParagraphDirection`. Active state reflects the paragraph
 * under the caret via `state.paragraphAlignment()` /
 * `paragraphDirection()`.
 *
 * Engine `Alignment` is logical (Start/End), not physical (Left/Right):
 * the BiDi paragraph direction decides which edge that maps to. The
 * button glyphs render the LTR-canonical icons; under RTL the engine
 * flips the actual rendering.
 */
import { type Component } from 'solid-js';
import {
    createEditorCommands,
    createEditorState,
    type Alignment,
    type Direction,
} from '@nge/core';
import './AlignmentButtons.css';

interface AlignBtn { value: Alignment; label: string; glyph: string }
const ALIGN_BTNS: AlignBtn[] = [
    { value: 'Start',   label: 'Align start',   glyph: '⯇' },
    { value: 'Center',  label: 'Align center',  glyph: '☰' },
    { value: 'End',     label: 'Align end',     glyph: '⯈' },
    { value: 'Justify', label: 'Justify',       glyph: '☷' },
];

interface DirBtn { value: Direction; label: string; glyph: string }
const DIR_BTNS: DirBtn[] = [
    { value: 'Ltr', label: 'Left to right', glyph: '→¶' },
    { value: 'Rtl', label: 'Right to left', glyph: '¶←' },
];

export const AlignmentButtons: Component = () => {
    const cmd = createEditorCommands();
    const state = createEditorState();

    /* Story editing v2 (#72) lifted the engine's story-mode gate for
       SetParagraphAlign / SetParagraphDirection. */
    const ready = () => state.selection() !== undefined;
    const currentAlign = () => state.paragraphAlignment();
    const currentDir = () => state.paragraphDirection();

    return (
        <div class="nge-align" role="group" aria-label="Alignment and direction">
            <div class="nge-align__group" role="group" aria-label="Paragraph alignment">
                {ALIGN_BTNS.map((b) => (
                    <button
                        class="nge-btn nge-btn--icon nge-align__btn"
                        type="button"
                        aria-label={b.label}
                        aria-pressed={currentAlign() === b.value}
                        data-active={currentAlign() === b.value}
                        title={b.label}
                        disabled={!ready()}
                        onClick={() => void cmd.setParagraphAlign(b.value)}
                    >
                        {b.glyph}
                    </button>
                ))}
            </div>
            <div class="nge-align__group" role="group" aria-label="Paragraph direction">
                {DIR_BTNS.map((b) => (
                    <button
                        class="nge-btn nge-align__btn nge-align__btn--dir"
                        type="button"
                        aria-label={b.label}
                        aria-pressed={currentDir() === b.value}
                        data-active={currentDir() === b.value}
                        title={b.label}
                        disabled={!ready()}
                        onClick={() => void cmd.setParagraphDirection(b.value)}
                    >
                        {b.glyph}
                    </button>
                ))}
            </div>
        </div>
    );
};
