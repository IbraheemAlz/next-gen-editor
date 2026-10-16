import { describe, expect, it } from 'vitest';
import type { RevisionSnapshot } from '@nge/core';
import { locationLabel } from './revisionLocation';

const rev = (extra: Partial<RevisionSnapshot>): RevisionSnapshot => ({
    block: 2,
    start: 3,
    end: 7,
    kind: 'insert',
    author: 'Bob',
    date: '',
    ...extra,
});

describe('locationLabel (issue #440)', () => {
    it('reads a body revision as block + range', () => {
        expect(locationLabel(rev({}))).toBe('block 2 · 3–7');
    });

    it('marks a change inside a top-level table', () => {
        expect(locationLabel(rev({ depth: 1 }))).toBe('block 2 · table · 3–7');
        expect(locationLabel(rev({ depth: 1, row: 0, start: 0, end: 0 }))).toBe(
            'block 2 · table · row 1',
        );
    });

    it('names the nesting depth of a change in a nested table', () => {
        expect(locationLabel(rev({ depth: 3 }))).toBe('block 2 · table depth 3 · 3–7');
        expect(locationLabel(rev({ depth: 2, row: 1 }))).toBe('block 2 · table depth 2 · row 2');
    });
});
