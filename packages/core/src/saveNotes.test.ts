/* Issue #485 - wording of the writer's save report (`lastSaveNotes()`),
 * and the reactive signal itself (fed by `DOCUMENT_SAVED`). The first half
 * is pure; the second drives `createEditorState` against a scripted engine
 * handle (`useEngine` mocked: `EngineProvider.tsx` is JSX the node-side
 * vitest does not compile). */
import { createRoot } from 'solid-js';
import { describe, expect, it, vi } from 'vitest';
import type { Event, SaveNote } from './types';
import { describeSaveNote, saveNoteCount, saveNoteKind, saveNotices } from './saveNotes';

const replaced = (count: number): SaveNote => ({ type: 'FORBIDDEN_CHARACTERS_REPLACED', count });
const clamped = (markers: number): SaveNote => ({ type: 'STALE_MARKUP_CLAMPED', markers });
const widened = (count: number): SaveNote => ({ type: 'INLINE_WRAPPER_WIDENED', count });

describe('describeSaveNote (#485)', () => {
    it('says how many characters were replaced, singular and plural', () => {
        expect(describeSaveNote(replaced(1))).toBe(
            '1 character could not be saved and was replaced.',
        );
        expect(describeSaveNote(replaced(3))).toBe(
            '3 characters could not be saved and were replaced.',
        );
    });

    it('words the two markup repairs', () => {
        expect(describeSaveNote(clamped(2))).toContain('2 form-field or content-control markers');
        expect(describeSaveNote(clamped(1))).toContain('1 form-field or content-control marker ');
        expect(describeSaveNote(widened(1))).toBe(
            '1 content control was widened to keep the file valid.',
        );
        expect(describeSaveNote(widened(4))).toContain('4 content controls were widened');
    });

    it('classifies every note type as its telemetry kind', () => {
        expect(saveNoteKind(replaced(1))).toBe('ForbiddenCharactersReplaced');
        expect(saveNoteKind(clamped(1))).toBe('StaleMarkupClamped');
        expect(saveNoteKind(widened(1))).toBe('InlineWrapperWidened');
    });
});

describe('saveNoteCount / saveNotices (#485)', () => {
    it('counts every occurrence across kinds', () => {
        expect(saveNoteCount([])).toBe(0);
        expect(saveNoteCount([replaced(3), clamped(2), widened(1)])).toBe(6);
    });

    it('a clean save warrants no notice', () => {
        expect(saveNotices([])).toEqual([]);
    });

    it('a repaired save is ONE save-notes notice with one line per note', () => {
        const [n, ...rest] = saveNotices([replaced(2), widened(1)]);
        expect(rest).toEqual([]);
        expect(n?.kind).toBe('save-notes');
        expect(n?.lines).toEqual([describeSaveNote(replaced(2)), describeSaveNote(widened(1))]);
        expect(n?.lines?.[0]).toContain('2 characters could not be saved and were replaced');
        expect(n?.action.length).toBeGreaterThan(5);
    });
});

/* ---- createEditorState().lastSaveNotes() ------------------------------- */

const listeners: Array<(e: Event) => void> = [];
const fakeEngine = {
    renderer: 'canvas2d',
    subscribe(fn: (e: Event) => void): () => void {
        listeners.push(fn);
        return () => {};
    },
};
vi.mock('./EngineProvider', () => ({ useEngine: () => fakeEngine }));

const emit = (e: unknown): void => {
    for (const l of listeners) l(e as Event);
};

describe('createEditorState().lastSaveNotes (#485)', () => {
    it('follows DOCUMENT_SAVED, replaced by every save and cleared by a new document', async () => {
        const { createEditorState } = await import('./createEditorState');
        const dispose = createRoot((d) => {
            const state = createEditorState();
            expect(state.lastSaveNotes()).toEqual([]);

            emit({ type: 'DOCUMENT_SAVED', bytes: new Uint8Array(), size: 0, notes: [replaced(3)] });
            expect(state.lastSaveNotes()).toEqual([replaced(3)]);
            const first = state.lastSaveNotes();

            /* The same notes on the NEXT save are a NEW array - a dismissed
               notice keyed on identity shows again. */
            emit({ type: 'DOCUMENT_SAVED', bytes: new Uint8Array(), size: 0, notes: [replaced(3)] });
            expect(state.lastSaveNotes()).toEqual([replaced(3)]);
            expect(state.lastSaveNotes()).not.toBe(first);

            /* A faithful save (no `notes` on the wire) and a text export
               clear the report. */
            emit({ type: 'DOCUMENT_SAVED', bytes: new Uint8Array(), size: 0 });
            expect(state.lastSaveNotes()).toEqual([]);

            emit({ type: 'DOCUMENT_SAVED', bytes: new Uint8Array(), size: 0, notes: [clamped(1)] });
            expect(state.lastSaveNotes()).toHaveLength(1);
            /* A different document leaves nothing to report about. */
            emit({ type: 'DOCUMENT_LOADED', paragraph_count: 1 });
            expect(state.lastSaveNotes()).toEqual([]);

            emit({ type: 'DOCUMENT_SAVED', bytes: new Uint8Array(), size: 0, notes: [widened(2)] });
            emit({ type: 'DOCUMENT_CLOSED' });
            expect(state.lastSaveNotes()).toEqual([]);
            return d;
        });
        dispose();
    });
});
