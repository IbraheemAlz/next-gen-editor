/**
 * NoteButtons — footnote / endnote authoring (issue #80).
 *
 *   - Body mode: "Footnote" / "Endnote" dispatch
 *     `Command::InsertFootnote` / `Command::InsertEndnote` at the caret.
 *     The engine splices the reference, renumbers every later marker
 *     and ENTERS the new note, so typing lands in the note immediately
 *     (Word parity). Disabled inside table cells — the engine rejects
 *     them (notes inside tables are out of scope for #80).
 *   - Note mode (`state.editingStory()?.area` is `'Footnote'` /
 *     `'Endnote'`): a single "Close" button returns to the body right
 *     after the reference (`Command::ExitHeaderFooter`; Esc and a click
 *     in the body text do the same).
 *
 * Every path here is real engine behaviour — no "Engine pending" gate.
 * Inside a header/footer story both inserts are disabled (the engine's
 * story gate rejects them; notes cannot nest in another story).
 */
import { Show, createMemo, type Component } from 'solid-js';
import { createEditorCommands, createEditorState } from '@nge/core';
import { focusEditorInput } from './focus';
import './NoteButtons.css';

export const NoteButtons: Component = () => {
    const cmd = createEditorCommands();
    const state = createEditorState();

    const ready = createMemo(() => state.selection() !== undefined);
    const inCell = createMemo(() => state.cellProperties() !== undefined);
    const story = () => state.editingStory();
    const noteArea = createMemo(() => {
        const area = story()?.area;
        return area === 'Footnote' || area === 'Endnote' ? area : undefined;
    });
    const canInsert = createMemo(() => ready() && !inCell() && story() === undefined);

    const why = (): string | undefined => {
        if (story() !== undefined) return 'Close the header or footer first';
        if (inCell()) return 'Notes inside table cells aren’t supported yet';
        return undefined;
    };

    const insert = async (kind: 'Footnote' | 'Endnote') => {
        if (!canInsert()) return;
        if (kind === 'Footnote') await cmd.insertFootnote();
        else await cmd.insertEndnote();
        focusEditorInput();
    };

    const close = async () => {
        await cmd.exitHeaderFooter();
        focusEditorInput();
    };

    return (
        <div class="nge-notes" role="group" aria-label="Footnotes and endnotes">
            <Show
                when={noteArea()}
                fallback={
                    <>
                        <button
                            class="nge-btn nge-notes__btn"
                            type="button"
                            aria-label="Insert footnote"
                            title={why() ?? 'Insert a footnote at the caret'}
                            disabled={!canInsert()}
                            onClick={() => void insert('Footnote')}
                        >
                            <span class="nge-notes__icon" aria-hidden="true">
                                ab¹
                            </span>
                            <span>Footnote</span>
                        </button>
                        <button
                            class="nge-btn nge-notes__btn"
                            type="button"
                            aria-label="Insert endnote"
                            title={why() ?? 'Insert an endnote at the caret'}
                            disabled={!canInsert()}
                            onClick={() => void insert('Endnote')}
                        >
                            <span class="nge-notes__icon" aria-hidden="true">
                                abⁱ
                            </span>
                            <span>Endnote</span>
                        </button>
                    </>
                }
            >
                {(area) => (
                    <button
                        class="nge-btn nge-notes__btn nge-notes__btn--close"
                        type="button"
                        aria-label={`Close ${area().toLowerCase()} editing`}
                        title={`Return to the document body (Esc)`}
                        onClick={() => void close()}
                    >
                        <span aria-hidden="true">✕</span>
                        <span>Close {area()}</span>
                    </button>
                )}
            </Show>
        </div>
    );
};
