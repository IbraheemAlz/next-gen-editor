/* Issue #406 - the DOC_OPEN sample's per-kind reader-warning counts mirror
 * `bridge::read_warning_counts` (kinds in declaration order, an entry
 * counting `count` times, a missing count as 1). */
import { describe, expect, it } from 'vitest';
import type { ReadWarning } from '../engine/types';
import { readWarningCounts } from './read-warning-counts';

const w = (kind: ReadWarning['kind'], count?: number): ReadWarning =>
    count === undefined
        ? ({ kind, detail: 'd' } as ReadWarning)
        : { kind, detail: 'd', count };

describe('readWarningCounts (#406)', () => {
    it('is empty for a clean open', () => {
        expect(readWarningCounts([])).toEqual([]);
    });

    it('folds by kind in declaration order, summing coalesced counts', () => {
        expect(
            readWarningCounts([
                w('NonCanonicalNamespaces', 1),
                w('MeasureClamped', 2),
                w('InvalidMeasure'),
                w('MeasureClamped', 1),
                w('TableNestingTooDeep', 0),
            ]),
        ).toEqual([
            { kind: 'TableNestingTooDeep', count: 1 },
            { kind: 'InvalidMeasure', count: 1 },
            { kind: 'MeasureClamped', count: 3 },
            { kind: 'NonCanonicalNamespaces', count: 1 },
        ]);
    });
});
