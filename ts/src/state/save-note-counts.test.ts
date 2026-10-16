/* Issue #485 - the DOC_SAVE sample's per-kind save-note counts mirror
 * `bridge::save_note_counts` (kinds in declaration order, an entry
 * counting at least once). */
import { describe, expect, it } from 'vitest';
import type { SaveNote } from '../engine/types';
import { saveNoteCounts } from './save-note-counts';

describe('saveNoteCounts (#485)', () => {
    it('is empty for a faithful save', () => {
        expect(saveNoteCounts([])).toEqual([]);
    });

    it('folds by kind in declaration order, summing repeats', () => {
        const notes: SaveNote[] = [
            { type: 'INLINE_WRAPPER_WIDENED', count: 1 },
            { type: 'STALE_MARKUP_CLAMPED', markers: 2 },
            { type: 'FORBIDDEN_CHARACTERS_REPLACED', count: 3 },
            { type: 'STALE_MARKUP_CLAMPED', markers: 1 },
            { type: 'INLINE_WRAPPER_WIDENED', count: 0 },
        ];
        expect(saveNoteCounts(notes)).toEqual([
            { kind: 'ForbiddenCharactersReplaced', count: 3 },
            { kind: 'StaleMarkupClamped', count: 3 },
            { kind: 'InlineWrapperWidened', count: 2 },
        ]);
    });
});
