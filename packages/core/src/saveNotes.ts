/**
 * Issue #485 — turn the writer's save report (`DOCUMENT_SAVED.notes`, kept
 * by `createEditorState().lastSaveNotes()`) into user-facing text.
 *
 * `format_docx` repairs a few things at save time instead of failing: a
 * character XML cannot carry is written as U+FFFD, stale markup is written
 * at a clamped position, a content control is widened around a regenerated
 * wrapper. Before #485 nothing told the user; a file that was repaired
 * looked exactly like a faithful one. Honest UX: say what changed.
 *
 * Headless and pure (no DOM, no engine): `@nge/ui`'s `RecoveryBanner`
 * renders the notice, a host with its own chrome can word it differently.
 */
import type { RecoveryNotice } from './recovery';
import type { SaveNote, SaveNoteKind } from './types';

/** How many occurrences the report stands for (all kinds). */
export function saveNoteCount(notes: readonly SaveNote[]): number {
    let total = 0;
    for (const n of notes) total += noteCount(n);
    return total;
}

function noteCount(n: SaveNote): number {
    switch (n.type) {
        case 'FORBIDDEN_CHARACTERS_REPLACED':
            return n.count;
        case 'STALE_MARKUP_CLAMPED':
            return n.markers;
        case 'INLINE_WRAPPER_WIDENED':
            return n.count;
    }
}

function plural(n: number, one: string, many: string): string {
    return n === 1 ? one : many;
}

/** The PascalCase class of a note (the telemetry code). A `Record`, so a
 *  new bridge variant fails `tsc` until it is classified here. */
const NOTE_KIND: Record<SaveNote['type'], SaveNoteKind> = {
    FORBIDDEN_CHARACTERS_REPLACED: 'ForbiddenCharactersReplaced',
    STALE_MARKUP_CLAMPED: 'StaleMarkupClamped',
    INLINE_WRAPPER_WIDENED: 'InlineWrapperWidened',
};

export function saveNoteKind(n: SaveNote): SaveNoteKind {
    return NOTE_KIND[n.type];
}

/** One line for one note: what was changed in the saved file. */
export function describeSaveNote(n: SaveNote): string {
    switch (n.type) {
        case 'FORBIDDEN_CHARACTERS_REPLACED':
            return (
                `${n.count} ${plural(n.count, 'character', 'characters')} could not be saved ` +
                `and ${plural(n.count, 'was', 'were')} replaced.`
            );
        case 'STALE_MARKUP_CLAMPED':
            return (
                `${n.markers} form-field or content-control ` +
                `${plural(n.markers, 'marker', 'markers')} could not be placed exactly and ` +
                `${plural(n.markers, 'was', 'were')} moved to the nearest valid position.`
            );
        case 'INLINE_WRAPPER_WIDENED':
            return (
                `${n.count} content ${plural(n.count, 'control', 'controls')} ` +
                `${plural(n.count, 'was', 'were')} widened to keep the file valid.`
            );
    }
}

/**
 * The notice a save report warrants; `[]` for a clean save (and for an
 * empty report). One line per note kind, in the order the engine sent.
 */
export function saveNotices(notes: readonly SaveNote[]): RecoveryNotice[] {
    if (notes.length === 0) return [];
    return [
        {
            kind: 'save-notes',
            title: 'Your file was saved with repairs',
            detail: 'The saved file differs from the document on screen:',
            lines: notes.map(describeSaveNote),
            action: 'Review the affected text, then save again.',
        },
    ];
}
