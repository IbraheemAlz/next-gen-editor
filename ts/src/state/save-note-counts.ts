/* Issue #485 - the telemetry `DOC_SAVE` sample's per-kind save-note
 * counts: `bridge::save_note_counts`, mirrored. Pure (no DOM, no engine)
 * so it is unit-testable on its own. */
import type { SaveNote, SaveNoteKind } from '../engine/types';

/** Issue #485 - mirrors `bridge::SaveNoteCount`. */
export interface SaveNoteCount {
    kind: SaveNoteKind;
    count: number;
}

/** `SaveNoteKind` declaration order (the Rust `Ord`): a `Record`, so a new
 *  bridge kind fails `tsc` until it is ranked here. */
const KIND_RANK: Record<SaveNoteKind, number> = {
    ForbiddenCharactersReplaced: 0,
    StaleMarkupClamped: 1,
    InlineWrapperWidened: 2,
};

/** The class and occurrence count of one wire note. */
function classify(n: SaveNote): [SaveNoteKind, number] {
    switch (n.type) {
        case 'FORBIDDEN_CHARACTERS_REPLACED':
            return ['ForbiddenCharactersReplaced', n.count];
        case 'STALE_MARKUP_CLAMPED':
            return ['StaleMarkupClamped', n.markers];
        case 'INLINE_WRAPPER_WIDENED':
            return ['InlineWrapperWidened', n.count];
    }
}

/** Issue #485 - `bridge::save_note_counts`: fold a save's notes by kind
 *  (a note counts at least once), kinds in ascending declaration order,
 *  each kind once. */
export function saveNoteCounts(notes: readonly SaveNote[]): SaveNoteCount[] {
    const byKind = new Map<SaveNoteKind, number>();
    for (const n of notes) {
        const [kind, count] = classify(n);
        byKind.set(kind, (byKind.get(kind) ?? 0) + Math.max(1, count));
    }
    return [...byKind]
        .sort(([a], [b]) => (KIND_RANK[a] ?? 99) - (KIND_RANK[b] ?? 99))
        .map(([kind, count]) => ({ kind, count }));
}
