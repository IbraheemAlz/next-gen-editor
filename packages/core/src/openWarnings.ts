/**
 * Issue #406 — turn the reader's warning report (`DOCUMENT_LOADED.warnings`,
 * `createEditorState().openWarnings()`) into user-facing text.
 *
 * The engine reports every non-fatal diagnostic of an open — a page margin
 * clamped into range, a measure that was ignored, a part read through
 * namespace normalisation (regenerate-only on save), a field closed early,
 * a relationship that escaped the package — as a typed `ReadWarningKind`
 * plus a one-line `detail` (the attribute and its raw value, the limit,
 * the target). Honest UX: a document that opened degraded must not look
 * like a clean open. Headless and pure: `@nge/ui`'s `OpenWarningsBanner`
 * renders these, and a host with its own chrome can render them however
 * it likes.
 */
import type { ReadWarning, ReadWarningKind } from './types';

/** One sentence per warning class: what happened and what it means. */
export const READ_WARNING_LABELS: Record<ReadWarningKind, string> = {
    TableNestingTooDeep:
        'A table is nested too deeply to edit; it is kept as-is and saved unchanged.',
    InvalidMeasure: 'A measurement could not be read; the default value is used instead.',
    MeasureClamped: 'A measurement was out of range and has been limited to the nearest valid value.',
    UnclosedField: 'A field was not closed properly; it was closed at the end of its paragraph.',
    StrayFieldChar: 'A stray field marker with no field was ignored.',
    FieldNestingTooDeep: 'Fields are nested too deeply; the innermost levels are shown as code.',
    NonCanonicalNamespaces:
        'A part of the file uses unusual XML namespace prefixes; it was read after normalising them and will be rewritten on save.',
    NotWordprocessingMl: 'The main document part is not a Word document body; it opened empty.',
    MainPartFallback:
        "The file's main-part relationship points at a missing part; the standard location was used.",
    UnsafeRelationshipTarget: 'A relationship pointing outside the file was ignored.',
    MalformedPart:
        'A part of the file is not well-formed XML; what could be repaired was repaired, and a repaired part will be rewritten on save.',
    LegacyContentNotImported: 'Not imported from this Word 97-2003 (.doc) file',
};

/** Issue #567 — a class of `.doc` content the importer does not model:
 *  one issue per class (its `count` is how many occurrences, shown in the
 *  details line), not one per table cell. */
const PER_CLASS_KINDS: ReadonlySet<ReadWarning['kind']> = new Set(['LegacyContentNotImported']);

/** Total number of issues (coalesced entries count once per occurrence;
 *  a per-class entry counts once). */
export function openWarningCount(warnings: readonly ReadWarning[]): number {
    return warnings.reduce(
        (n, w) => n + (PER_CLASS_KINDS.has(w.kind) ? 1 : Math.max(1, w.count ?? 1)),
        0,
    );
}

/** The banner's one-sentence summary: a document imported from the legacy
 *  `.doc` format says so (and that a save writes `.docx`, issue #567);
 *  any other degraded open keeps the #406 wording. */
export function openWarningSummary(warnings: readonly ReadWarning[]): string {
    if (warnings.some((w) => w.kind === 'LegacyContentNotImported')) {
        return (
            'This document was imported from the Word 97-2003 (.doc) format. ' +
            'Content that is not supported yet is listed below; saving writes a .docx file.'
        );
    }
    return 'Some values in this file could not be read as written and were corrected or ignored.';
}

/**
 * One line for a details list: the class label, then the specifics
 * (`w:pgMar/@w:top = "99999" → 31680 twips`), the part when the reader
 * named one, and the repeat count.
 */
export function describeReadWarning(w: ReadWarning): string {
    const label = READ_WARNING_LABELS[w.kind] ?? w.kind;
    const where = w.part ? ` [${w.part}]` : '';
    const detail = w.detail ? ` (${w.detail})` : '';
    const times = (w.count ?? 1) > 1 ? ` ×${w.count}` : '';
    return `${label}${detail}${where}${times}`;
}
