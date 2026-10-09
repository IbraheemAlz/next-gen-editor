/* Issue #406 - wording of the reader's warning report for the open
 * document (`openWarnings()`). Pure functions: no engine, no DOM. */
import { describe, expect, it } from 'vitest';
import { describeReadWarning, openWarningCount, READ_WARNING_LABELS } from './openWarnings';
import type { ReadWarning } from './types';

const clamped: ReadWarning = {
    kind: 'MeasureClamped',
    detail: 'w:pgMar/@w:top = "99999" → 31680 twips',
    count: 1,
};

describe('openWarningCount (#406)', () => {
    it('counts every occurrence of a coalesced entry, a missing count as 1', () => {
        expect(openWarningCount([])).toBe(0);
        expect(openWarningCount([clamped])).toBe(1);
        expect(
            openWarningCount([
                { ...clamped, count: 3 },
                { kind: 'InvalidMeasure', detail: 'x' } as ReadWarning,
            ]),
        ).toBe(4);
    });
});

describe('describeReadWarning (#406)', () => {
    it('leads with the class label and carries the detail', () => {
        expect(describeReadWarning(clamped)).toBe(
            `${READ_WARNING_LABELS.MeasureClamped} (w:pgMar/@w:top = "99999" → 31680 twips)`,
        );
    });

    it('names the part and the repeat count when there are any', () => {
        const ns: ReadWarning = {
            kind: 'NonCanonicalNamespaces',
            part: 'word/styles.xml',
            detail: 'WordprocessingML bound to `x:`',
            count: 2,
        };
        const line = describeReadWarning(ns);
        expect(line.startsWith(READ_WARNING_LABELS.NonCanonicalNamespaces)).toBe(true);
        expect(line).toContain('[word/styles.xml]');
        expect(line.endsWith('×2')).toBe(true);
    });

    it('has a label for every kind', () => {
        for (const label of Object.values(READ_WARNING_LABELS)) {
            expect(label.length).toBeGreaterThan(10);
        }
        /* Issues #439 / #434 / #435 added `MalformedPart`. */
        expect(Object.keys(READ_WARNING_LABELS)).toHaveLength(11);
    });
});
