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
};

/** Total number of issues (coalesced entries count once per occurrence). */
export function openWarningCount(warnings: readonly ReadWarning[]): number {
    return warnings.reduce((n, w) => n + Math.max(1, w.count ?? 1), 0);
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
