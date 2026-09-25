/**
 * FieldButtons — dynamic-field authoring (issues #43 / #77). One
 * toolbar group with three affordances:
 *
 *   1. **Insert** — a popover listing every field kind the engine
 *      resolves live:
 *        Page Number → `PAGE`      (the page's formatted number)
 *        Page Count  → `NUMPAGES`  (total pages; forces full pagination)
 *        Date        → `DATE`      (the worker-injected render date)
 *        Time        → `TIME`      (the worker-injected clock, #77)
 *        File Name   → `FILENAME`  (the opened file's name, #77)
 *        Author      → `AUTHOR`    (`docProps/core.xml` creator, #77)
 *      Fields are authored at the caret in the BODY or in a
 *      header/footer STORY — a page-number footer is the primary use,
 *      so the insert entries stay enabled while `state.editingStory()`
 *      is active. The engine rejects table-cell carets (`Event::Error`;
 *      the cell reader cannot round-trip fields yet) — the insert
 *      entries grey out there via `cellProperties`.
 *   2. **Update fields (F9)** — `Command::UpdateFields`: every field
 *      (body + every part) is re-resolved and stamped as ONE undo step.
 *   3. **Field codes (Alt+F9)** — `Command::SetFieldCodeView`: a
 *      pressed toggle mirroring `state.fieldCodeView()`; while on, the
 *      canvas paints `{ INSTRUCTION }` codes in place of results.
 *
 * Issue #81 — the menu also carries **Table of Contents**
 * (`Command::InsertToc`, Word's `TOC \o "1-3" \h \z \u`): the entries are
 * generated from the document's headings with dot leaders and live page
 * numbers. A TOC belongs in the body, so the entry greys out in table
 * cells and header / footer / note stories. With the caret in a TOC the
 * field chip grows an **Update table** button (F9 regenerates every TOC
 * from the current headings and pagination). Real engine behaviour end
 * to end — no "Engine pending" gate.
 *
 * When the selection addresses a field (`state.fieldAtCaret()` — a
 * click inside a field selects it whole, issue #77), a chip names its
 * keyword and opens the **field-code editor**: a plain `<input>`
 * prefilled with the instruction; Apply dispatches
 * `Command::SetFieldInstruction` against the field's own position (the
 * cached result stays until the next update — Word parity).
 *
 * Every path here is real engine behaviour — no "Engine pending" gate
 * is needed. FILENAME resolves only when the document was opened with
 * a name (`OpenDocument.name`) and AUTHOR only when the file carries a
 * `docProps/core.xml` creator; otherwise the inserted cached text
 * stands, exactly as in a viewer that never evaluates fields.
 */
import { Show, createEffect, createMemo, createSignal, type Component } from 'solid-js';
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
    {
        kind: 'Time',
        label: 'Time',
        hint: 'The current time (updates when the document is opened)',
    },
    {
        kind: 'FileName',
        label: 'File Name',
        hint: 'The document’s file name (resolves once the document has a name)',
    },
    {
        kind: 'Author',
        label: 'Author',
        hint: 'The document author from its core properties',
    },
];

export const FieldButtons: Component = () => {
    const cmd = createEditorCommands();
    const state = createEditorState();
    const [open, setOpen] = createSignal(false);
    const [editing, setEditing] = createSignal(false);
    const [draft, setDraft] = createSignal('');

    const inCell = createMemo(() => state.cellProperties() !== undefined);
    const ready = createMemo(() => state.selection() !== undefined);
    const canInsert = createMemo(() => ready() && !inCell());
    /* Issue #81 — a TOC is body-only. */
    const canInsertToc = createMemo(
        () => canInsert() && state.editingStory() === undefined,
    );
    const inToc = () => field()?.keyword === 'TOC';
    const codes = () => state.fieldCodeView();
    const field = () => state.fieldAtCaret();

    /* The editor draft follows the addressed field until the user starts
       typing; leaving the field closes the editor. */
    createEffect(() => {
        const f = field();
        if (!f) {
            setEditing(false);
            return;
        }
        if (!editing()) setDraft(f.instruction);
    });

    const insert = async (kind: FieldKind) => {
        setOpen(false);
        if (!canInsert()) return;
        await cmd.insertField(kind);
        focusEditorInput();
    };

    const insertToc = async () => {
        setOpen(false);
        if (!canInsertToc()) return;
        await cmd.insertToc();
        focusEditorInput();
    };

    const update = async () => {
        setOpen(false);
        if (!ready()) return;
        await cmd.updateFields();
        focusEditorInput();
    };

    const toggleCodes = async () => {
        setOpen(false);
        if (!ready()) return;
        await cmd.setFieldCodeView(!codes());
        focusEditorInput();
    };

    const applyCode = async () => {
        const f = field();
        const code = draft().trim();
        if (!f || code.length === 0) return;
        setEditing(false);
        /* Address the field by its own end so the edit lands on THIS
           field whatever the selection did meanwhile. */
        await cmd.setFieldInstruction(code, f.end);
        focusEditorInput();
    };

    const cancelCode = () => {
        setEditing(false);
        setDraft(field()?.instruction ?? '');
        focusEditorInput();
    };

    return (
        <div class="nge-fields" role="group" aria-label="Fields">
            <button
                class="nge-btn nge-fields__trigger"
                type="button"
                aria-haspopup="menu"
                aria-expanded={open()}
                disabled={!ready()}
                title="Insert a dynamic field, update fields (F9), or show field codes (Alt+F9)"
                onClick={() => setOpen((v) => !v)}
            >
                <span aria-hidden="true">⧉</span>
                <span>Field</span>
            </button>
            <button
                class={`nge-btn nge-fields__toggle${codes() ? ' nge-fields__toggle--on' : ''}`}
                type="button"
                aria-pressed={codes()}
                disabled={!ready()}
                title="Show field codes instead of results (Alt+F9)"
                onClick={() => void toggleCodes()}
            >
                <span aria-hidden="true">{'{ }'}</span>
                <span class="nge-fields__toggle-label">Codes</span>
            </button>
            <Show when={field()}>
                {(f) => (
                    <span class="nge-fields__chip" title={f().instruction}>
                        <span class="nge-fields__chip-key">{f().keyword}</span>
                        <Show when={inToc()}>
                            <button
                                class="nge-fields__chip-btn"
                                type="button"
                                title="Rebuild the table of contents from the current headings and page numbers (F9)"
                                onClick={() => void update()}
                            >
                                Update table
                            </button>
                        </Show>
                        <button
                            class="nge-fields__chip-btn"
                            type="button"
                            title="Edit this field’s code"
                            aria-expanded={editing()}
                            onClick={() => {
                                setDraft(f().instruction);
                                setEditing((v) => !v);
                            }}
                        >
                            Edit code
                        </button>
                    </span>
                )}
            </Show>
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
                                disabled={!canInsert()}
                                title={
                                    inCell()
                                        ? 'Fields inside table cells aren’t supported yet'
                                        : f.hint
                                }
                                onClick={() => void insert(f.kind)}
                            >
                                <span>{f.label}</span>
                            </button>
                        </li>
                    ))}
                    <li role="separator" class="nge-fields__sep" />
                    <li role="none">
                        <button
                            role="menuitem"
                            class="nge-fields__item"
                            type="button"
                            disabled={!canInsertToc()}
                            title={
                                canInsertToc()
                                    ? 'Insert a table of contents built from the document’s headings (levels 1–3, linked, with page numbers)'
                                    : 'A table of contents goes in the document body — not in a table, header, footer or note'
                            }
                            onClick={() => void insertToc()}
                        >
                            <span>Table of Contents</span>
                        </button>
                    </li>
                    <li role="separator" class="nge-fields__sep" />
                    <li role="none">
                        <button
                            role="menuitem"
                            class="nge-fields__item"
                            type="button"
                            title="Re-resolve every field in the document (F9)"
                            onClick={() => void update()}
                        >
                            <span>Update fields</span>
                            <kbd class="nge-fields__kbd">F9</kbd>
                        </button>
                    </li>
                    <li role="none">
                        <button
                            role="menuitemcheckbox"
                            aria-checked={codes()}
                            class={`nge-fields__item${codes() ? ' nge-fields__item--active' : ''}`}
                            type="button"
                            title="Show field codes instead of results (Alt+F9)"
                            onClick={() => void toggleCodes()}
                        >
                            <span>{codes() ? 'Show field results' : 'Show field codes'}</span>
                            <kbd class="nge-fields__kbd">Alt+F9</kbd>
                        </button>
                    </li>
                </ul>
            </Show>
            <Show when={editing() && field()}>
                <form
                    class="nge-fields__editor"
                    role="dialog"
                    aria-label="Field code"
                    onSubmit={(e) => {
                        e.preventDefault();
                        void applyCode();
                    }}
                >
                    <label class="nge-fields__editor-label">
                        <span>Field code</span>
                        <input
                            class="nge-fields__input"
                            type="text"
                            value={draft()}
                            spellcheck={false}
                            autocomplete="off"
                            onInput={(e) => setDraft(e.currentTarget.value)}
                            onKeyDown={(e) => {
                                if (e.key === 'Escape') {
                                    e.preventDefault();
                                    cancelCode();
                                }
                            }}
                        />
                    </label>
                    <div class="nge-fields__editor-actions">
                        <button
                            class="nge-btn nge-fields__editor-btn"
                            type="submit"
                            disabled={draft().trim().length === 0}
                            title="Replace the field code (the result updates on the next F9)"
                        >
                            Apply
                        </button>
                        <button
                            class="nge-btn nge-fields__editor-btn"
                            type="button"
                            onClick={cancelCode}
                        >
                            Cancel
                        </button>
                    </div>
                </form>
            </Show>
        </div>
    );
};
