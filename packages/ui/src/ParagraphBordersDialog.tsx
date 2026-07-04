/**
 * ParagraphBordersDialog — modal per-edge border picker for the caret
 * paragraph (issue #41). Replaces the old binary "single hairline on/off"
 * toggle with the full style/width/colour + per-side editor shared with
 * the cell border dialog (`BorderEditor`).
 *
 * Prefills each edge from `state.paragraphBorders()` (the additive
 * `Event::SelectionChanged.paragraph_borders` read-back) so it opens
 * reflecting the paragraph's live borders. Apply dispatches
 * `setParagraphBorders`; when every edge is cleared it routes through
 * `clearParagraphBorders` so the `<w:pBdr>` element is dropped entirely.
 */
import { createSignal, createEffect, type Component } from 'solid-js';
import {
    createEditorCommands,
    createEditorState,
    type BridgeCellBorders,
} from '@nge/core';
import { Dialog } from './Dialog';
import { BorderEditor } from './BorderEditor';

const EMPTY_BORDERS: BridgeCellBorders = {
    top: undefined,
    right: undefined,
    bottom: undefined,
    left: undefined,
};

const anyEdge = (b: BridgeCellBorders): boolean =>
    b.top !== undefined ||
    b.right !== undefined ||
    b.bottom !== undefined ||
    b.left !== undefined;

export interface ParagraphBordersDialogProps {
    open: boolean;
    onClose: () => void;
}

export const ParagraphBordersDialog: Component<ParagraphBordersDialogProps> = (
    props,
) => {
    const cmd = createEditorCommands();
    const state = createEditorState();
    const [borders, setBorders] = createSignal<BridgeCellBorders>(EMPTY_BORDERS);
    const [error, setError] = createSignal<string | null>(null);

    createEffect(() => {
        if (!props.open) return;
        setBorders(state.paragraphBorders() ?? EMPTY_BORDERS);
        setError(null);
    });

    const apply = async () => {
        try {
            if (anyEdge(borders())) {
                await cmd.setParagraphBorders(borders());
            } else {
                await cmd.clearParagraphBorders();
            }
            props.onClose();
        } catch (e) {
            setError(String(e));
        }
    };

    return (
        <Dialog
            open={props.open}
            title="Paragraph Borders"
            onClose={props.onClose}
            size="md"
            footer={
                <>
                    <button class="nge-btn" type="button" onClick={props.onClose}>
                        Cancel
                    </button>
                    <button
                        class="nge-btn nge-btn--primary"
                        type="button"
                        onClick={() => void apply()}
                    >
                        Apply
                    </button>
                </>
            }
        >
            <form class="nge-form" onSubmit={(e) => e.preventDefault()}>
                <div class="nge-form__row nge-form__row--block">
                    <span class="nge-form__label">Borders</span>
                    <BorderEditor
                        value={borders()}
                        onChange={setBorders}
                        idPrefix="nge-para"
                    />
                </div>
                {error() && (
                    <div class="nge-form__hint" role="alert" style={{ color: 'var(--nge-color-danger)' }}>
                        {error()}
                    </div>
                )}
            </form>
        </Dialog>
    );
};
