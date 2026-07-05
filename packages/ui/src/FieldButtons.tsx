/**
 * FieldButtons — dynamic-field authoring (issue #43). One popover with
 * the three field kinds the layout engine resolves live:
 *
 *   - Page Number  → `PAGE`     (the page's formatted number)
 *   - Page Count   → `NUMPAGES` (total pages; forces full pagination)
 *   - Date         → `DATE`     (the worker-injected render date)
 *
 * Fields are authored at the caret in the BODY or in a header/footer
 * STORY — a page-number footer is the primary use, so this control
 * deliberately stays enabled while `state.editingStory()` is active.
 * The engine rejects table-cell carets (`Event::Error`; the cell
 * reader cannot round-trip fields yet) — the button greys out there
 * via `cellProperties`.
 */
import { Show, createMemo, createSignal, type Component } from 'solid-js';
import {
    createEditorCommands,
    createEditorState,
    type FieldKind,
} from '@nge/core';
import { focusEditorInput } from './focus';
import './FieldButtons.css';

interface FieldEntry {
    kind: FieldKind;
    label: string;
    hint: string;
}
const FIELDS: FieldEntry[] = [
    {
        kind: 'Page',
        label: 'Page Number',
        hint: 'The page’s own number — updates as pages reflow',
    },
    {
        kind: 'NumPages',
        label: 'Page Count',
        hint: 'Total pages in the document',
    },
    {
        kind: 'Date',
        label: 'Date',
        hint: 'Today’s date (updates when the document is opened)',
    },
];

export const FieldButtons: Component = () => {
    const cmd = createEditorCommands();
    const state = createEditorState();
    const [open, setOpen] = createSignal(false);

    const inCell = createMemo(() => state.cellProperties() !== undefined);
    const ready = createMemo(() => state.selection() !== undefined && !inCell());

    const insert = async (kind: FieldKind) => {
        setOpen(false);
        if (!ready()) return;
        await cmd.insertField(kind);
        focusEditorInput();
    };

    return (
        <div class="nge-fields" role="group" aria-label="Insert field">
            <button
                class="nge-btn nge-fields__trigger"
                type="button"
                aria-haspopup="menu"
                aria-expanded={open()}
                disabled={!ready()}
                title={
                    inCell()
                        ? 'Fields inside table cells aren’t supported yet'
                        : 'Insert a dynamic field (page number, page count, date)'
                }
                onClick={() => setOpen((v) => !v)}
            >
                <span aria-hidden="true">⧉</span>
                <span>Field</span>
            </button>
            <Show when={open()}>
                <ul
                    class="nge-fields__menu"
                    role="menu"
                    aria-label="Field kinds"
                    onMouseLeave={() => setOpen(false)}
                >
                    {FIELDS.map((f) => (
                        <li role="none">
                            <button
                                role="menuitem"
                                class="nge-fields__item"
                                type="button"
                                title={f.hint}
                                onClick={() => void insert(f.kind)}
                            >
                                <span>{f.label}</span>
                            </button>
                        </li>
                    ))}
                </ul>
            </Show>
        </div>
    );
};
