/**
 * InsertTableButton — single-click insert of a 3×3 table at the caret.
 *
 * The fuller table grid-picker (drag to choose rows × cols) is a
 * follow-up; the 3×3 default mirrors Word's quick-insert and exercises
 * the existing `cmd.insertTableAtCaret` path. Cell-level editing
 * surfaces via the right-click `TableContextMenu` already mounted by
 * `SdkShelf`.
 */
import { createMemo, type Component } from 'solid-js';
import { createEditorCommands, createEditorState } from '@nge/core';
import { protectionGate } from './protectionGate';
import './InsertTableButton.css';

export interface InsertTableButtonProps {
    rows?: number;
    cols?: number;
}

export const InsertTableButton: Component<InsertTableButtonProps> = (props) => {
    const cmd = createEditorCommands();
    const state = createEditorState();

    const ready = createMemo(() => state.selection() !== undefined);
    /* Issue #449 - INSERT_TABLE is refused by every protection mode. */
    const gate = protectionGate(state, 'insertTableAtCaret');
    const rows = () => props.rows ?? 3;
    const cols = () => props.cols ?? 3;

    const insert = async () => {
        if (!ready() || gate.locked()) return;
        await cmd.insertTableAtCaret(rows(), cols());
    };

    return (
        <button
            class="nge-btn nge-itable"
            type="button"
            aria-label={`Insert ${rows()}×${cols()} table`}
            title={gate.title(`Insert ${rows()}×${cols()} table`)}
            disabled={!ready() || gate.locked()}
            onClick={() => void insert()}
        >
            <span class="nge-itable__icon" aria-hidden="true">▦</span>
            <span class="nge-itable__label">Table</span>
        </button>
    );
};
