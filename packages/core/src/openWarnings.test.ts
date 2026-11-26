/* Issue #406 - wording of the reader's warning report for the open
 * document (`openWarnings()`). Pure functions: no engine, no DOM. */
import { describe, expect, it } from 'vitest';
import {
    describeReadWarning,
    openWarningCount,
    openWarningSummary,
    READ_WARNING_LABELS,
} from './openWarnings';
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
        /* Issues #439 / #434 / #435 added `MalformedPart`, #567
         * `LegacyContentNotImported`. */
        expect(Object.keys(READ_WARNING_LABELS)).toHaveLength(12);
    });
});

describe('a legacy .doc import report (#567)', () => {
    const tables: ReadWarning = {
        kind: 'LegacyContentNotImported',
        detail: 'tables: their cell text was imported as plain paragraphs',
        count: 47,
    };
    const styles: ReadWarning = {
        kind: 'LegacyContentNotImported',
        detail: 'paragraph and character styles and fonts: only direct formatting was imported',
        count: 1,
    };

    it('counts one issue per class, not per table cell', () => {
        expect(openWarningCount([tables, styles])).toBe(2);
        expect(openWarningCount([tables, clamped])).toBe(2);
    });

    it('says the document came from a .doc and that a save writes .docx', () => {
        const summary = openWarningSummary([tables]);
        expect(summary).toContain('Word 97-2003 (.doc)');
        expect(summary).toContain('.docx');
        expect(openWarningSummary([clamped])).toContain('corrected or ignored');
    });

    it('names the class and how many occurrences', () => {
        expect(describeReadWarning(tables)).toBe(
            `${READ_WARNING_LABELS.LegacyContentNotImported} (${tables.detail}) ×47`,
        );
    });
});
